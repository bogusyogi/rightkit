#![cfg(windows)]

use rightkit_credentials::{
    system_backend, Config, Interaction, Namespace, Store, UNIVERSAL_GUARD_ENV,
};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod common;

/// Every credential this file writes is named under this prefix.
const SERVICE_PREFIX: &str = "dev.rightkit.secrets.lock.";

const ROLE: &str = "RIGHTKIT_CREDENTIALS_LOCK_CHILD";
const ACCOUNT: &str = "db-key";

struct Children(Vec<Child>);

impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Credential(Namespace);

impl Drop for Credential {
    fn drop(&mut self) {
        let _ = system_backend().delete(&self.0, ACCOUNT, Interaction::NonInteractive);
    }
}

#[test]
fn eight_processes_observe_one_key() {
    if !common::real_user_stores_allowed("eight_processes_observe_one_key") {
        return;
    }
    let service = format!(
        "dev.rightkit.secrets.lock.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    common::assert_test_name(&service, SERVICE_PREFIX);
    let credential = Credential(Namespace::new(&service).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut children = Children(Vec::new());
    for index in 0..8 {
        children.0.push(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child_get_or_create", "--nocapture"])
                .env(ROLE, index.to_string())
                .env("RIGHTKIT_CREDENTIALS_LOCK_NAMESPACE", &service)
                .env("RIGHTKIT_CREDENTIALS_LOCK_DIR", dir.path())
                .env_remove(UNIVERSAL_GUARD_ENV)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    // Every child is ready before any get-or-create starts.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(0..8).all(|index| dir.path().join(format!("ready-{index}")).exists()) {
        assert!(
            Instant::now() < deadline,
            "children did not become ready within 10s"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for child in &mut children.0 {
        child.stdin.take().unwrap().write_all(b"start\n").unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let finished = children
            .0
            .iter_mut()
            .all(|child| child.try_wait().unwrap().is_some());
        if finished {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "children did not finish within 30s"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for child in children.0.drain(..) {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let persisted = system_backend()
        .get(&credential.0, ACCOUNT, Interaction::NonInteractive)
        .unwrap()
        .expect("key must be persisted");
    let mut created = 0;
    for index in 0..8 {
        let result = std::fs::read_to_string(dir.path().join(format!("result-{index}"))).unwrap();
        let (value, won) = result
            .split_once('\n')
            .expect("missing child result fields");
        assert!(
            value.as_bytes() == persisted.expose_bytes(),
            "child observed another key"
        );
        match won {
            "true" => created += 1,
            "false" => {}
            _ => panic!("invalid child creation flag"),
        }
    }
    assert_eq!(created, 1, "exactly one process must create the key");
    assert!(system_backend()
        .delete(&credential.0, ACCOUNT, Interaction::NonInteractive)
        .unwrap());
}

#[test]
fn child_get_or_create() {
    let index = match std::env::var(ROLE) {
        Ok(value) => value.parse::<usize>().expect("invalid child index"),
        Err(std::env::VarError::NotPresent) => return,
        Err(_) => panic!("invalid child role"),
    };
    assert!(index < 8, "unknown child index");
    let service =
        std::env::var("RIGHTKIT_CREDENTIALS_LOCK_NAMESPACE").expect("missing child namespace");
    // The child writes only the credential its parent named, and the parent ran the gate.
    common::assert_test_name(&service, SERVICE_PREFIX);
    let dir = std::path::PathBuf::from(
        std::env::var_os("RIGHTKIT_CREDENTIALS_LOCK_DIR").expect("missing child directory"),
    );
    let store =
        Store::open(Config::new(Namespace::new(service).unwrap()).timeout(Duration::from_secs(20)));
    assert!(!store.guard_active(), "child must use OS credentials");
    std::fs::write(dir.join(format!("ready-{index}")), b"ready").unwrap();
    let mut start = String::new();
    std::io::stdin().read_line(&mut start).unwrap();
    assert_eq!(start, "start\n", "invalid child start signal");
    let (key, created) = store.get_or_create_key(ACCOUNT, 32).unwrap();
    std::fs::write(
        dir.join(format!("result-{index}")),
        format!("{}\n{created}", key.expose_str().unwrap()),
    )
    .unwrap();
}
