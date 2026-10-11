//! Replace mode / durability, overwrite-on-corrupt, quarantine naming, trailing newline, and the
//! flat migration hook.
use rightkit_fs::atomic::testing::on_replace;
use rightkit_fs::atomic::{
    ReplaceApi, ReplaceCall, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    REPLACEFILE_WRITE_THROUGH,
};
use rightkit_settings::{
    Error, FileFormat, OnCorrupt, QuarantineName, ReplaceMode, Schema, Store, StoreOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
struct Za {
    z: u32,
    a: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
struct Cfg {
    z: u32,
    a: u32,
    new_name: String,
    n: u32,
}

type Call = (ReplaceApi, u32);
type Migrated = Result<(serde_json::Map<String, Value>, bool), Error>;

fn schema() -> Schema {
    Schema::new("w", 1)
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".lock"))
        .collect();
    v.sort();
    v
}

fn recorder() -> (
    Rc<RefCell<Vec<ReplaceCall>>>,
    rightkit_fs::atomic::testing::HookGuard,
) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let c = calls.clone();
    let guard = on_replace(move |call| {
        c.borrow_mut().push(call.clone());
        None
    });
    (calls, guard)
}

// ---- 1. replace mode, durable, retry --------------------------------------------------------

#[test]
fn configured_replace_mode_and_flags_reach_the_replace_call() {
    use ReplaceApi::{MoveFileEx, Rename, ReplaceFile};
    let both = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    // (mode, durable) -> (first creation, replacing an existing file), as the platform does it.
    let table: [(ReplaceMode, bool, Call, Call); 5] = [
        (ReplaceMode::Rename, true, (Rename, 0), (Rename, 0)),
        (
            ReplaceMode::ReplaceFile,
            true,
            (MoveFileEx, both),
            (ReplaceFile, REPLACEFILE_WRITE_THROUGH),
        ),
        (
            ReplaceMode::ReplaceFile,
            false,
            (MoveFileEx, MOVEFILE_REPLACE_EXISTING),
            (ReplaceFile, 0),
        ),
        (
            ReplaceMode::MoveWriteThrough,
            true,
            (MoveFileEx, both),
            (MoveFileEx, both),
        ),
        (
            ReplaceMode::MoveWriteThrough,
            false,
            (MoveFileEx, both),
            (MoveFileEx, both),
        ),
    ];
    for (mode, durable, create, replace) in table {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        let (calls, _guard) = recorder();
        let opts = StoreOptions::new(schema())
            .format(FileFormat::Flat)
            .replace_mode(mode)
            .durable(durable);
        let s = Store::<Cfg>::open_path(&p, opts).unwrap();
        s.update(|v| {
            v.n = 5;
            Ok(())
        })
        .unwrap();
        let calls = calls.borrow();
        assert_eq!(calls.len(), 2, "{mode:?} durable={durable}");
        let expect = |want: (ReplaceApi, u32)| if cfg!(windows) { want } else { (Rename, 0) };
        assert_eq!(
            (calls[0].api, calls[0].flags),
            expect(create),
            "{mode:?} durable={durable} create"
        );
        assert_eq!(
            (calls[1].api, calls[1].flags),
            expect(replace),
            "{mode:?} durable={durable} replace"
        );
        assert_eq!(calls[1].target, p);
        println!(
            "ok  {mode:?} durable={durable}: {:?} / {:?}",
            (calls[0].api, calls[0].flags),
            (calls[1].api, calls[1].flags)
        );
    }
}

#[test]
fn default_store_options_keep_rename_and_durable() {
    let d = tempfile::tempdir().unwrap();
    let (calls, _guard) = recorder();
    let s = Store::<Cfg>::open_path(d.path().join("s.json"), StoreOptions::new(schema())).unwrap();
    s.update(|v| {
        v.n = 1;
        Ok(())
    })
    .unwrap();
    for c in calls.borrow().iter() {
        assert_eq!((c.api, c.flags), (ReplaceApi::Rename, 0));
    }
}

