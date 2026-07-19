use agent_room_core::runtime::{load_runtime, write_runtime, RuntimeInfo};

#[test]
fn runtime_discovery_is_owner_local_and_tokens_never_enter_url() {
    let run_dir = tempfile::tempdir().unwrap();
    let info = RuntimeInfo {
        room_id: "runtime-room".into(),
        pid: 4242,
        port: 43119,
        base_url: "http://127.0.0.1:43119".into(),
        database: run_dir.path().join("room.sqlite3"),
        pairing_code: "PAIR_SECRET".into(),
        human_bearer: "HUMAN_SECRET".into(),
    };
    let path = write_runtime(run_dir.path(), &info).unwrap();
    assert_eq!(path, run_dir.path().join("runtime.json"));
    let loaded = load_runtime(run_dir.path()).unwrap();
    assert_eq!(loaded, info);
    assert!(!loaded.base_url.contains("PAIR_SECRET"));
    assert!(!loaded.base_url.contains("HUMAN_SECRET"));
    assert!(agent_room_core::runtime::is_owner_restricted(&path).unwrap());
}

#[test]
fn runtime_discovery_refuses_overwrite() {
    let run_dir = tempfile::tempdir().unwrap();
    let info = RuntimeInfo {
        room_id: "runtime-room".into(),
        pid: 1,
        port: 1,
        base_url: "http://127.0.0.1:1".into(),
        database: run_dir.path().join("room.sqlite3"),
        pairing_code: "a".into(),
        human_bearer: "b".into(),
    };
    write_runtime(run_dir.path(), &info).unwrap();
    assert_eq!(
        write_runtime(run_dir.path(), &info).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
}
