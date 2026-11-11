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

/// Name prefixes (compared ASCII case-insensitively, as Windows environment names are) of every
/// variable that carries, or ever carried, licence or device material.
const LICENCE_ENV_PREFIXES: [&str; 3] = [
    "RIGHTKIT_LICENSE_",
    "RIGHTKIT_SIGNED_LICENSE_",
    "RIGHTKIT_DEVICE_",
];

/// Whether `name` is a licence or device variable that must never reach an agent tool process, a
/// hook, an MCP server or a `${VAR}` expansion.
pub fn is_licence_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    LICENCE_ENV_PREFIXES
        .iter()
        .any(|prefix| upper.starts_with(prefix))
}
