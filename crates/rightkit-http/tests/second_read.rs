//! Regression tests for the second independent read of 0.3.0
//! (docs/audits/2026-10-11-astra-rightkit-http-second-read.md). Each test names its finding and
//! was written from the finding's stated sequence before the fix. The async finding (4) is in
//! `second_read_async.rs`; the OAuth finding (6) is in `rightkit-oauth/tests/e2e.rs`.

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use common::RawServer;
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;

const GZIP_HELLO: [u8; 57] = [
    31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 203, 72, 205, 201, 201, 87, 72, 175, 202, 44, 80, 72, 202,
    79, 169, 84, 72, 203, 47, 82, 40, 201, 72, 85, 72, 201, 47, 207, 203, 201, 79, 76, 81, 40, 73,
    45, 46, 1, 0, 90, 233, 8, 170, 37, 0, 0, 0,
];

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..loopback_config()
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("a030b-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn part_of(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

fn resume_opts() -> DownloadOptions<'static> {
    DownloadOptions {
        resume: true,
        ..DownloadOptions::default()
    }
}

// ------------------------------------------------------------------ finding 1 (Windows)
//
// The interleaving itself (a link swapped in between the inspection and the open) is driven
// by the unit test `a_part_swapped_for_a_link_after_the_check_is_never_followed` in
// `src/download.rs`, which reaches the private open. These end-to-end tests cover what the
// Unix-only test in `audit_fixes.rs` does not: a link already in place on Windows.

#[cfg(windows)]
#[test]
fn f1_windows_part_symlink_is_replaced_and_its_target_untouched() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "fresh bytes"));
    let dir = scratch("f1-symlink");
    let victim = dir.join("victim.txt");
    fs::write(&victim, b"precious").unwrap();
    let dest = dir.join("out.bin");
    let part = part_of(&dest);
    if let Err(e) = std::os::windows::fs::symlink_file(&victim, &part) {
        // Creating a symlink needs Developer Mode or an elevated token. A runner that
        // cannot is not qualified to run this suite: fail rather than pass untested.
        panic!(
            "cannot create a symbolic link here ({e}); Windows qualification runners must \
             allow symlink creation"
        );
    }
    let report = Client::new(cfg())
        .download_to_path(&server.url("/f"), &dest, resume_opts())
        .unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
    assert_eq!(fs::read(&dest).unwrap(), b"fresh bytes");
    assert_eq!(report.bytes, 11);
    assert!(!part.exists());
}

#[cfg(windows)]
#[test]
fn f1_windows_part_junction_is_never_written_through() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "fresh bytes"));
    let dir = scratch("f1-junction");
    let target = dir.join("target_dir");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep.txt"), b"precious").unwrap();
    let dest = dir.join("out.bin");
    let part = part_of(&dest);
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&part)
        .arg(&target)
        .output();
    match made {
        Ok(out) if out.status.success() => {}
        other => panic!("mklink /J failed ({other:?}); the junction fixture must be created"),
    }
    // This covers a junction already in place, which a plain directory check also refuses;
    // the no-follow open is proved by the swap test in `src/download.rs`, which needs a
    // file symlink. Either outcome is acceptable: the junction is removed and the download
    // proceeds, or it is refused. A write through it, into the target directory, is not.
    let _ = Client::new(cfg()).download_to_path(&server.url("/f"), &dest, resume_opts());
    assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"precious");
    assert_eq!(
        fs::read_dir(&target).unwrap().count(),
        1,
        "nothing may be created inside the junction target"
    );
}

