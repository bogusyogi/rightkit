/// Initialize structured logging from the RUST_LOG env var (default filter:
/// info) through RightKit's stderr tracing installer. Call once from main();
/// a second call is a harmless no-op because a global subscriber already
/// exists.
pub fn init() {
    // A second call is a harmless no-op: a global subscriber already exists.
    let _ = crate::install_stderr_tracing("info");
}
