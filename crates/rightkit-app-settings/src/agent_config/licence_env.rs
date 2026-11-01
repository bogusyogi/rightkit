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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_licence_and_device_name_is_recognised_in_any_case() {
        for name in [
            "RIGHTKIT_SIGNED_LICENSE_TOKEN",
            "RIGHTKIT_LICENSE_DEVICE_ID",
            "RIGHTKIT_LICENSE_FEATURES",
            "RIGHTKIT_LICENSE_KEY",
            ENV_LICENSE_DIR,
            ENV_LICENSE_SERVICE,
            ENV_LICENSE_ACCOUNT,
            ENV_LICENSE_DEVICE_SERVICE,
            ENV_LICENSE_DEVICE_ACCOUNT,
            "RIGHTKIT_DEVICE_ID",
            "rightkit_signed_license_token",
        ] {
            assert!(is_licence_env_name(name), "{name}");
        }
    }

    #[test]
    fn unrelated_names_are_left_alone() {
        for name in [
            "PATH",
            "RIGHTKIT_CONTEXT",
            "RIGHTKIT_EMBED_MODEL_DIR",
            "ORT_DYLIB_PATH",
            "LICENSE_DIR",
        ] {
            assert!(!is_licence_env_name(name), "{name}");
        }
    }
}
