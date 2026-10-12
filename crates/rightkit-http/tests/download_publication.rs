//! Regression tests for the comparative review of `download_to_path` publication (Unix only:
//! symbolic links and permission bits).
//!
//! Blocker 1: a destination whose parent is a link to a `.rk-download` directory passed the
//! lexical check and could replace another download's claim or partial file. The parent is now
//! resolved and refused. Blocker 2: the partial file had the default mode and the rename
//! replaced a `0600` destination with a `0644` file. The partial is now `0600`; the published
//! file takes the mode of a regular file it replaces, masked to `0o777`, and is `0600` when
//! there is none.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{claim_of, part_of, plant_part};
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;
use sha2::{Digest, Sha256};

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..loopback_config()
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("publication-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Every entry under `dir` (relative path, kind, mode, and bytes for a file), sorted. A
/// changed, added or removed entry changes the result.
fn snapshot(dir: &Path) -> Vec<(String, String, u32, Vec<u8>)> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<(String, String, u32, Vec<u8>)>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if meta.is_dir() {
                out.push((rel, "dir".into(), meta.mode() & 0o7777, Vec::new()));
                walk(root, &path, out);
            } else if meta.is_file() {
                let bytes = fs::read(&path).unwrap();
                out.push((rel, "file".into(), meta.mode() & 0o7777, bytes));
            } else {
                out.push((rel, "other".into(), meta.mode() & 0o7777, Vec::new()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

// ------------------------------------------------------------------ blocker 1
/// `alias` links to `dir/.rk-download`, which holds the claim and partial file of
/// `dir/out.bin`. A destination `alias/<claim name>` (the review's reproduction), one inside a
/// real subdirectory of the coordination directory reached through the alias, and a relative
/// spelling through the alias are all refused. No request is sent, and nothing in the
/// coordination directory is created, removed or changed.
#[test]
fn a_destination_whose_parent_links_to_the_download_directory_is_refused_and_nothing_changes() {
    let hits = MockServer::start(|_, _| MockResponse::text(200, "intruder"));
    let dir = scratch("alias");
    let victim = dir.join("out.bin");
    plant_part(&victim, b"AAAA");
    let coord = dir.join(".rk-download");
    fs::create_dir(coord.join("sub")).unwrap();
    fs::write(coord.join("sub").join("keep.bin"), b"keep").unwrap();
    let alias = dir.join("alias");
    symlink(&coord, &alias).unwrap();

    let claim_name = claim_of(&victim).file_name().unwrap().to_owned();
    let part_name = part_of(&victim).file_name().unwrap().to_owned();
    let before = snapshot(&dir);
    let claim_bytes_before = fs::read(claim_of(&victim)).unwrap();
    assert_eq!(fs::read(part_of(&victim)).unwrap(), b"AAAA");

    for dest in [
        alias.join(&claim_name),
        alias.join(&part_name),
        alias.join("sub").join("new.bin"),
        alias.join("sub").join("..").join("fresh.bin"),
    ] {
        for resume in [false, true] {
            let err = Client::new(cfg())
                .download_to_path(
                    &hits.url("/f"),
                    &dest,
                    DownloadOptions {
                        resume,
                        ..DownloadOptions::default()
                    },
                )
                .expect_err("a destination inside the download directory must be refused");
            assert!(
                matches!(&err, DownloadError::Io(m) if m.contains(".rk-download")),
                "{dest:?}: {err:?}"
            );
        }
    }

    assert_eq!(hits.request_count(), 0, "no request may be sent");
    assert_eq!(snapshot(&dir), before, "nothing may be created or changed");
    assert_eq!(fs::read(claim_of(&victim)).unwrap(), claim_bytes_before);
    assert_eq!(fs::read(part_of(&victim)).unwrap(), b"AAAA");
}

/// A link to an ordinary directory is not refused: the download lands in the real directory,
/// and the report names the path the caller gave.
#[test]
fn a_destination_whose_parent_links_to_an_ordinary_directory_still_downloads() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "payload"));
    let dir = scratch("alias-ok");
    let real = dir.join("real");
    fs::create_dir(&real).unwrap();
    let alias = dir.join("alias");
    symlink(&real, &alias).unwrap();
    let dest = alias.join("out.bin");
    let report = Client::new(cfg())
        .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
        .unwrap();
    assert_eq!(report.path, dest);
    assert_eq!(fs::read(real.join("out.bin")).unwrap(), b"payload");
    assert!(real.join(".rk-download").is_dir());
    assert!(!dir.join(".rk-download").exists());
}

/// `important.bin/` and `important.bin/.` are read by `Path::file_name` as `important.bin`.
/// They are refused as given, before any normalisation: the file keeps its bytes and its mode,
/// no request is sent and nothing is created.
#[test]
fn a_directory_style_destination_never_replaces_the_file_it_names() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "intruder"));
    let dir = scratch("dir-style");
    let file = dir.join("important.bin");
    fs::write(&file, b"precious").unwrap();
    set_mode(&file, 0o640);
    let before = snapshot(&dir);

    for suffix in ["/", "/.", "//", "/./", "/.."] {
        let mut raw = file.clone().into_os_string();
        raw.push(suffix);
        let dest = PathBuf::from(raw);
        for resume in [false, true] {
            let err = Client::new(cfg())
                .download_to_path(
                    &server.url("/f"),
                    &dest,
                    DownloadOptions {
                        resume,
                        ..DownloadOptions::default()
                    },
                )
                .expect_err("a destination that names a directory must be refused");
            assert!(
                matches!(&err, DownloadError::Io(m) if m.contains("names a directory")),
                "{dest:?}: {err:?}"
            );
        }
    }

    assert_eq!(server.request_count(), 0, "no request may be sent");
    assert_eq!(fs::read(&file).unwrap(), b"precious");
    assert_eq!(mode_of(&file), 0o640);
    assert_eq!(snapshot(&dir), before, "nothing may be created or changed");
}

