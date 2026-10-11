use rightkit_settings::{atomic_write_with, Error, Schema, Store};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, Mutex};

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

#[test]
fn legacy_flat_file_migrates_repairs_and_persists_once() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("demo");
    std::fs::create_dir_all(&p).unwrap();
    // v0 flat file: old key name, plus one wrong-typed field.
    std::fs::write(
        p.join("settings.json"),
        r#"{"colorTheme":"legacy","font_px":"huge","notify":false}"#,
    )
    .unwrap();
    let s = Store::<Prefs>::open(d.path(), schema()).unwrap();
    assert_eq!(
        s.get(),
        Prefs {
            theme: "auto".into(),
            font_px: 16,
            notify: false
        }
    );
    drop(s);
    let doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(p.join("settings.json")).unwrap()).unwrap();
    assert_eq!(doc["schemaVersion"], 2);
    assert_eq!(doc["values"]["theme"], "auto");
}

#[test]
fn corrupt_file_is_quarantined_and_backup_restored() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Store::<Prefs>::open(d.path(), schema()).unwrap();
        s.update(|v| {
            v.font_px = 20;
            Ok(())
        })
        .unwrap();
        s.update(|v| {
            v.font_px = 22;
            Ok(())
        })
        .unwrap(); // .bak now holds 20
    }
    let main = d.path().join("demo/settings.json");
    std::fs::write(&main, b"{ not json").unwrap();
    let s = Store::<Prefs>::open(d.path(), schema()).unwrap();
    assert_eq!(s.get().font_px, 20);
    let quarantined = std::fs::read_dir(d.path().join("demo"))
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains(".corrupt-"));
    assert!(quarantined);
}

#[test]
fn newer_schema_is_refused_without_overwrite() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("demo");
    std::fs::create_dir_all(&p).unwrap();
    let body = r#"{"schemaVersion":9,"namespace":"demo","values":{}}"#;
    std::fs::write(p.join("settings.json"), body).unwrap();
    assert!(matches!(
        Store::<Prefs>::open(d.path(), schema()),
        Err(Error::NewerSchema { found: 9, .. })
    ));
    assert_eq!(
        std::fs::read_to_string(p.join("settings.json")).unwrap(),
        body
    );
}

#[test]
fn patch_validates_notifies_and_rejects_atomically() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::<Prefs>::open(d.path(), schema()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let sink = seen.clone();
    let _sub = s.subscribe(move |c| sink.lock().unwrap().push(c.keys.clone()));

    s.patch(&json!({"theme":"dark","notify":false}), None)
        .unwrap();
    assert_eq!(s.get().theme, "dark");
    assert!(s.patch(&json!({"font_px":"x"}), None).is_err());
    assert!(s.patch(&json!({"bogus":1}), None).is_err());
    assert!(s.patch(&json!({"theme":"x"}), Some(&["font_px"])).is_err());
    s.patch(&json!({"theme":null}), None).unwrap(); // reset to default
    assert_eq!(s.get().theme, "auto");
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            vec!["notify".to_string(), "theme".to_string()],
            vec!["theme".to_string()]
        ]
    );
}

#[test]
fn failed_replace_keeps_old_file() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("a.json");
    std::fs::write(&f, b"old").unwrap();
    let r = atomic_write_with(&f, b"new", |_, _| Err(std::io::Error::other("boom")));
    assert!(r.is_err());
    assert_eq!(std::fs::read(&f).unwrap(), b"old");
    let leftovers = std::fs::read_dir(d.path()).unwrap().count();
    assert_eq!(leftovers, 1);
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Counters {
    counts: std::collections::BTreeMap<String, u32>,
}

/// Child half of the cross-process test: its own `Store` on the shared file.
#[test]
fn child_worker() {
    let (Ok(root), Ok(name)) = (
        std::env::var("RK_SETTINGS_ROOT"),
        std::env::var("RK_SETTINGS_NAME"),
    ) else {
        return;
    };
    let s = Store::<Counters>::open(std::path::Path::new(&root), Schema::new("shared", 1)).unwrap();
    for _ in 0..40 {
        s.update(|c| {
            *c.counts.entry(name.clone()).or_default() += 1;
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn concurrent_processes_and_stores_never_lose_each_others_keys() {
    let d = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut kids: Vec<_> = (0..3)
        .map(|i| {
            std::process::Command::new(&exe)
                .args(["--exact", "child_worker", "--nocapture"])
                .env("RK_SETTINGS_ROOT", d.path())
                .env("RK_SETTINGS_NAME", format!("proc{i}"))
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    // Two more writers in this process, each with its own cached store.
    let threads: Vec<_> = (0..2)
        .map(|i| {
            let root = d.path().to_path_buf();
            std::thread::spawn(move || {
                let s = Store::<Counters>::open(&root, Schema::new("shared", 1)).unwrap();
                for _ in 0..40 {
                    s.update(|c| {
                        *c.counts.entry(format!("thread{i}")).or_default() += 1;
                        Ok(())
                    })
                    .unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    for k in &mut kids {
        assert!(k.wait().unwrap().success());
    }
    let s = Store::<Counters>::open(d.path(), Schema::new("shared", 1)).unwrap();
    let counts = s.get().counts;
    assert_eq!(counts.len(), 5, "{counts:?}");
    assert!(counts.values().all(|n| *n == 40), "lost update: {counts:?}");
}

#[test]
fn stale_reader_sees_other_writers_and_cas_rejects_stale_revision() {
    let d = tempfile::tempdir().unwrap();
    let a = Store::<Prefs>::open(d.path(), schema()).unwrap();
    let b = Store::<Prefs>::open(d.path(), schema()).unwrap();
    let seen = a.revision();
    b.update(|v| {
        v.font_px = 30;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        a.get().font_px,
        30,
        "get() reflects the other store's commit"
    );

    // `a` still holds the old revision: its compare-and-swap must fail, untouched.
    let err = a
        .update_at(seen, |v| {
            v.theme = "dark".into();
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(err, Error::Conflict { .. }), "{err}");
    assert_eq!(b.get().theme, "auto");
    // Plain update merges onto the fresh value instead of clobbering font_px.
    a.update(|v| {
        v.theme = "dark".into();
        Ok(())
    })
    .unwrap();
    assert_eq!(
        b.get(),
        Prefs {
            theme: "dark".into(),
            font_px: 30,
            notify: true
        }
    );
    let now = a.revision();
    a.update_at(now, |v| {
        v.notify = false;
        Ok(())
    })
    .unwrap();
    assert!(!b.get().notify);
}
