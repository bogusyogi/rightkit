//! Shared by this crate's suites: the missing-asset policy and the native OCR canary.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rightkit_ocr::{worker, BackendPreference, Isolation, Ocr, OcrOptions, WorkerCommand};

/// Reports an asset that this host does not have, then returns so the caller returns from the
/// test. Prints one `SKIP <test>: <what> (looked at <where>)` line to stderr. With
/// `RIGHTKIT_REQUIRE_ASSETS=1` it panics with that same text instead, so a host that is meant to
/// have the asset cannot pass by skipping. The test name is the name of the test thread.
pub fn skip_missing(what: &str, looked_at: &str) {
    let test = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    let message = format!("SKIP {test}: {what} (looked at {looked_at})");
    if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
        panic!("{message}");
    }
    eprintln!("{message}");
}

/// `rel` found by walking up from this crate: `<ancestor>/<rel>`, then `<ancestor>/claude/<rel>`.
/// A fixed `../..` depth breaks when the checkout moves, so workspace assets are searched for.
pub fn workspace_asset(rel: &str) -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .flat_map(|dir| [dir.join(rel), dir.join("claude").join(rel)])
        .find(|path| path.exists())
}

/// Probes the native OCR backend with its own canary, in an isolated worker. `Err` says why it
/// cannot read text on this host. Cached once per test binary.
pub fn native_ready() -> Result<(), String> {
    static READY: OnceLock<Result<(), String>> = OnceLock::new();
    READY
        .get_or_init(|| {
            let worker = WorkerCommand::new(
                env!("CARGO_BIN_EXE_rightkit-ocr"),
                [worker::DEFAULT_WORKER_ARG],
            );
            let ocr = Ocr::new(
                OcrOptions::new(BackendPreference::Native)
                    .with_isolation(Isolation::Worker(worker)),
            );
            match ocr.probe_native() {
                Ok(true) => Ok(()),
                Ok(false) => Err("the native backend reports itself unavailable".to_owned()),
                Err(error) => Err(error),
            }
        })
        .clone()
}

/// `true` when the native OCR canary reads text. Otherwise it reports a SKIP (or panics under
/// `RIGHTKIT_REQUIRE_ASSETS=1`) and returns `false`. The OS recognizer is the asset: on a hosted
/// runner its text-recognition service can fail even on the canary.
pub fn native_ready_or_skip() -> bool {
    match native_ready() {
        Ok(()) => true,
        Err(reason) => {
            skip_missing(
                "a native OS text recognizer that reads the canary (Apple Vision or Windows.Media.Ocr)",
                &format!("the isolated canary worker: {reason}"),
            );
            false
        }
    }
}