#[test]
fn a_failed_replace_is_an_io_error_and_leaves_the_old_file_and_no_temp() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let s = Store::<Cfg>::open_path(&p, StoreOptions::new(schema()).sidecars(false)).unwrap();
    let before = (fs::read(&p).unwrap(), listing(d.path()));
    let _guard = on_replace(|_| Some(std::io::Error::other("injected")));
    let r = s.update(|v| {
        v.n = 9;
        Ok(())
    });
    assert!(matches!(r, Err(Error::Io(_))), "{r:?}");
    assert_eq!((fs::read(&p).unwrap(), listing(d.path())), before);
}

#[test]
fn sharing_violation_retry_reaches_the_replace_call() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let opts = StoreOptions::new(schema())
        .replace_mode(ReplaceMode::MoveWriteThrough)
        .retry_sharing_violation(3, Duration::ZERO);
    let s = Store::<Cfg>::open_path(&p, opts).unwrap();
    let calls = Rc::new(RefCell::new(0u32));
    let c = calls.clone();
    let _guard = on_replace(move |_| {
        let mut n = c.borrow_mut();
        *n += 1;
        // ERROR_SHARING_VIOLATION on the first two attempts.
        (*n <= 2).then(|| std::io::Error::from_raw_os_error(32))
    });
    let r = s.update(|v| {
        v.n = 2;
        Ok(())
    });
    if cfg!(windows) {
        r.unwrap();
        assert_eq!(*calls.borrow(), 3, "two retries then success");
        assert_eq!(s.get().n, 2);
    } else {
        // Inert off Windows: the first error stands.
        assert!(matches!(r, Err(Error::Io(_))));
        assert_eq!(*calls.borrow(), 1);
    }
}

// ---- 2. OnCorrupt::Overwrite ----------------------------------------------------------------

#[test]
fn overwrite_never_renames_so_a_failed_write_keeps_the_old_bytes() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    fs::write(&p, b"{ broken").unwrap();
    let before = listing(d.path());
    {
        let _guard = on_replace(|_| Some(std::io::Error::other("injected")));
        let opened = Store::<Cfg>::open_path(
            &p,
            StoreOptions::new(schema()).on_corrupt(OnCorrupt::Overwrite),
        );
        assert!(matches!(opened, Err(Error::Io(_))));
    }
    assert_eq!(
        fs::read(&p).unwrap(),
        b"{ broken",
        "file untouched by the failed write"
    );
    assert_eq!(listing(d.path()), before, "no quarantine file, no temp");
    // Without the fault the same open replaces it, still without a quarantine copy.
    let s = Store::<Cfg>::open_path(
        &p,
        StoreOptions::new(schema()).on_corrupt(OnCorrupt::Overwrite),
    )
    .unwrap();
    assert_eq!(s.get(), Cfg::default());
    assert!(serde_json::from_slice::<Value>(&fs::read(&p).unwrap()).is_ok());
    assert!(
        listing(d.path()).iter().all(|n| !n.contains("corrupt")),
        "{:?}",
        listing(d.path())
    );
}

#[test]
fn overwrite_never_copies_a_damaged_file_over_a_good_backup() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let bak = d.path().join("s.json.bak");
    let good = serde_json::to_vec(&json!({
        "schemaVersion": 1, "namespace": "w", "values": {"n": 4}, "revision": 1
    }))
    .unwrap();
    fs::write(&bak, &good).unwrap();
    fs::write(&p, b"{ broken").unwrap();
    let s = Store::<Cfg>::open_path(
        &p,
        StoreOptions::new(schema()).on_corrupt(OnCorrupt::Overwrite),
    )
    .unwrap();
    assert_eq!(s.get().n, 4, "restored from the backup");
    assert_eq!(fs::read(&bak).unwrap(), good, "the good .bak is intact");
}

#[test]
fn overwrite_with_keep_copy_copies_before_the_replace_never_renames() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    fs::write(&p, b"{ broken").unwrap();
    let opts = || {
        StoreOptions::new(schema())
            .on_corrupt(OnCorrupt::Overwrite)
            .keep_corrupt_copy(true)
            .quarantine_name(QuarantineName::Fixed(".corrupt".into()))
    };
    {
        let _guard = on_replace(|_| Some(std::io::Error::other("injected")));
        assert!(Store::<Cfg>::open_path(&p, opts()).is_err());
    }
    // The copy exists, and the original is still in place.
    assert_eq!(
        fs::read(d.path().join("s.json.corrupt")).unwrap(),
        b"{ broken"
    );
    assert_eq!(fs::read(&p).unwrap(), b"{ broken");
    Store::<Cfg>::open_path(&p, opts()).unwrap();
    assert_eq!(
        fs::read(d.path().join("s.json.corrupt")).unwrap(),
        b"{ broken"
    );
    assert!(serde_json::from_slice::<Value>(&fs::read(&p).unwrap()).is_ok());
}

