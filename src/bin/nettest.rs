//! Minimal network diagnostic, built alongside the main binary.
//!
//! On 32-bit PowerPC musl every reactor-driven operation hangs. A tokio built
//! with tracing on the reactor path narrows it to a single call: the I/O driver
//! receives the epoll event with the correct token, resolves it to the right
//! `ScheduledIo`, and then never returns from
//!
//!     self.readiness.fetch_update(AcqRel, Acquire, |curr| ...)
//!
//! `readiness` is an `AtomicUsize`, native on this target, and `fetch_update`
//! is a plain `compare_exchange_weak` retry loop in userspace. A loop that
//! never converges there makes no syscalls at all, which is why strace showed
//! 224 calls and then silence while the process kept burning CPU — it is not
//! blocked, it is spinning.
//!
//! Usage: nettest [host] [port] [stage]
//!        nettest --self [stage]      loopback only, no external network
//!
//! Stages run in order of increasing risk, and each can be selected on its own
//! so that one which hangs cannot hide the results of the others:
//!
//!   atomic  AtomicUsize CAS primitives on their own — no I/O, no tokio
//!   std     blocking resolve, connect and request — the baseline
//!   mio     raw mio: poll, register, connect — no tokio at all
//!   noto    tokio I/O with no timer involved at all
//!   ct      tokio, current_thread runtime
//!   hybrid  blocking connect handed to tokio via from_std, async request
//!   mt      tokio, multi-thread runtime with 1 and 2 workers
//!   all     every stage in that order (default)

use std::cell::Cell;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, ToSocketAddrs as _};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Set by any stage that fails or stalls, so the exit code reflects it.
static FAILED: AtomicBool = AtomicBool::new(false);

fn fail(msg: String) {
    FAILED.store(true, Ordering::Relaxed);
    eprintln!("{msg}");
}

fn step(name: &str) {
    eprintln!("[{name}] ...");
}

fn ok(name: &str, started: Instant) {
    eprintln!("[{name}] ok in {:?}", started.elapsed());
}

/// Writes a request and reads the first bytes of the response, asynchronously.
async fn async_request(s: &mut tokio::net::TcpStream, host: &str) -> std::io::Result<String> {
    let req = format!("GET / HTTP/1.0\r\nHost: {host}\r\n\r\n");
    s.write_all(req.as_bytes()).await?;
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf[..n.min(40)]).into_owned())
}

/// Connects and requests on the current runtime, each half bounded by a timeout
/// so a stall is reported rather than waited on forever.
async fn tokio_probe(label: &str, addr: SocketAddr, host: &str) {
    step(&format!("{label}: connect (10s timeout)"));
    let t = Instant::now();
    match tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    {
        Ok(Ok(mut s)) => {
            ok(&format!("{label}: connect"), t);

            step(&format!("{label}: request (10s timeout)"));
            let t = Instant::now();
            match tokio::time::timeout(Duration::from_secs(10), async_request(&mut s, host)).await {
                Ok(Ok(head)) => {
                    ok(&format!("{label}: request"), t);
                    eprintln!("       -> {head:?}");
                }
                Ok(Err(e)) => fail(format!("[{label}: request] FAILED: {e}")),
                Err(_) => fail(format!("[{label}: request] TIMED OUT after 10s")),
            }
        }
        Ok(Err(e)) => fail(format!("[{label}: connect] FAILED: {e}")),
        Err(_) => fail(format!("[{label}: connect] TIMED OUT after 10s")),
    }
}

/// Serves one canned HTTP response on a loopback port, so the tokio stages can
/// run with no external network — which is what makes this usable from CI.
fn spawn_loopback() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("loopback addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 512];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    addr
}

/// How many attempts each atomic probe gets before it gives up and reports a
/// failure. An uncontended CAS should succeed on the first or second try, so
/// anything that exhausts this is not making progress at all.
const ATOMIC_TRIES: u32 = 1_000_000;

