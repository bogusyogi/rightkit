//! Impostor host for the host-verification tests: `svc_fakehost <suite-root> <service>`.
//!
//! Publishes a real `service.json` for `<service>` whose pid is this process, listens on the
//! endpoint, and prints `READY`. For every connection it then reads until the peer closes, a
//! newline arrives, or 10 seconds pass, and prints `CONN bytes=<n>` before closing: the number of
//! bytes the client wrote. A client that refused this host before writing anything reports 0. It
//! never answers a byte.
#[cfg(unix)]
fn main() {
    use rightkit_service::{endpoint_for, Suite, PROTOCOL_MAJOR, PROTOCOL_MINOR};
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    let a: Vec<String> = std::env::args().skip(1).collect();
    let suite = Suite::at(&a[0]).expect("suite");
    let dir = suite.service_dir(&a[1]);
    std::fs::create_dir_all(&dir).expect("service dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let endpoint = endpoint_for(&suite, &a[1]);
    let _ = std::fs::remove_file(&endpoint);
    let listener = UnixListener::bind(&endpoint).expect("bind");
    let record = serde_json::json!({
        "protocol_major": PROTOCOL_MAJOR, "protocol_minor": PROTOCOL_MINOR, "service": a[1],
        "role": "impostor", "rank": 1, "pid": std::process::id(), "endpoint": endpoint,
        "version": "test", "started_ms": 0
    });
    std::fs::write(
        dir.join("service.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .expect("record");
    println!("READY");
    for conn in listener.incoming() {
        let Ok(mut stream) = conn else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut total = 0usize;
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    total += n;
                    if buf[..n].contains(&b'\n') {
                        break;
                    }
                }
            }
        }
        println!("CONN bytes={total}");
    }
}

#[cfg(not(unix))]
fn main() {
    println!("unsupported");
}
