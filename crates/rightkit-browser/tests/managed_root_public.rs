//! Public-API check: `ManagedRoot` is nameable and constructible from outside the crate, and
//! `install::ensure_chrome_for_testing` can be referenced as a function item that takes it.
//! The installer is never called, so no network access and no filesystem writes happen here.
#![cfg(feature = "install")]

use rightkit_browser::ManagedRoot;
use std::path::PathBuf;

#[test]
fn managed_root_is_public_and_constructible() {
    let dir: PathBuf = std::env::temp_dir().join("rightkit-browser-managed-root-public");
    let root = ManagedRoot::at(dir.clone());
    assert_eq!(root.path(), dir.as_path());

    // Path computation only: resolve() reads the environment and does not touch the disk.
    let _suite_root = ManagedRoot::resolve();
}

#[test]
fn ensure_chrome_for_testing_is_referenceable_as_function_item() {
    // Named but not called: no download, no extraction.
    let _entry = rightkit_browser::install::ensure_chrome_for_testing;
}
