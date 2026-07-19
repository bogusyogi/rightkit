use agent_room_core::protocol::{EventKind, Phase, Room, RoomConfig, SeatConfig};
use agent_room_core::store::RoomStore;

fn room(root: &std::path::Path) -> Room {
    Room::new(RoomConfig {
        room_id: "persisted-room".into(),
        mode: "review".into(),
        at_budget: "abstain".into(),
        workspace_root: root.to_path_buf(),
        seats: vec![
            SeatConfig::new("claude", "Claude", "claude", "reviewer"),
            SeatConfig::new("codex", "Codex", "codex", "implementer"),
            SeatConfig::new("adrian", "Adrian", "human", "human"),
        ],
    })
    .unwrap()
}

#[test]
fn sqlite_schema_persists_room_and_reloads_private_position() {
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run");
    let mut value = room(root.path());
    value.advance("adrian", Phase::Positions).unwrap();
    value
        .post("claude", "p1", EventKind::Position, "private read", None)
        .unwrap();

    let mut store = RoomStore::create(&run, &value).unwrap();
    store.save(&value).unwrap();
    let loaded = RoomStore::open(&run).unwrap().load().unwrap();
    assert_eq!(loaded.room_id, value.room_id);
    assert!(!loaded
        .visible_events("codex", 0)
        .unwrap()
        .iter()
        .any(|e| e.body == "private read"));

    let tables = store.table_names().unwrap();
    for required in [
        "room",
        "seats",
        "motions",
        "messages",
        "findings",
        "contests",
        "dispositions",
        "receipts",
        "requests",
    ] {
        assert!(
            tables.contains(&required.to_string()),
            "missing table {required}"
        );
    }
    assert_eq!(store.row_count("messages").unwrap(), 1);
    assert_eq!(store.row_count("requests").unwrap(), 1);
}

#[test]
fn export_refuses_unsealed_room_and_writes_digest_verified_jsonl_when_sealed() {
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run");
    let mut value = room(root.path());
    let mut store = RoomStore::create(&run, &value).unwrap();
    assert_eq!(
        store.export_jsonl(&value).unwrap_err().code,
        "room_not_sealed"
    );

    value.advance("adrian", Phase::Positions).unwrap();
    value.advance("adrian", Phase::PeerDebate).unwrap();
    value.advance("adrian", Phase::Dispositions).unwrap();
    value.advance("adrian", Phase::AuthorReview).unwrap();
    value.advance("adrian", Phase::Voting).unwrap();
    value.advance("adrian", Phase::Verdict).unwrap();
    let seal = value.seal("adrian").unwrap();
    store.save(&value).unwrap();
    let export = store.export_jsonl(&value).unwrap();
    assert!(export.path.is_file());
    assert_eq!(export.sha256, seal.digest);
    assert!(RoomStore::verify_export(&export.path, &export.sha256).unwrap());
}
