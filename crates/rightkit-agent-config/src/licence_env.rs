//! Environment names that belong to the licence path.
//!
//! The desktop shell hands the engine daemon only NON-secret locators (where the licence record and
//! the device identity live); the daemon opens, reads and verifies the stored record itself. No
//! signed token, licence key or raw device id is ever placed in an environment or argument list.
//!
//! The locators are consumed once at process start and removed from the daemon's own environment
//! (see the engine's `license::capture_launch_locators`), and every child-process or `${VAR}`
//! expansion path additionally refuses a licence-named variable through [`is_licence_env_name`], so
//! a shell tool, hook or MCP server never sees one even if one is set later.

/// The licence directory of the shell (`<app data>/license`): the operation lock, the rotation lock
/// directory and the anti-rollback ledger live there.
pub const ENV_LICENSE_DIR: &str = "RIGHTKIT_LICENSE_DIR";
/// The credential namespace (service) of the licence record.
pub const ENV_LICENSE_SERVICE: &str = "RIGHTKIT_LICENSE_SERVICE";
/// The credential account of the licence record.
pub const ENV_LICENSE_ACCOUNT: &str = "RIGHTKIT_LICENSE_ACCOUNT";
/// The credential namespace (service) of the device identity.
pub const ENV_LICENSE_DEVICE_SERVICE: &str = "RIGHTKIT_LICENSE_DEVICE_SERVICE";
/// The credential account of the device identity.
pub const ENV_LICENSE_DEVICE_ACCOUNT: &str = "RIGHTKIT_LICENSE_DEVICE_ACCOUNT";

/// Absolute path of the licence owner's executable (the desktop app). The owner locators name the
/// owner, not the host, so they keep the `RIGHTKIT_` prefix like the variables above; a host that
/// wants its own prefix builds the name with [`host_license_env_name`].
pub const ENV_LICENSE_OWNER_EXE: &str = "RIGHTKIT_LICENSE_OWNER_EXE";
/// The macOS Developer team identifier that signs the licence owner.
pub const ENV_LICENSE_OWNER_TEAM: &str = "RIGHTKIT_LICENSE_OWNER_TEAM";
/// The Windows Authenticode signer common name of the licence owner.
pub const ENV_LICENSE_OWNER_SIGNER: &str = "RIGHTKIT_LICENSE_OWNER_SIGNER";

/// `<host env prefix>_LICENSE_<suffix>` (for example `OWNER_EXE`), the licence variable name in
/// the host's own namespace ([`crate::env_prefix`]). A name built this way is always refused by
/// [`is_licence_env_name`].
pub fn host_license_env_name(suffix: &str) -> String {
    format!("{}_LICENSE_{suffix}", crate::env_prefix())
}

/// Name prefixes (compared ASCII case-insensitively, as Windows environment names are) of every
/// variable that carries, or ever carried, licence or device material, under the `RIGHTKIT_`
/// prefix. They are refused for every host.
const LICENCE_ENV_PREFIXES: [&str; 3] = [
    "RIGHTKIT_LICENSE_",
    "RIGHTKIT_SIGNED_LICENSE_",
    "RIGHTKIT_DEVICE_",
];

/// The same families under the host's own env prefix ([`crate::env_prefix`]): a host named
/// `MYAPP` also has `MYAPP_LICENSE_`, `MYAPP_SIGNED_LICENSE_` and `MYAPP_DEVICE_` refused, without
/// a separate call. Added to [`LICENCE_ENV_PREFIXES`], never instead of it.
const LICENCE_ENV_HOST_FAMILIES: [&str; 3] = ["LICENSE_", "SIGNED_LICENSE_", "DEVICE_"];

/// Whether `name` is a licence or device variable that must never reach an agent tool process, a
/// hook, an MCP server or a `${VAR}` expansion. The `RIGHTKIT_` families are always refused; the
/// host env prefix's families are refused as well.
pub fn is_licence_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if LICENCE_ENV_PREFIXES
        .iter()
        .any(|prefix| upper.starts_with(prefix))
    {
        return true;
    }
    let host = crate::env_prefix().to_ascii_uppercase();
    LICENCE_ENV_HOST_FAMILIES
        .iter()
        .any(|family| upper.starts_with(&format!("{host}_{family}")))
}
