use rightkit_settings::{
    Edit, Error, FileBackend, FileFormat, NullMeans, Schema, SettingsBackend, Store, StoreOptions,
    Stored,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    theme: String,
    font_px: u32,
    notify: bool,
}
impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: "auto".into(),
            font_px: 16,
            notify: true,
        }
    }
}

fn schema() -> Schema {
    Schema::new("demo", 2)
        .migration(1, |m| {
            if let Some(t) = m.remove("colorTheme") {
                m.insert("theme".into(), t);
            }
            Ok(())
        })
        .migration(2, |m| {
            if m.get("theme") == Some(&json!("legacy")) {
                m.insert("theme".into(), json!("auto"));
            }
            Ok(())
        })
}

fn read_json(p: &Path) -> Value {
    serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn open_path_uses_the_given_file_outside_any_namespace_directory() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("elsewhere").join("prefs.json");
    let s = Store::<Prefs>::open_path(&p, StoreOptions::new(schema())).unwrap();
    s.update(|v| {
        v.theme = "dark".into();
        Ok(())
    })
    .unwrap();
    assert_eq!(s.path(), p.as_path());
    assert!(!d.path().join("demo").exists());
    let doc = read_json(&p);
    assert_eq!(doc["schemaVersion"], 2);
    assert_eq!(doc["values"]["theme"], "dark");
}

#[test]
fn defaults_provider_leaves_no_file_until_the_first_save() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("app").join("settings.json");
    let opts = StoreOptions::new(schema())
        .defaults_provider(|| json!({"theme": "dark", "font_px": 18, "notify": false}));
    let s = Store::<Prefs>::open_path(&p, opts).unwrap();
    assert_eq!(
        s.get(),
        Prefs {
            theme: "dark".into(),
            font_px: 18,
            notify: false
        }
    );
    assert!(
        !d.path().join("app").exists(),
        "open must not create anything"
    );
    // An update that changes nothing commits nothing.
    s.update(|v| {
        v.font_px = 18;
        Ok(())
    })
    .unwrap();
    assert!(!p.exists());
    // The first real change writes the file, starting from the provider's values.
    s.patch(&json!({"font_px": 22}), None).unwrap();
    let doc = read_json(&p);
    assert_eq!(doc["values"]["font_px"], 22);
    assert_eq!(doc["values"]["theme"], "dark");
    assert_eq!(doc["values"]["notify"], false);
}

#[test]
fn persist_defaults_on_first_save_writes_unchanged_defaults() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("app").join("settings.json");
    let opts = StoreOptions::new(schema())
        .defaults_provider(|| json!({"theme": "dark", "font_px": 18, "notify": false}))
        .persist_defaults_on_first_save(true);
    let s = Store::<Prefs>::open_path(&p, opts).unwrap();
    assert!(!p.exists(), "open must still write nothing");
    // The first save changes nothing, but the file is written with the defaults.
    s.update(|v| {
        v.font_px = 18;
        Ok(())
    })
    .unwrap();
    let doc = read_json(&p);
    assert_eq!(doc["values"]["theme"], "dark");
    assert_eq!(doc["values"]["font_px"], 18);
    assert_eq!(doc["values"]["notify"], false);
    // Later no-change saves do not rewrite: the revision stays put.
    let rev = s.revision();
    s.update(|_| Ok(())).unwrap();
    assert_eq!(s.revision(), rev);
}

#[test]
fn read_only_open_creates_nothing_and_refuses_writes() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("never").join("settings.json");
    let s = Store::<Prefs>::open_read_only(&missing, StoreOptions::new(schema())).unwrap();
    assert_eq!(s.get(), Prefs::default());
    assert!(!d.path().join("never").exists());
    assert!(s
        .update(|v| {
            v.notify = false;
            Ok(())
        })
        .is_err());
    assert!(s.patch(&json!({"notify": false}), None).is_err());

    // Existing file: open, get and revision leave the directory exactly as it was.
    let live = d.path().join("live").join("settings.json");
    {
        let writer = Store::<Prefs>::open_path(&live, StoreOptions::new(schema())).unwrap();
        writer
            .update(|v| {
                v.font_px = 21;
                Ok(())
            })
            .unwrap();
    }
    let before = listing(live.parent().unwrap());
    let r = Store::<Prefs>::open_read_only(&live, StoreOptions::new(schema())).unwrap();
    assert_eq!(r.get().font_px, 21);
    assert_eq!(r.revision(), 2);
    assert_eq!(listing(live.parent().unwrap()), before);

    // A second process holding the writer lock does not block a read-only open.
    let held = File::create(format!("{}.lock", live.display())).unwrap();
    held.lock().unwrap();
    let started = Instant::now();
    let r2 = Store::<Prefs>::open_read_only(&live, StoreOptions::new(schema())).unwrap();
    assert_eq!(r2.get().font_px, 21);
    assert!(started.elapsed() < Duration::from_secs(5));
    held.unlock().unwrap();
}

