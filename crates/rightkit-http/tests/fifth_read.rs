//! Regression tests for the fifth independent read
//! (docs/audits/2026-10-11-astra-http-fifth-read.md). Finding 9 is the destination-alias class:
//! the download's lock and partial files were derived from the destination name and sat beside
//! it, so a different spelling of the destination could replace them. They now live in a hidden
//! `.rk-download` directory under a hash name. Finding 10 is the `416` that published saved
//! bytes: a `416` now always restarts from zero. Finding 11 (Windows system proxy) and the
//! unit-test half of finding 5 are not here: the latter is in `src/download.rs`.

mod common;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use common::{claim_of, coord_dir, part_of, plant_claim, RawServer};
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
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("fifth-{name}"));
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

// ------------------------------------------------------------------ finding 10
/// The audit's sequence. A chunked `200` with a strong ETag `"v1"` and no total delivers
/// `AAAA` and is cut before the terminating chunk. The reconnect sends a range, and the server
/// (which replaced the resource) answers `416` with `Content-Range: bytes */4` and ETag
/// `"v2"`. The saved total is unknown, so nothing contradicted the length, and the old code
/// published the stale `AAAA`. A `416` is now a restart: the file is `BBBB`.
#[test]
fn f10_a_416_with_a_contradicting_etag_never_publishes_the_saved_bytes() {
    let heads: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    let server = RawServer::start(move |s, head, index| {
        seen.lock().unwrap().push(head.to_ascii_lowercase());
        let reply: &[u8] = match index {
            0 => {
                b"HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nAAAA\r\n"
            }
            1 => {
                b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */4\r\nETag: \"v2\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            }
            _ => {
                b"HTTP/1.1 200 OK\r\nETag: \"v2\"\r\nContent-Length: 4\r\nConnection: close\r\n\r\nBBBB"
            }
        };
        let _ = s.write_all(reply);
        let _ = s.flush();
    });
    let dir = scratch("f10-etag");
    let dest = dir.join("out.bin");
    let report = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                max_reconnects: 3,
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"BBBB");
    assert_eq!(report.bytes, 4);
    assert_eq!(report.resumed_from, 0);
    assert_eq!(report.attempts, 3);
    let heads = heads.lock().unwrap();
    assert_eq!(heads.len(), 3, "{heads:?}");
    assert!(!heads[0].contains("\r\nrange:"), "{}", heads[0]);
    assert!(heads[1].contains("\r\nrange: bytes=4-"), "{}", heads[1]);
    assert!(heads[1].contains("\r\nif-range: \"v1\""), "{}", heads[1]);
    assert!(
        !heads[2].contains("\r\nrange:"),
        "the restart must be a plain request: {}",
        heads[2]
    );
}

/// A server that answers every request with `416` ends the download in that error. The saved
/// bytes are not published, even though they hash to the digest the caller expects: a `416`
/// says nothing about them. The restart sends no range, so there is no loop.
#[test]
fn f10_a_server_that_answers_every_request_with_416_publishes_nothing() {
    let server =
        MockServer::start(|_, _| MockResponse::new(416).header("Content-Range", "bytes */4"));
    let dir = scratch("f10-always");
    let dest = dir.join("out.bin");
    common::plant_part(&dest, b"AAAA");
    let err = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some(sha_hex(b"AAAA")),
                ..DownloadOptions::default()
            },
        )
        .expect_err("a 416 must not complete the download");
    assert!(
        matches!(&err, DownloadError::Http(e) if e.status() == Some(416)),
        "{err:?}"
    );
    assert!(!dest.exists(), "the saved bytes must not be published");
    assert_eq!(server.request_count(), 2, "one ranged, one plain");
    assert_eq!(server.requests()[1].header("range"), None);
}

