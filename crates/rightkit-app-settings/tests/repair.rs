//! Table of repair / `read_with_defaults` cases. Every case runs through the three entry points
//! that must agree: `read_with_options`, `Store::open_read_only`, and `Store::open_path` (plus a
//! reopen of what `open_path` left on disk).
use rightkit_app_settings::{
    read_with_defaults, read_with_options, Error, FileFormat, NullMeans, OnCorrupt, ReadOptions,
    Schema, Store, StoreOptions,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt::Debug;
use std::fs;
use std::path::Path;

/// Two required fields, no serde defaults. `Default` is all-zero so a leak of `T::default()` shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct Req {
    output_dir: String,
    keep_media: bool,
}

/// A legacy alias on a required field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct Aliased {
    #[serde(alias = "tdt_model_dir")]
    model_dir: String,
    keep: bool,
}

/// One required field, one serde-default field, one Option.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct Mixed {
    name: String,
    #[serde(default)]
    size: u32,
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Strict {
    a: String,
    b: bool,
}

/// An alias on an Option field, so a null legacy alias is a valid value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct AliasedOpt {
    #[serde(alias = "tdt_model_dir")]
    model_dir: Option<String>,
    keep: bool,
}

enum File {
    Missing,
    Raw(&'static str),
    Values(Value),
}

fn schema() -> Schema {
    Schema::new("t", 1)
}

fn write(dir: &Path, file: &File) -> std::path::PathBuf {
    let p = dir.join("settings.json");
    match file {
        File::Missing => {}
        File::Raw(raw) => fs::write(&p, raw).unwrap(),
        File::Values(v) => fs::write(
            &p,
            serde_json::to_vec(&json!({
                "schemaVersion": 1, "namespace": "t", "values": v, "revision": 1
            }))
            .unwrap(),
        )
        .unwrap(),
    }
    p
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn check<T>(name: &str, file: File, defaults: Value, fill: bool, expect: T)
where
    T: Serialize + DeserializeOwned + Default + Clone + PartialEq + Debug,
{
    // Read-only entry points: result right, nothing touched.
    let d = tempfile::tempdir().unwrap();
    let p = write(d.path(), &file);
    let before = (fs::read(&p).ok(), listing(d.path()));
    let got: T = read_with_options(
        &p,
        &defaults,
        ReadOptions::new(FileFormat::Envelope).fill_optional(fill),
    )
    .unwrap_or_else(|e| panic!("{name}: read_with_options: {e}"));
    assert_eq!(got, expect, "{name}: read_with_options");
    let dc = defaults.clone();
    let opts = StoreOptions::new(schema())
        .defaults_provider(move || dc.clone())
        .fill_optional(fill);
    let ro = Store::<T>::open_read_only(&p, opts)
        .unwrap_or_else(|e| panic!("{name}: open_read_only: {e}"));
    assert_eq!(ro.get(), expect, "{name}: open_read_only");
    assert_eq!(
        (fs::read(&p).ok(), listing(d.path())),
        before,
        "{name}: read-only paths wrote something"
    );

    // Writing open: same value; and what it leaves behind reads back the same.
    let d = tempfile::tempdir().unwrap();
    let p = write(d.path(), &file);
    let dc = defaults.clone();
    let opts = move || {
        StoreOptions::new(schema())
            .defaults_provider({
                let dc = dc.clone();
                move || dc.clone()
            })
            .fill_optional(fill)
    };
    let s = Store::<T>::open_path(&p, opts()).unwrap_or_else(|e| panic!("{name}: open_path: {e}"));
    assert_eq!(s.get(), expect, "{name}: open_path");
    drop(s);
    let again = Store::<T>::open_path(&p, opts()).unwrap();
    assert_eq!(again.get(), expect, "{name}: reopen after open_path");
    println!("ok  {name}");
}

#[test]
fn case_table() {
    let req_d = json!({"output_dir": "/default", "keep_media": true});
    let req = |o: &str, k: bool| Req {
        output_dir: o.into(),
        keep_media: k,
    };
    let ali_d = json!({"model_dir": "/new", "keep": true});
    let ali = |m: &str, k: bool| Aliased {
        model_dir: m.into(),
        keep: k,
    };
    let mix_d = json!({"name": "dflt", "size": 9, "note": "dn"});
    let mix = |n: &str, s: u32, note: Option<&str>| Mixed {
        name: n.into(),
        size: s,
        note: note.map(Into::into),
    };

    // 1. legacy alias
    check(
        "1a alias-only file keeps the legacy value",
        File::Values(json!({"tdt_model_dir": "/old", "keep": false})),
        ali_d.clone(),
        false,
        ali("/old", false),
    );
    check(
        "1b alias, required sibling omitted -> default",
        File::Values(json!({"tdt_model_dir": "/old"})),
        ali_d.clone(),
        false,
        ali("/old", true),
    );
    check(
        "1c alias and canonical both present -> alphabetically first",
        File::Values(json!({"tdt_model_dir": "/old", "model_dir": "/canon"})),
        ali_d.clone(),
        false,
        ali("/canon", true),
    );
    check(
        "1d invalid alias value -> default",
        File::Values(json!({"tdt_model_dir": 5, "keep": false})),
        ali_d.clone(),
        false,
        ali("/new", false),
    );
    check(
        "1e canonical-only file",
        File::Values(json!({"model_dir": "/c", "keep": false})),
        ali_d.clone(),
        false,
        ali("/c", false),
    );

    // 10. null legacy alias beside its canonical key: the null one is dropped
    let alo = |m: Option<&str>, k: bool| AliasedOpt {
        model_dir: m.map(Into::into),
        keep: k,
    };
    let alo_d = json!({"model_dir": "/new", "keep": true});
    check(
        "10a String: null alias beside valued canonical",
        File::Values(json!({"tdt_model_dir": null, "model_dir": "/x", "keep": false})),
        ali_d.clone(),
        false,
        ali("/x", false),
    );
    check(
        "10b String: null canonical beside valued alias",
        File::Values(json!({"model_dir": null, "tdt_model_dir": "/old", "keep": false})),
        ali_d.clone(),
        false,
        ali("/old", false),
    );
    check(
        "10c String: both null -> default",
        File::Values(json!({"model_dir": null, "tdt_model_dir": null, "keep": false})),
        ali_d.clone(),
        false,
        ali("/new", false),
    );
    check(
        "10d Option: null alias beside valued canonical",
        File::Values(json!({"tdt_model_dir": null, "model_dir": "/x", "keep": false})),
        alo_d.clone(),
        false,
        alo(Some("/x"), false),
    );
    check(
        "10e Option: null canonical beside valued alias",
        File::Values(json!({"model_dir": null, "tdt_model_dir": "/old", "keep": false})),
        alo_d.clone(),
        false,
        alo(Some("/old"), false),
    );
    check(
        "10f Option: both null -> unset",
        File::Values(json!({"model_dir": null, "tdt_model_dir": null, "keep": false})),
        alo_d.clone(),
        false,
        alo(None, false),
    );
    check(
        "10g Option: valued spellings still keep the alphabetically first",
        File::Values(json!({"tdt_model_dir": "/old", "model_dir": "/canon", "keep": false})),
        alo_d.clone(),
        false,
        alo(Some("/canon"), false),
    );

    // 2. required fields, no usable file: the caller's defaults, never T::default()
    check(
        "2a missing file",
        File::Missing,
        req_d.clone(),
        false,
        req("/default", true),
    );
    check(
        "2b unparseable json, no bak",
        File::Raw("{ broken"),
        req_d.clone(),
        false,
        req("/default", true),
    );
    check(
        "2c empty values object",
        File::Values(json!({})),
        req_d.clone(),
        false,
        req("/default", true),
    );
    check(
        "2d non-object json",
        File::Raw("[1,2]"),
        req_d.clone(),
        false,
        req("/default", true),
    );

    // 3. partial validity
    check(
        "3a valid output_dir, invalid keep_media",
        File::Values(json!({"output_dir": "/custom", "keep_media": "yes"})),
        req_d.clone(),
        false,
        req("/custom", true),
    );
    check(
        "3b invalid output_dir, valid keep_media",
        File::Values(json!({"output_dir": 7, "keep_media": false})),
        req_d.clone(),
        false,
        req("/default", false),
    );
    check(
        "3c both invalid",
        File::Values(json!({"output_dir": 7, "keep_media": "x"})),
        req_d.clone(),
        false,
        req("/default", true),
    );
    check(
        "3d one valid key, other omitted",
        File::Values(json!({"keep_media": false})),
        req_d.clone(),
        false,
        req("/default", false),
    );
    check(
        "3e fill_optional does not change required handling",
        File::Values(json!({"output_dir": "/custom", "keep_media": "yes"})),
        req_d.clone(),
        true,
        req("/custom", true),
    );

    // 4. unknown fields never block a read
    check(
        "4a unknown fields",
        File::Values(json!({"output_dir": "/c", "keep_media": false, "zzz": 1, "aaa": {"x": [1]}})),
        req_d.clone(),
        false,
        req("/c", false),
    );
    check(
        "4b unknown fields under deny_unknown_fields",
        File::Values(json!({"a": "x", "b": true, "extra": 1})),
        json!({"a": "d", "b": false}),
        false,
        Strict {
            a: "x".into(),
            b: true,
        },
    );
    check(
        "4c deny_unknown_fields and an invalid field",
        File::Values(json!({"a": 1, "b": true, "extra": 1})),
        json!({"a": "d", "b": false}),
        false,
        Strict {
            a: "d".into(),
            b: true,
        },
    );

    // 5. optional fields: not filled by default, filled with fill_optional
    check(
        "5a omitted optionals stay unset",
        File::Values(json!({"name": "f"})),
        mix_d.clone(),
        false,
        mix("f", 0, None),
    );
    check(
        "5b fill_optional fills them",
        File::Values(json!({"name": "f"})),
        mix_d.clone(),
        true,
        mix("f", 9, Some("dn")),
    );
    check(
        "5c omitted required still defaulted",
        File::Values(json!({})),
        mix_d.clone(),
        false,
        mix("dflt", 0, None),
    );
    check(
        "5d invalid optional takes the default",
        File::Values(json!({"name": "f", "size": "x"})),
        mix_d.clone(),
        false,
        mix("f", 9, None),
    );
    check(
        "5e missing file uses defaults whole",
        File::Missing,
        mix_d.clone(),
        false,
        mix("dflt", 9, Some("dn")),
    );
    check(
        "5f present optionals are kept",
        File::Values(json!({"name": "f", "size": 3, "note": "n"})),
        mix_d.clone(),
        true,
        mix("f", 3, Some("n")),
    );
    check(
        "5g alias plus fill_optional",
        File::Values(json!({"tdt_model_dir": "/old"})),
        ali_d.clone(),
        true,
        ali("/old", true),
    );
}

#[test]
fn resolution_does_not_depend_on_key_order() {
    let d = tempfile::tempdir().unwrap();
    let defaults = json!({"model_dir": "/new", "keep": true});
    let a = d.path().join("a.json");
    let b = d.path().join("b.json");
    fs::write(
        &a,
        r#"{"tdt_model_dir":"/old","model_dir":"/canon","keep":"x"}"#,
    )
    .unwrap();
    fs::write(
        &b,
        r#"{"keep":"x","model_dir":"/canon","tdt_model_dir":"/old"}"#,
    )
    .unwrap();
    let va: Aliased = read_with_defaults(&a, FileFormat::Flat, &defaults).unwrap();
    let vb: Aliased = read_with_defaults(&b, FileFormat::Flat, &defaults).unwrap();
    assert_eq!(va, vb);
    assert_eq!(
        va,
        Aliased {
            model_dir: "/canon".into(),
            keep: true
        }
    );
}

#[test]
fn unknown_fields_are_retained_on_write_when_retaining() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.json");
    fs::write(
        &p,
        r#"{"output_dir":"/c","keep_media":"bad","zzz":{"k":1}}"#,
    )
    .unwrap();
    let opts = StoreOptions::new(schema())
        .format(FileFormat::Flat)
        .defaults_provider(|| json!({"output_dir": "/d", "keep_media": true}));
    let s = Store::<Req>::open_path(&p, opts).unwrap();
    s.update(|v| {
        v.keep_media = false;
        Ok(())
    })
    .unwrap();
    let on_disk: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    assert_eq!(on_disk["zzz"], json!({"k": 1}));
    assert_eq!(on_disk["output_dir"], "/c");
    assert_eq!(on_disk["keep_media"], false);
}

#[test]
fn an_incomplete_defaults_object_is_an_error_not_a_silent_reset() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let r: Result<Req, Error> =
        read_with_defaults(&p, FileFormat::Envelope, &json!({"output_dir": "/d"}));
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    // A complete file still reads when the defaults are incomplete.
    fs::write(&p, r#"{"output_dir":"/f","keep_media":true}"#).unwrap();
    let ok: Req = read_with_defaults(&p, FileFormat::Flat, &json!({"output_dir": "/d"})).unwrap();
    assert_eq!(
        ok,
        Req {
            output_dir: "/f".into(),
            keep_media: true
        }
    );
}

#[test]
fn io_errors_other_than_not_found_are_errors_and_nothing_is_written() {
    // The settings path is a directory: a read error that is not NotFound, on every platform.
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    fs::create_dir(&p).unwrap();
    fs::write(
        d.path().join("settings.json.bak"),
        r#"{"output_dir":"/bak","keep_media":true}"#,
    )
    .unwrap();
    let before = listing(d.path());
    let defaults = json!({"output_dir": "/d", "keep_media": true});
    let r: Result<Req, Error> = read_with_defaults(&p, FileFormat::Flat, &defaults);
    assert!(matches!(r, Err(Error::Io(_))), "{r:?}");
    let opts = || {
        StoreOptions::new(schema())
            .format(FileFormat::Flat)
            .defaults_provider({
                let d = defaults.clone();
                move || d.clone()
            })
    };
    assert!(matches!(
        Store::<Req>::open_read_only(&p, opts()),
        Err(Error::Io(_))
    ));
    assert!(matches!(
        Store::<Req>::open_path(&p, opts()),
        Err(Error::Io(_))
    ));
    assert_eq!(listing(d.path()), before);
    assert!(p.is_dir());
}

#[test]
fn sidecars_off_never_reads_the_bak_even_when_the_file_is_missing() {
    let defaults = json!({"output_dir": "/d", "keep_media": true});
    let stale = r#"{"schemaVersion":1,"namespace":"t","values":{"output_dir":"/stale","keep_media":false},"revision":3}"#;
    for refuse in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        fs::write(d.path().join("settings.json.bak"), stale).unwrap();
        let dc = defaults.clone();
        let mut opts = StoreOptions::new(schema())
            .sidecars(false)
            .defaults_provider(move || dc.clone());
        if refuse {
            opts = opts.on_corrupt(OnCorrupt::Refuse);
        }
        let want = Req {
            output_dir: "/d".into(),
            keep_media: true,
        };
        let ro = Store::<Req>::open_read_only(&p, opts.clone()).unwrap();
        assert_eq!(ro.get(), want, "refuse={refuse}");
        let s = Store::<Req>::open_path(&p, opts).unwrap();
        assert_eq!(s.get(), want, "refuse={refuse}");
        let r: Req = read_with_options(
            &p,
            &defaults,
            ReadOptions::new(FileFormat::Envelope).sidecars(false),
        )
        .unwrap();
        assert_eq!(r, want, "refuse={refuse}");
        // With sidecars on (default), the bak is still used.
        let r: Req = read_with_defaults(&p, FileFormat::Envelope, &defaults).unwrap();
        assert_eq!(
            r,
            Req {
                output_dir: "/stale".into(),
                keep_media: false
            }
        );
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct Note {
    note: Option<String>,
}

#[test]
fn null_patch_on_a_key_the_file_lacks_stores_null() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.json");
    let opts = StoreOptions::new(schema())
        .null_means(NullMeans::SetNull)
        .allow_unknown_keys(|k| k.starts_with("sig:"));
    let s = Store::<Note>::open_path(&p, opts).unwrap();
    // Declared key the file does not hold.
    s.patch(&json!({"note": null}), None).unwrap();
    let doc = |p: &Path| -> Value { serde_json::from_slice(&fs::read(p).unwrap()).unwrap() };
    assert_eq!(doc(&p)["values"]["note"], Value::Null);
    // Open key the file does not hold: stored as null, not dropped.
    s.patch(&json!({"sig:a": null}), None).unwrap();
    assert!(doc(&p)["values"].as_object().unwrap().contains_key("sig:a"));
    assert_eq!(doc(&p)["values"]["sig:a"], Value::Null);
    // The preview agrees with the commit.
    let prev = s.patch_dry_run(&json!({"sig:b": null}), None).unwrap();
    assert_eq!(prev.document.get("sig:b"), Some(&Value::Null));
    // Under SetNull, null on a held open key stores null (removal is remove_keys only).
    s.patch(&json!({"sig:c": "x"}), None).unwrap();
    s.patch(&json!({"sig:c": null}), None).unwrap();
    assert_eq!(doc(&p)["values"]["sig:c"], Value::Null);

    // Default (Reset) mode: null on an absent open key is still a removal / no-op.
    let p2 = d.path().join("r.json");
    let r = Store::<Note>::open_path(
        &p2,
        StoreOptions::new(schema()).allow_unknown_keys(|k| k.starts_with("sig:")),
    )
    .unwrap();
    r.patch(&json!({"sig:a": null}), None).unwrap();
    let after = fs::read(&p2).ok();
    if let Some(bytes) = after {
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(!v["values"].as_object().unwrap().contains_key("sig:a"));
    }
}

fn doc(p: &Path) -> Value {
    serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
struct Cfg {
    theme: String,
    note: Option<String>,
}

fn cfg_file(dir: &Path) -> std::path::PathBuf {
    write(dir, &File::Values(json!({"theme": "x", "note": "n"})))
}

fn open_keys(k: &str) -> bool {
    k.starts_with("sig:")
}

#[test]
fn case_9_explicit_remove_is_independent_of_null_means() {
    for nm in [NullMeans::Reset, NullMeans::SetNull] {
        let d = tempfile::tempdir().unwrap();
        let p = cfg_file(d.path());
        let s = Store::<Cfg>::open_path(
            &p,
            StoreOptions::new(schema())
                .null_means(nm)
                .allow_unknown_keys(open_keys),
        )
        .unwrap();
        s.patch(&json!({"sig:a": "A"}), None).unwrap();
        // Absent key: nothing created, no write.
        let before = fs::read(&p).unwrap();
        let prev = s.remove_keys_dry_run(&["sig:zzz"]).unwrap();
        assert!(prev.change.keys.is_empty(), "{nm:?}");
        s.remove_keys(&["sig:zzz"]).unwrap();
        assert_eq!(fs::read(&p).unwrap(), before, "{nm:?}: absent remove wrote");
        assert!(!doc(&p)["values"]
            .as_object()
            .unwrap()
            .contains_key("sig:zzz"));
        // Present open key: dry run agrees with commit, then it is deleted.
        let prev = s.remove_keys_dry_run(&["sig:a"]).unwrap();
        assert_eq!(prev.change.keys, vec!["sig:a".to_string()], "{nm:?}");
        assert!(!prev.document.contains_key("sig:a"));
        assert_eq!(fs::read(&p).unwrap(), before, "dry run wrote");
        s.remove_keys(&["sig:a"]).unwrap();
        let v = doc(&p);
        assert!(
            !v["values"].as_object().unwrap().contains_key("sig:a"),
            "{nm:?}"
        );
        assert_eq!(
            v["values"].as_object().unwrap().len(),
            prev.document.len(),
            "{nm:?}"
        );
        // Declared Option key: back to the absent-key value; a key outside the schema is refused.
        s.remove_keys(&["note"]).unwrap();
        assert_eq!(s.get().note, None, "{nm:?}");
        assert!(matches!(s.remove_keys(&["nope"]), Err(Error::Invalid(_))));
    }
}

#[test]
fn case_9_remove_through_the_backend_trait() {
    use rightkit_app_settings::{Edit, FileBackend, SettingsBackend};
    let d = tempfile::tempdir().unwrap();
    let p = cfg_file(d.path());
    let b = FileBackend::new(&p, FileFormat::Envelope);
    let remove = |key: &'static str| {
        let b = &b;
        move || {
            b.patch(&mut |cur| {
                let Some(cur) = cur else {
                    return Ok(Edit::Keep);
                };
                if !cur.values.contains_key(key) {
                    return Ok(Edit::Keep);
                }
                let mut next = cur.clone();
                next.values.remove(key);
                Ok(Edit::Commit(next))
            })
            .unwrap()
        }
    };
    let before = fs::read(&p).unwrap();
    assert_eq!(remove("absent")(), None);
    assert_eq!(fs::read(&p).unwrap(), before, "absent key: no write");
    assert!(remove("note")().is_some());
    assert!(!doc(&p)["values"].as_object().unwrap().contains_key("note"));
}

#[test]
fn case_11_default_null_means_is_reset_and_a_null_for_an_absent_key_creates_nothing() {
    assert_eq!(NullMeans::default(), NullMeans::Reset);
    let d = tempfile::tempdir().unwrap();
    // The file holds no `note` key and no open key.
    let p = write(d.path(), &File::Values(json!({"theme": "x"})));
    let s = Store::<Cfg>::open_path(
        &p,
        StoreOptions::new(schema()).allow_unknown_keys(open_keys),
    )
    .unwrap();
    let before = fs::read(&p).unwrap();
    s.patch(&json!({"note": null, "sig:a": null}), None)
        .unwrap();
    assert_eq!(fs::read(&p).unwrap(), before, "Reset: no write");
    let keys = doc(&p)["values"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(keys, vec!["theme".to_string()], "Reset: nothing created");
    let prev = s
        .patch_dry_run(&json!({"note": null, "sig:a": null}), None)
        .unwrap();
    assert!(prev.change.keys.is_empty());

    // Only an explicit SetNull creates the keys, and dry run agrees.
    let d2 = tempfile::tempdir().unwrap();
    let p2 = write(d2.path(), &File::Values(json!({"theme": "x"})));
    let s2 = Store::<Cfg>::open_path(
        &p2,
        StoreOptions::new(schema())
            .null_means(NullMeans::SetNull)
            .allow_unknown_keys(open_keys),
    )
    .unwrap();
    let prev = s2
        .patch_dry_run(&json!({"note": null, "sig:a": null}), None)
        .unwrap();
    assert_eq!(prev.document.get("sig:a"), Some(&Value::Null));
    s2.patch(&json!({"note": null, "sig:a": null}), None)
        .unwrap();
    let v = doc(&p2);
    assert_eq!(v["values"]["note"], Value::Null);
    assert_eq!(v["values"]["sig:a"], Value::Null);
}

#[test]
fn set_null_does_not_write_for_unrelated_absent_null_fields() {
    // Under SetNull a patch that nulls nothing must not be turned into a write by an unrelated
    // declared null the file does not hold.
    let d = tempfile::tempdir().unwrap();
    let p = write(d.path(), &File::Values(json!({"theme": "x"})));
    let s = Store::<Cfg>::open_path(
        &p,
        StoreOptions::new(schema()).null_means(NullMeans::SetNull),
    )
    .unwrap();
    let before = fs::read(&p).unwrap();
    s.patch(&json!({"theme": "x"}), None).unwrap();
    assert_eq!(fs::read(&p).unwrap(), before);
}

fn null_store(dir: &Path, nm: NullMeans) -> (std::path::PathBuf, Store<Cfg>) {
    let p = cfg_file(dir);
    let s = Store::<Cfg>::open_path(
        &p,
        StoreOptions::new(schema())
            .null_means(nm)
            .allow_unknown_keys(open_keys),
    )
    .unwrap();
    (p, s)
}

fn has(p: &Path, k: &str) -> bool {
    doc(p)["values"].as_object().unwrap().contains_key(k)
}

#[test]
fn case_12_open_key_null_semantics_per_mode() {
    let nul = json!({"sig:a": null});
    // SetNull, held -> null; dry run agrees; second apply writes nothing and keeps the bytes.
    let d = tempfile::tempdir().unwrap();
    let (p, s) = null_store(d.path(), NullMeans::SetNull);
    s.patch(&json!({"sig:a": "A"}), None).unwrap();
    let prev = s.patch_dry_run(&nul, None).unwrap();
    assert_eq!(prev.change.keys, vec!["sig:a".to_string()]);
    assert_eq!(prev.document.get("sig:a"), Some(&Value::Null));
    s.patch(&nul, None).unwrap();
    assert_eq!(
        doc(&p)["values"]["sig:a"],
        Value::Null,
        "12a SetNull held -> null"
    );
    let once = fs::read(&p).unwrap();
    let prev2 = s.patch_dry_run(&nul, None).unwrap();
    assert!(
        prev2.change.keys.is_empty(),
        "12b dry run of the second apply"
    );
    assert_eq!(prev2.document, prev.document);
    s.patch(&nul, None).unwrap();
    assert_eq!(
        fs::read(&p).unwrap(),
        once,
        "12b SetNull twice: no second write"
    );
    // remove_keys under SetNull removes the null-valued key; dry run agrees.
    let prev = s.remove_keys_dry_run(&["sig:a"]).unwrap();
    assert!(!prev.document.contains_key("sig:a"));
    s.remove_keys(&["sig:a"]).unwrap();
    assert!(
        !has(&p, "sig:a"),
        "12f remove_keys removes a null-valued key"
    );

    // SetNull, absent -> null (case 7), dry run agrees, then idempotent.
    let d = tempfile::tempdir().unwrap();
    let (p, s) = null_store(d.path(), NullMeans::SetNull);
    let prev = s.patch_dry_run(&nul, None).unwrap();
    assert_eq!(prev.document.get("sig:a"), Some(&Value::Null));
    s.patch(&nul, None).unwrap();
    assert_eq!(
        doc(&p)["values"]["sig:a"],
        Value::Null,
        "12c SetNull absent -> null"
    );

    // Reset, held -> removed; dry run agrees.
    let d = tempfile::tempdir().unwrap();
    let (p, s) = null_store(d.path(), NullMeans::Reset);
    s.patch(&json!({"sig:a": "A"}), None).unwrap();
    let prev = s.patch_dry_run(&nul, None).unwrap();
    assert!(!prev.document.contains_key("sig:a"));
    s.patch(&nul, None).unwrap();
    assert!(!has(&p, "sig:a"), "12d Reset held -> removed");

    // Reset, absent -> nothing, no write; dry run agrees.
    let before = fs::read(&p).unwrap();
    let prev = s.patch_dry_run(&nul, None).unwrap();
    assert!(prev.change.keys.is_empty());
    s.patch(&nul, None).unwrap();
    assert_eq!(fs::read(&p).unwrap(), before, "12e Reset absent -> nothing");
}