/// Exercises the atomic primitives the reactor depends on, with every loop
/// bounded so a broken one reports instead of hanging.
///
/// `ScheduledIo::set_readiness` is `AtomicUsize::fetch_update`, which is a
/// `compare_exchange_weak` retry loop. Nothing else is involved: no I/O, no
/// tokio, no threads, and the atomic is uncontended, so each of these should
/// succeed immediately. If `atomic` fails here while `std` and `mio` pass, the
/// fault is in the target's atomics rather than anywhere in tokio.
fn atomic_probe() {
    use Ordering::{AcqRel, Acquire, Relaxed, Release};

    // The exact call the driver makes, with the same orderings and the same
    // starting value, but with the closure counting its own iterations so it
    // can bail out rather than spin forever.
    let cell = AtomicUsize::new(0);
    let iters = Cell::new(0u32);
    let t = Instant::now();
    let res = cell.fetch_update(AcqRel, Acquire, |curr| {
        iters.set(iters.get() + 1);
        if iters.get() > ATOMIC_TRIES {
            None
        } else {
            // TICK.pack(1, WRITABLE) — what set_readiness stores for a
            // writable event on a freshly registered socket.
            Some((curr & !0xffff) | 0x1_0002)
        }
    });
    match res {
        Ok(_) if cell.load(Relaxed) == 0x1_0002 => {
            eprintln!(
                "[atomic: fetch_update] ok in {:?}, {} closure call(s)",
                t.elapsed(),
                iters.get()
            );
        }
        Ok(prev) => fail(format!(
            "[atomic: fetch_update] WRONG: returned Ok({prev:#x}) but the cell holds {:#x}",
            cell.load(Relaxed)
        )),
        Err(_) => fail(format!(
            "[atomic: fetch_update] NO PROGRESS: gave up after {} closure calls in {:?}, \
             cell still {:#x} — compare_exchange_weak never succeeded",
            iters.get(),
            t.elapsed(),
            cell.load(Relaxed)
        )),
    }

    // The same thing one level down, to separate a broken compare_exchange_weak
    // from a broken fetch_update built on a working one.
    let cell = AtomicUsize::new(0);
    let mut tries = 0u32;
    let mut spurious = 0u32;
    let t = Instant::now();
    let outcome = loop {
        if tries >= ATOMIC_TRIES {
            break Err(format!("{spurious} spurious failures and never succeeded"));
        }
        tries += 1;
        match cell.compare_exchange_weak(0, 0x1_0002, AcqRel, Acquire) {
            Ok(_) => break Ok(()),
            // An uncontended CAS can fail spuriously, but the value it hands
            // back must still be the one it compared against. Anything else
            // means the comparison itself is wrong.
            Err(0) => spurious += 1,
            Err(other) => break Err(format!("compared against 0 but was given back {other:#x}")),
        }
    };
    match outcome {
        Ok(()) => eprintln!(
            "[atomic: compare_exchange_weak] ok in {:?}, {tries} try/tries, {spurious} spurious",
            t.elapsed()
        ),
        Err(why) => fail(format!(
            "[atomic: compare_exchange_weak] NO PROGRESS after {tries} tries in {:?}: {why}",
            t.elapsed()
        )),
    }

    // The strong form, which must not fail spuriously at all.
    let cell = AtomicUsize::new(0);
    match cell.compare_exchange(0, 0x1_0002, AcqRel, Acquire) {
        Ok(_) => eprintln!("[atomic: compare_exchange] ok"),
        Err(other) => fail(format!(
            "[atomic: compare_exchange] FAILED on an uncontended cell, was given back {other:#x}"
        )),
    }

    // Single-instruction read-modify-writes, which the rest of the runtime and
    // every Arc lean on. If these work while the CAS forms do not, the fault is
    // specific to compare-and-swap rather than to atomics in general.
    let cell = AtomicUsize::new(0);
    cell.fetch_or(0x2, AcqRel);
    cell.fetch_add(0x1_0000, AcqRel);
    let got = cell.load(Acquire);
    if got == 0x1_0002 {
        eprintln!("[atomic: fetch_or + fetch_add] ok");
    } else {
        fail(format!(
            "[atomic: fetch_or + fetch_add] WRONG: expected 0x10002, got {got:#x}"
        ));
    }

    // Loads and stores, for completeness — if these were broken nothing at all
    // would run, so this is the sanity check on the probe itself.
    let cell = AtomicUsize::new(0);
    cell.store(0x1_0002, Release);
    let got = cell.load(Acquire);
    if got == 0x1_0002 {
        eprintln!("[atomic: store + load] ok");
    } else {
        fail(format!(
            "[atomic: store + load] WRONG: expected 0x10002, got {got:#x}"
        ));
    }
}

