//! End-to-end tests driving the built binary against an in-process LibreSpeed
//! backend, so the full flow — server list, ping, download, upload, telemetry,
//! report rendering — is exercised without touching the network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const BIN: &str = env!("CARGO_BIN_EXE_librespeed-cli");
const GARBAGE_LEN: usize = 2 * 1024 * 1024;

/// A mock LibreSpeed backend. Dropping it leaves the thread running; tests are
/// short-lived processes, so that is fine.
struct MockBackend {
    addr: SocketAddr,
    telemetry_hits: Arc<AtomicUsize>,
}

impl MockBackend {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock backend");
        let addr = listener.local_addr().expect("mock backend address");
        let telemetry_hits = Arc::new(AtomicUsize::new(0));

        let hits = telemetry_hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let hits = hits.clone();
                std::thread::spawn(move || {
                    let _ = handle(stream, hits);
                });
            }
        });

        Self {
            addr,
            telemetry_hits,
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Writes a server list pointing at this backend and returns its path.
    fn server_list(&self, name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "librespeed-cli-test-{name}-{}.json",
            self.addr.port()
        ));
        let json = format!(
            r#"[{{"name":"Mock {name}","server":"{}","id":1,"dlURL":"garbage.php","ulURL":"empty.php","pingURL":"empty.php","getIpURL":"getIP.php","sponsorName":"Mock Sponsor","sponsorURL":"https://example.invalid"}}]"#,
            self.url()
        );
        std::fs::write(&path, json).expect("write server list");
        path
    }
}

fn handle(mut stream: TcpStream, telemetry_hits: Arc<AtomicUsize>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    // Request line.
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let path = target.split('?').next().unwrap_or_default().to_string();

    // Headers.
    let mut content_length = 0usize;
    let mut chunked = false;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }

    // Body.
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line)? == 0 {
                break;
            }
            let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
            if size == 0 {
                break;
            }
            let mut chunk = vec![0u8; size];
            if reader.read_exact(&mut chunk).is_err() {
                break;
            }
            body.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            let _ = reader.read_exact(&mut crlf);
        }
    } else if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        reader.read_exact(&mut buf)?;
        body = buf;
    }

    let respond = |stream: &mut TcpStream,
                   body: &[u8],
                   content_type: &str|
     -> std::io::Result<()> {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(body)?;
        stream.flush()
    };

    match (method.as_str(), path.as_str()) {
        (_, "/empty.php") => respond(&mut stream, b"", "text/plain")?,
        ("GET", "/garbage.php") => respond(
            &mut stream,
            &vec![0u8; GARBAGE_LEN],
            "application/octet-stream",
        )?,
        ("GET", "/getIP.php") => {
            let json = br#"{"processedString":"203.0.113.7 - Example ISP","rawIspInfo":{"ip":"203.0.113.7","hostname":"host.example","city":"Testville","region":"Testshire","country":"XX","loc":"0,0","org":"AS64496 Example ISP","postal":"00000","timezone":"UTC","readme":"https://ipinfo.io/missingauth"}}"#;
            respond(&mut stream, json, "application/json")?
        }
        ("POST", "/results/telemetry.php") => {
            assert!(
                String::from_utf8_lossy(&body).contains(r#"name="ispinfo""#),
                "telemetry payload must be multipart with an ispinfo field"
            );
            telemetry_hits.fetch_add(1, Ordering::Relaxed);
            respond(&mut stream, b"id 4815162342", "text/plain")?
        }
        _ => {
            stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )?;
        }
    }

    let _ = stream.shutdown(Shutdown::Write);
    Ok(())
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("run librespeed-cli")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn version_reports_program_name_and_license() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    let stdout = stdout_of(&out);
    assert!(stdout.starts_with("librespeed-cli v"));
    assert!(stdout.contains("GNU Lesser General Public License v3.0"));
}

#[test]
fn csv_header_matches_the_go_implementation() {
    let out = run(&["--csv-header"]);
    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out).trim_end(),
        "Timestamp,Server Name,Address,Ping,Jitter,Download,Upload,Share,IP"
    );
}

#[test]
fn list_renders_id_name_url_and_sponsor() {
    let backend = MockBackend::start();
    let list = backend.server_list("list");

    let out = run(&["--local-json", list.to_str().unwrap(), "--list"]);
    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out).trim_end(),
        format!(
            "1: Mock list ({})  [Sponsor: Mock Sponsor @ https://example.invalid]",
            backend.url()
        )
    );
}