// ------------------------------------------------------------------ finding 9
/// `out.bin` is downloading and holds its claim. Every spelling that used to alias the lock
/// or partial file beside it (`out.bin.lock`, an upper-case spelling, one with a Kelvin sign
/// that APFS folds onto `k`, `out.bin.part`) is now an ordinary destination: those downloads
/// succeed, replace only their own destinations, and leave the claim and the partial file of
/// `out.bin` alone. A second download of `out.bin` is still refused, and the first finishes
/// with exactly the bytes it was served.
#[test]
fn f9_no_destination_spelling_reaches_a_held_claim_or_partial_file() {
    let hits = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let started_tx = Mutex::new(started_tx);
    let release_rx = Mutex::new(release_rx);
    let seen = hits.clone();
    let slow = RawServer::start(move |s, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n");
        let _ = s.write_all(&[b'a'; 50]);
        let _ = s.flush();
        let _ = started_tx.lock().unwrap().send(());
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10));
        let _ = s.write_all(&[b'b'; 50]);
        let _ = s.flush();
    });
    let fast = MockServer::start(|_, _| MockResponse::text(200, "other"));
    let dir = scratch("f9-held");
    let dest = dir.join("out.bin");
    let url = slow.url("/f");

    let first = {
        let (url, dest) = (url.clone(), dest.clone());
        std::thread::spawn(move || {
            Client::new(cfg()).download_to_path(&url, &dest, DownloadOptions::default())
        })
    };
    started_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the first download must reach the server");
    let claim_before = fs::metadata(claim_of(&dest)).map(|m| m.len());

    let mut others = Vec::new();
    for name in [
        "out.bin.lock",
        "OUT.BIN.LOCK",
        "out.bin.loc\u{212A}",
        "out.bin.part",
        "OUT.BIN.PART",
    ] {
        let result = Client::new(cfg()).download_to_path(
            &fast.url("/f"),
            &dir.join(name),
            DownloadOptions::default(),
        );
        others.push((name, result));
    }
    let again = Client::new(cfg()).download_to_path(&url, &dest, DownloadOptions::default());
    let claim_after = fs::metadata(claim_of(&dest)).map(|m| m.len());
    let part_during = fs::metadata(part_of(&dest)).map(|m| m.len());

    // Let the first one finish before asserting, so a failure does not strand its thread.
    release_tx.send(()).unwrap();
    let first = first.join().unwrap();

    for (name, result) in &others {
        let report = result
            .as_ref()
            .unwrap_or_else(|e| panic!("{name:?} must be an ordinary destination: {e:?}"));
        assert_eq!(report.bytes, 5, "{name:?}");
    }
    for name in ["out.bin.lock", "out.bin.part"] {
        assert_eq!(fs::read(dir.join(name)).unwrap(), b"other", "{name}");
    }
    assert!(
        matches!(&again, Err(DownloadError::Io(m)) if m.contains("in progress")),
        "the claim of out.bin was not held any more: {again:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the refused call sent a request"
    );
    assert_eq!(claim_before.unwrap(), claim_after.unwrap());
    assert!(
        part_during.is_ok(),
        "the partial file of out.bin disappeared: {part_during:?}"
    );
    let report = first.unwrap();
    assert_eq!(report.bytes, 100);
    let mut expected = vec![b'a'; 50];
    expected.extend_from_slice(&[b'b'; 50]);
    assert_eq!(fs::read(&dest).unwrap(), expected);
    assert!(!part_of(&dest).exists());
}

/// A destination named like the download directory, or inside it, is refused before any
/// request or filesystem access, in every spelling a case-insensitive volume equates with it.
#[test]
fn f9_destinations_in_the_download_directory_are_refused_before_any_io() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let server = MockServer::start(move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        MockResponse::text(200, "x")
    });
    let dir = scratch("f9-dir");
    for dest in [
        dir.join(".rk-download"),
        dir.join(".RK-DOWNLOAD"),
        dir.join(".r\u{212A}-download"),
        dir.join(".rk-download").join("out.bin"),
        dir.join(".RK-Download").join("lock"),
    ] {
        let err = Client::new(cfg())
            .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
            .expect_err("a destination in the download directory must be refused");
        assert!(matches!(err, DownloadError::Io(_)), "{dest:?}: {err:?}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no request may be sent");
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        0,
        "nothing may be created"
    );
}

/// After a download the parent holds the destination and the hidden directory, and the hidden
/// directory holds one claim file named by a hash and nothing else.
#[test]
fn f9_the_download_keeps_nothing_beside_the_destination() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "x"));
    let dir = scratch("f9-layout");
    let dest = dir.join("out.bin");
    Client::new(cfg())
        .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
        .unwrap();
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, [".rk-download", "out.bin"]);
    let inside: Vec<PathBuf> = fs::read_dir(coord_dir(&dest))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(inside, [claim_of(&dest)]);
}

/// A partial file with no claim record behind it was not made by a download of this
/// destination, so even with the right hash it is not resumed: no range is requested.
#[test]
fn f9_a_partial_file_without_a_claim_record_is_not_resumed() {
    let server = MockServer::start(|_, _| MockResponse::new(200).body(b"NEWDATA".to_vec()));
    let dir = scratch("f9-unclaimed");
    let dest = dir.join("out.bin");
    fs::create_dir_all(coord_dir(&dest)).unwrap();
    fs::write(part_of(&dest), b"OLD!").unwrap();
    let report = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                resume: true,
                expected_sha256: Some(sha_hex(b"NEWDATA")),
                ..DownloadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(report.resumed_from, 0);
    assert_eq!(fs::read(&dest).unwrap(), b"NEWDATA");
    assert_eq!(server.requests()[0].header("range"), None);
}

/// The same partial file under a claim that records another name is not resumed either, and a
/// claim that records this destination is: the record is what decides.
#[test]
fn f9_the_claim_record_decides_whether_a_partial_file_is_resumed() {
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
    let dir = scratch("f9-record");
    let opts = || DownloadOptions {
        resume: true,
        expected_sha256: Some(sha_hex(&data)),
        ..DownloadOptions::default()
    };

    // Another name in the claim: the partial file is discarded, the whole file is fetched.
    let foreign = dir.join("foreign.bin");
    plant_claim(&foreign);
    fs::write(claim_of(&foreign), b"some other destination").unwrap();
    fs::write(part_of(&foreign), &data[..4]).unwrap();
    let report = Client::new(cfg())
        .download_to_path(&server.url("/f"), &foreign, opts())
        .unwrap();
    assert_eq!(report.resumed_from, 0);
    assert_eq!(fs::read(&foreign).unwrap(), data);
    assert_eq!(server.requests()[0].header("range"), None);

    // This destination's own record: resumed.
    let own = dir.join("own.bin");
    plant_claim(&own);
    fs::write(part_of(&own), &data[..4]).unwrap();
    let report = Client::new(cfg())
        .download_to_path(&server.url("/f"), &own, opts())
        .unwrap();
    assert_eq!(report.resumed_from, 4);
    assert_eq!(fs::read(&own).unwrap(), data);
    assert_eq!(server.requests()[1].header("range"), Some("bytes=4-"));
}