// ------------------------------------------------------------------ finding 2
/// Two resumed calls on one destination. The second must be refused before it touches `.part`
/// or sends a request; the first must finish with exactly the bytes it was served.
#[test]
fn f2_a_second_download_of_the_same_destination_is_refused_while_the_first_runs() {
    let hits = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let started_tx = Mutex::new(started_tx);
    let release_rx = Mutex::new(release_rx);
    let seen = hits.clone();
    let server = RawServer::start(move |s, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n");
        let _ = s.write_all(&[b'a'; 50]);
        let _ = s.flush();
        let _ = started_tx.lock().unwrap().send(());
        // Hold the rest of the body until the test lets go (or drops the sender).
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10));
        let _ = s.write_all(&[b'b'; 50]);
        let _ = s.flush();
    });
    let dir = scratch("f2-lock");
    let dest = dir.join("out.bin");
    let url = server.url("/f");

    let first = {
        let (url, dest) = (url.clone(), dest.clone());
        std::thread::spawn(move || Client::new(cfg()).download_to_path(&url, &dest, resume_opts()))
    };
    started_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the first download must reach the server");

    let second = Client::new(cfg()).download_to_path(&url, &dest, resume_opts());
    let refused = matches!(&second, Err(DownloadError::Io(m)) if m.contains("in progress"));
    // Let the first one finish before asserting, so a failure does not strand its thread.
    release_tx.send(()).unwrap();
    let first = first.join().unwrap();

    assert!(refused, "the second download was not refused: {second:?}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the refused call must send no request"
    );
    let report = first.unwrap();
    assert_eq!(report.bytes, 100);
    let mut expected = vec![b'a'; 50];
    expected.extend_from_slice(&[b'b'; 50]);
    assert_eq!(fs::read(&dest).unwrap(), expected);
    assert!(!part_of(&dest).exists());
}

/// The original failure: a second writer's bytes are in the file, both call counters say 100,
/// and `expected_bytes` is 100. Verification must read the real length of the file.
#[test]
fn f2_verification_uses_the_real_file_length_not_the_byte_counter() {
    let server = MockServer::start(|_, _| MockResponse::new(200).body(vec![b'a'; 100]));
    let dir = scratch("f2-len");
    let dest = dir.join("out.bin");
    let part = part_of(&dest);
    let mut appended = false;
    let mut rogue = |_p: &DownloadProgress| {
        if !appended {
            appended = true;
            // A writer that ignores the lock (an older crate version) appends through its own
            // handle, as the second resumed call did.
            let mut other = fs::OpenOptions::new().append(true).open(&part).unwrap();
            other.write_all(&[b'b'; 100]).unwrap();
        }
    };
    let err = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                expected_bytes: Some(100),
                resume: true,
                on_progress: Some(&mut rogue),
                ..DownloadOptions::default()
            },
        )
        .unwrap_err();
    assert!(
        matches!(
            err,
            DownloadError::LengthMismatch {
                expected: 100,
                actual: 200
            }
        ),
        "{err:?}"
    );
    assert!(!dest.exists(), "200 bytes must not be published as 100");
    assert!(!part.exists(), "the corrupted .part is deleted");
}

#[test]
fn f2_the_destination_lock_is_released_after_failure_and_after_success() {
    let server = MockServer::start(|_, i| {
        if i == 0 {
            MockResponse::new(404)
        } else {
            MockResponse::text(200, "ok")
        }
    });
    let dir = scratch("f2-release");
    let dest = dir.join("out.bin");
    let c = Client::new(cfg());
    let failed = c.download_to_path(&server.url("/f"), &dest, DownloadOptions::default());
    assert!(matches!(failed, Err(DownloadError::Http(_))), "{failed:?}");
    for _ in 0..2 {
        let report = c
            .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
            .unwrap();
        assert_eq!(report.bytes, 2);
    }
    assert_eq!(fs::read(&dest).unwrap(), b"ok");
}