#[test]
fn simple_output_reports_all_three_measurements() {
    let backend = MockBackend::start();
    let list = backend.server_list("simple");

    let out = run(&[
        "--local-json",
        list.to_str().unwrap(),
        "--server",
        "1",
        "--duration",
        "1",
        "--no-icmp",
        "--simple",
    ]);
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = stdout_of(&out);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "unexpected output: {stdout:?}");
    assert!(lines[0].starts_with("Ping:\t") && lines[0].contains("\tJitter:\t"));
    assert!(lines[1].starts_with("Download rate:\t") && lines[1].ends_with(" Mbps"));
    assert!(lines[2].starts_with("Upload rate:\t") && lines[2].ends_with(" Mbps"));

    // Quiet modes keep stderr free of the interactive UI.
    assert!(
        out.stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn json_report_has_the_expected_shape() {
    let backend = MockBackend::start();
    let list = backend.server_list("json");

    let out = run(&[
        "--local-json",
        list.to_str().unwrap(),
        "--server",
        "1",
        "--duration",
        "1",
        "--no-icmp",
        "--json",
    ]);
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    let reports: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("valid JSON");
    let report = &reports[0];

    assert_eq!(report["server"]["name"], "Mock json");
    assert_eq!(report["server"]["url"], backend.url());
    assert_eq!(report["client"]["ip"], "203.0.113.7");
    assert_eq!(report["client"]["org"], "AS64496 Example ISP");
    // `readme` is deliberately stripped from the report.
    assert!(report["client"].get("readme").is_none());

    for key in ["timestamp", "ping", "jitter", "upload", "download", "share"] {
        assert!(report.get(key).is_some(), "missing key {key}");
    }
    assert!(report["bytes_received"].as_u64().unwrap() > 0);
    assert!(report["bytes_sent"].as_u64().unwrap() > 0);
    assert!(report["download"].as_f64().unwrap() > 0.0);
    assert!(report["upload"].as_f64().unwrap() > 0.0);
}

#[test]
fn csv_report_honours_a_custom_delimiter() {
    let backend = MockBackend::start();
    let list = backend.server_list("csv");

    let out = run(&[
        "--local-json",
        list.to_str().unwrap(),
        "--server",
        "1",
        "--duration",
        "1",
        "--no-icmp",
        "--csv",
        "--csv-delimiter",
        ";",
    ]);
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = stdout_of(&out);
    // No trailing newline, and the row splits into the nine documented columns.
    assert!(!stdout.ends_with('\n'));
    let fields: Vec<&str> = stdout.split(';').collect();
    assert_eq!(fields.len(), 9, "unexpected row: {stdout:?}");
    assert_eq!(fields[1], "Mock csv");
    assert_eq!(fields[2], backend.url());
    assert_eq!(fields[8], "203.0.113.7");
}

#[test]
fn telemetry_produces_a_share_link() {
    let backend = MockBackend::start();
    let list = backend.server_list("telemetry");

    let out = run(&[
        "--local-json",
        list.to_str().unwrap(),
        "--server",
        "1",
        "--duration",
        "1",
        "--no-icmp",
        "--simple",
        "--telemetry-server",
        &backend.url(),
        "--telemetry-level",
        "full",
    ]);
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = stdout_of(&out);
    assert!(
        stdout.contains(&format!(
            "Share your result: {}/results/?id=4815162342",
            backend.url()
        )),
        "unexpected output: {stdout:?}"
    );
    assert_eq!(backend.telemetry_hits.load(Ordering::Relaxed), 1);
}

#[test]
fn no_download_and_no_upload_skip_their_tests() {
    let backend = MockBackend::start();
    let list = backend.server_list("skip");

    let out = run(&[
        "--local-json",
        list.to_str().unwrap(),
        "--server",
        "1",
        "--duration",
        "1",
        "--no-icmp",
        "--json",
        "--no-download",
        "--no-upload",
    ]);
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    let reports: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("valid JSON");
    assert_eq!(reports[0]["download"], 0.0);
    assert_eq!(reports[0]["upload"], 0.0);
    assert_eq!(reports[0]["bytes_received"], 0);
    assert_eq!(reports[0]["bytes_sent"], 0);
}

#[test]
fn unknown_server_id_fails_cleanly() {
    let backend = MockBackend::start();
    let list = backend.server_list("unknown");

    let out = run(&["--local-json", list.to_str().unwrap(), "--server", "999"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("specified server(s) not found"));
}

#[test]
fn concurrent_below_one_is_rejected() {
    let out = run(&["--concurrent", "0"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Concurrent requests cannot be lower than 1")
    );
}
