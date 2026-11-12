//! Racing moves out of one state on the real file store: exactly one wins, and what the store
//! reads back is the winner's move.
#![cfg(feature = "file")]

use std::sync::{Arc, Barrier};
use std::thread;

use rightkit_approvals::{
    Binding, Change, FileStore, Grant, Origin, Record, Request, State, Store,
};

const ROUNDS: usize = 64;

fn record(id: &str) -> Record {
    Record::pending(Request {
        id: id.to_string(),
        kind: "act".into(),
        scope: "tool".into(),
        origin: Origin {
            door: "local".into(),
            filer: String::new(),
            client: "agent".into(),
        },
        binding: Binding::new("args", "dest"),
        summary: "do the thing".into(),
        detail: serde_json::Value::Null,
        created_ms: 1,
        expires_ms: u64::MAX,
    })
}

/// Runs `a` and `b` as `from -> a.to` and `from -> b.to` at the same moment on a fresh record
/// (first moved to `from` through `setup`), and returns which won.
fn race(
    store: &FileStore,
    id: &str,
    setup: &[(State, Change)],
    from: State,
    a: Change,
    b: Change,
) -> (bool, bool) {
    store.insert(&record(id)).unwrap();
    for (state, change) in setup {
        assert!(store.transition(id, *state, change).unwrap());
    }
    let barrier = Arc::new(Barrier::new(2));
    let run = |change: Change| {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        let id = id.to_string();
        thread::spawn(move || {
            barrier.wait();
            store.transition(&id, from, &change).unwrap()
        })
    };
    let (ta, tb) = (run(a), run(b));
    (ta.join().unwrap(), tb.join().unwrap())
}

#[test]
fn approve_and_deny_never_both_win() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path());
    for round in 0..ROUNDS {
        let id = format!("ad{round}");
        let approve = Change::to(State::Approved, 10).grant(Grant::Once { valid_ms: 60_000 });
        let deny = Change::to(State::Denied, 10).reason("user");
        let (approved, denied) = race(&store, &id, &[], State::Pending, approve, deny);
        assert!(
            approved ^ denied,
            "round {round}: approve {approved}, deny {denied}"
        );
        let state = store.get(&id).unwrap().unwrap().state;
        assert_eq!(
            state,
            if approved {
                State::Approved
            } else {
                State::Denied
            }
        );
    }
}

#[test]
fn revoke_and_claim_never_both_win() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path());
    for round in 0..ROUNDS {
        let id = format!("rc{round}");
        let approve = Change::to(State::Approved, 10).grant(Grant::Once { valid_ms: 60_000 });
        let claim = Change::to(State::Claimed, 20);
        let revoke = Change::to(State::Denied, 20).reason("revoked");
        let (claimed, revoked) = race(
            &store,
            &id,
            &[(State::Pending, approve)],
            State::Approved,
            claim,
            revoke,
        );
        assert!(
            claimed ^ revoked,
            "round {round}: claim {claimed}, revoke {revoked}"
        );
        let state = store.get(&id).unwrap().unwrap().state;
        assert_eq!(
            state,
            if claimed {
                State::Claimed
            } else {
                State::Denied
            }
        );
    }
}

#[test]
fn a_taken_guard_without_its_marker_still_reads_as_the_move() {
    // A crash after the guard and before the descriptive marker: the move happened.
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path());
    store.insert(&record("g1")).unwrap();
    let path = store.path_of("act", "dest", "g1").unwrap();
    let approve = Change::to(State::Approved, 10).grant(Grant::Once { valid_ms: 60_000 });
    assert!(store.transition("g1", State::Pending, &approve).unwrap());
    let marker = path.with_file_name("dest.g1.json.approved");
    assert!(marker.exists());
    std::fs::remove_file(&marker).unwrap();
    assert_eq!(store.get("g1").unwrap().unwrap().state, State::Approved);
    // And nothing else leaves pending: the guard is taken.
    let deny = Change::to(State::Denied, 11).reason("user");
    assert!(!store.transition("g1", State::Pending, &deny).unwrap());
    assert_eq!(store.get("g1").unwrap().unwrap().state, State::Approved);
}
