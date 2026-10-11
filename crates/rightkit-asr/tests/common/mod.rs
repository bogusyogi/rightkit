//! Shared by this crate's suites: the missing-asset policy for hosted runners.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Reports an asset that this host does not have, then returns so the caller returns from the
/// test. Prints one `SKIP <test>: <what> (looked at <where>)` line to stderr. With
/// `RIGHTKIT_REQUIRE_ASSETS=1` it panics with that same text instead, so a host that is meant to
/// have the asset cannot pass by skipping. The test name is the name of the test thread.
pub fn skip_missing(what: &str, looked_at: &str) {
    let test = std::thread::current().name().unwrap_or("unnamed").to_owned();
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

/// `gate!(option, what, looked_at)`: the value inside `Some`. Otherwise it reports the missing
/// asset (a SKIP line, or a panic under `RIGHTKIT_REQUIRE_ASSETS=1`) and returns from the test.
#[allow(unused_macros)]
macro_rules! gate {
    ($opt:expr, $what:expr, $looked_at:expr $(,)?) => {
        match $opt {
            Some(v) => v,
            None => {
                $crate::common::skip_missing($what, $looked_at);
                return;
            }
        }
    };
}
