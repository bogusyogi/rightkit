use agent_room_core::protocol::{Phase, Room, RoomConfig, SeatConfig};
use agent_room_core::store::RoomStore;
use std::fs;
use std::process::Command;

fn sealed_run(run_dir: &std::path::Path, workspace: &std::path::Path) {
    let mut room = Room::new(RoomConfig {
        room_id: "prune-room".into(),
        mode: "debate".into(),
        at_budget: "abstain".into(),
        workspace_root: workspace.to_path_buf(),
        seats: vec![SeatConfig::new("adrian", "Adrian", "human", "human")],
    })
    .unwrap();
    for phase in [
        Phase::Positions,
        Phase::PeerDebate,
        Phase::Dispositions,
        Phase::AuthorReview,
        Phase::Voting,
        Phase::Verdict,
    ] {
        room.advance("adrian", phase).unwrap();
    }
    room.seal("adrian").unwrap();
    let mut store = RoomStore::create(run_dir, &room).unwrap();
    store.export_jsonl(&room).unwrap();
}

#[test]
fn prune_refuses_digest_invalid_export_and_preserves_mutable_state() {
    let root = tempfile::tempdir().unwrap();
    let run_dir = root.path().join("run");
    sealed_run(&run_dir, root.path());
    fs::write(run_dir.join("transcript.jsonl"), b"tampered\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_room"))
        .args(["prune", "--run-dir", run_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("export_digest_invalid"));
    assert!(run_dir.join("room.sqlite3").is_file());
}

#[test]
fn prune_removes_only_mutable_room_artifacts_after_verified_sealed_export() {
    let root = tempfile::tempdir().unwrap();
    let run_dir = root.path().join("run");
    sealed_run(&run_dir, root.path());
    for name in [
        "runtime.json",
        ".serve-config.json",
        "seat-codex.credentials.json",
        "service.stdout.log",
        "service.stderr.log",
        "room.sqlite3-wal",
        "room.sqlite3-shm",
    ] {
        fs::write(run_dir.join(name), b"mutable").unwrap();
    }
    fs::write(run_dir.join("operator-note.md"), b"keep me").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_room"))
        .args(["prune", "--run-dir", run_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(run_dir.join("transcript.jsonl").is_file());
    assert!(run_dir.join("transcript.receipt.json").is_file());
    assert!(run_dir.join("prune.receipt.json").is_file());
    assert!(run_dir.join("operator-note.md").is_file());
    assert!(!run_dir.join("room.sqlite3").exists());
    assert!(!run_dir.join("runtime.json").exists());
    assert!(!run_dir.join("seat-codex.credentials.json").exists());
}
