mod denial_report;
mod implementation;
#[cfg(target_os = "linux")]
mod landlock;

pub use denial_report::*;
// Denial taxonomy is RightKit's (`rightkit-sandbox`); re-exported so call sites keep
// `crate::sandbox::{classify_denial, DenialCategory, DenialSignature, DenialSource}`.
pub use rightkit_sandbox::{classify_denial, DenialCategory, DenialSignature, DenialSource};
// Protected-secret planner and deny-read path logic are RightKit's.
pub use rightkit_sandbox::{
    path_is_protected_secret, path_is_within, plan_deny_read_paths, secret_paths_within,
    secret_roots, secret_roots_under, user_profile_root, USERPROFILE_SECRET_FOLDERS,
};

pub use implementation::*;

#[cfg(windows)]
pub(crate) use implementation::resolve_program;