#[test]
fn flat_round_trip_keeps_unknown_fields_key_order_and_formatting() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    let body = "{\n    \"theme\": \"dark\",\n    \"font_px\": 18,\n    \"x-extra\": {\"nested\":  [1,2,  3]},\n    \"notify\": true\n}\n";
    fs::write(&p, body).unwrap();
    let opts = StoreOptions::new(schema()).format(FileFormat::Flat);

    let s = Store::<Prefs>::open_path(&p, opts.clone()).unwrap();
    assert_eq!(s.get().font_px, 18);
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        body,
        "opening a clean flat file must not rewrite it"
    );

    s.update(|v| {
        v.font_px = 20;
        Ok(())
    })
    .unwrap();
    // Only the changed value is rewritten; the unknown field and the 4-space style are kept.
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        body.replace("\"font_px\": 18", "\"font_px\": 20")
    );
    let doc = read_json(&p);
    assert!(
        doc.get("schemaVersion").is_none(),
        "still flat, no envelope"
    );
    assert_eq!(doc["x-extra"], json!({"nested": [1, 2, 3]}));
    drop(s);

    let again = Store::<Prefs>::open_path(&p, opts).unwrap();
    assert_eq!(again.get().font_px, 20);
}

#[test]
fn flat_refuses_envelope_files_and_envelope_adoption_is_one_way() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    fs::write(&p, "{\"colorTheme\": \"dark\", \"font_px\": 14}").unwrap();

    // Flat mode runs no migrations: the old key is an unknown field and stays put.
    {
        let flat =
            Store::<Prefs>::open_path(&p, StoreOptions::new(schema()).format(FileFormat::Flat))
                .unwrap();
        assert_eq!(flat.get().theme, "auto");
        assert_eq!(flat.get().font_px, 14);
    }
    assert!(read_json(&p).get("colorTheme").is_some());

    // Opting in to the envelope adopts the flat object as version 0 and runs the migrations.
    let env = Store::<Prefs>::open_path(&p, StoreOptions::new(schema())).unwrap();
    assert_eq!(env.get().theme, "dark");
    let doc = read_json(&p);
    assert_eq!(doc["schemaVersion"], 2);
    assert_eq!(doc["values"]["theme"], "dark");

    // Flat readers now refuse the envelope instead of misreading it.
    let refused =
        Store::<Prefs>::open_path(&p, StoreOptions::new(schema()).format(FileFormat::Flat));
    assert!(matches!(refused, Err(Error::Invalid(_))));
}

#[test]
fn old_envelope_files_load_without_rewriting() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("demo").join("settings.json");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let body = "{\n  \"namespace\": \"demo\",\n  \"revision\": 7,\n  \"schemaVersion\": 2,\n  \"values\": {\"font_px\": 18, \"notify\": true, \"theme\": \"dark\"}\n}";
    fs::write(&p, body).unwrap();

    let s = Store::<Prefs>::open(d.path(), schema()).unwrap();
    assert_eq!(
        s.get(),
        Prefs {
            theme: "dark".into(),
            font_px: 18,
            notify: true
        }
    );
    assert_eq!(s.revision(), 7);
    assert_eq!(fs::read_to_string(&p).unwrap(), body);

    s.update(|v| {
        v.notify = false;
        Ok(())
    })
    .unwrap();
    assert_eq!(s.revision(), 8);
}