// ------------------------------------------------------------------ finding 3
/// Blank lines before the status line are accepted by the transport. The wire sniffer used
/// to require `HTTP/` at byte 0, saw no head, and let the decoded bytes through.
#[test]
fn f3_chunked_gzip_behind_leading_blank_lines_is_refused() {
    const LEADS: [&[u8]; 3] = [b"\r\n", b"\r\n\r\n", b"\n"];
    for (i, lead) in LEADS.into_iter().enumerate() {
        let server = RawServer::start(move |s, _, _| {
            let mut out = lead.to_vec();
            out.extend_from_slice(
                b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            );
            out.extend_from_slice(format!("{:x}\r\n", GZIP_HELLO.len()).as_bytes());
            out.extend_from_slice(&GZIP_HELLO);
            out.extend_from_slice(b"\r\n0\r\n\r\n");
            let _ = s.write_all(&out);
        });
        let dir = scratch(&format!("f3-gzip-{i}"));
        let dest = dir.join("out.bin");
        let result = Client::new(cfg()).download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions::default(),
        );
        assert!(
            result.is_err(),
            "decoded bytes were accepted behind {lead:?}: {result:?}"
        );
        assert!(!dest.exists(), "behind {lead:?}");
    }
}

/// Bare line feeds end a head for the transport's parser too.
#[test]
fn f3_chunked_gzip_with_bare_line_feeds_is_refused() {
    let server = RawServer::start(|s, _, _| {
        let mut out = b"HTTP/1.1 200 OK\nContent-Encoding: gzip\nTransfer-Encoding: chunked\nConnection: close\n\n".to_vec();
        out.extend_from_slice(format!("{:x}\r\n", GZIP_HELLO.len()).as_bytes());
        out.extend_from_slice(&GZIP_HELLO);
        out.extend_from_slice(b"\r\n0\r\n\r\n");
        let _ = s.write_all(&out);
    });
    let dir = scratch("f3-lf");
    let dest = dir.join("out.bin");
    let result =
        Client::new(cfg()).download_to_path(&server.url("/f"), &dest, DownloadOptions::default());
    assert!(result.is_err(), "decoded bytes were accepted: {result:?}");
    assert!(!dest.exists());
}

/// Positive control for the two tests above: the same leading blank line in front of an
/// identity body is accepted by the transport and by the download, so those tests are about
/// the encoding and not about the blank line.
#[test]
fn f3_identity_chunked_behind_a_blank_line_still_downloads() {
    let server = RawServer::start(|s, _, _| {
        let _ = s.write_all(
            b"\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        );
    });
    let dir = scratch("f3-ok");
    let dest = dir.join("out.bin");
    let report = Client::new(cfg())
        .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
        .unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"hello");
    assert_eq!(report.bytes, 5);
}

// ------------------------------------------------------------------ finding 5
/// A retryable 503 whose body never arrives. The pre-drain check passes (the backoff fits in
/// the deadline), the drain runs until the deadline, and the retry sleep then has to be
/// clamped to what is left, not slept in full.
#[test]
fn f5_blocking_retry_sleep_does_not_outlive_the_request_deadline() {
    let server = RawServer::start(|s, _, _| {
        let _ = s.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 1\r\nConnection: close\r\n\r\n",
        );
        let _ = s.flush();
        std::thread::sleep(Duration::from_secs(8));
    });
    let c = Client::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1000),
            max_backoff: Duration::from_millis(1000),
            multiplier: 1.0,
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        request_timeout: Duration::from_secs(10),
        response_timeout: Some(Duration::from_secs(10)),
        ..loopback_config()
    });
    let started = Instant::now();
    let result = c.send_any(&Request::get(server.url("/")).deadline(Duration::from_millis(1500)));
    let took = started.elapsed();
    // The deadline error is the expected outcome; the last 503 is an acceptable one if the
    // clock left a sliver of budget. Only the elapsed time is the regression.
    match &result {
        Err(HttpError::Timeout { .. }) => {}
        Ok(resp) if resp.status == 503 => {}
        other => panic!("unexpected outcome: {other:?}"),
    }
    // Deadline 1.5 s. Before the fix: drain until 1.5 s, then the full 1 s backoff: 2.5 s.
    assert!(
        took < Duration::from_millis(2000),
        "slept past the deadline: {took:?}"
    );
}
