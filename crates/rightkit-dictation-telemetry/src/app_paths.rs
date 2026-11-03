use std::path::PathBuf;

pub use rightkit_dictation_core::app_paths::{app_data_dir_name, app_data_root};

pub fn app_data_file(name: &str) -> PathBuf {
    app_data_root().join(name)
}

/// Dropped by `relaunch_for_update` immediately before this process exits, and
/// consumed (deleted) by the next launch's startup gate.
///
/// The gate defers engine prewarm while system load is high, which is right for
/// the login boot storm it was built for and wrong for an update relaunch: the
/// load it samples there is the update ITSELF finishing — old process tearing
/// down, tarball extracted, Gatekeeper revalidating the fresh bundle — so the
/// gate waits out a storm that is already over and burns its full ceiling
/// (measured 20,042ms, load never below threshold). A restart the app
/// initiated on an already-warm machine has nothing to yield to.
pub fn update_relaunch_marker() -> PathBuf {
    app_data_file("update-relaunch.pending")
}
