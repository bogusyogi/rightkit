#![cfg(feature = "tracing")]

use rightkit_logs::install_stderr_tracing;

/// One test per binary: the initializer owns the process-wide subscriber.
#[test]
fn installs_once_then_reports_the_second_install() {
    install_stderr_tracing("info").expect("first install");
    tracing::info!("stderr line");
    assert!(install_stderr_tracing("info").is_err());
}
