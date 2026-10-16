//! `DriveBackend` and `DriveAuth` against an in-process fake of the Drive v3 `appDataFolder`
//! slice (tests/common/fake_drive.rs). The fake follows Google's reference, so these tests prove
//! the backend's use of the documented API and its handling of the documented limits (no write
//! precondition); the behaviour of real Drive is not exercised here.

mod common;

use std::sync::{Arc, Mutex};

use common::fake_drive::FakeDrive;
use rightkit_http::testing::loopback_config;
use rightkit_http::Client;
use rightkit_sync::drive::*;
use rightkit_sync::wire::WireRecord;
use rightkit_sync::*;

struct Tok {
    token: Mutex<Option<String>>,
    replace_with: Mutex<Option<String>>,
    rejections: Mutex<Vec<String>>,
}

impl Tok {
    fn new(token: &str) -> Arc<Self> {
        Arc::new(Self {
            token: Mutex::new(Some(token.to_string())),
            replace_with: Mutex::new(None),
            rejections: Mutex::new(Vec::new()),
        })
    }
}

impl DriveTokenSource for Tok {
    fn access_token(&self) -> SyncResult<Option<String>> {
        Ok(self.token.lock().unwrap().clone())
    }
    fn rejected(&self, token: &str) {
        self.rejections.lock().unwrap().push(token.to_string());
        if let Some(next) = self.replace_with.lock().unwrap().take() {
            *self.token.lock().unwrap() = Some(next);
        }
    }
    fn account_scope(&self) -> Option<String> {
        Some("scope-1".into())
    }
}

fn config(drive: &FakeDrive, zone: &str) -> DriveConfig {
    let mut cfg = DriveConfig::new(zone);
    cfg.api_base = drive.base_url();
    cfg
}

fn backend_with(tok: Arc<Tok>, cfg: DriveConfig) -> DriveBackend {
    DriveBackend::new(Client::new(loopback_config()), tok, cfg)
}

fn backend(drive: &FakeDrive, zone: &str) -> DriveBackend {
    backend_with(Tok::new("t1"), config(drive, zone))
}

fn rec(name: &str, updated: i64, text: &str) -> RawRecord {
    let mut r = RawRecord::new("Note", name);
    r.set("id", FieldValue::Text(name.into()));
    r.set("text", FieldValue::Text(text.into()));
    r.set("createdAt", FieldValue::Date(100));
    r.set("updatedAt", FieldValue::Date(updated));
    r
}

fn wire(r: &RawRecord) -> Vec<u8> {
    serde_json::to_vec(&WireRecord::from(r)).unwrap()
}

fn one(mut outcomes: Vec<RecordOutcome>) -> RecordOutcome {
    assert_eq!(outcomes.len(), 1);
    outcomes.remove(0)
}

fn save(b: &DriveBackend, r: &RawRecord, policy: SavePolicy) -> RecordOutcome {
    one(b.save_records(std::slice::from_ref(r), policy).unwrap())
}

fn text(r: &RawRecord) -> String {
    r.text("text").expect("text field")
}

#[test]
fn prepare_creates_the_zone_folder_once_and_fetch_needs_it() {
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    assert_eq!(b.fetch_changes(None).unwrap_err(), SyncError::ZoneMissing);
    b.prepare().unwrap();
    b.prepare().unwrap();
    assert_eq!(d.folders("notes"), 1);
    assert_eq!(b.max_batch(), 25);
    // A second device preparing finds the same folder.
    backend(&d, "notes").prepare().unwrap();
    assert_eq!(d.folders("notes"), 1);
}

#[test]
fn two_folders_of_one_name_resolve_to_the_smallest_id() {
    let d = FakeDrive::start(&["t1"]);
    d.with(|s| {
        s.insert(
            "appDataFolder",
            "notes",
            common::fake_drive::FOLDER_MIME,
            vec![],
            Some("zz-late"),
        );
        s.insert(
            "appDataFolder",
            "notes",
            common::fake_drive::FOLDER_MIME,
            vec![],
            Some("aa-first"),
        );
    });
    let b = backend(&d, "notes");
    b.prepare().unwrap();
    save(&b, &rec("n1", 100, "v1"), SavePolicy::Overwrite)
        .result
        .unwrap();
    d.with(|s| {
        let id = s.ids_named("n1.json").remove(0);
        assert_eq!(s.files[&id].parent, "aa-first");
    });
}