/// Connects with mio directly and waits for writability, using nothing from
/// tokio. Returns once the connection is established or the poll times out.
fn raw_mio_probe(addr: SocketAddr) -> std::io::Result<()> {
    use mio::{Events, Interest, Poll, Token};

    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(16);
    let mut sock = mio::net::TcpStream::connect(addr)?;
    poll.registry()
        .register(&mut sock, Token(0), Interest::WRITABLE)?;

    poll.poll(&mut events, Some(Duration::from_secs(10)))?;
    for event in events.iter() {
        if event.token() == Token(0) && event.is_writable() {
            // A connect error surfaces here rather than from connect() itself.
            return match sock.take_error()? {
                Some(e) => Err(e),
                None => Ok(()),
            };
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "no writable event within 10s",
    ))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut host = args.next().unwrap_or_else(|| "librespeed.org".to_string());
    let self_test = host == "--self";
    if self_test {
        host = "127.0.0.1".to_string();
    }
    let host = host;
    let port: u16 = if self_test {
        0
    } else {
        args.next().and_then(|p| p.parse().ok()).unwrap_or(80)
    };
    let stage = args.next().unwrap_or_else(|| "all".to_string());

    eprintln!("stage: {stage}  (atomic | std | mio | noto | ct | hybrid | mt | all)");

    // The atomic probe touches no sockets, so on its own it needs neither a
    // loopback listener nor name resolution. That makes it runnable anywhere,
    // including on targets where the network stages would be pointless.
    if stage == "atomic" {
        eprintln!("--- atomics: no I/O, no tokio ---");
        atomic_probe();
        if FAILED.load(Ordering::Relaxed) {
            eprintln!("done, with failures");
            std::process::exit(1);
        }
        eprintln!("done");
        std::process::exit(0);
    }

    if self_test {
        let addr = spawn_loopback();
        eprintln!("self test against {addr}");
        return run_stages(addr, &host, &stage);
    }

    // Resolution is blocking and known to work, so it happens once up front and
    // every stage below shares the result.
    step("resolve");
    let t = Instant::now();
    let addrs: Vec<_> = match (host.as_str(), port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => {
            fail(format!("[resolve] FAILED: {e}"));
            std::process::exit(1);
        }
    };
    ok("resolve", t);
    eprintln!("       -> {addrs:?}");
    run_stages(addrs[0], &host, &stage);
}

