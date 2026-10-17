//! `suite::archive::install_tar_gz_expecting` pins the archive's top-level wrapper.

use rightkit_models::suite::archive::{install_tar_gz, install_tar_gz_expecting};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn tar_gz(dir: &Path, name: &str, members: &[(&str, &[u8])]) -> PathBuf {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in members {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        builder.append_data(&mut header, path, *data).unwrap();
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&builder.into_inner().unwrap()).unwrap();
    let path = dir.join(name);
    fs::write(&path, gz.finish().unwrap()).unwrap();
    path
}

#[test]
fn matching_wrapper_installs() {
    let t = tempfile::tempdir().unwrap();
    let a = tar_gz(t.path(), "a.tar.gz", &[("pkg/model.bin", b"v1")]);
    let root = t.path().join("models");
    install_tar_gz_expecting(&a, &root, "pkg", "demo").unwrap();
    assert_eq!(fs::read(root.join("demo/model.bin")).unwrap(), b"v1");
}

#[test]
fn wrong_wrapper_is_refused_and_keeps_the_previous_install() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("models");
    let good = tar_gz(t.path(), "good.tar.gz", &[("pkg/model.bin", b"v1")]);
    install_tar_gz_expecting(&good, &root, "pkg", "demo").unwrap();

    let bad = tar_gz(t.path(), "bad.tar.gz", &[("other/model.bin", b"v2")]);
    let err = install_tar_gz_expecting(&bad, &root, "pkg", "demo").unwrap_err();
    assert!(err.contains("expected wrapper"), "{err}");
    assert_eq!(fs::read(root.join("demo/model.bin")).unwrap(), b"v1");
    let names: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["demo".to_string()], "no staging or aside left");
}

#[test]
fn second_top_level_entry_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("models");
    let bad = tar_gz(
        t.path(),
        "two.tar.gz",
        &[("pkg/a", b"1"), ("stray.txt", b"2")],
    );
    assert!(install_tar_gz_expecting(&bad, &root, "pkg", "demo").is_err());
    assert!(!root.join("demo").exists());
}

#[test]
fn invalid_wrapper_name_is_refused_and_plain_install_still_accepts_any() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("models");
    let a = tar_gz(t.path(), "a.tar.gz", &[("whatever/f", b"1")]);
    assert!(install_tar_gz_expecting(&a, &root, "../x", "demo").is_err());
    install_tar_gz(&a, &root, "demo").unwrap();
    assert_eq!(fs::read(root.join("demo/f")).unwrap(), b"1");
}