// ---- 3. quarantine naming -------------------------------------------------------------------

#[test]
fn fixed_quarantine_name_overwrites_the_previous_quarantine() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let opts =
        || StoreOptions::new(schema()).quarantine_name(QuarantineName::Fixed(".corrupt".into()));
    fs::write(&p, b"{ first").unwrap();
    Store::<Cfg>::open_path(&p, opts()).unwrap();
    fs::write(&p, b"{ second").unwrap();
    Store::<Cfg>::open_path(&p, opts()).unwrap();
    assert_eq!(
        fs::read(d.path().join("s.json.corrupt")).unwrap(),
        b"{ second"
    );
    let corrupt: Vec<_> = listing(d.path())
        .into_iter()
        .filter(|n| n.contains("corrupt"))
        .collect();
    assert_eq!(corrupt, vec!["s.json.corrupt".to_string()]);
}

#[test]
fn timestamped_quarantine_is_the_default() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    fs::write(&p, b"{ first").unwrap();
    Store::<Cfg>::open_path(&p, StoreOptions::new(schema())).unwrap();
    assert!(listing(d.path())
        .iter()
        .any(|n| n.starts_with("s.json.corrupt-")));
}

#[test]
fn a_bad_quarantine_suffix_is_refused_at_open() {
    for bad in ["", "a/b", "a\\b"] {
        let d = tempfile::tempdir().unwrap();
        let r = Store::<Cfg>::open_path(
            d.path().join("s.json"),
            StoreOptions::new(schema()).quarantine_name(QuarantineName::Fixed(bad.into())),
        );
        assert!(matches!(r, Err(Error::BadSchema(_))), "{bad:?}");
    }
}

// ---- 4. trailing newline --------------------------------------------------------------------

#[test]
fn trailing_newline_does_not_sort_keys_or_rewrite_an_unchanged_file() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let original = "{\n  \"z\": 1,\n  \"a\": 2\n}";
    fs::write(&p, original).unwrap();
    let opts = |nl: bool| {
        StoreOptions::new(schema())
            .format(FileFormat::Flat)
            .trailing_newline(nl)
    };
    let s = Store::<Za>::open_path(&p, opts(true)).unwrap();
    // Nothing changed: the bytes stay exactly as they were.
    s.patch(&json!({"a": 2}), None).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), original);
    // A real change keeps key order and adds the newline.
    s.patch(&json!({"a": 3}), None).unwrap();
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        "{\n  \"z\": 1,\n  \"a\": 3\n}\n"
    );
    // Off (default): the file stays without one.
    let d2 = tempfile::tempdir().unwrap();
    let p2 = d2.path().join("s.json");
    fs::write(&p2, original).unwrap();
    let s2 = Store::<Za>::open_path(&p2, opts(false)).unwrap();
    s2.patch(&json!({"a": 3}), None).unwrap();
    assert!(!fs::read(&p2).unwrap().ends_with(b"\n"));
}

#[test]
fn trailing_newline_on_a_new_flat_file_and_on_the_envelope() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("flat.json");
    Store::<Cfg>::open_path(
        &p,
        StoreOptions::new(schema())
            .format(FileFormat::Flat)
            .trailing_newline(true),
    )
    .unwrap();
    let text = fs::read_to_string(&p).unwrap();
    assert!(text.ends_with("}\n") && text.contains("\n  \""), "{text:?}");
    let q = d.path().join("env.json");
    Store::<Cfg>::open_path(&q, StoreOptions::new(schema()).trailing_newline(true)).unwrap();
    assert!(fs::read(&q).unwrap().ends_with(b"\n"));
    // Default: unchanged, no newline.
    let r = d.path().join("plain.json");
    Store::<Cfg>::open_path(&r, StoreOptions::new(schema()).format(FileFormat::Flat)).unwrap();
    assert!(!fs::read(&r).unwrap().ends_with(b"\n"));
}

