//! Shared-runtime reference counting and GC against real temp directories.

use rightkit_models::suite::paths;
use rightkit_models::{ErrorCode, Store};
use std::fs;
use std::path::{Path, PathBuf};

fn runtime(tools: &Path, kind: &str, version: &str) -> PathBuf {
    let dir = tools.join(kind).join(version);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("onnxruntime.dll"), b"dll").unwrap();
    dir
}

fn fixture() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "heardright"));
    (tmp, store)
}

#[test]
fn tools_root_is_sibling_of_models_unless_overridden() {
    let (tmp, store) = fixture();
    assert_eq!(store.tools_root(), tmp.path().join("tools"));
    let other = tmp.path().join("elsewhere");
    assert_eq!(store.with_tools_root(&other).tools_root(), other);
}

#[test]
fn refcounted_runtime_survives_until_last_app_unregisters() {
    let (_tmp, store) = fixture();
    let tools = store.tools_root();
    let v1 = runtime(&tools, "onnxruntime", "1.20.1");
    let v0 = runtime(&tools, "onnxruntime", "1.19.0");
    store
        .register_shared("heardright", "onnxruntime", "1.20.1")
        .unwrap();
    store
        .register_shared("scraperight", "onnxruntime", "1.20.1")
        .unwrap();
    store
        .register_shared("scraperight", "onnxruntime", "1.20.1")
        .unwrap(); // idempotent

    // The unreferenced other version goes; the referenced one stays.
    assert_eq!(
        store.gc_shared(&["onnxruntime"]).unwrap(),
        vec![("onnxruntime".to_string(), "1.19.0".to_string())]
    );
    assert!(!v0.exists() && v1.exists());

    // One app leaves: still referenced by the other.
    assert!(store.unregister_app("heardright").unwrap().is_empty());
    assert!(store.gc_shared(&["onnxruntime"]).unwrap().is_empty());
    assert!(v1.join("onnxruntime.dll").exists());

    // Last app leaves: the version is reported, then removed.
    assert_eq!(
        store.unregister_shared("scraperight").unwrap(),
        vec![("onnxruntime".to_string(), "1.20.1".to_string())]
    );
    assert_eq!(
        store.gc_shared(&["onnxruntime"]).unwrap(),
        vec![("onnxruntime".to_string(), "1.20.1".to_string())]
    );
    assert!(!v1.exists());
    assert!(tools.join("refs").is_dir(), "tools/refs is never touched");
}

#[test]
fn gc_shared_only_considers_listed_kinds() {
    let (_tmp, store) = fixture();
    let tools = store.tools_root();
    let ytdlp = runtime(&tools, "yt-dlp", "1.0");
    let ort = runtime(&tools, "onnxruntime", "1.19.0");
    assert!(store.gc_shared(&[]).unwrap().is_empty());
    assert!(ytdlp.exists() && ort.exists(), "empty list removes nothing");
    assert_eq!(
        store.gc_shared(&["onnxruntime"]).unwrap(),
        vec![("onnxruntime".to_string(), "1.19.0".to_string())]
    );
    assert!(ytdlp.exists(), "unregistered, unlisted tool survives");
    assert!(!ort.exists());
    assert!(store.gc_shared(&["../x"]).is_err());
}

#[test]
fn corrupt_ref_aborts_gc_with_nothing_removed() {
    let (_tmp, store) = fixture();
    let dir = runtime(&store.tools_root(), "onnxruntime", "1.19.0");
    store
        .register_shared("heardright", "onnxruntime", "1.20.1")
        .unwrap();
    fs::write(store.tool_refs_dir().join("broken.json"), b"{ not json").unwrap();
    assert!(store.gc_shared(&["onnxruntime"]).is_err());
    assert!(dir.exists());
    assert!(store.unregister_shared("heardright").is_err());
}

#[test]
fn invalid_kind_or_version_is_rejected() {
    let (_tmp, store) = fixture();
    for (kind, version) in [
        ("", "1.0"),
        ("onnxruntime", ""),
        ("..", "1.0"),
        ("onnxruntime", ".."),
        ("a/b", "1.0"),
        ("onnxruntime", "1/../2"),
        ("onnxruntime", "1\\2"),
        ("refs", "1.0"),
        ("onnx@x", "1.0"),
    ] {
        let err = store
            .register_shared("heardright", kind, version)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidSpec, "{kind:?} {version:?}");
        assert!(paths::shared_runtime_dir_at(Path::new("t"), kind, version).is_err());
    }
    assert!(!store.tool_refs_dir().exists(), "nothing was written");
    assert_eq!(
        paths::shared_runtime_dir_at(Path::new("t"), "onnxruntime", "1.20.1").unwrap(),
        Path::new("t").join("onnxruntime").join("1.20.1")
    );
}

#[cfg(unix)]
#[test]
fn gc_shared_never_follows_symlinks() {
    let (tmp, store) = fixture();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), b"x").unwrap();
    let kind = store.tools_root().join("onnxruntime");
    fs::create_dir_all(&kind).unwrap();
    std::os::unix::fs::symlink(&outside, kind.join("9.9.9")).unwrap();
    assert!(store.gc_shared(&["onnxruntime"]).unwrap().is_empty());
    assert!(outside.join("keep.txt").exists());
}

/// Test stores may reach the local mock server on 127.0.0.1 and nothing else.
fn local(store: Store) -> Store {
    store
        .with_http_options(rightkit_models::http::ModelHttpOptions {
            proxy: rightkit_models::http::ProxyMode::None,
            ..Default::default()
        })
        .with_address_policy(
            rightkit_http::AddressPolicy::public_web()
                .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        )
}

#[test]
fn activate_installs_in_place_at_current_and_keeps_one_previous() {
    let (_tmp, store) = fixture();
    let tools = store.tools_root();
    let v1 = runtime(&tools, "onnxruntime", "1.20.1");
    let v2 = runtime(&tools, "onnxruntime", "1.21.0");
    fs::write(v2.join("onnxruntime.dll"), b"dll-2").unwrap();
    let current = paths::shared_runtime_current_dir_at(&tools, "onnxruntime").unwrap();
    assert_eq!(current, tools.join("onnxruntime").join("current"));

    assert_eq!(
        store
            .activate_shared_runtime("onnxruntime", "1.20.1")
            .unwrap(),
        current
    );
    assert_eq!(fs::read(current.join("onnxruntime.dll")).unwrap(), b"dll");
    // Idempotent: nothing is moved aside when the files already match.
    store
        .activate_shared_runtime("onnxruntime", "1.20.1")
        .unwrap();
    assert!(!tools.join("onnxruntime").join("current.previous").exists());

    // An update replaces `current` in place and keeps exactly one `.previous`.
    store
        .activate_shared_runtime("onnxruntime", "1.21.0")
        .unwrap();
    assert_eq!(fs::read(current.join("onnxruntime.dll")).unwrap(), b"dll-2");
    assert_eq!(
        fs::read(tools.join("onnxruntime/current.previous/onnxruntime.dll")).unwrap(),
        b"dll"
    );
    assert!(!tools.join("onnxruntime").join("current.staging").exists());
    assert!(
        v1.exists() && v2.exists(),
        "version directories are untouched"
    );

    // The fixed names are not versions: they cannot be registered and GC never removes them.
    assert!(store
        .register_shared("heardright", "onnxruntime", "current")
        .is_err());
    assert!(store.gc_shared(&["onnxruntime"]).unwrap().len() == 2);
    assert!(current.join("onnxruntime.dll").exists());
    assert!(tools.join("onnxruntime/current.previous").exists());
}
