//! End to end through the public rules: an approval binds one exact effect, and the re-check at
//! execution refuses a changed draft (and voids the approval), an expired or revoked approval, an
//! approver that is not the owner (a guest or a device that is not owner-paired), and a second use.

use rightkit_approvals::authority::{begin, recheck};
use rightkit_approvals::rules::{self, Decision, Limits, Outcome};
use rightkit_approvals::{
    Approver, Authority, Binding, Error, Grant, MemoryStore, Origin, Record, Refusal, Request,
    Retention, State, Store, now_ms,
};
use serde_json::json;

/// One fixed instant near the real clock: `MemoryStore::get` sweeps by the real clock, so the
/// records must live in real time.
fn t0() -> u64 {
    static T0: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *T0.get_or_init(now_ms)
}

struct Fixed(Result<Approver, Refusal>);

impl Authority for Fixed {
    fn approver(&self, _record: &Record) -> Result<Approver, Refusal> {
        self.0.clone()
    }
}

fn owner() -> Fixed {
    Fixed(Ok(Approver::OwnerKey {
        key_id: "k1".into(),
        device: "mac-owner-check".into(),
    }))
}

fn store() -> MemoryStore {
    MemoryStore::new(Retention {
        max_pending: 16,
        max_decided: 16,
        max_done: 16,
        closed_visible_ms: 60_000,
    })
}

fn send(id: &str, body: &str, revision: u64) -> Request {
    Request {
        id: id.into(),
        kind: "send".into(),
        scope: "mail".into(),
        origin: Origin {
            door: "phone".into(),
            filer: "agent".into(),
            client: "chat:1".into(),
        },
        binding: Binding::of_effect(
            &json!({"to": ["a@example.com"], "subject": "Hi", "body": body}),
            "a@example.com",
            t0() + 60_000,
            revision,
        ),
        summary: "Send a reply".into(),
        detail: json!(null),
        created_ms: t0(),
        expires_ms: t0() + 60_000,
    }
}

fn approved(store: &MemoryStore, request: Request) -> String {
    let limits = Limits {
        pending_cap: 8,
        per_client: false,
    };
    let id = request.id.clone();
    rules::file(store, request, &limits, t0()).unwrap();
    let decision = Decision {
        approve: true,
        shown: None,
        domain: "test",
        proof: None,
        grant: Grant::Once { valid_ms: 60_000 },
        reason: Some("user"),
    };
    rules::decide(store, &id, &decision, t0()).unwrap().unwrap();
    id
}

#[test]
fn exact_effect_runs_once_and_a_changed_draft_voids_the_approval() {
    let store = store();
    let id = approved(&store, send("r1", "Yes, the 15th works.", 1));

    // A different body is a different digest: refused, and the approval is void for good.
    let edited = send("r1", "Yes, the 16th works.", 1);
    assert_eq!(
        recheck(&store, &owner(), &id, &edited, t0() + 1).unwrap_err(),
        Error::Changed
    );
    let again = send("r1", "Yes, the 15th works.", 1);
    assert_eq!(
        recheck(&store, &owner(), &id, &again, t0() + 2).unwrap_err(),
        Error::Denied
    );
    assert_eq!(store.get(&id).unwrap().unwrap().state, State::Denied);
}

#[test]
fn the_exact_effect_passes_once_then_is_used() {
    let store = store();
    let id = approved(&store, send("r2", "Yes.", 3));
    let same = send("r2", "Yes.", 3);

    let began = begin(&store, &owner(), &id, &same, t0() + 1).unwrap();
    assert_eq!(began.record.state, State::Claimed);
    rules::settle(&store, &id, Outcome::Done, Some("gmail-id-1"), t0() + 2).unwrap();

    assert_eq!(
        begin(&store, &owner(), &id, &same, t0() + 3).unwrap_err(),
        Error::Used
    );
}

#[test]
fn a_changed_destination_or_revision_also_voids() {
    let store = store();
    let id = approved(&store, send("r3", "Yes.", 1));
    let mut moved = send("r3", "Yes.", 1);
    moved.binding.destination = "b@example.com".into();
    assert_eq!(
        recheck(&store, &owner(), &id, &moved, t0() + 1).unwrap_err(),
        Error::Changed
    );

    let id = approved(&store, send("r4", "Yes.", 1));
    let newer = send("r4", "Yes.", 2);
    assert_eq!(
        recheck(&store, &owner(), &id, &newer, t0() + 1).unwrap_err(),
        Error::Changed
    );
}

#[test]
fn expired_and_revoked_approvals_are_refused() {
    let store = store();
    let id = approved(&store, send("r5", "Yes.", 1));
    let same = send("r5", "Yes.", 1);
    assert_eq!(
        recheck(&store, &owner(), &id, &same, t0() + 60_001).unwrap_err(),
        Error::Expired
    );

    let id = approved(&store, send("r6", "Yes.", 1));
    rules::revoke(&store, &id, "revoked", t0() + 1).unwrap().unwrap();
    assert_eq!(
        recheck(&store, &owner(), &id, &send("r6", "Yes.", 1), t0() + 2).unwrap_err(),
        Error::Denied
    );
}

#[test]
fn only_the_owner_approves() {
    let store = store();
    let same = send("r7", "Yes.", 1);

    let id = approved(&store, same.clone());
    let guest = Fixed(Ok(Approver::PairedDevice {
        device_id: "cloud-1".into(),
        owner_paired: true,
        guest: true,
        active: true,
    }));
    assert_eq!(
        recheck(&store, &guest, &id, &same, t0() + 1)
            .unwrap_err()
            .refusal()
            .code,
        "approver_not_owner"
    );

    let unpaired = Fixed(Ok(Approver::PairedDevice {
        device_id: "phone-9".into(),
        owner_paired: false,
        guest: false,
        active: true,
    }));
    assert_eq!(
        recheck(&store, &unpaired, &id, &same, t0() + 1)
            .unwrap_err()
            .refusal()
            .code,
        "approver_not_owner"
    );

    let revoked_key = Fixed(Err(Refusal::new("approver_revoked", "key revoked")));
    assert_eq!(
        recheck(&store, &revoked_key, &id, &same, t0() + 1)
            .unwrap_err()
            .refusal()
            .code,
        "approver_revoked"
    );

    let paired = Fixed(Ok(Approver::PairedDevice {
        device_id: "phone-1".into(),
        owner_paired: true,
        guest: false,
        active: true,
    }));
    assert!(recheck(&store, &paired, &id, &same, t0() + 1).is_ok());
}

#[test]
fn a_session_grant_never_approves_one_exact_effect() {
    let store = store();
    let request = send("r8", "Yes.", 1);
    let id = request.id.clone();
    let limits = Limits {
        pending_cap: 8,
        per_client: false,
    };
    rules::file(&store, request.clone(), &limits, t0()).unwrap();
    let decision = Decision {
        approve: true,
        shown: None,
        domain: "test",
        proof: None,
        grant: Grant::Open,
        reason: Some("user"),
    };
    rules::decide(&store, &id, &decision, t0()).unwrap().unwrap();
    assert_eq!(
        recheck(&store, &owner(), &id, &request, t0() + 1).unwrap_err(),
        Error::Malformed
    );
}
