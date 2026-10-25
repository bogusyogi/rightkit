//! The owner side of the engine snapshot: what the licence owner hands an engine process.
//!
//! The engine never reads the credential store (only the owner binary does), so the owner reads
//! the stored record through the same fenced native source every other reader uses and sends the
//! three values of [`crate::SNAPSHOT_NAMES`] over `rightkit_credentials::handoff`. A licence it
//! could not read is sent as a failed read, never as "no licence", so a transient store problem
//! cannot look like Free to the engine and cannot leave the engine on a stale token either.

use rightkit_credentials::handoff::{ErrorCode, Message, Mode, Snapshot};
use rightkit_credentials::Secret;

use super::LicenseClient;
use crate::{
    NativeCredential, NativeEntitlementSource, CLOCK_FRESH, CLOCK_INITIALIZED, SNAPSHOT_CLOCK,
    SNAPSHOT_DEVICE, SNAPSHOT_NAMES, SNAPSHOT_TOKEN,
};

/// One name of a snapshot before it is written.
enum Item {
    Value(Secret),
    Absent,
    Failed,
}

/// Put `items` into a fresh snapshot. The names are constants that pass the handoff name check and
/// are distinct, which the tests assert; an insert that failed anyway would leave the entry
/// missing, and a missing token is "no licence" on the engine, so Pro stays denied.
fn snapshot_of(items: [(&str, Item); 3]) -> Snapshot {
    let mut snapshot = Snapshot::new();
    for (name, item) in items {
        let _ = match item {
            Item::Value(secret) => snapshot.insert_present(name, secret),
            Item::Absent => snapshot.insert_absent(name),
            Item::Failed => snapshot.insert_failed(name, ErrorCode::StoreError),
        };
    }
    snapshot
}

impl LicenseClient {
    /// The engine snapshot of the stored licence: the signed token, the device binding it was
    /// issued for and whether the anti-rollback ledger exists (see the `engine_snapshot` module
    /// docs). Never fails: a licence that is absent, revoked, unsigned or not yet migrated is an
    /// absent snapshot (the engine is Free); a record that cannot be read right now (a busy
    /// operation lock, an unresolved write, an unreadable store) is a failed read, which the engine
    /// treats as unreadable and so refuses Pro.
    ///
    /// A client built with `LicenseSetup::Disabled` has no licence product, so its snapshot is
    /// absent. Send a fresh snapshot when the engine starts and after every change
    /// (`LicenseClient::invalidation`); the engine replaces what it holds as a whole.
    pub fn engine_snapshot(&self) -> Snapshot {
        let read = match self.native_entitlement_source() {
            Ok(source) => source.read(),
            Err(_) => Ok(NativeCredential::Absent),
        };
        match read {
            Ok(NativeCredential::Signed(signed)) => snapshot_of([
                (
                    SNAPSHOT_TOKEN,
                    Item::Value(Secret::from_string(signed.token)),
                ),
                (
                    SNAPSHOT_DEVICE,
                    Item::Value(Secret::from_string(signed.device_binding)),
                ),
                (
                    SNAPSHOT_CLOCK,
                    Item::Value(Secret::from_string(if signed.clock_initialized {
                        CLOCK_INITIALIZED
                    } else {
                        CLOCK_FRESH
                    })),
                ),
            ]),
            Ok(_) => snapshot_of([
                (SNAPSHOT_TOKEN, Item::Absent),
                (SNAPSHOT_DEVICE, Item::Absent),
                (SNAPSHOT_CLOCK, Item::Absent),
            ]),
            Err(_) => snapshot_of([
                (SNAPSHOT_TOKEN, Item::Failed),
                (SNAPSHOT_DEVICE, Item::Failed),
                (SNAPSHOT_CLOCK, Item::Failed),
            ]),
        }
    }

    /// Answer an engine's handoff request for the snapshot: a Reply with the request's id, or an
    /// Error with its id and a typed code. Only a snapshot-mode request for exactly the three
    /// names of [`crate::SNAPSHOT_NAMES`] is answered; any other name is `NotAllowed` and any other
    /// shape (strict mode, a missing or repeated name, a message that is not a request) is
    /// `Invalid`, so the owner's licence record is never released through another name.
    ///
    /// The transport must have verified the caller (same app, same user, signed path) first, as for
    /// `HandoffPolicy::respond`.
    pub fn answer_engine_request(&self, request: &Message) -> Message {
        let Message::Request { id, mode, names } = request else {
            return Message::error(request.id(), ErrorCode::Invalid, None);
        };
        if let Some(stranger) = names
            .iter()
            .find(|name| !SNAPSHOT_NAMES.contains(&name.as_str()))
        {
            return Message::error(*id, ErrorCode::NotAllowed, Some(stranger.as_str()));
        }
        let whole = names.len() == SNAPSHOT_NAMES.len()
            && SNAPSHOT_NAMES
                .iter()
                .all(|wanted| names.iter().any(|name| name == wanted));
        if *mode != Mode::Snapshot || !whole {
            return Message::error(*id, ErrorCode::Invalid, None);
        }
        Message::reply_snapshot(*id, self.engine_snapshot())
    }
}