#[test]
fn save_and_fetch_round_trip_uses_the_documented_requests() {
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();

    let mut full = rec("n1", 100, "héllo \"quoted\"");
    full.set("count", FieldValue::I64(-7));
    full.set("score", FieldValue::F64(2.5));
    full.set("blob", FieldValue::Bytes(vec![0, 1, 255]));
    full.set("tags", FieldValue::Strings(vec!["a".into(), "b".into()]));
    let outcomes = b
        .save_records(
            &[full.clone(), rec("n2", 101, "two")],
            SavePolicy::Overwrite,
        )
        .unwrap();
    assert!(outcomes
        .iter()
        .all(|o| o.result.is_ok() && o.server_record.is_none()));
    assert_eq!(d.record_files(), 2);

    // The stored file is the shared wire encoding, without a change tag.
    let stored = d.content_json("n1.json").unwrap();
    assert_eq!(stored["name"], "n1");
    assert_eq!(stored["type"], "Note");
    assert!(stored.get("change_tag").is_none());

    let got = b.fetch_record("n1").unwrap().unwrap();
    assert_eq!(got.record_name, "n1");
    assert_eq!(got.text("text").as_deref(), Some("héllo \"quoted\""));
    assert_eq!(got.get("count"), Some(&FieldValue::I64(-7)));
    assert_eq!(got.get("score"), Some(&FieldValue::F64(2.5)));
    assert_eq!(got.get("blob"), Some(&FieldValue::Bytes(vec![0, 1, 255])));
    assert_eq!(got.get("updatedAt"), Some(&FieldValue::Date(100)));
    assert!(got.change_tag.as_deref().is_some_and(|t| t.contains(':')));
    assert_eq!(b.fetch_record("absent").unwrap(), None);

    // A second overwrite updates the same file in place.
    let mut again = rec("n1", 200, "edited");
    again.change_tag = got.change_tag.clone();
    save(&b, &again, SavePolicy::Overwrite).result.unwrap();
    assert_eq!(d.record_files(), 2);
    assert_eq!(d.version_of("n1.json"), Some(2));

    let reqs = d.server.requests();
    assert!(reqs
        .iter()
        .all(|r| r.header("authorization") == Some("Bearer t1")));
    let uploads: Vec<_> = reqs
        .iter()
        .filter(|r| r.path().starts_with("/upload/drive/v3/files"))
        .collect();
    assert!(uploads.len() >= 3);
    for u in &uploads {
        assert!(u
            .query_pairs()
            .contains(&("uploadType".into(), "multipart".into())));
        assert!(u
            .header("content-type")
            .is_some_and(|c| c.starts_with("multipart/related; boundary=")));
    }
    assert!(uploads.iter().any(|u| u.method == "POST"));
    assert!(uploads.iter().any(|u| u.method == "PATCH"));
    for r in reqs.iter().filter(|r| r.path() == "/drive/v3/files") {
        if r.method == "GET" {
            assert!(r
                .query_pairs()
                .contains(&("spaces".into(), "appDataFolder".into())));
        }
    }
}

