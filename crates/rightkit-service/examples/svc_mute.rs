//! Mute test host (Unix socket): `svc_mute <suite-root> <service> <accept|noaccept>`.
//! Publishes a real service.json and listens on its socket, then either accepts
//! every connection and never writes a byte (`accept`), or never calls accept()
//! at all (`noaccept`). Prints `READY` once discoverable.
#[cfg(unix)]
fn main() {
    use rightkit_service::{endpoint_for, Suite, PROTOCOL_MAJOR, PROTOCOL_MINOR};
    use std::os::unix::net::UnixListener;
    let a: Vec<String> = std::env::args().skip(1).collect();
    let suite = Suite::at(&a[0]).expect("suite");
    let dir = suite.private_service_subdir(&a[1], "").unwrap_or_else(|_| suite.service_dir(&a[1]));
    std::fs::create_dir_all(&dir).expect("service dir");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    let endpoint = endpoint_for(&suite, &a[1]);
    let _ = std::fs::remove_file(&endpoint);
    let l = UnixListener::bind(&endpoint).expect("bind");
    let rec = serde_json::json!({
        "protocol_major": PROTOCOL_MAJOR, "protocol_minor": PROTOCOL_MINOR, "service": a[1], "role": "mute", "rank": 1,
        "pid": std::process::id(), "endpoint": endpoint, "version": "test", "started_ms": 0
    });
    std::fs::write(dir.join("service.json"), serde_json::to_vec(&rec).unwrap()).expect("record");
    println!("READY");
    let mut held = Vec::new();
    loop {
        if a[2] == "accept" {
            if let Ok((s, _)) = l.accept() {
                held.push(s);
            }
        } else {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
}
#[cfg(not(unix))]
fn main() {
    println!("unsupported");
}