// ------------------------------------------------------------------ blocker 2
/// An existing destination keeps its permission bits when the download replaces it, and holds
/// the downloaded bytes.
#[test]
fn an_existing_destination_keeps_its_mode_when_replaced() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "downloaded"));
    let dir = scratch("keep-mode");
    for (name, mode) in [
        ("private.bin", 0o600),
        ("group.bin", 0o640),
        ("exec.bin", 0o755),
        ("readonly.bin", 0o400),
    ] {
        let dest = dir.join(name);
        fs::write(&dest, b"old secret").unwrap();
        set_mode(&dest, mode);
        let report = Client::new(cfg())
            .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
            .unwrap();
        assert_eq!(report.bytes, 10, "{name}");
        assert_eq!(fs::read(&dest).unwrap(), b"downloaded", "{name}");
        assert_eq!(mode_of(&dest), mode, "{name}");
    }
}

/// A replaced file's setuid bit is not carried onto the downloaded bytes; its other bits are.
#[test]
fn a_setuid_bit_on_the_replaced_file_is_not_carried_over() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "downloaded"));
    let dir = scratch("setuid");
    let dest = dir.join("out.bin");
    fs::write(&dest, b"old").unwrap();
    set_mode(&dest, 0o4755);
    assert_eq!(
        mode_of(&dest),
        0o4755,
        "the fixture must have the setuid bit"
    );
    Client::new(cfg())
        .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
        .unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"downloaded");
    assert_eq!(mode_of(&dest), 0o755);
}

/// A destination that is a link is replaced by a regular file, as before; the link's target
/// keeps its bytes and its mode, and the new file is `0600`.
#[test]
fn a_destination_link_is_replaced_and_its_target_is_untouched() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "downloaded"));
    let dir = scratch("dest-link");
    let secret = dir.join("secret.txt");
    fs::write(&secret, b"secret").unwrap();
    set_mode(&secret, 0o600);
    let dest = dir.join("out.bin");
    symlink(&secret, &dest).unwrap();
    Client::new(cfg())
        .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
        .unwrap();
    assert!(!fs::symlink_metadata(&dest).unwrap().is_symlink());
    assert_eq!(fs::read(&dest).unwrap(), b"downloaded");
    assert_eq!(mode_of(&dest), 0o600);
    assert_eq!(fs::read(&secret).unwrap(), b"secret");
    assert_eq!(mode_of(&secret), 0o600);
}

