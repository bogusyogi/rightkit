//! End-to-end proofs of the helper backend: the real signed Swift CloudKit helper against the app's
//! real CloudKit container. Both tests are `#[ignore]`d (entitled helper and iCloud account needed).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_sync::helper::{CloudKitHelper, HelperConfig};
use rightkit_sync::*;

#[derive(Clone, Debug, PartialEq)]
struct Note {
    id: String,
    text: String,
    tags: Vec<String>,
    body: Vec<u8>,
    created: i64,
    updated: i64,
}

struct NoteMapper;
impl RecordMapper for NoteMapper {
    type Item = Note;
    fn record_type(&self) -> &str {
        "Note"
    }
    fn header(&self, n: &Note) -> ItemHeader {
        ItemHeader {
            id: n.id.clone(),
            created_at_ms: n.created,
            updated_at_ms: n.updated,
            origin_device: None,
        }
    }
    fn encode(&self, n: &Note, r: &mut RawRecord) {
        r.set("text", FieldValue::Text(n.text.clone()));
        r.set("tags", FieldValue::Strings(n.tags.clone()));
        r.set("body", FieldValue::Bytes(n.body.clone()));
    }
    fn decode(&self, r: &RawRecord) -> Result<Note, String> {
        Ok(Note {
            id: r.text("id").ok_or("no id")?,
            text: r.text("text").ok_or("no text")?,
            tags: r
                .get("tags")
                .and_then(FieldValue::as_strings)
                .map(<[String]>::to_vec)
                .unwrap_or_default(),
            body: match r.get("body") {
                Some(FieldValue::Bytes(b)) => b.clone(),
                _ => Vec::new(),
            },
            created: r.date_ms("createdAt").ok_or("no createdAt")?,
            updated: r.date_ms("updatedAt").ok_or("no updatedAt")?,
        })
    }
}

#[derive(Default, Clone)]
struct Local(Arc<Mutex<BTreeMap<String, Note>>>);
impl Local {
    fn put(&self, id: &str, text: &str, created: i64, updated: i64) {
        self.put_full(id, text, vec![], vec![], created, updated);
    }
    fn put_full(&self, id: &str, text: &str, tags: Vec<String>, body: Vec<u8>, c: i64, u: i64) {
        self.0.lock().unwrap().insert(
            id.into(),
            Note {
                id: id.into(),
                text: text.into(),
                tags,
                body,
                created: c,
                updated: u,
            },
        );
    }
    fn get(&self, id: &str) -> Option<Note> {
        self.0.lock().unwrap().get(id).cloned()
    }
}
impl LocalStore<Note> for Local {
    fn all(&self) -> SyncResult<Vec<Note>> {
        Ok(self.0.lock().unwrap().values().cloned().collect())
    }
    fn remove(&self, id: &str) -> SyncResult<()> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
    fn apply_remote(&self, items: &[Note]) -> SyncResult<Vec<ApplyOutcome>> {
        let mut m = self.0.lock().unwrap();
        for n in items {
            m.insert(n.id.clone(), n.clone());
        }
        Ok(items.iter().map(|_| ApplyOutcome::Applied).collect())
    }
}

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<(String, String)>>);
impl SyncObserver for Events {
    fn event(&self, name: &'static str, detail: &str) {
        self.0
            .lock()
            .unwrap()
            .push((name.to_string(), detail.to_string()));
    }
}

