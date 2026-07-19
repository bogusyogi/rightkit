use agent_room_core::runtime::load_runtime;
use agent_room_core::store::RoomStore;
use std::process::Command;

#[test]
fn spawn_say_and_watch_once_use_one_persisted_room() {
    let run_dir = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("packet.md"),
        "# Pinned review packet\n",
    )
    .unwrap();
    let room = env!("CARGO_BIN_EXE_room");
    let spawned = Command::new(room)
        .args([
            "spawn",
            "--run-dir",
            run_dir.path().to_str().unwrap(),
            "--room-id",
            "cli-smoke",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--seats",
            "claude,codex",
            "--mode",
            "debate",
            "--scope-ref",
            "packet.md",
            "--no-open",
            "--no-seats",
        ])
        .status()
        .unwrap();
    assert!(spawned.success());
    eprintln!("spawn returned");
    let runtime = load_runtime(run_dir.path()).unwrap();
    assert_ne!(runtime.port, 0);
    for seat in ["claude", "codex"] {
        let credentials: serde_json::Value = serde_json::from_slice(
            &std::fs::read(run_dir.path().join(format!("seat-{seat}.credentials.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(credentials["bootstrap"], true);
        assert!(credentials["bearer_token"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert!(credentials["resume_token"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }
    let persisted = RoomStore::open(run_dir.path()).unwrap().load().unwrap();
    assert_eq!(persisted.phase, agent_room_core::protocol::Phase::Positions);
    let scope = persisted
        .motions
        .values()
        .find(|motion| motion.kind == "review_scope")
        .expect("spawned room has deterministic review scope");
    assert_eq!(scope.scope_refs[0].locator, "packet.md");
    assert!(scope.scope_digest.is_some());
    eprintln!("runtime loaded");

    let said = Command::new(room)
        .args([
            "say",
            "--run-dir",
            run_dir.path().to_str().unwrap(),
            "--body",
            "human smoke message",
        ])
        .output()
        .unwrap();
    assert!(
        said.status.success(),
        "{}",
        String::from_utf8_lossy(&said.stderr)
    );
    eprintln!("say returned");
    let watched = Command::new(room)
        .args([
            "watch",
            "--run-dir",
            run_dir.path().to_str().unwrap(),
            "--once",
        ])
        .output()
        .unwrap();
    assert!(
        watched.status.success(),
        "{}",
        String::from_utf8_lossy(&watched.stderr)
    );
    assert!(String::from_utf8_lossy(&watched.stdout).contains("human smoke message"));
    eprintln!("watch returned");

    let streamed = Command::new(room)
        .args([
            "watch",
            "--run-dir",
            run_dir.path().to_str().unwrap(),
            "--max-events",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        streamed.status.success(),
        "{}",
        String::from_utf8_lossy(&streamed.stderr)
    );
    let streamed_stdout = String::from_utf8_lossy(&streamed.stdout);
    assert!(streamed_stdout.contains("\"seq\":"), "{streamed_stdout}");
    assert!(streamed_stdout.contains("\"kind\":"), "{streamed_stdout}");
    eprintln!("stream watch returned");

    terminate(runtime.pid);
}

#[cfg(windows)]
fn terminate(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

#[cfg(unix)]
fn terminate(pid: u32) {
    let _ = Command::new("kill").arg(pid.to_string()).output();
}
