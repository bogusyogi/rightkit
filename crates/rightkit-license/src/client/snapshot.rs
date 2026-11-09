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
    ClockStore, NativeCredential, NativeEntitlementSource, CLOCK_FRESH, CLOCK_INITIALIZED,
    SNAPSHOT_CLOCK, SNAPSHOT_CLOCK_STATE, SNAPSHOT_DEVICE, SNAPSHOT_NAMES,
    SNAPSHOT_NAMES_WITH_CLOCK, SNAPSHOT_TOKEN,
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
fn snapshot_of<const N: usize>(items: [(&str, Item); N]) -> Snapshot {
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
    /// absent snapshot (the engine is Free); a record that cannot be read right now (a write in
    /// flight, an unresolved write, an unreadable store, or an operation holding the lock with no
    /// active signed record stored) is a failed read, which the engine treats as unreadable and so
    /// refuses Pro. An operation that is only on the network (a renew) does not make the read fail:
    /// the stored active signed record is sent as it stands (`ClientNativeEntitlementSource`).
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

    /// [`engine_snapshot`](Self::engine_snapshot) plus the anti-rollback high-water mark
    /// ([`SNAPSHOT_CLOCK_STATE`]), for an owner that keeps its ledger in the credential store
    /// (`LicenseClientConfig::with_credential_clock`) where an engine cannot read it (rightkit#1477).
    /// The mark is the owner's real ledger, read through the client's own clock store (the file or
    /// the credential store, under the same fences as every other clock access) and sent as the
    /// ledger's JSON. A licence that is absent or not signed is an absent mark. When a signed
    /// licence is stored but the ledger cannot be read right now (a held lock, an unresolved write,
    /// an unreadable or corrupt store), the mark is a failed read and so is the whole answer on the
    /// engine, which refuses Pro rather than verifying against a ledger it was not given. A signed
    /// licence with no ledger yet is sent as `fresh` with an absent mark, as in the three-name
    /// snapshot.
    pub fn engine_snapshot_with_clock(&self) -> Snapshot {
        let read = match self.native_entitlement_source() {
            Ok(source) => source.read(),
            Err(_) => Ok(NativeCredential::Absent),
        };
        match read {
            Ok(NativeCredential::Signed(signed)) => {
                let mark = match self.native_clock_store().map(|store| store.load()) {
                    Ok(Ok(Some(state))) => Item::Value(Secret::from_string(state.encode())),
                    Ok(Ok(None)) => Item::Absent,
                    Ok(Err(_)) | Err(_) => Item::Failed,
                };
                snapshot_of([
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
                    (SNAPSHOT_CLOCK_STATE, mark),
                ])
            }
            Ok(_) => snapshot_of([
                (SNAPSHOT_TOKEN, Item::Absent),
                (SNAPSHOT_DEVICE, Item::Absent),
                (SNAPSHOT_CLOCK, Item::Absent),
                (SNAPSHOT_CLOCK_STATE, Item::Absent),
            ]),
            Err(_) => snapshot_of([
                (SNAPSHOT_TOKEN, Item::Failed),
                (SNAPSHOT_DEVICE, Item::Failed),
                (SNAPSHOT_CLOCK, Item::Failed),
                (SNAPSHOT_CLOCK_STATE, Item::Failed),
            ]),
        }
    }

    /// Answer an engine's handoff request for the snapshot: a Reply with the request's id, or an
    /// Error with its id and a typed code. Only a snapshot-mode request for exactly the three
    /// names of [`crate::SNAPSHOT_NAMES`] (answered with [`engine_snapshot`](Self::engine_snapshot))
    /// or exactly the four of [`crate::SNAPSHOT_NAMES_WITH_CLOCK`] (answered with
    /// [`engine_snapshot_with_clock`](Self::engine_snapshot_with_clock)) is answered; any other name
    /// is `NotAllowed` and any other shape (strict mode, a missing or repeated name, a message that is not a request) is
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
            .find(|name| !SNAPSHOT_NAMES_WITH_CLOCK.contains(&name.as_str()))
        {
            return Message::error(*id, ErrorCode::NotAllowed, Some(stranger.as_str()));
        }
        let asks_for = |wanted: &[&str]| {
            names.len() == wanted.len()
                && wanted
                    .iter()
                    .all(|wanted| names.iter().any(|name| name == wanted))
        };
        if *mode != Mode::Snapshot {
            return Message::error(*id, ErrorCode::Invalid, None);
        }
        if asks_for(&SNAPSHOT_NAMES) {
            Message::reply_snapshot(*id, self.engine_snapshot())
        } else if asks_for(&SNAPSHOT_NAMES_WITH_CLOCK) {
            Message::reply_snapshot(*id, self.engine_snapshot_with_clock())
        } else {
            Message::error(*id, ErrorCode::Invalid, None)
        }
    }
}
