//! The WebDriver bridge is an authenticated adapter: a real `rightkit-qa-driver serve`
//! process must refuse every request that lacks the per-launch credential or carries
//! a foreign Host/Origin, and answer one that has them.
#![cfg(target_os = "macos")]
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

fn request(port: u16, host: &str, extra: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut r = String::new();
    s.read_to_string(&mut r).unwrap();
    (
        r.split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0),
        r,
    )
}

#[test]
fn bridge_requires_credential_host_and_no_origin() {
    let dir = std::env::temp_dir().join(format!("rkc-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cred_file = dir.join("bridge.cred");
    let _ = std::fs::remove_file(&cred_file);
    let mut child = Command::new(env!("CARGO_BIN_EXE_rightkit-qa-driver"))
        .args([
            "serve",
            &std::process::id().to_string(),
            "0",
            cred_file.to_str().unwrap(),
        ])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(
        !line.contains(&std::fs::read_to_string(&cred_file).unwrap()),
        "credential must never be printed"
    );
    let port: u16 = line
        .split("127.0.0.1:")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .and_then(|p| p.parse().ok())
        .expect("port in banner");
    assert_eq!(
        std::fs::metadata(&cred_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let cred = std::fs::read_to_string(&cred_file).unwrap();
    assert_eq!(cred.len(), 64);
    let host = format!("127.0.0.1:{port}");

    assert_eq!(request(port, &host, "", "/status").0, 401, "no credential");
    assert_eq!(
        request(port, &host, "Authorization: Bearer nope\r\n", "/status").0,
        401,
        "wrong credential"
    );
    let auth = format!("Authorization: Bearer {cred}\r\n");
    assert_eq!(
        request(port, "evil.example", &auth, "/status").0,
        403,
        "rebinding Host"
    );
    assert_eq!(
        request(
            port,
            &host,
            &format!("{auth}Origin: http://evil.example\r\n"),
            "/status"
        )
        .0,
        403,
        "browser origin"
    );
    let (code, body) = request(port, &host, &auth, "/status");
    assert_eq!(code, 200);
    assert!(body.contains("\"ready\":true"), "{body}");
    assert_eq!(
        request(port, &host, &auth, "/session/none/screenshot").0,
        404,
        "session routes still dispatch only after auth"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