// ---- 5. flat migration hook -----------------------------------------------------------------

fn rename_hook(
    calls: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Vec<String>>>>,
) -> impl Fn(serde_json::Map<String, Value>) -> Migrated + Send + Sync + 'static {
    move |mut m| {
        calls.fetch_add(1, Ordering::SeqCst);
        seen.lock().unwrap().push(m.keys().cloned().collect());
        match m.remove("old_name") {
            Some(v) => {
                m.insert("new_name".into(), v);
                Ok((m, true))
            }
            None => Ok((m, false)),
        }
    }
}

#[test]
fn flat_migration_runs_once_on_open_before_defaults_and_commits_once() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    fs::write(&p, "{\n  \"old_name\": \"x\",\n  \"n\": 1\n}").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let opts = StoreOptions::new(schema())
        .format(FileFormat::Flat)
        .defaults_provider(|| json!({"z": 8, "a": 9, "new_name": "dflt", "n": 7}))
        .flat_migration(rename_hook(calls.clone(), seen.clone()));
    let s = Store::<Cfg>::open_path(&p, opts.clone()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "once");
    assert_eq!(
        seen.lock().unwrap()[0],
        vec!["n".to_string(), "old_name".to_string()],
        "the hook sees the stored keys only, no defaults"
    );
    assert_eq!(s.get().new_name, "x");
    let on_disk: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    assert_eq!(on_disk["new_name"], "x");
    assert!(on_disk.get("old_name").is_none());
    assert_eq!(on_disk["n"], 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "get and the commit do not run it again"
    );
    // A second open runs it (once more), finds nothing to change, and writes nothing.
    let bytes = fs::read(&p).unwrap();
    Store::<Cfg>::open_path(&p, opts).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(fs::read(&p).unwrap(), bytes);
}

#[test]
fn flat_migration_runs_before_repair() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    fs::write(&p, r#"{"old_name": "x", "n": 1}"#).unwrap();
    // The hook produces an invalid `n`; repair then takes the default for it.
    let opts = StoreOptions::new(schema())
        .format(FileFormat::Flat)
        .defaults_provider(|| json!({"z": 0, "a": 0, "new_name": "", "n": 7}))
        .flat_migration(|mut m| {
            m.insert("n".into(), json!("bad"));
            Ok((m, true))
        });
    let s = Store::<Cfg>::open_path(&p, opts).unwrap();
    assert_eq!(s.get().n, 7);
}

#[test]
fn flat_migration_is_not_run_read_only_for_missing_files_or_envelopes() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mk = |format| {
        StoreOptions::new(schema())
            .format(format)
            .flat_migration(rename_hook(calls.clone(), seen.clone()))
    };
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    fs::write(&p, r#"{"old_name": "x"}"#).unwrap();
    let before = fs::read(&p).unwrap();
    let ro = Store::<Cfg>::open_read_only(&p, mk(FileFormat::Flat)).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0, "read-only");
    assert_eq!(ro.get().new_name, "", "the unmigrated file");
    assert_eq!(fs::read(&p).unwrap(), before);
    // Missing file: nothing to migrate.
    Store::<Cfg>::open_path(d.path().join("missing.json"), mk(FileFormat::Flat)).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0, "missing file");
    // Envelope format: the hook is for flat files only.
    Store::<Cfg>::open_path(d.path().join("env.json"), mk(FileFormat::Envelope)).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0, "envelope");
}

#[test]
fn a_failing_flat_migration_aborts_the_open_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    fs::write(&p, r#"{"old_name": "x"}"#).unwrap();
    let before = (fs::read(&p).unwrap(), listing(d.path()));
    let opts = StoreOptions::new(schema())
        .format(FileFormat::Flat)
        .flat_migration(|_| Err(Error::Invalid("nope".into())));
    assert!(matches!(
        Store::<Cfg>::open_path(&p, opts),
        Err(Error::Invalid(_))
    ));
    assert_eq!((fs::read(&p).unwrap(), listing(d.path())), before);
}