/// A fresh download. The partial file is owner-only when the request arrives and after the
/// first chunk is written (the progress callback runs then), and the published file has the
/// documented mode, `0600`.
#[test]
fn a_fresh_download_is_owner_only_while_partial_and_0600_when_published() {
    let dir = scratch("fresh");
    let dest = dir.join("out.bin");
    let part = part_of(&dest);

    let at_request: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let (seen, watched) = (at_request.clone(), part.clone());
    let server = MockServer::start(move |_, _| {
        seen.lock().unwrap().push(mode_of(&watched));
        MockResponse::new(200).body(b"fresh bytes".to_vec())
    });

    let mut during_body = Vec::new();
    let mut cb = |_: &DownloadProgress| during_body.push(mode_of(&part));
    let report = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                on_progress: Some(&mut cb),
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.resumed_from, 0);
    assert_eq!(fs::read(&dest).unwrap(), b"fresh bytes");
    assert_eq!(mode_of(&dest), 0o600);
    assert!(!part.exists(), "the partial file was renamed away");

    let at_request = at_request.lock().unwrap();
    assert_eq!(at_request.len(), 1);
    assert!(!during_body.is_empty(), "the progress callback must run");
    for mode in at_request.iter().chain(during_body.iter()) {
        assert_eq!(
            mode & 0o077,
            0,
            "group/world access on the partial: {mode:o}"
        );
    }
}

/// A leftover partial made world-readable (as an older version or another tool would have
/// made it) is narrowed to `0600` before the call sends its request, so before any byte is
/// appended, and the resume really happened (a range was requested and four bytes were kept).
#[test]
fn a_resumed_world_readable_partial_is_narrowed_before_use() {
    let data = b"0123456789".to_vec();
    let dir = scratch("resume");
    let dest = dir.join("out.bin");
    plant_part(&dest, &data[..4]);
    let part = part_of(&dest);
    set_mode(&part, 0o644);
    assert_eq!(mode_of(&part), 0o644);

    let at_request: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let (seen, watched, served) = (at_request.clone(), part.clone(), data.clone());
    let server = MockServer::start(move |req, _| {
        seen.lock().unwrap().push(mode_of(&watched));
        if req.header("range") == Some("bytes=4-") {
            MockResponse::new(206)
                .header("Content-Range", "bytes 4-9/10")
                .body(served[4..].to_vec())
        } else {
            MockResponse::new(200).body(served.clone())
        }
    });

    let mut during_body = Vec::new();
    let mut cb = |_: &DownloadProgress| during_body.push(mode_of(&part));
    let report = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some(sha_hex(&data)),
                on_progress: Some(&mut cb),
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        report.resumed_from, 4,
        "the leftover partial must be resumed"
    );
    assert_eq!(server.requests()[0].header("range"), Some("bytes=4-"));
    assert_eq!(fs::read(&dest).unwrap(), data);
    assert_eq!(mode_of(&dest), 0o600);

    assert_eq!(
        at_request.lock().unwrap().as_slice(),
        [0o600],
        "narrowed before the request, so before any appended byte"
    );
    assert!(!during_body.is_empty());
    assert!(during_body.iter().all(|m| *m == 0o600), "{during_body:?}");
}

/// A resumed partial that is replacing a `0600` destination: the published file is `0600`.
#[test]
fn a_resumed_download_over_a_private_destination_stays_private() {
    let data = b"0123456789".to_vec();
    let served = data.clone();
    let server = MockServer::start(move |req, _| {
        if req.header("range") == Some("bytes=4-") {
            MockResponse::new(206)
                .header("Content-Range", "bytes 4-9/10")
                .body(served[4..].to_vec())
        } else {
            MockResponse::new(200).body(served.clone())
        }
    });
    let dir = scratch("resume-private");
    let dest = dir.join("out.bin");
    fs::write(&dest, b"old secret").unwrap();
    set_mode(&dest, 0o600);
    plant_part(&dest, &data[..4]);
    set_mode(&part_of(&dest), 0o644);
    let report = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some(sha_hex(&data)),
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.resumed_from, 4);
    assert_eq!(fs::read(&dest).unwrap(), data);
    assert_eq!(mode_of(&dest), 0o600);
}
