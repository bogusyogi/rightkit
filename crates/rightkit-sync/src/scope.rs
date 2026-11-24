//! The one account-scope function (features `drive` and `http`).
//!
//! `AuthProvider::account_scope` must change when the account, the zone or the sync key changes,
//! so the engine pauses for consent instead of publishing into a zone sealed with other keys.
//! Every device of an account must compute the same string.
//!
//! The encoding is length-prefixed, so no two different inputs share an encoding (the older
//! `|`-joined forms did not have that property). With a key id it is byte-for-byte
//! `rightkit_sync_keys::account_scope`, which this crate cannot call (neither crate depends on the
//! other). Keep the two in step: the domain label and the field order are the contract. That
//! function also refuses an empty or over-long component and control characters; this one does not,
//! and yields the same string for every input that one accepts.

/// Domain label shared with `rightkit-sync-keys`.
const DOMAIN_WITH_KEY: &[u8] = b"rightkit-sync-keys/v1/account-scope";
/// Used when no key id is bound, so a keyless scope can never equal a keyed one.
const DOMAIN_NO_KEY: &[u8] = b"rightkit-sync/v1/account-scope-no-key";

fn push_lp(buf: &mut Vec<u8>, field: &[u8]) {
    buf.extend_from_slice(&(field.len() as u32).to_be_bytes());
    buf.extend_from_slice(field);
}

/// Lowercase hex SHA-256 over the domain label and the length-prefixed `app`, `zone`,
/// `account_id` and, when given, `key_id`. Bind the key id whenever the zone is sealed
/// (`cipher`): a rotation or reset then changes the scope.
pub fn account_scope(app: &str, zone: &str, account_id: &str, key_id: Option<&str>) -> String {
    let mut buf = Vec::new();
    buf.extend_from_slice(if key_id.is_some() {
        DOMAIN_WITH_KEY
    } else {
        DOMAIN_NO_KEY
    });
    push_lp(&mut buf, app.as_bytes());
    push_lp(&mut buf, zone.as_bytes());
    push_lp(&mut buf, account_id.as_bytes());
    if let Some(key_id) = key_id {
        push_lp(&mut buf, key_id.as_bytes());
    }
    rightkit_fs::digest::sha256_bytes(&buf)
}