#[test]
fn first_fetch_lists_page_by_page_then_follows_the_change_feed() {
    let d = FakeDrive::start(&["t1"]);
    let writer = backend(&d, "notes");
    writer.prepare().unwrap();
    let records: Vec<RawRecord> = (0..5)
        .map(|i| rec(&format!("n{i}"), 100 + i, "v1"))
        .collect();
    assert!(writer
        .save_records(&records, SavePolicy::Overwrite)
        .unwrap()
        .iter()
        .all(|o| o.result.is_ok()));

    let mut cfg = config(&d, "notes");
    cfg.page_size = 2;
    let reader = backend_with(Tok::new("t1"), cfg);
    let mut since: Option<Vec<u8>> = None;
    let mut names = Vec::new();
    let mut pages = 0;
    // The reader never called prepare: the folder is resolved on first use.
    loop {
        let page = reader.fetch_changes(since.as_deref()).unwrap();
        pages += 1;
        assert!(page.deleted_record_names.is_empty());
        names.extend(page.records.iter().map(|r| r.record_name.clone()));
        since = page.new_token.clone();
        if !page.more_coming {
            break;
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(names, vec!["n0", "n1", "n2", "n3", "n4"]);
    let starts = d
        .server
        .requests()
        .iter()
        .filter(|r| r.path() == "/drive/v3/changes/startPageToken")
        .count();
    assert_eq!(
        starts, 1,
        "the start token is read once, before the listing"
    );

    // Caught up.
    let page = reader.fetch_changes(since.as_deref()).unwrap();
    assert!(page.records.is_empty() && !page.more_coming);
    since = page.new_token;

    // An edit and a new record arrive through the feed; foreign files do not.
    let mut edit = rec("n2", 300, "v2");
    edit.change_tag = reader.fetch_record("n2").unwrap().unwrap().change_tag;
    save(&writer, &edit, SavePolicy::Overwrite).result.unwrap();
    save(&writer, &rec("n9", 400, "new"), SavePolicy::Overwrite)
        .result
        .unwrap();
    d.with(|s| {
        s.insert(
            "appDataFolder",
            "other.json",
            "application/json",
            b"{}".to_vec(),
            None,
        );
        let other = s.insert(
            "appDataFolder",
            "other-zone",
            common::fake_drive::FOLDER_MIME,
            vec![],
            None,
        );
        s.insert(
            &other,
            "n7.json",
            "application/json",
            wire(&rec("n7", 1, "x")),
            None,
        );
    });
    let page = reader.fetch_changes(since.as_deref()).unwrap();
    let got: Vec<(String, String)> = page
        .records
        .iter()
        .map(|r| (r.record_name.clone(), text(r)))
        .collect();
    assert_eq!(
        got,
        vec![
            ("n2".to_string(), "v2".to_string()),
            ("n9".to_string(), "new".to_string())
        ]
    );
    let page2 = reader.fetch_changes(page.new_token.as_deref()).unwrap();
    assert!(page2.records.is_empty());
}

#[test]
fn conditional_saves_conflict_with_the_server_record() {
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();

    save(&b, &rec("n1", 100, "v1"), SavePolicy::IfServerUnchanged)
        .result
        .unwrap();

    // Creating a name that exists is refused and hands back the server's record.
    let o = save(&b, &rec("n1", 150, "dup"), SavePolicy::IfServerUnchanged);
    assert_eq!(o.result, Err(SyncError::Conflict));
    let server = o.server_record.expect("server record");
    assert_eq!(text(&server), "v1");
    assert_eq!(d.record_files(), 1);

    // An edit on the fetched tag lands.
    let mut edit = rec("n1", 200, "v2");
    edit.change_tag = server.change_tag.clone();
    save(&b, &edit, SavePolicy::IfServerUnchanged)
        .result
        .unwrap();
    assert_eq!(d.version_of("n1.json"), Some(2));

    // The same (now stale) tag is refused, with the current content and tag.
    let mut stale = rec("n1", 300, "v3");
    stale.change_tag = server.change_tag.clone();
    let o = save(&b, &stale, SavePolicy::IfServerUnchanged);
    assert_eq!(o.result, Err(SyncError::Conflict));
    let now = o.server_record.unwrap();
    assert_eq!(text(&now), "v2");
    assert!(now.change_tag.as_deref().unwrap().ends_with(":2"));
    assert_eq!(
        text(&d_record(&d, "n1.json")),
        "v2",
        "a refused save writes nothing"
    );

    // Retrying on the returned tag lands.
    let mut retry = rec("n1", 300, "v3");
    retry.change_tag = now.change_tag.clone();
    save(&b, &retry, SavePolicy::IfServerUnchanged)
        .result
        .unwrap();
    assert_eq!(text(&d_record(&d, "n1.json")), "v3");

    // Another client wrote first: refused too.
    let fresh = b.fetch_record("n1").unwrap().unwrap();
    let id = d.ids_named("n1.json").remove(0);
    d.with(|s| s.patch(&id, wire(&rec("n1", 999, "other-device"))));
    let mut mine = rec("n1", 400, "mine");
    mine.change_tag = fresh.change_tag;
    let o = save(&b, &mine, SavePolicy::IfServerUnchanged);
    assert_eq!(o.result, Err(SyncError::Conflict));
    assert_eq!(text(&o.server_record.unwrap()), "other-device");

    // The file this write was based on vanished: a conflict without a server record.
    d.with(|s| s.remove(&id));
    let mut orphan = rec("n1", 500, "orphan");
    orphan.change_tag = Some(format!("{id}:5"));
    let o = save(&b, &orphan, SavePolicy::IfServerUnchanged);
    assert_eq!(o.result, Err(SyncError::Conflict));
    assert!(o.server_record.is_none());
}

fn d_record(d: &FakeDrive, file: &str) -> RawRecord {
    let v = d.content_json(file).expect("file content");
    serde_json::from_value::<WireRecord>(v)
        .unwrap()
        .into_raw()
        .unwrap()
}

/// Drive documents no write precondition, so a competing write that commits between the
/// backend's version check and its PATCH is overwritten. This pins the documented race window
/// (module docs of `drive`): the save reports success and the last writer wins.
#[test]
fn a_write_inside_the_check_then_write_window_is_overwritten() {
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();
    save(&b, &rec("n1", 100, "v1"), SavePolicy::IfServerUnchanged)
        .result
        .unwrap();
    let base = b.fetch_record("n1").unwrap().unwrap();
    let id = d.ids_named("n1.json").remove(0);
    let rival = wire(&rec("n1", 150, "rival"));
    d.hook_before_patch({
        let id = id.clone();
        move |s| s.patch(&id, rival)
    });
    let mut mine = rec("n1", 200, "ours");
    mine.change_tag = base.change_tag;
    save(&b, &mine, SavePolicy::IfServerUnchanged)
        .result
        .unwrap();
    assert_eq!(text(&d_record(&d, "n1.json")), "ours");
    assert_eq!(
        d.version_of("n1.json"),
        Some(3),
        "the rival's write was version 2"
    );
}

#[test]
fn concurrent_creation_keeps_the_smallest_file_id() {
    // Conditional create: the rival lands after our "does not exist" listing and wins.
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();
    let rival = wire(&rec("n1", 150, "rival"));
    d.hook_before_create(move |s| {
        let folder = s.folder_id("notes").unwrap();
        s.insert(
            &folder,
            "n1.json",
            "application/json",
            rival,
            Some("a-rival"),
        );
    });
    let o = save(&b, &rec("n1", 200, "ours"), SavePolicy::IfServerUnchanged);
    assert_eq!(o.result, Err(SyncError::Conflict));
    assert_eq!(text(&o.server_record.unwrap()), "rival");
    assert_eq!(
        d.ids_named("n1.json"),
        vec!["a-rival".to_string()],
        "our duplicate was removed"
    );

    // Overwrite create: the content goes into the winning file.
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();
    let rival = wire(&rec("n1", 150, "rival"));
    d.hook_before_create(move |s| {
        let folder = s.folder_id("notes").unwrap();
        s.insert(
            &folder,
            "n1.json",
            "application/json",
            rival,
            Some("a-rival"),
        );
    });
    save(&b, &rec("n1", 200, "ours"), SavePolicy::Overwrite)
        .result
        .unwrap();
    assert_eq!(d.ids_named("n1.json"), vec!["a-rival".to_string()]);
    assert_eq!(text(&d_record(&d, "n1.json")), "ours");

    // A rival with a larger id loses: it stays, ours is the smallest and wins.
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();
    let rival = wire(&rec("n1", 150, "rival"));
    d.hook_before_create(move |s| {
        let folder = s.folder_id("notes").unwrap();
        s.insert(
            &folder,
            "n1.json",
            "application/json",
            rival,
            Some("zzz-rival"),
        );
    });
    save(&b, &rec("n1", 200, "ours"), SavePolicy::IfServerUnchanged)
        .result
        .unwrap();
    assert_eq!(d.ids_named("n1.json").len(), 2);
}

#[test]
fn tombstones_are_kept_and_removed_files_are_not_deletions() {
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();
    save(&b, &rec("n1", 100, "v1"), SavePolicy::Overwrite)
        .result
        .unwrap();
    save(&b, &rec("n2", 101, "v1"), SavePolicy::Overwrite)
        .result
        .unwrap();
    let cursor = b.fetch_changes(None).unwrap().new_token;

    let mut t = b.fetch_record("n1").unwrap().unwrap();
    write_tombstone(&mut t, "n1", 500, &[]);
    save(&b, &t, SavePolicy::Overwrite).result.unwrap();
    assert_eq!(
        d.record_files(),
        2,
        "a deletion is a record write, not a Drive delete"
    );

    let page = b.fetch_changes(cursor.as_deref()).unwrap();
    assert_eq!(page.records.len(), 1);
    let tomb = &page.records[0];
    assert_eq!(tomb.record_name, "n1");
    assert_eq!(read_tombstone(tomb), Some(("n1".to_string(), 500)));
    assert!(tomb.get("text").is_none());
    let tomb = b.fetch_record("n1").unwrap().unwrap();
    assert!(tomb.date_ms("deletedAt").is_some());

    // A record file removed behind our back is not turned into a deletion on this device.
    let id = d.ids_named("n2.json").remove(0);
    d.with(|s| s.remove(&id));
    let page = b.fetch_changes(page.new_token.as_deref()).unwrap();
    assert!(page.records.is_empty());
    assert!(page.deleted_record_names.is_empty());
}

#[test]
fn a_removed_zone_folder_is_reported_missing_and_recovers_after_prepare() {
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();
    save(&b, &rec("n1", 100, "v1"), SavePolicy::Overwrite)
        .result
        .unwrap();
    let cursor = b.fetch_changes(None).unwrap().new_token;

    d.delete_app_data();
    assert_eq!(
        b.fetch_changes(cursor.as_deref()).unwrap_err(),
        SyncError::ZoneMissing
    );
    assert_eq!(b.fetch_changes(None).unwrap_err(), SyncError::ZoneMissing);

    b.prepare().unwrap();
    assert_eq!(d.folders("notes"), 1);
    assert!(b.fetch_changes(None).unwrap().records.is_empty());
    save(&b, &rec("n1", 100, "again"), SavePolicy::Overwrite)
        .result
        .unwrap();
    assert_eq!(b.fetch_changes(None).unwrap().records.len(), 1);
}

fn is_retry(e: &SyncError, secs: f64) -> bool {
    matches!(e, SyncError::RetryAfter { secs: s, .. } if (*s - secs).abs() < f64::EPSILON)
}

#[test]
fn drive_errors_map_to_engine_classes() {
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();

    d.fail_next(403, &[], "userRateLimitExceeded");
    assert!(is_retry(&b.fetch_record("x").unwrap_err(), 30.0));
    d.fail_next(403, &[("Retry-After", "7")], "rateLimitExceeded");
    assert!(is_retry(&b.fetch_record("x").unwrap_err(), 7.0));
    d.fail_next(429, &[("Retry-After", "11")], "rateLimitExceeded");
    assert!(is_retry(&b.fetch_record("x").unwrap_err(), 11.0));
    d.fail_next(429, &[], "rateLimitExceeded");
    assert!(is_retry(&b.fetch_record("x").unwrap_err(), 30.0));
    for status in [500, 502, 503, 504] {
        d.fail_next(status, &[], "backendError");
        assert!(
            is_retry(&b.fetch_record("x").unwrap_err(), 30.0),
            "{status}"
        );
    }
    d.fail_next(403, &[], "storageQuotaExceeded");
    assert!(matches!(
        b.fetch_record("x").unwrap_err(),
        SyncError::Unavailable(_)
    ));
    d.fail_next(403, &[], "insufficientPermissions");
    assert!(matches!(
        b.fetch_record("x").unwrap_err(),
        SyncError::Unavailable(_)
    ));
    d.fail_next(400, &[], "badRequest");
    assert!(matches!(
        b.fetch_record("x").unwrap_err(),
        SyncError::Unavailable(_)
    ));
    d.fail_next(409, &[], "conflict");
    assert!(matches!(
        b.fetch_record("x").unwrap_err(),
        SyncError::Unavailable(_)
    ));

    // 404 on a record is "absent", never an error.
    assert_eq!(b.fetch_record("absent").unwrap(), None);

    // Cursors: foreign bytes, an unknown feed token and an unknown listing token all replay.
    assert_eq!(
        b.fetch_changes(Some(&b"garbage"[..])).unwrap_err(),
        SyncError::TokenExpired
    );
    assert_eq!(
        b.fetch_changes(Some(&br#"{"v":1,"p":"chg","t":"99999"}"#[..]))
            .unwrap_err(),
        SyncError::TokenExpired
    );
    assert_eq!(
        b.fetch_changes(Some(&br#"{"v":1,"p":"list","t":"zzz","s":"0"}"#[..]))
            .unwrap_err(),
        SyncError::TokenExpired
    );
    d.fail_next(410, &[], "gone");
    assert_eq!(
        b.fetch_changes(Some(&br#"{"v":1,"p":"chg","t":"0"}"#[..]))
            .unwrap_err(),
        SyncError::TokenExpired
    );
}

#[test]
fn a_401_refreshes_the_token_once_then_reports_signed_out() {
    let d = FakeDrive::start(&["t1"]);
    let tok = Tok::new("t1");
    let b = backend_with(tok.clone(), config(&d, "notes"));
    b.prepare().unwrap();

    // The server stops accepting t1; the source hands out t2, which the server accepts.
    d.revoke("t1");
    d.accept("t2");
    *tok.replace_with.lock().unwrap() = Some("t2".into());
    assert_eq!(b.fetch_record("x").unwrap(), None);
    assert_eq!(*tok.rejections.lock().unwrap(), vec!["t1".to_string()]);

    // No replacement: the second 401 is final.
    d.revoke("t2");
    assert!(matches!(
        b.fetch_record("x").unwrap_err(),
        SyncError::Unavailable(_)
    ));

    // Signed out: nothing is sent.
    *tok.token.lock().unwrap() = None;
    let before = d.server.request_count();
    assert!(matches!(
        b.fetch_record("x").unwrap_err(),
        SyncError::Unavailable(_)
    ));
    assert_eq!(d.server.request_count(), before);
}

#[test]
fn a_service_error_on_the_first_record_fails_the_call() {
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();
    d.fail_next(503, &[], "backendError");
    let err = b
        .save_records(
            &[rec("n1", 1, "a"), rec("n2", 2, "b")],
            SavePolicy::Overwrite,
        )
        .unwrap_err();
    assert!(is_retry(&err, 30.0));
    assert_eq!(d.record_files(), 0);
}

#[test]
fn an_oversized_record_is_refused_without_sending_anything() {
    let d = FakeDrive::start(&["t1"]);
    let mut cfg = config(&d, "notes");
    cfg.max_record_bytes = 300;
    let b = backend_with(Tok::new("t1"), cfg);
    b.prepare().unwrap();
    let before = d.server.request_count();
    let big = rec("n1", 1, &"x".repeat(500));
    let o = save(&b, &big, SavePolicy::Overwrite);
    assert!(matches!(o.result, Err(SyncError::Persistence(_))));
    assert_eq!(d.server.request_count(), before);
    // A small one still goes through in the same call.
    let out = b
        .save_records(&[big, rec("n2", 2, "ok")], SavePolicy::Overwrite)
        .unwrap();
    assert!(out[0].result.is_err() && out[1].result.is_ok());
}

#[test]
fn unreadable_files_are_skipped_counted_and_never_overwritten() {
    let d = FakeDrive::start(&["t1"]);
    let b = backend(&d, "notes");
    b.prepare().unwrap();
    d.with(|s| {
        let folder = s.folder_id("notes").unwrap();
        s.insert(
            &folder,
            "bad.json",
            "application/json",
            b"not json".to_vec(),
            None,
        );
    });
    save(&b, &rec("good", 1, "ok"), SavePolicy::Overwrite)
        .result
        .unwrap();
    let page = b.fetch_changes(None).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(b.malformed_skipped(), 1);

    let o = save(&b, &rec("bad", 2, "mine"), SavePolicy::IfServerUnchanged);
    assert!(matches!(o.result, Err(SyncError::Persistence(_))));
    assert_eq!(
        d.with(|s| {
            let id = s.ids_named("bad.json").remove(0);
            s.files[&id].content.clone()
        }),
        b"not json".to_vec()
    );
}

#[test]
fn drive_auth_follows_the_token_source() {
    let tok = Tok::new("t1");
    let auth = DriveAuth::new(tok.clone());
    assert!(auth.enabled());
    auth.availability().unwrap();
    assert_eq!(auth.account_scope().as_deref(), Some("scope-1"));
    assert_eq!(auth.bearer().unwrap().as_deref(), Some("t1"));
    let g = auth.generation();
    auth.set_enabled(false);
    assert!(!auth.enabled());
    assert_ne!(auth.generation(), g, "disabling cancels an in-flight pass");

    *tok.token.lock().unwrap() = None;
    assert!(matches!(
        auth.availability(),
        Err(SyncError::Unavailable(_))
    ));

    let closure = DriveAuth::new(Arc::new(FnToken(|| Ok(Some("abc".to_string())))));
    assert_eq!(closure.bearer().unwrap().as_deref(), Some("abc"));

    assert_eq!(
        account_scope("app", "notes", "u1"),
        account_scope("app", "notes", "u1")
    );
    assert_ne!(
        account_scope("app", "notes", "u1"),
        account_scope("app", "notes", "u2")
    );
    assert!(sync_config().require_consent_on_scope_change);
}
