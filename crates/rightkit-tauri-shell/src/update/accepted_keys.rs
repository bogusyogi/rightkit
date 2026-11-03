//! Which updater key checks a release, so the suite can rotate its updater key without stranding
//! an installed app (feature `update-license`).
//!
//! The updater plugin verifies a download with exactly one public key, fixed when the check is
//! built. The suite lists every key an app accepts (`rightkit_license::suite_api::updater_keys`),
//! and a release names the key that signed it in its minisign signature. [`check_across_keys`]
//! therefore asks the update server with the transport's configured key first and, when the
//! release turns out to be signed by another accepted key, asks again with that key, so the plugin
//! verifies the download with the right one. The plugin still does all the cryptography; this
//! module only chooses which of the accepted keys it uses, and refuses a release whose key is not
//! accepted before anything is downloaded.

use rightkit_license::suite_api::{self, UpdaterKey};

use super::TransportError;

/// Check for an update across `keys`.
///
/// `fetch(public_key)` asks the update server with that key and answers the release found (any
/// type `T` that carries its signature), `None` when there is no newer one. `signature_of` reads
/// the minisign signature text out of a release. `now` is unix seconds, `None` when the clock
/// cannot be read (a key with an end date is then refused).
///
/// * No `keys` (a transport configured with one key of its own): the first answer stands, as
///   before.
/// * Otherwise the signature's key id must name a listed key that has not ended
///   ([`TransportError::Verify`] when it is malformed, unknown or ended).
/// * A release signed by the key it was fetched with is returned as it is.
/// * A release signed by another accepted key is fetched again with that key. The second answer is
///   the one used, and only if it names that same key (a server that changes its mind between the
///   two requests is refused, never mixed). If the second answer is "no update", there is none.
pub(super) fn check_across_keys<T>(
    primary: &str,
    keys: &[UpdaterKey],
    now: Option<i64>,
    mut fetch: impl FnMut(&str) -> Result<Option<T>, TransportError>,
    signature_of: impl Fn(&T) -> &str,
) -> Result<Option<T>, TransportError> {
    let Some(found) = fetch(primary)? else {
        return Ok(None);
    };
    if keys.is_empty() {
        return Ok(Some(found));
    }
    let chosen = suite_api::select_updater_key(keys, signature_of(&found), now)
        .map_err(|_| TransportError::Verify)?;
    if chosen.public_key == primary {
        return Ok(Some(found));
    }
    let Some(again) = fetch(chosen.public_key)? else {
        return Ok(None);
    };
    match suite_api::select_updater_key(keys, signature_of(&again), now) {
        Ok(second) if second.public_key == chosen.public_key => Ok(Some(again)),
        _ => Err(TransportError::Verify),
    }
}
