use super::*;
use std::fs;
use std::path::PathBuf;

#[test]
fn installs_expected_top_level_directory() {
    let root = test_dir("ok");
    let archive = root.join("model.tar.gz");
    write_archive(&archive, &[("model/config.json", b"{}".as_slice())]);

    install_single_top_level_tar_gz(&archive, &root.join("models"), "model").unwrap();

    assert_eq!(
        fs::read_to_string(root.join("models/model/config.json")).unwrap(),
        "{}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn installs_legacy_archive_layout_as_a_versioned_directory_without_touching_legacy() {
    let root = test_dir("versioned");
    let archive = root.join("model.tar.gz");
    let models = root.join("models");
    fs::create_dir_all(models.join("model-legacy")).unwrap();
    fs::write(models.join("model-legacy/keep.txt"), b"legacy").unwrap();
    write_archive(&archive, &[("model-legacy/config.json", b"new".as_slice())]);

    install_single_top_level_tar_gz_as(&archive, &models, "model-legacy", "model-current").unwrap();

    assert_eq!(
        fs::read_to_string(models.join("model-current/config.json")).unwrap(),
        "new"
    );
    assert_eq!(
        fs::read_to_string(models.join("model-legacy/keep.txt")).unwrap(),
        "legacy"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn refuses_a_different_top_level_directory_and_keeps_the_previous_install() {
    let root = test_dir("wrong-wrapper");
    let models = root.join("models");
    let good = root.join("good.tar.gz");
    write_archive(&good, &[("model/config.json", b"v1".as_slice())]);
    install_single_top_level_tar_gz(&good, &models, "model").unwrap();

    let bad = root.join("bad.tar.gz");
    write_archive(&bad, &[("other/config.json", b"v2".as_slice())]);
    assert!(install_single_top_level_tar_gz(&bad, &models, "model").is_err());
    assert_eq!(
        fs::read_to_string(models.join("model/config.json")).unwrap(),
        "v1"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn skips_macos_appledouble_members() {
    let root = test_dir("appledouble");
    let archive = root.join("model.tar.gz");
    write_archive(
        &archive,
        &[
            ("._model", b"metadata".as_slice()),
            ("__MACOSX/._model", b"metadata".as_slice()),
            ("model/config.json", b"{}".as_slice()),
        ],
    );

    install_single_top_level_tar_gz(&archive, &root.join("models"), "model").unwrap();

    assert_eq!(
        fs::read_to_string(root.join("models/model/config.json")).unwrap(),
        "{}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn rejects_symlink_member() {
    let root = test_dir("symlink");
    let archive = root.join("model.tar.gz");
    write_symlink_archive(&archive, "model/link", "../outside");

    assert!(install_single_top_level_tar_gz(&archive, &root.join("models"), "model").is_err());
    let _ = fs::remove_dir_all(root);
}

/// Simulates a hard kill between `rename(dest -> aside)` and
/// `rename(installed -> dest)`: no in-process error ever happens, the
/// process just stops. Build that exact on-disk state directly (not by
/// injecting a failure into `install_single_top_level_tar_gz_as`, which
/// only covers in-process `Err`) and confirm recovery promotes the
/// orphaned aside back into place.
#[test]
fn recovers_orphaned_previous_install_when_dest_is_missing() {
    let root = test_dir("recover-orphan");
    let models = root.join("models");
    fs::create_dir_all(&models).unwrap();
    let aside = models.join(".model.previous-aside");
    fs::create_dir_all(&aside).unwrap();
    fs::write(aside.join("marker.txt"), b"previously-active-model").unwrap();
    // `dest` is missing entirely — the crash landed after the first
    // rename and before the second.
    assert!(!models.join("model").exists());

    let recovered = recover_orphaned_previous_install(&models, "model").unwrap();

    assert!(recovered, "orphaned aside must be reported as recovered");
    assert!(!aside.exists(), "aside must be gone after promotion");
    assert_eq!(
        fs::read_to_string(models.join("model/marker.txt")).unwrap(),
        "previously-active-model",
        "the previously-active model must be active again at dest"
    );
    let _ = fs::remove_dir_all(root);
}

/// If `dest` already exists (the interrupted install's second rename
/// actually landed, or a later cycle produced a fresh valid `dest`, and
/// only the aside cleanup itself was interrupted), `dest` must be
/// trusted and the stale aside discarded rather than clobbering a valid
/// current model with a stale previous one.
#[test]
fn discards_stale_aside_when_dest_is_already_valid() {
    let root = test_dir("recover-stale-aside");
    let models = root.join("models");
    fs::create_dir_all(&models).unwrap();
    let aside = models.join(".model.previous-aside");
    fs::create_dir_all(&aside).unwrap();
    fs::write(aside.join("marker.txt"), b"stale-previous-model").unwrap();
    let dest = models.join("model");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("marker.txt"), b"current-active-model").unwrap();

    let recovered = recover_orphaned_previous_install(&models, "model").unwrap();

    assert!(!recovered, "a valid dest must not be reported as recovered");
    assert!(!aside.exists(), "the stale aside must be discarded");
    assert_eq!(
        fs::read_to_string(dest.join("marker.txt")).unwrap(),
        "current-active-model",
        "the valid current model at dest must be untouched"
    );
    let _ = fs::remove_dir_all(root);
}

/// No aside on disk at all is the common case (every normal install
/// cleans its aside up) and must be a cheap, side-effect-free no-op.
#[test]
fn no_op_when_no_aside_exists() {
    let root = test_dir("recover-no-op");
    let models = root.join("models");
    fs::create_dir_all(&models).unwrap();
    fs::create_dir_all(models.join("model")).unwrap();
    fs::write(models.join("model/marker.txt"), b"only-model").unwrap();

    let recovered = recover_orphaned_previous_install(&models, "model").unwrap();

    assert!(!recovered);
    assert_eq!(
        fs::read_to_string(models.join("model/marker.txt")).unwrap(),
        "only-model"
    );
    let _ = fs::remove_dir_all(root);
}

fn test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rightkit-archive-install-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_archive(path: &Path, files: &[(&str, &[u8])]) {
    let file = fs::File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, *name, *data).unwrap();
    }
    builder.finish().unwrap();
}

fn write_symlink_archive(path: &Path, name: &str, target: &str) {
    let file = fs::File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_link_name(target).unwrap();
    header.set_cksum();
    builder
        .append_data(&mut header, name, std::io::empty())
        .unwrap();
    builder.finish().unwrap();
}