#[test]
fn open_keys_patch_persists_retains_and_dry_run_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("mail.json");
    let opts = StoreOptions::new(schema())
        .allow_unknown_keys(|k| k.strip_prefix("signature:").is_some_and(|a| !a.is_empty()));
    let s = Store::<Prefs>::open_path(&p, opts).unwrap();

    s.patch(&json!({"signature:acct-1": "Thanks,\nA"}), None)
        .unwrap();
    // A typed update must not drop the dynamic key.
    s.update(|v| {
        v.font_px = 20;
        Ok(())
    })
    .unwrap();
    assert_eq!(read_json(&p)["values"]["signature:acct-1"], "Thanks,\nA");

    // Keys the validator refuses, and open keys under an allow-list, are rejected.
    assert!(s.patch(&json!({"other:acct-1": "x"}), None).is_err());
    assert!(s
        .patch(&json!({"signature:acct-1": "x"}), Some(&["theme"]))
        .is_err());

    // Dry run reports the change and writes nothing.
    let before = fs::read(&p).unwrap();
    let preview = s
        .patch_dry_run(&json!({"signature:acct-2": "B", "theme": "dark"}), None)
        .unwrap();
    assert_eq!(
        preview.change.keys,
        vec!["signature:acct-2".to_string(), "theme".to_string()]
    );
    assert_eq!(preview.value.theme, "dark");
    assert_eq!(fs::read(&p).unwrap(), before);

    // Null removes an open key.
    s.patch(&json!({"signature:acct-1": null}), None).unwrap();
    assert!(read_json(&p)["values"].get("signature:acct-1").is_none());
}

/// An in-memory backend, standing in for an app's encrypted store. Counts loads so tests can
/// tell a cache hit from a re-read.
#[derive(Clone, Default)]
struct MemoryBackend {
    doc: Arc<Mutex<Option<Stored>>>,
    loads: Arc<AtomicUsize>,
}

impl MemoryBackend {
    fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }
}

impl SettingsBackend for MemoryBackend {
    fn load(&self) -> Result<Option<Stored>, Error> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Ok(self.doc.lock().unwrap().clone())
    }

    fn stamp(&self) -> Result<Option<String>, Error> {
        Ok(self
            .doc
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.revision.to_string()))
    }

    fn patch(
        &self,
        edit: &mut dyn FnMut(Option<&Stored>) -> Result<Edit, Error>,
    ) -> Result<Option<u64>, Error> {
        let mut slot = self.doc.lock().unwrap();
        match edit(Option::as_ref(&*slot))? {
            Edit::Keep => Ok(None),
            Edit::Commit(mut next) => {
                let revision = Option::as_ref(&*slot).map_or(0, |s| s.revision) + 1;
                next.revision = revision;
                *slot = Some(next);
                Ok(Some(revision))
            }
        }
    }
}

#[test]
fn custom_backend_plugs_in_through_the_trait() {
    let shared = MemoryBackend::default();
    let a = Store::<Prefs>::with_backend(shared.clone(), StoreOptions::new(schema())).unwrap();
    let b = Store::<Prefs>::with_backend(shared.clone(), StoreOptions::new(schema())).unwrap();
    a.update(|v| {
        v.font_px = 24;
        Ok(())
    })
    .unwrap();
    assert_eq!(b.get().font_px, 24);
    assert_eq!(b.revision(), 2);
    let doc = shared.load().unwrap().unwrap();
    assert_eq!(doc.version, Some(2));
    assert_eq!(doc.namespace.as_deref(), Some("demo"));

    // The stock file backend also goes through with_backend.
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("via-backend.json");
    let file = FileBackend::new(&p, FileFormat::Flat);
    let f =
        Store::<Prefs>::with_backend(file, StoreOptions::new(schema()).format(FileFormat::Flat))
            .unwrap();
    f.update(|v| {
        v.theme = "dark".into();
        Ok(())
    })
    .unwrap();
    assert_eq!(read_json(&p)["theme"], "dark");
}

#[test]
fn get_serves_the_cache_until_the_stamp_changes() {
    let shared = MemoryBackend::default();
    let a = Store::<Prefs>::with_backend(shared.clone(), StoreOptions::new(schema())).unwrap();
    a.update(|v| {
        v.font_px = 24;
        Ok(())
    })
    .unwrap();
    let b = Store::<Prefs>::with_backend(shared.clone(), StoreOptions::new(schema())).unwrap();
    assert_eq!(b.get().font_px, 24); // first read populates the cache
    let before = shared.loads();
    for _ in 0..5 {
        assert_eq!(b.get().font_px, 24);
    }
    assert_eq!(shared.loads(), before, "unchanged stamp must not re-read");

    // Another store commits: the stamp moves and the next get sees the new value.
    a.update(|v| {
        v.font_px = 25;
        Ok(())
    })
    .unwrap();
    assert_eq!(b.get().font_px, 25);
    assert!(shared.loads() > before);
}

