//! M10-O3: cross-instance change notification (`Store::poll_external`).

use rightkit_app_settings::{Schema, Store, StoreOptions};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    theme: String,
    font_px: u32,
}
impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: "auto".into(),
            font_px: 16,
        }
    }
}

fn opts() -> StoreOptions {
    StoreOptions::new(Schema::new("demo", 1))
}

#[test]
fn poll_external_reports_a_change_from_another_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let a = Store::<Prefs>::open_path(&path, opts()).unwrap();
    let b = Store::<Prefs>::open_path(&path, opts()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let sink = seen.clone();
    let _sub = a.subscribe(move |c| sink.lock().unwrap().push(c.keys.clone()));
    // Settle the cache, then change the file through the other store.
    assert!(a.poll_external().unwrap().keys.is_empty());
    b.update(|p| {
        p.font_px = 20;
        Ok(())
    })
    .unwrap();
    let change = a.poll_external().unwrap();
    assert_eq!(change.keys, vec!["font_px".to_string()]);
    assert_eq!(a.get().font_px, 20);
    assert_eq!(*seen.lock().unwrap(), vec![vec!["font_px".to_string()]]);
    // Nothing new: no second event.
    assert!(a.poll_external().unwrap().keys.is_empty());
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn poll_external_does_not_report_own_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let a = Store::<Prefs>::open_path(&path, opts()).unwrap();
    let count = Arc::new(Mutex::new(0u32));
    let c = count.clone();
    let _sub = a.subscribe(move |_| *c.lock().unwrap() += 1);
    a.update(|p| {
        p.theme = "dark".into();
        Ok(())
    })
    .unwrap();
    assert_eq!(*count.lock().unwrap(), 1);
    assert!(a.poll_external().unwrap().keys.is_empty());
    assert_eq!(*count.lock().unwrap(), 1);
}