/// The real Swift helper against the app's real CloudKit container. No memory backend (removed in
/// RightKitSwift 0.9.1, rightkit#1307) and no dummy data: the helper talks to the real container.
///
/// Needs a Development-signed helper that holds the container entitlement and a signed-in iCloud
/// account, so these tests are `#[ignore]`d and run only when asked for, and they FAIL (never skip)
/// when the inputs are missing:
///
/// ```text
/// RIGHTKIT_CLOUDKIT_HELPER=<signed rightkit-cloudkit-helper>      (else swift/RightKitSwift/.build)
/// RIGHTKIT_CLOUDKIT_CONTAINER=iCloud.<the app's real container id>
/// RIGHTKIT_CLOUDKIT_SUBSCRIPTION=<subscription id the app uses>
/// cargo test --test helper_e2e -- --ignored real_swift_helper
/// ```
///
/// Each run uses a fresh zone (`rk-e2e-<pid>-<time>`) in the private database, so a run never sees
/// another run's records and never touches the app's own zones. The helper offers no zone delete, so
/// the empty zone of a run remains; remove it in the CloudKit console when convenient.
fn swift_helper() -> PathBuf {
    if let Ok(p) = std::env::var("RIGHTKIT_CLOUDKIT_HELPER") {
        return PathBuf::from(p);
    }
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../swift/RightKitSwift/.build");
    ["release", "debug"]
        .iter()
        .map(|c| base.join(c).join("rightkit-cloudkit-helper"))
        .find(|p| p.exists())
        .expect("rightkit-cloudkit-helper not found: build it or set RIGHTKIT_CLOUDKIT_HELPER")
}

fn real_config(zone: &str) -> HelperConfig {
    let need = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for this test"))
    };
    HelperConfig::new(swift_helper())
        .arg("--container")
        .arg(need("RIGHTKIT_CLOUDKIT_CONTAINER"))
        .arg("--zone")
        .arg(zone)
        .arg("--subscription")
        .arg(need("RIGHTKIT_CLOUDKIT_SUBSCRIPTION"))
}

/// A fresh ledger file per engine: the real durable ledger the app writes, in the temp folder.
fn ledger_path() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "rk-sync-e2e-ledger-{}-{}.json",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ))
}

fn fresh_zone() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("rk-e2e-{}-{nanos}", std::process::id())
}

