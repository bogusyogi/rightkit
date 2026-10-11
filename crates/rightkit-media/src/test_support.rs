//! Test-only helpers for this crate's unit tests. The integration suites have their own copy in
//! `tests/common/mod.rs`, which a unit test cannot reach.

/// Reports an asset that this host does not have, then returns so the caller returns from the
/// test. Prints one `SKIP <test>: <what> (looked at <where>)` line, or panics with that text under
/// `RIGHTKIT_REQUIRE_ASSETS=1`, so a host that is meant to have the asset cannot pass by skipping.
pub(crate) fn skip_missing(what: &str, looked_at: &str) {
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
