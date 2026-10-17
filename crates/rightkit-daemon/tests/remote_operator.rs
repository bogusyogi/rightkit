//! What an operator screen needs from the daemon (issue #306): the device listing, device roles
//! taken from the signing device's enrolment, and the event cursor. In-process, manual clock.

mod common;

use common::*;
use rightkit_daemon::contract::{role_scope, Caller};
use std::time::Duration;

#[test]
fn the_device_listing_reports_every_device_without_its_key() {
    let h = Harness::new();
    let first = h.enrol_scopes(&["venture", "role:operator"]);
    h.advance(5);
    let second = h.enrol_scopes(&["channel"]);
    h.advance(5);
    let third = h.enrol_scopes(&["account", "role:phone", "role:operator"]);
    h.daemon.revoke_device(&second.id).unwrap();

    let list = h.daemon.list_devices();
    assert_eq!(list.len(), 3);
    // Oldest enrolment first.
    assert_eq!(
        list.iter().map(|d| d.device_id.as_str()).collect::<Vec<_>>(),
        vec![first.id.as_str(), second.id.as_str(), third.id.as_str()]
    );
    assert_eq!(list[0].role.as_deref(), Some("operator"));
    assert_eq!(list[0].enrolled_at, T0);
    assert!(!list[0].revoked);
    // Revoked devices stay in the listing, marked.
    assert!(list[1].revoked);
    assert_eq!(list[1].role, None);
    // Two role scopes: no role, never an arbitrary pick.
    assert_eq!(list[2].role, None);
    assert!(list.iter().all(|d| !d.linked));
    // No public key in anything a screen can read.
    let rendered = format!("{list:?}");
    assert!(!rendered.contains(&first.public_key_b64()));
}

#[test]
fn a_forgotten_device_leaves_the_listing() {
    let h = Harness::new();
    let dev = h.enrol();
    h.daemon.revoke_device(&dev.id).unwrap();
    assert_eq!(h.daemon.list_devices().len(), 1);
    h.daemon.forget_device(&dev.id).unwrap();
    assert!(h.daemon.list_devices().is_empty());
}

#[test]
fn a_cursor_reads_lifecycle_events_in_order() {
    let h = Harness::new();
    let first = h.daemon.events_since(None, 0, 100);
    let before = first.events.len();
    assert_eq!(first.next as usize, before);

    let dev = h.enrol();
    h.daemon.revoke_device(&dev.id).unwrap();
    let page = h.daemon.events_since(Some(&first.epoch), first.next, 100);
    assert!(!page.reset);
    let names: Vec<&str> = page
        .events
        .iter()
        .filter_map(|e| e["event"].as_str())
        .collect();
    assert!(names.contains(&"ENROLLED"), "{names:?}");
    assert!(names.contains(&"DEVICE_REVOKED"), "{names:?}");
    let seqs: Vec<u64> = page.events.iter().filter_map(|e| e["seq"].as_u64()).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");

    // Nothing newer: an empty page that keeps the cursor.
    let quiet = h.daemon.events_since(Some(&page.epoch), page.next, 100);
    assert!(quiet.events.is_empty());
    assert_eq!(quiet.next, page.next);

    // A cursor from another run is reset, not silently empty.
    let foreign = h.daemon.events_since(Some("another-run"), page.next, 100);
    assert!(foreign.reset);
}

#[test]
fn waiting_for_events_returns_when_one_is_written() {
    let h = Harness::new();
    let start = h.daemon.events_since(None, 0, 100);
    let daemon = h.daemon.clone();
    let epoch = start.epoch.clone();
    let waiter = std::thread::spawn(move || {
        daemon.wait_events(Some(&epoch), start.next, 100, Duration::from_secs(20))
    });
    std::thread::sleep(Duration::from_millis(100));
    h.daemon.emit("OPERATOR_PING", serde_json::json!({}));
    let page = waiter.join().unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0]["event"], "OPERATOR_PING");
}

#[test]
fn a_device_caller_has_the_role_its_enrolment_granted() {
    let operator = Caller::device("dev_1", ["venture".to_string(), role_scope("operator")]);
    assert_eq!(operator.role(), Some("operator"));
    let none = Caller::device("dev_2", ["venture"]);
    assert_eq!(none.role(), None);
    let two = Caller::device("dev_3", [role_scope("operator"), role_scope("phone")]);
    assert_eq!(two.role(), None);
    let empty = Caller::device("dev_4", ["role:"]);
    assert_eq!(empty.role(), None);
}