#[test]
#[ignore = "needs a signed CloudKit helper, the real container and an iCloud account (RIGHTKIT_CLOUDKIT_*)"]
fn real_swift_helper_real_container_converges_two_engines() {
    let events = Arc::new(Events::default());
    // One helper process, one fresh zone of the real container, shared by both engines.
    let helper = CloudKitHelper::new(real_config(&fresh_zone()), events.clone());
    let mk = |helper: &CloudKitHelper| {
        let local = Local::default();
        let ledger = ledger_path();
        let clock = Arc::new(TestClock(AtomicI64::new(1_000_000)));
        let engine = SyncEngineBuilder::new(
            Arc::new(NoteMapper),
            Arc::new(helper.backend()),
            Arc::new(local.clone()),
            Box::new(FileStorage::new(ledger.clone())),
        )
        .auth(Arc::new(helper.auth()))
        .clock(clock.clone())
        .build()
        .unwrap();
        (local, engine, FileStorage::new(ledger), clock)
    };
    let (la, mut ea, sa, ca) = mk(&helper);
    let (lb, mut eb, _sb, cb) = mk(&helper);
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
    la.put_full(
        "rich",
        "body+tags",
        vec!["x".into(), "y".into()],
        big.clone(),
        50,
        50,
    );
    la.put("n1", "keep-me", 60, 60);
    la.put("n2", "doomed", 61, 61);
    assert_eq!(ea.run_pass().outcome, PassOutcome::Synced);
    let rep = eb.run_pass();
    assert_eq!(rep.downloaded, 3, "{rep:?}");
    let rich = lb.get("rich").unwrap();
    assert_eq!(rich.tags, ["x", "y"]);
    assert_eq!(
        rich.body, big,
        "bytes cross Swift CKAsset-style mapping intact"
    );
    assert_eq!(rich.created, 50, "createdAt immutable origin time survives");

    // Edit + delete: LWW edit lands, tombstone removes remotely and clears content.
    ca.0.fetch_add(3_600_000, Ordering::SeqCst);
    cb.0.fetch_add(3_600_000, Ordering::SeqCst);
    la.put("n1", "edited-on-a", 60, 700);
    la.0.lock().unwrap().remove("n2");
    ea.note_deleted("n2").unwrap();
    assert_eq!(ea.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(eb.run_pass().outcome, PassOutcome::Synced);
    assert_eq!(lb.get("n1").unwrap().text, "edited-on-a");
    assert!(lb.get("n2").is_none());
    let tomb = helper
        .backend()
        .fetch_record("n2")
        .unwrap()
        .expect("tombstone record");
    assert!(tomb.date_ms("deletedAt").is_some());
    assert!(
        tomb.get("text").is_none(),
        "tombstone cleared the content field: {tomb:?}"
    );

    // Ledgers are bound to the helper-reported scope.
    let ledger: Ledger = serde_json::from_slice(&sa.load().unwrap().unwrap()).unwrap();
    assert!(
        ledger
            .scope
            .as_deref()
            .is_some_and(|scope| !scope.is_empty()),
        "ledger bound to the helper-reported account scope"
    );
    assert_eq!(helper.spawn_count(), 1);
    helper.shutdown();
}

#[test]
#[ignore = "needs a signed CloudKit helper, the real container and an iCloud account (RIGHTKIT_CLOUDKIT_*)"]
fn real_swift_helper_refuses_stale_conditional_save_and_reports_server_record() {
    let zone = fresh_zone();
    let helper = CloudKitHelper::new(real_config(&zone), Arc::new(Events::default()));
    // A second helper process on the same zone is "another device" (a real second writer).
    let other_device = CloudKitHelper::new(real_config(&zone), Arc::new(Events::default()));
    helper.auth().availability().unwrap();
    let backend = helper.backend();
    backend.prepare().unwrap();
    let mapper = NoteMapper;
    let mut base = RawRecord::new("Note", "n0");
    for (k, v) in [
        ("id", FieldValue::Text("n0".into())),
        ("createdAt", FieldValue::Date(1)),
        ("updatedAt", FieldValue::Date(10)),
    ] {
        base.set(k, v);
    }
    mapper.encode(
        &Note {
            id: "n0".into(),
            text: "v1".into(),
            tags: vec![],
            body: vec![],
            created: 1,
            updated: 10,
        },
        &mut base,
    );
    // Insert (no tag) succeeds; inserting the same name again is refused.
    let r = backend
        .save_records(&[base.clone()], SavePolicy::IfServerUnchanged)
        .unwrap();
    assert!(r[0].result.is_ok());
    let r = backend
        .save_records(&[base.clone()], SavePolicy::IfServerUnchanged)
        .unwrap();
    assert_eq!(r[0].result, Err(SyncError::Conflict));
    let fetched = backend.fetch_record("n0").unwrap().unwrap();
    let tag = fetched
        .change_tag
        .clone()
        .expect("server change tag travels to Rust");

    // Another device writes: the tag is now stale.
    let mut theirs = fetched.clone();
    theirs.set("text", FieldValue::Text("from-other-device".into()));
    theirs.set("updatedAt", FieldValue::Date(99));
    let r = other_device
        .backend()
        .save_records(&[theirs], SavePolicy::IfServerUnchanged)
        .unwrap();
    assert!(r[0].result.is_ok(), "{:?}", r[0].result);
    let mut mine = fetched.clone();
    mine.set("text", FieldValue::Text("mine-stale".into()));
    let r = backend
        .save_records(&[mine.clone()], SavePolicy::IfServerUnchanged)
        .unwrap();
    assert_eq!(r[0].result, Err(SyncError::Conflict));
    let server = r[0]
        .server_record
        .clone()
        .expect("conflict carries the server record");
    assert_eq!(server.text("text").as_deref(), Some("from-other-device"));
    assert_ne!(server.change_tag.as_deref(), Some(tag.as_str()));
    // On top of the server's tag the save lands.
    let mut retry = server.clone();
    retry.set("text", FieldValue::Text("merged".into()));
    let r = backend
        .save_records(&[retry], SavePolicy::IfServerUnchanged)
        .unwrap();
    assert!(r[0].result.is_ok(), "{:?}", r[0].result);
    assert_eq!(
        backend
            .fetch_record("n0")
            .unwrap()
            .unwrap()
            .text("text")
            .as_deref(),
        Some("merged")
    );
    helper.shutdown();
    other_device.shutdown();
}
