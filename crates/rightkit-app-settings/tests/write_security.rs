//! `StoreOptions::write_security`: the owner-private profile reaches the file the store writes, and
//! the default profile is unchanged. The ACL and mode work is rightkit-fs's; these tests read the
//! result back through its checker, so they hold on Windows, macOS and Linux alike.

use rightkit_app_settings::{
    Document, Error, FileBackend, FileFormat, Schema, Store, StoreOptions, WriteSecurity,
};
use rightkit_fs::private::is_owner_only_strict;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    theme: String,
    n: u32,
}

fn options(profile: WriteSecurity) -> StoreOptions {
    StoreOptions::new(Schema::new("secure", 1)).write_security(profile)
}

#[test]
fn the_default_profile_is_the_default_option() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let s = Store::<Prefs>::open_path(&p, StoreOptions::new(Schema::new("secure", 1))).unwrap();
    s.update(|v| {
        v.n = 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(s.get().n, 1);
    assert_eq!(WriteSecurity::default(), WriteSecurity::Default);
}

#[test]
fn an_owner_private_store_writes_an_owner_only_file() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("nested").join("s.json");
    let s = Store::<Prefs>::open_path(&p, options(WriteSecurity::OwnerPrivate)).unwrap();
    s.update(|v| {
        v.theme = "dark".into();
        Ok(())
    })
    .unwrap();
    assert!(is_owner_only_strict(&p).unwrap(), "settings file");
    // A second write goes over an existing file and keeps it owner-only.
    s.update(|v| {
        v.n = 2;
        Ok(())
    })
    .unwrap();
    assert!(is_owner_only_strict(&p).unwrap(), "after a replace");
}

#[test]
fn the_backup_of_an_owner_private_store_is_owner_only() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let s = Store::<Prefs>::open_path(&p, options(WriteSecurity::OwnerPrivate)).unwrap();
    for n in 1..=2 {
        s.update(|v| {
            v.n = n;
            Ok(())
        })
        .unwrap();
    }
    let bak = d.path().join("s.json.bak");
    assert!(bak.exists());
    assert!(is_owner_only_strict(&bak).unwrap(), "backup");
}

#[test]
fn switching_a_default_file_to_owner_private_tightens_it_on_the_next_write() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let plain = Store::<Prefs>::open_path(&p, options(WriteSecurity::Default)).unwrap();
    plain
        .update(|v| {
            v.n = 1;
            Ok(())
        })
        .unwrap();
    drop(plain);
    let private = Store::<Prefs>::open_path(&p, options(WriteSecurity::OwnerPrivate)).unwrap();
    private
        .update(|v| {
            v.n = 2;
            Ok(())
        })
        .unwrap();
    assert!(is_owner_only_strict(&p).unwrap());
    assert_eq!(private.get().n, 2);
}

#[test]
fn a_document_and_a_file_backend_take_the_profile_too() {
    let d = tempfile::tempdir().unwrap();
    let via_options = d.path().join("a.json");
    let private = options(WriteSecurity::OwnerPrivate);
    let doc = Document::open_path_with(&via_options, &private).unwrap();
    doc.put(1, json!({"k": 1}), None).unwrap();
    assert!(is_owner_only_strict(&via_options).unwrap(), "document");

    let via_backend = d.path().join("b.json");
    let backend = FileBackend::new(&via_backend, FileFormat::Envelope)
        .write_security(WriteSecurity::OwnerPrivate);
    let doc = Document::with_backend(backend, "secure").unwrap();
    doc.put(1, json!({"k": 1}), None).unwrap();
    assert!(is_owner_only_strict(&via_backend).unwrap(), "backend");
}

#[test]
fn the_protection_error_names_the_file_and_the_reason() {
    let e = Error::Protection {
        path: "C:/x/settings.json".into(),
        reason: "cannot be made owner-only".into(),
    };
    let text = e.to_string();
    assert!(text.contains("settings.json"), "{text}");
    assert!(text.contains("cannot be made owner-only"), "{text}");
}