fn run_stages(addr: SocketAddr, host: &str, stage: &str) {
    let want = |s: &str| stage == "all" || stage == s;

    // atomic: the primitives underneath everything else, with no I/O at all.
    //         The reactor hangs inside AtomicUsize::fetch_update on 32-bit
    //         PowerPC, so this runs first and answers whether the fault is in
    //         tokio or in the target's compare-and-swap.
    if want("atomic") {
        eprintln!("--- atomics: no I/O, no tokio ---");
        atomic_probe();
    }

    // std: the baseline. If this fails, nothing below is meaningful.
    if want("std") {
        eprintln!("--- std: blocking ---");
        step("std: connect");
        let t = Instant::now();
        match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
            Ok(mut s) => {
                ok("std: connect", t);
                step("std: request");
                let t = Instant::now();
                let req = format!("GET / HTTP/1.0\r\nHost: {host}\r\n\r\n");
                let _ = s.write_all(req.as_bytes());
                let mut buf = [0u8; 64];
                match s.read(&mut buf) {
                    Ok(n) => {
                        ok("std: request", t);
                        eprintln!("       -> {:?}", String::from_utf8_lossy(&buf[..n.min(40)]));
                    }
                    Err(e) => fail(format!("[std: request] FAILED: {e}")),
                }
            }
            Err(e) => fail(format!("[std: connect] FAILED: {e}")),
        }
    }

    // mio: the layer under tokio, driven by hand. This is the question a
    //      maintainer will ask first — whether the fault is in mio's epoll
    //      handling or in tokio's use of it — and it decides where the bug
    //      report belongs.
    if want("mio") {
        eprintln!("--- raw mio: no tokio ---");
        step("mio: connect + wait for writable (10s)");
        let t = Instant::now();
        match raw_mio_probe(addr) {
            Ok(()) => ok("mio: connect", t),
            Err(e) => fail(format!("[mio: connect] FAILED: {e}")),
        }
    }

    // noto: the same I/O with no tokio::time::timeout anywhere. Every stage
    //       that hangs so far wrapped the I/O in a timer, and the only stage
    //       that passed was a bare sleep with no I/O — so the timer is as much
    //       a suspect as the reactor. tokio's AtomicU64 falls back to a Mutex
    //       on targets without 64-bit atomics, and it is the timer entry state
    //       that uses it. No timeout here: interrupt if it hangs.
    if want("noto") {
        eprintln!("--- tokio: I/O without any timer (Ctrl+C if it hangs) ---");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        rt.block_on(async {
            step("noto: connect");
            let t = Instant::now();
            match tokio::net::TcpStream::connect(addr).await {
                Ok(mut s) => {
                    ok("noto: connect", t);
                    step("noto: request");
                    let t = Instant::now();
                    match async_request(&mut s, host).await {
                        Ok(head) => {
                            ok("noto: request", t);
                            eprintln!("       -> {head:?}");
                        }
                        Err(e) => fail(format!("[noto: request] FAILED: {e}")),
                    }
                }
                Err(e) => fail(format!("[noto: connect] FAILED: {e}")),
            }
        });
    }

    // ct: driver on the same thread, no cross-thread wakeups.
    if want("ct") {
        eprintln!("--- tokio: current_thread runtime ---");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(tokio_probe("current_thread", addr, host));
    }

    // hybrid: blocking connect, socket handed to tokio, request asynchronous.
    if want("hybrid") {
        eprintln!("--- hybrid: blocking connect, async request ---");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");

        rt.block_on(async {
            step("hybrid: blocking connect");
            let t = Instant::now();
            let std_stream = match tokio::task::spawn_blocking(move || {
                let s = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
                s.set_nonblocking(true)?;
                Ok::<_, std::io::Error>(s)
            })
            .await
            {
                Ok(Ok(s)) => {
                    ok("hybrid: blocking connect", t);
                    s
                }
                Ok(Err(e)) => return fail(format!("[hybrid: blocking connect] FAILED: {e}")),
                Err(e) => return fail(format!("[hybrid: blocking connect] JOIN FAILED: {e}")),
            };

            step("hybrid: from_std");
            let t = Instant::now();
            let mut s = match tokio::net::TcpStream::from_std(std_stream) {
                Ok(s) => {
                    ok("hybrid: from_std", t);
                    s
                }
                Err(e) => return fail(format!("[hybrid: from_std] FAILED: {e}")),
            };

            step("hybrid: async request (10s timeout)");
            let t = Instant::now();
            match tokio::time::timeout(Duration::from_secs(10), async_request(&mut s, host)).await {
                Ok(Ok(head)) => {
                    ok("hybrid: async request", t);
                    eprintln!("       -> {head:?}");
                }
                Ok(Err(e)) => fail(format!("[hybrid: async request] FAILED: {e}")),
                Err(_) => fail("[hybrid: async request] TIMED OUT after 10s".to_string()),
            }
        });
    }

    // mt: the configuration the program actually uses, and the one that hangs.
    if want("mt") {
        for workers in [1usize, 2] {
            eprintln!("--- tokio: multi_thread, {workers} worker(s) ---");
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(workers)
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(tokio_probe(&format!("mt{workers}"), addr, host));
        }
    }

    if FAILED.load(Ordering::Relaxed) {
        eprintln!("done, with failures");
        std::process::exit(1);
    }
    eprintln!("done");
    std::process::exit(0);
}
