/// Initialize structured logging from the RUST_LOG env var (default filter:
/// info) through RightKit's stderr tracing installer. Call once from main();
/// a second call is a harmless no-op because a global subscriber already
/// exists.
pub fn init() {
    // A second install fails because a subscriber already exists; that is documented as a no-op.
    let _already_installed = rightkit_logs::install_stderr_tracing("info");
}
