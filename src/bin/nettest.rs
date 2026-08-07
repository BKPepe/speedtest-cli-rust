//! Minimal network diagnostic, built alongside the main binary.
//!
//! On 32-bit PowerPC musl the speed test stalls after the TCP connection is
//! established: strace shows `connect` returning EINPROGRESS, epoll reporting
//! EPOLLOUT, and then the task never being polled again. This walks the same
//! ground in stages so the failing layer can be identified without guessing.
//!
//! Usage: nettest [host] [port]

use std::io::{Read as _, Write as _};
use std::net::ToSocketAddrs as _;
use std::time::{Duration, Instant};

fn step(name: &str) {
    eprintln!("[{name}] ...");
}

fn ok(name: &str, started: Instant) {
    eprintln!("[{name}] ok in {:?}", started.elapsed());
}

fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "librespeed.org".to_string());
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(80);

    // 1. Blocking resolution and connection, for a baseline that does not
    //    involve the async runtime at all.
    step("std: resolve");
    let t = Instant::now();
    let addrs: Vec<_> = match (host.as_str(), port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => {
            eprintln!("[std: resolve] FAILED: {e}");
            std::process::exit(1);
        }
    };
    ok("std: resolve", t);
    eprintln!("       -> {addrs:?}");

    let addr = addrs[0];

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

    // 2. The proposed workaround: connect with the blocking API, hand the
    //    ready socket to tokio, and do the request asynchronously. If stage 2
    //    hangs in connect but this succeeds, async read/write is fine and only
    //    the async connect needs avoiding.
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
            Ok(Err(e)) => {
                eprintln!("[hybrid: blocking connect] FAILED: {e}");
                return;
            }
            Err(e) => {
                eprintln!("[hybrid: blocking connect] JOIN FAILED: {e}");
                return;
            }
        };

        step("hybrid: from_std");
        let t = Instant::now();
        let mut s = match tokio::net::TcpStream::from_std(std_stream) {
            Ok(s) => {
                ok("hybrid: from_std", t);
                s
            }
            Err(e) => {
                eprintln!("[hybrid: from_std] FAILED: {e}");
                return;
            }
        };

        step("hybrid: async request (10s timeout)");
        let t = Instant::now();
        let req = format!("GET / HTTP/1.0\r\nHost: {host}\r\n\r\n");
        let fut = async {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            s.write_all(req.as_bytes()).await?;
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).await?;
            Ok::<_, std::io::Error>(String::from_utf8_lossy(&buf[..n.min(40)]).into_owned())
        };
        match tokio::time::timeout(Duration::from_secs(10), fut).await {
            Ok(Ok(head)) => {
                ok("hybrid: async request", t);
                eprintln!("       -> {head:?}");
            }
            Ok(Err(e)) => eprintln!("[hybrid: async request] FAILED: {e}"),
            Err(_) => eprintln!("[hybrid: async request] TIMED OUT after 10s"),
        }
    });

    // 3. The same over tokio. If the blocking path above worked and this hangs,
    //    the fault is in the async I/O driver rather than in the network.
    //    This runs last: when it hangs, nothing after it would execute.
    for workers in [1usize, 2] {
        eprintln!("--- tokio, {workers} worker thread(s) ---");
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[tokio: build runtime] FAILED: {e}");
                continue;
            }
        };

        rt.block_on(async {
            step("tokio: sleep 100ms");
            let t = Instant::now();
            tokio::time::sleep(Duration::from_millis(100)).await;
            ok("tokio: sleep 100ms", t);

            step("tokio: resolve");
            let t = Instant::now();
            match tokio::net::lookup_host((host.as_str(), port)).await {
                Ok(a) => {
                    let a: Vec<_> = a.collect();
                    ok("tokio: resolve", t);
                    eprintln!("       -> {a:?}");
                }
                Err(e) => eprintln!("[tokio: resolve] FAILED: {e}"),
            }

            step("tokio: connect (10s timeout)");
            let t = Instant::now();
            match tokio::time::timeout(
                Duration::from_secs(10),
                tokio::net::TcpStream::connect(addr),
            )
            .await
            {
                Ok(Ok(mut s)) => {
                    ok("tokio: connect", t);

                    step("tokio: request (10s timeout)");
                    let t = Instant::now();
                    let req = format!("GET / HTTP/1.0\r\nHost: {host}\r\n\r\n");
                    let fut = async {
                        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                        s.write_all(req.as_bytes()).await?;
                        let mut buf = [0u8; 64];
                        let n = s.read(&mut buf).await?;
                        Ok::<_, std::io::Error>(
                            String::from_utf8_lossy(&buf[..n.min(40)]).into_owned(),
                        )
                    };
                    match tokio::time::timeout(Duration::from_secs(10), fut).await {
                        Ok(Ok(head)) => {
                            ok("tokio: request", t);
                            eprintln!("       -> {head:?}");
                        }
                        Ok(Err(e)) => eprintln!("[tokio: request] FAILED: {e}"),
                        Err(_) => eprintln!("[tokio: request] TIMED OUT after 10s"),
                    }
                }
                Ok(Err(e)) => eprintln!("[tokio: connect] FAILED: {e}"),
                Err(_) => eprintln!("[tokio: connect] TIMED OUT after 10s"),
            }
        });
    }

    eprintln!("done");
    std::process::exit(0);
}
