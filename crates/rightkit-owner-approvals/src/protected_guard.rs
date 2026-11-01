//! What each platform adds to the engine's protected item (`protected_store.rs`), so that a
//! process of the same user cannot hand the engine an item of its own making.
//!
//! * **macOS** (`mac.rs`, `seal.rs`): the item is a legacy file-keychain generic password. The
//!   data-protection keychain with an access group would be the stronger home, but it needs the
//!   restricted `keychain-access-groups` entitlement, which a Developer ID build can carry only with
//!   an embedded provisioning profile; the host app signs with `preEmbedProvisioningProfile: false`
//!   (`scripts/release/mac-payload.mjs`), ships none, and `docs/clipboard-spec.md` records that
//!   shipped builds carry no such entitlement. So the engine refuses an item whose keychain
//!   *partition list* is not the one Security gives an item created by code signed with the host app's team
//!   (`teamid:<team>`, the team the app registers with `set_signing_team`), and reads it from the login keychain by path. That is defence in depth
//!   only: a process of the same user can make a keychain of its own, give its item that partition
//!   list (it knows its own password) and move the file onto the login keychain's path. So the blob
//!   is also sealed with an HMAC (the same seal as Windows), whose key sits in
//!   `protected-settings.seal` and, once a PIN is set, is wrapped with a key derived from the
//!   PIN. A blob without a valid seal is read as closed defaults.
//! * **Windows** (`win.rs`): the Credential Manager item is writable by any process of the user, so
//!   the stored blob is sealed with an HMAC. The key is held in a second place (a DPAPI-protected
//!   file beside owner app's data) and, once a PIN is set, DPAPI also takes entropy derived
//!   from the PIN. A blob without a valid seal is read as closed defaults (`protected_store.rs`).
//! * Both seals are not versioned: an older valid blob can be put back (rollback), and without a PIN
//!   code that runs as the user can read the key (see docs/settings.md).
//! * Other platforms add nothing.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(any(windows, target_os = "macos"))]
mod seal;
#[cfg(windows)]
mod win;

/// Whether a stored blob's seal proves the engine wrote it.
pub enum Seal {
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    Valid,
    Unproven,
}

/// Refuses an item that the host app's own signed code did not create (macOS). Called before and after the
/// item is read; every other platform accepts.
pub fn check_item(service: &str, account: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        mac::check_item(service, account)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (service, account);
        Ok(())
    }
}

/// Reads the engine's item pinned to the login keychain, checked before and after the read (macOS).
/// `None` on every other platform, where the caller reads through the credential store. `Some(Ok(None))`
/// is "no item".
pub fn read_item(service: &str, account: &str) -> Option<Result<Option<String>, String>> {
    #[cfg(target_os = "macos")]
    {
        Some(mac::read_item(service, account))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (service, account);
        None
    }
}

/// Whether `mac` is the engine's seal over `message` (macOS, Windows). Other platforms have no seal,
/// so no blob is proven there (fail closed).
pub fn check_seal(message: &[u8], mac: Option<&str>) -> Seal {
    #[cfg(windows)]
    {
        win::check_seal(message, mac)
    }
    #[cfg(target_os = "macos")]
    {
        seal::file::check_seal(message, mac)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (message, mac);
        Seal::Unproven
    }
}

/// Whether the seal key in use came from the owner's PIN in this process (macOS, Windows). Only
/// such a key vouches for a blob that loosens a gate; false on platforms with no seal.
pub fn key_from_pin() -> bool {
    #[cfg(any(windows, target_os = "macos"))]
    {
        seal::key_from_pin()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        false
    }
}

/// The seal (hex) over `message`, or `None` when the seal key cannot be used (a PIN is set and has
/// not been entered since the engine started, or the platform has no seal).
pub fn make_seal(message: &[u8], pin_set: bool) -> Option<String> {
    #[cfg(windows)]
    {
        win::make_seal(message, pin_set)
    }
    #[cfg(target_os = "macos")]
    {
        seal::file::make_seal(message, pin_set)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (message, pin_set);
        None
    }
}

/// The owner's PIN was just verified: the seal key can be unwrapped with it (macOS, Windows).
/// Touch ID or Windows Hello alone does not call this.
pub fn pin_entered(pin: &str) {
    #[cfg(windows)]
    win::pin_entered(pin);
    #[cfg(target_os = "macos")]
    seal::file::pin_entered(pin);
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = pin;
}

/// The the host app PIN was set or changed: the seal key is wrapped with a key from the new PIN (macOS, Windows).
pub fn pin_changed(pin: &str) {
    #[cfg(windows)]
    win::pin_changed(pin);
    #[cfg(target_os = "macos")]
    seal::file::pin_changed(pin);
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = pin;
}

/// Whether the seal key is wrapped with the PIN (macOS, Windows); false elsewhere.
pub fn pin_bound() -> bool {
    #[cfg(windows)]
    {
        win::pin_bound()
    }
    #[cfg(target_os = "macos")]
    {
        seal::file::pin_bound()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        false
    }
}

/// Forgets the seal key and its file (macOS, Windows); used when the owner resets the protected store.
pub fn reset_seal() {
    #[cfg(windows)]
    win::reset_seal();
    #[cfg(target_os = "macos")]
    seal::file::reset_seal();
}
