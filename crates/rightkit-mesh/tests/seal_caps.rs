//! Pure-logic tests of the mesh: the end-to-end seal (round trip, tamper, wrong peer, padding) and
//! the relay caps. No network, no iroh endpoint.
use rightkit_mesh::caps::{check_small, month_key, RelayBudget, MB, SMALL_ITEM_MAX};
use rightkit_mesh::seal::{open, peek_seq, seal, PairKeys};

fn pair() -> (PairKeys, PairKeys, [u8; 32], [u8; 32]) {
    let (a, b) = ([1u8; 32], [2u8; 32]);
    let (ab, ba) = ([7u8; 32], [9u8; 32]);
    // a sends with ab, b opens with ab (its recv); and the reverse.
    let a_keys = PairKeys { peer: b, epoch: 1, send: ab, recv: ba };
    let b_keys = PairKeys { peer: a, epoch: 1, send: ba, recv: ab };
    (a_keys, b_keys, a, b)
}

#[test]
fn seal_round_trips_and_carries_the_sequence() {
    let (a_keys, b_keys, a, b) = pair();
    let record = [5u8; 16];
    let sealed = seal(&a_keys, &a, &record, 42, b"hello mesh").unwrap();
    assert_eq!(peek_seq(&sealed), Some(42));
    let (seq, plain) = open(&[b_keys], &a, &b, &record, &sealed).unwrap();
    assert_eq!(seq, 42);
    assert_eq!(plain, b"hello mesh");
}

#[test]
fn sealed_size_is_bucketed_not_exact() {
    let (a_keys, _, a, _) = pair();
    let short = seal(&a_keys, &a, &[0u8; 16], 1, b"x").unwrap();
    let longer = seal(&a_keys, &a, &[0u8; 16], 1, &[0u8; 100]).unwrap();
    assert_eq!(short.len(), longer.len(), "both pad to the smallest bucket");
}

#[test]
fn a_tampered_item_or_another_record_does_not_open() {
    let (a_keys, b_keys, a, b) = pair();
    let record = [5u8; 16];
    let mut sealed = seal(&a_keys, &a, &record, 3, b"secret").unwrap();
    assert!(open(&[b_keys.clone()], &a, &b, &[6u8; 16], &sealed).is_err(), "bound to its record id");
    let last = sealed.len() - 1;
    sealed[last] ^= 1;
    assert!(open(&[b_keys], &a, &b, &record, &sealed).is_err(), "tag check");
}

#[test]
fn the_wrong_peer_cannot_open() {
    let (a_keys, _, a, b) = pair();
    let stranger = PairKeys { peer: a, epoch: 1, send: [0u8; 32], recv: [3u8; 32] };
    let sealed = seal(&a_keys, &a, &[1u8; 16], 1, b"for b only").unwrap();
    assert!(open(&[stranger], &a, &b, &[1u8; 16], &sealed).is_err());
    assert!(open(&[], &a, &b, &[1u8; 16], &sealed).is_err());
    assert!(open(&[], &a, &b, &[1u8; 16], &[0u8; 4]).is_err(), "malformed");
}

#[test]
fn small_items_and_month_keys() {
    assert!(check_small(SMALL_ITEM_MAX).is_ok());
    assert!(check_small(SMALL_ITEM_MAX + 1).is_err());
    assert_eq!(month_key(0), "1970-01");
    assert_eq!(month_key(1_761_955_200_000), "2025-11");
}

#[test]
fn relay_budget_reserves_refunds_and_persists() {
    let dir = std::env::temp_dir().join(format!("rk-mesh-budget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("budget.json");
    let now = 1_761_955_200_000;
    let mut budget = RelayBudget::open(path.clone(), 10 * MB, now).unwrap();
    budget.reserve(now, 4 * MB).unwrap();
    assert_eq!(budget.remaining(now), 6 * MB);
    assert!(budget.reserve(now, 7 * MB).is_err(), "over the monthly limit");
    budget.refund(MB).unwrap();
    assert_eq!(budget.used(now), 3 * MB);
    let mut reopened = RelayBudget::open(path, 10 * MB, now).unwrap();
    assert_eq!(reopened.used(now), 3 * MB, "persisted across reopen");
    let _ = std::fs::remove_dir_all(&dir);
}
