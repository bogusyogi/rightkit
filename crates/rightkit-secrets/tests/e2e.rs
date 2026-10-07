#[cfg(target_os = "macos")]
use rightkit_secrets::{migrate_between, Config, Error, Namespace, Store};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::Arc;

#[cfg(target_os = "macos")]
#[test]
fn isolated_keychain_real_process_api_round_trip() {
    let isolated = rightkit_secrets::macos::IsolatedKeychain::create().unwrap();
    let ns = Namespace::new("rightkit.test.isolated").unwrap();
    let store = Store::open(Config::new(ns).backend(Arc::new(isolated.backend())));
    store.set_str("account", "isolated-fixture").unwrap();
    assert!(store.exists("account").unwrap());
    assert_eq!(
        store.get("account").unwrap().unwrap().expose_str().unwrap(),
        "isolated-fixture"
    );
    store.delete("account").unwrap();
    assert!(!store.exists("account").unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn isolated_keychain_migration_locked_presence_and_cli_redaction() {
    let isolated = rightkit_secrets::macos::IsolatedKeychain::create().unwrap();
    let from = Store::open(
        Config::new(Namespace::new("rightkit.test.process.legacy").unwrap())
            .backend(Arc::new(isolated.backend())),
    );
    let to = from.with_namespace(Namespace::new("rightkit.test.process.current").unwrap());
    from.set_str("migrated", "isolated-migration-fixture")
        .unwrap();
    let report = migrate_between(&from, &to, &["migrated"]);
    assert_eq!(report.moved, vec!["migrated"]);
    assert_eq!(
        to.get("migrated").unwrap().unwrap().expose_str().unwrap(),
        "isolated-migration-fixture"
    );

    let marker = tempfile::tempdir().unwrap().path().join("migration.json");
    let marker_legacy =
        from.with_namespace(Namespace::new("rightkit.test.process.marker-legacy").unwrap());
    marker_legacy.set_str("marker", "marker-fixture").unwrap();
    let marker_report = to
        .migrate_legacy_namespaces(
            &[marker_legacy.namespace().clone()],
            &["marker"],
            Some(&marker),
        )
        .unwrap()
        .unwrap();
    assert_eq!(marker_report.moved, vec!["marker"]);
    assert!(marker.exists());

    let path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    std::fs::write(&path, b" file-fixture \n").unwrap();
    assert_eq!(
        rightkit_secrets::migrate_plaintext_file(&to, "file-account", &path).unwrap(),
        rightkit_secrets::FileMigration::Migrated
    );
    assert!(!path.exists());
    let env = |name: &str| (name == "PRIMARY").then(|| "env-fixture".to_string());
    let resolved = to.resolve("missing", &["PRIMARY"], &env).unwrap().unwrap();
    assert_eq!(resolved.1, rightkit_secrets::Source::Environment);

    from.set_str("locked", "locked-fixture").unwrap();
    isolated.lock().unwrap();
    let presence = from.exists("locked");
    assert!(
        matches!(presence, Ok(true)),
        "locked-keychain presence must not request bytes or prompt: {presence:?}"
    );
    let read = from.get("locked");
    assert!(
        matches!(read, Err(Error::InteractionRequired | Error::Denied)),
        "locked non-interactive read must not return bytes: {read:?}"
    );
    isolated.unlock().unwrap();

    let mut set = Command::new(env!("CARGO_BIN_EXE_rightkit-secrets"));
    set.args([
        "set",
        "--namespace",
        "rightkit.test.cli",
        "--account",
        "cli-account",
        "--isolated-keychain",
        isolated.path().to_str().unwrap(),
        "--keychain-password",
        isolated.password(),
    ]);
    set.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = set.spawn().unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"cli-secret-fixture")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"stored\":true"));
    assert!(!stdout.contains("cli-secret-fixture"));

    let status = Command::new(env!("CARGO_BIN_EXE_rightkit-secrets"))
        .args([
            "status",
            "--namespace",
            "rightkit.test.cli",
            "--account",
            "cli-account",
            "--isolated-keychain",
            isolated.path().to_str().unwrap(),
            "--keychain-password",
            isolated.password(),
        ])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(status_out.contains("\"present\":true"));
    assert!(!status_out.contains("cli-secret-fixture"));
}

#[cfg(target_os = "macos")]
#[test]
fn concurrent_isolated_processes_create_one_key() {
    let isolated = rightkit_secrets::macos::IsolatedKeychain::create().unwrap();
    let mut children = Vec::new();
    for _ in 0..4 {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_ensure_key", "--nocapture"])
            .env("RIGHTKIT_SECRETS_CHILD_KEYCHAIN", isolated.path())
            .env("RIGHTKIT_SECRETS_CHILD_PASSWORD", isolated.password())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        children.push(child);
    }
    let mut created = 0;
    let mut adopted = 0;
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        created += text
            .lines()
            .filter(|line| line.trim() == "created=true")
            .count();
        adopted += text
            .lines()
            .filter(|line| line.trim() == "created=false")
            .count();
    }
    assert_eq!(created, 1, "exactly one process must win add-if-absent");
    assert_eq!(adopted, 3, "other processes must adopt winner");
}

#[cfg(target_os = "macos")]
#[test]
fn child_ensure_key() {
    let (Some(path), Some(password)) = (
        std::env::var_os("RIGHTKIT_SECRETS_CHILD_KEYCHAIN"),
        std::env::var_os("RIGHTKIT_SECRETS_CHILD_PASSWORD"),
    ) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let password = password.to_string_lossy().into_owned();
    let kc = rightkit_secrets::macos::KeychainBackend::open_file(&path, Some(&password)).unwrap();
    let store = Store::open(
        Config::new(Namespace::new("rightkit.test.concurrent").unwrap()).backend(Arc::new(kc)),
    );
    let (_, created) = store.get_or_create_key("db-key", 32).unwrap();
    println!("created={created}");
}