#[cfg(unix)]
#[test]
fn unreadable_file_is_an_error_and_never_overwritten_with_defaults() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    {
        let s = Store::<Prefs>::open_path(&p, StoreOptions::new(schema())).unwrap();
        s.update(|v| {
            v.font_px = 30;
            Ok(())
        })
        .unwrap();
    }
    let bytes = fs::read(&p).unwrap();
    let before = listing(d.path());
    fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&p).is_ok() {
        // Running as root: permissions do not restrict reads, so there is nothing to test.
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        return;
    }
    let opened = Store::<Prefs>::open_path(&p, StoreOptions::new(schema()));
    assert!(
        matches!(opened, Err(Error::Io(_))),
        "open must fail, not default"
    );
    let opened_ro = Store::<Prefs>::open_read_only(&p, StoreOptions::new(schema()));
    assert!(matches!(opened_ro, Err(Error::Io(_))));
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        fs::read(&p).unwrap(),
        bytes,
        "the unreadable file must survive untouched"
    );
    assert_eq!(
        listing(d.path()),
        before,
        "no backup, quarantine, or defaults were written"
    );
    let s = Store::<Prefs>::open_path(&p, StoreOptions::new(schema())).unwrap();
    assert_eq!(s.get().font_px, 30);
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Nullable {
    note: Option<String>,
}
impl Default for Nullable {
    fn default() -> Self {
        Self {
            note: Some("hello".into()),
        }
    }
}

#[test]
fn null_means_reset_or_set_null() {
    let d = tempfile::tempdir().unwrap();
    let reset = Store::<Nullable>::open_path(
        d.path().join("reset.json"),
        StoreOptions::new(Schema::new("n", 1)),
    )
    .unwrap();
    reset.patch(&json!({"note": null}), None).unwrap();
    assert_eq!(reset.get().note.as_deref(), Some("hello"));

    let set = Store::<Nullable>::open_path(
        d.path().join("set.json"),
        StoreOptions::new(Schema::new("n", 1)).null_means(NullMeans::SetNull),
    )
    .unwrap();
    set.patch(&json!({"note": null}), None).unwrap();
    assert_eq!(set.get().note, None);
    assert_eq!(
        read_json(&d.path().join("set.json"))["values"]["note"],
        Value::Null
    );

    // SetNull on a type that rejects null fails, and nothing is written.
    let strict = Store::<Prefs>::open_path(
        d.path().join("strict.json"),
        StoreOptions::new(schema()).null_means(NullMeans::SetNull),
    )
    .unwrap();
    let before = fs::read(d.path().join("strict.json")).unwrap();
    assert!(strict.patch(&json!({"theme": null}), None).is_err());
    assert_eq!(fs::read(d.path().join("strict.json")).unwrap(), before);
}

#[test]
fn validate_values_rejects_edits_beyond_type_checks() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("v.json");
    let opts = StoreOptions::new(schema()).validate_values(|values| {
        let px = values.get("font_px").and_then(Value::as_u64).unwrap_or(0);
        if (8..=72).contains(&px) {
            Ok(())
        } else {
            Err(format!("font_px {px} outside 8..=72"))
        }
    });
    let s = Store::<Prefs>::open_path(&p, opts).unwrap();
    s.patch(&json!({"font_px": 20}), None).unwrap();
    let before = fs::read(&p).unwrap();
    let err = s.patch(&json!({"font_px": 200}), None).unwrap_err();
    assert!(
        matches!(err, Error::Invalid(ref m) if m.contains("outside")),
        "{err}"
    );
    assert!(s
        .update(|v| {
            v.font_px = 1;
            Ok(())
        })
        .is_err());
    assert!(s.patch_dry_run(&json!({"font_px": 500}), None).is_err());
    assert_eq!(fs::read(&p).unwrap(), before);
    assert_eq!(s.get().font_px, 20);
}

#[test]
fn failing_migration_aborts_open_without_writing() {
    fn boom(_: &mut serde_json::Map<String, Value>) -> Result<(), Error> {
        Err(Error::Invalid("cannot migrate".into()))
    }
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("demo").join("settings.json");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let body = "{\"colorTheme\": \"dark\"}";
    fs::write(&p, body).unwrap();
    let bad = Schema::new("demo", 1).migration(1, boom);
    let err = Store::<Prefs>::open(d.path(), bad).err().unwrap();
    assert!(matches!(err, Error::Invalid(ref m) if m == "cannot migrate"));
    assert_eq!(fs::read_to_string(&p).unwrap(), body);
}
