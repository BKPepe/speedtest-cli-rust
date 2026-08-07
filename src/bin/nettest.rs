//! Minimal network diagnostic, built alongside the main binary.
//!
//! On 32-bit PowerPC musl every reactor-driven operation deadlocks: strace
//! shows epoll delivering the event with the correct token, the waking thread
//! then parking on a futex, and nothing polling again — 224 syscalls in total,
//! no spin. Timers and blocking I/O are unaffected, and it reproduces with LTO
//! off at opt-level 1, so it is not a codegen artefact.
//!
//! Usage: nettest [host] [port] [stage]
//!
//! Stages run in order of increasing risk, and each can be selected on its own
//! so that one which hangs cannot hide the results of the others:
//!
//!   std     blocking resolve, connect and request — the baseline
//!   ct      tokio, current_thread runtime
//!   hybrid  blocking connect handed to tokio via from_std, async request
//!   mt      tokio, multi-thread runtime with 1 and 2 workers
//!   all     every stage in that order (default)

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, ToSocketAddrs as _};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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
                Ok(Err(e)) => eprintln!("[{label}: request] FAILED: {e}"),
                Err(_) => eprintln!("[{label}: request] TIMED OUT after 10s"),
            }
        }
        Ok(Err(e)) => eprintln!("[{label}: connect] FAILED: {e}"),
        Err(_) => eprintln!("[{label}: connect] TIMED OUT after 10s"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "librespeed.org".to_string());
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(80);
    let stage = args.next().unwrap_or_else(|| "all".to_string());
    let want = |s: &str| stage == "all" || stage == s;

    eprintln!("stage: {stage}  (std | ct | hybrid | mt | all)");

    // Resolution is blocking and known to work, so it happens once up front and
    // every stage below shares the result.
    step("resolve");
    let t = Instant::now();
    let addrs: Vec<_> = match (host.as_str(), port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => {
            eprintln!("[resolve] FAILED: {e}");
            std::process::exit(1);
        }
    };
    ok("resolve", t);
    eprintln!("       -> {addrs:?}");
    let addr = addrs[0];

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
                    Err(e) => eprintln!("[std: request] FAILED: {e}"),
                }
            }
            Err(e) => eprintln!("[std: connect] FAILED: {e}"),
        }
    }

    // ct: driver on the same thread, no cross-thread wakeups.
    if want("ct") {
        eprintln!("--- tokio: current_thread runtime ---");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(tokio_probe("current_thread", addr, &host));
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
                Ok(Err(e)) => return eprintln!("[hybrid: blocking connect] FAILED: {e}"),
                Err(e) => return eprintln!("[hybrid: blocking connect] JOIN FAILED: {e}"),
            };

            step("hybrid: from_std");
            let t = Instant::now();
            let mut s = match tokio::net::TcpStream::from_std(std_stream) {
                Ok(s) => {
                    ok("hybrid: from_std", t);
                    s
                }
                Err(e) => return eprintln!("[hybrid: from_std] FAILED: {e}"),
            };

            step("hybrid: async request (10s timeout)");
            let t = Instant::now();
            match tokio::time::timeout(Duration::from_secs(10), async_request(&mut s, &host)).await
            {
                Ok(Ok(head)) => {
                    ok("hybrid: async request", t);
                    eprintln!("       -> {head:?}");
                }
                Ok(Err(e)) => eprintln!("[hybrid: async request] FAILED: {e}"),
                Err(_) => eprintln!("[hybrid: async request] TIMED OUT after 10s"),
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
            rt.block_on(tokio_probe(&format!("mt{workers}"), addr, &host));
        }
    }

    eprintln!("done");
    std::process::exit(0);
}
