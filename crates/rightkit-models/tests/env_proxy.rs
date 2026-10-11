//! Environment proxies are honoured by default. This test sets a process-wide environment
//! variable, so it lives alone in its own test binary: no other test can build a client while
//! `HTTP_PROXY` is set.

use rightkit_models::download::{download_resumable, part_path_for, DownloadRequest};
use rightkit_models::http::ModelHttp;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// An HTTP proxy that is also the origin: `CONNECT` is answered with 200, then the tunnelled
/// request gets the body; an absolute-form request gets it directly.
fn proxy() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut out = stream;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let target = parts.next().unwrap_or("/").to_string();
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                            break;
                        }
                    }
                    log.lock().unwrap().push(format!("{method} {target}"));
                    if method == "CONNECT" {
                        let _ = out.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                        continue;
                    }
                    let _ = out.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nenv-proxy",
                    );
                    return;
                }
            });
        }
    });
    (base, seen)
}

#[test]
fn environment_proxy_variable_is_honoured_by_default() {
    let (base, seen) = proxy();
    // The default transport reads the environment when it is built, so set it first.
    std::env::set_var("HTTP_PROXY", &base);
    for name in [
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        std::env::remove_var(name);
    }
    let client = ModelHttp::default();
    std::env::remove_var("HTTP_PROXY");

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("f.bin");
    download_resumable(
        &client,
        DownloadRequest {
            url: "http://models.example.org/e.bin",
            part_path: &part_path_for(&dest),
            dest_path: &dest,
            expected_sha256: None,
            authorization: None,
            cancelled: &|| false,
        },
        |_, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"env-proxy");
    // The destination reached the proxy by name (CONNECT or absolute form), not resolved here.
    let seen: Vec<String> = seen.lock().unwrap().clone();
    assert!(
        seen.iter().any(|r| r.contains("models.example.org")),
        "{seen:?}"
    );
}
