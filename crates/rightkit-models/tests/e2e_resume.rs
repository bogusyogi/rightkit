//! End-to-end download recovery against rightkit-qa's MockServer, reached
//! through the store's loopback URL rewrite: pause mid-download, resume with
//! `Range` (206), the connection drops mid-resume, the server then stops
//! honouring `Range` (200), and the install restarts that file from zero.
//! The final bytes are checked byte-for-byte and against the sha256 pin.

use rightkit_models::*;
use rightkit_qa::mock::{MockServer, Reply, Request};
use sha2::{Digest, Sha256};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const SIZE: usize = 2_000_000;
const DROP_AFTER: u64 = 300_000;
const ROUTE: &str = "/org/demo/resolve/main/model.bin";

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

#[test]
fn pause_resume_drop_then_no_range_restart_yields_exact_bytes() {
    let body: Vec<u8> = (0..SIZE)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761).rotate_left(7) as u8)
        .collect();
    let etag = format!("\"{}\"", &sha(&body)[..16]);
    let calls = Arc::new(AtomicUsize::new(0));
    let (b2, e2, c2) = (body.clone(), etag.clone(), calls.clone());
    // 1st request: full 200 (the client pauses it). 2nd: honours Range, then
    // drops after DROP_AFTER bytes. Later: a server without resume.
    let mock = MockServer::start_with(move |req: &Request, _: &str| {
        if !req.is("GET", ROUTE) {
            return None;
        }
        let r = Reply::bytes("application/octet-stream", b2.clone()).with_header("ETag", &e2);
        Some(match c2.fetch_add(1, Ordering::SeqCst) {
            0 => r,
            1 => r.cut_after(DROP_AFTER),
            _ => r.ignore_range(),
        })
    })
    .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path().join("models"), "e2e")
        .with_url_rewrite(&mock.base)
        .unwrap();
    let spec = ModelSpec::new(
        "demo",
        vec![RevisionSpec {
            revision: "1".into(),
            files: vec![FileSpec {
                path: "model.bin".into(),
                url: format!("https://models.example.invalid{ROUTE}"),
                bytes: Some(SIZE as u64),
                sha256: Some(sha(&body)),
                ..Default::default()
            }],
            ..Default::default()
        }],
    );
    let part = store
        .model_dir("demo")
        .unwrap()
        .join(".staging/1/model.bin.part");

    // 1. Pause mid-download: staging and the .part are kept, nothing recorded.
    let o = InstallOptions {
        require_pinned: true,
        ..Default::default()
    };
    let pause = o.pause.clone();
    let err = store
        .install(&spec, &o, |e| {
            if let Event::Progress { file_done, .. } = e {
                if file_done > 500_000 {
                    pause.store(true, Ordering::Relaxed);
                }
            }
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Paused, "{err}");
    let kept = fs::metadata(&part).unwrap().len();
    assert!(kept > 500_000 && kept < SIZE as u64, "kept {kept}");
    assert_eq!(&fs::read(&part).unwrap()[..], &body[..kept as usize]);
    assert!(store.state("demo").unwrap().active.is_none());
    assert_eq!(store.paused("demo").unwrap().len(), 1);

    // 2+3. Resume: Range from `kept` (206), dropped after DROP_AFTER bytes;
    // the retry asks for the rest by Range again, gets a full 200 and
    // restarts from zero.
    let o = InstallOptions {
        require_pinned: true,
        ..Default::default()
    };
    let mut progress = Vec::new();
    let mut resumed = false;
    let out = store
        .install(&spec, &o, |e| match e {
            Event::Resumed { .. } => resumed = true,
            Event::Progress { file_done, .. } => progress.push(file_done),
            _ => {}
        })
        .unwrap();
    assert!(resumed);
    assert_eq!(out.key, "1");

    let seen: Vec<_> = mock
        .seen()
        .into_iter()
        .filter(|s| s.is("GET", ROUTE))
        .collect();
    assert_eq!(seen.len(), 3, "full, resumed+dropped, restarted");
    assert_eq!(seen[0].header("range"), None);
    assert_eq!(
        seen[1].header("range"),
        Some(format!("bytes={kept}-").as_str())
    );
    assert_eq!(seen[1].header("if-range"), Some(etag.as_str()));
    let after_drop = kept + DROP_AFTER;
    assert_eq!(
        seen[2].header("range"),
        Some(format!("bytes={after_drop}-").as_str()),
        "the dropped resume kept every byte it received"
    );
    // Progress: appended from `kept` up to the drop, then restarted from zero.
    let first_resume = progress.first().copied().unwrap();
    assert!(
        first_resume > kept,
        "resume appends after {kept}: {first_resume}"
    );
    let peak = progress
        .iter()
        .position(|&d| d == after_drop)
        .expect("reached the drop");
    assert!(
        progress[peak + 1..].first().is_some_and(|&d| d < kept),
        "no-range reply restarts from zero"
    );
    assert_eq!(*progress.last().unwrap(), SIZE as u64);

    // Final bytes: exact, pinned, recorded, staging gone.
    let dir = store.active_dir("demo").unwrap();
    assert_eq!(
        fs::read(dir.join("model.bin")).unwrap(),
        body,
        "no spliced bytes"
    );
    assert!(store.verify("demo", "1", VerifyMode::Full));
    assert!(!part.exists());
    assert!(store.paused("demo").unwrap().is_empty());
}
