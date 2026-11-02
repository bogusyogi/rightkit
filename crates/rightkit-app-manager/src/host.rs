//! What the host app tells this crate about itself: its identity (so it never lists, quits or
//! uninstalls itself), its state folder and whether its privileged helper is approved.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The host app's identity, matched case-insensitively by prefix.
#[derive(Clone, Debug, Default)]
pub struct HostIdentity {
    /// Bundle id prefix, for example `com.example.myapp`.
    pub bundle_id_prefix: String,
    /// App or process name prefix, for example `MyApp`.
    pub name_prefix: String,
    /// The name shown in refusals, for example `MyApp`.
    pub display_name: String,
}

static IDENTITY: OnceLock<HostIdentity> = OnceLock::new();
static STATE_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static HELPER_ENABLED: AtomicBool = AtomicBool::new(false);

/// Set the host app's identity. Only the first call takes effect; returns `false` otherwise.
pub fn set_host_identity(identity: HostIdentity) -> bool {
    IDENTITY.set(identity).is_ok()
}

/// Name the host app's state folder (inventory, icon and update caches). Only the first call takes
/// effect; returns `false` otherwise.
pub fn set_state_directory(dir: PathBuf) -> bool {
    STATE_DIRECTORY.set(dir).is_ok()
}

/// Report whether the host app's privileged helper is approved (default `false`).
pub fn set_privileged_helper_enabled(enabled: bool) {
    HELPER_ENABLED.store(enabled, Ordering::Relaxed);
}

#[cfg(unix)]
pub(crate) fn state_directory() -> Option<PathBuf> {
    STATE_DIRECTORY.get().cloned()
}

#[cfg(unix)]
pub(crate) fn privileged_helper_enabled() -> bool {
    HELPER_ENABLED.load(Ordering::Relaxed)
}

#[cfg(unix)]
pub(crate) fn display_name() -> String {
    IDENTITY
        .get()
        .map(|id| id.display_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "This app".to_owned())
}

#[cfg(unix)]
fn prefix_match(text: &str, prefix: &str) -> bool {
    !prefix.is_empty() && text.to_lowercase().starts_with(&prefix.to_lowercase())
}

#[cfg(unix)]
pub(crate) fn is_host_bundle_id(bundle_id: &str) -> bool {
    IDENTITY
        .get()
        .is_some_and(|id| prefix_match(bundle_id, &id.bundle_id_prefix))
}

#[cfg(unix)]
pub(crate) fn is_host_name(name: &str) -> bool {
    IDENTITY
        .get()
        .is_some_and(|id| prefix_match(name, &id.name_prefix))
}

#[cfg(unix)]
/// The account's home folder: `HOME` (`USERPROFILE` on Windows); on Unix the password database's
/// home when `HOME` is unset or `/`.
pub(crate) fn account_home() -> PathBuf {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let env = std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    #[cfg(unix)]
    if env == std::path::Path::new("/")
        && let Some(dir) = passwd_home()
    {
        return dir;
    }
    env
}

#[cfg(unix)]
fn passwd_home() -> Option<PathBuf> {
    let mut buffer = vec![0 as libc::c_char; 16_384];
    // SAFETY: an all-zero `passwd` is a valid out-parameter for `getpwuid_r`.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: the buffer outlives the call and `found` points into `entry` or is null.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if status != 0 || found.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: `pw_dir` is a NUL-terminated string inside `buffer`.
    let dir = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}
