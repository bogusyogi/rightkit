// rightkit-mesh 0.1.0 consumed by exact registry pin (no default features), outside the workspace.
//
// Proves the pure public surface a host relies on before any device connects: the end-to-end seal
// (round trip, the sender sequence in the clear header, bucketed padding, binding to the record id,
// tamper and wrong-key refusal, malformed input), the relay caps (small-item limit, month keys) and
// the persisted monthly relay budget (reserve, refuse over the limit, refund, reopen).
// Does not prove: iroh endpoints, live sessions, the mailbox server, sync or transfers between
// devices (they need two endpoints and a relay), and the macOS fast datapath (feature off).
// State: a per-process directory under the OS temp directory, removed at the end. No network.
use rightkit_mesh::caps::{check_small, month_key, RelayBudget, MB, SMALL_ITEM_MAX};
use rightkit_mesh::seal::{open, peek_seq, seal, PairKeys};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let (a, b) = ([1u8; 32], [2u8; 32]);
    let a_keys = PairKeys {
        peer: b,
        epoch: 1,
        send: [7u8; 32],
        recv: [9u8; 32],
    };
    let b_keys = PairKeys {
        peer: a,
        epoch: 1,
        send: [9u8; 32],
        recv: [7u8; 32],
    };
    let record = [5u8; 16];

    let sealed = seal(&a_keys, &a, &record, 42, b"hello mesh").expect("seal");
    check(
        peek_seq(&sealed) == Some(42),
        "the sender sequence is readable without opening",
    );
    let opened = open(&[b_keys.clone()], &a, &b, &record, &sealed);
    check(
        matches!(&opened, Ok((42, p)) if p == b"hello mesh"),
        "the paired device opens the item",
    );
    let short = seal(&a_keys, &a, &record, 1, b"x").expect("seal");
    let longer = seal(&a_keys, &a, &record, 1, &[0u8; 100]).expect("seal");
    check(short.len() == longer.len(), "sizes are padded to buckets");
    check(
        open(&[b_keys.clone()], &a, &b, &[6u8; 16], &sealed).is_err(),
        "an item is bound to its record id",
    );
    let mut tampered = sealed.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    check(
        open(&[b_keys.clone()], &a, &b, &record, &tampered).is_err(),
        "a tampered item does not open",
    );
    let stranger = PairKeys {
        peer: a,
        epoch: 1,
        send: [0u8; 32],
        recv: [3u8; 32],
    };
    check(
        open(&[stranger], &a, &b, &record, &sealed).is_err(),
        "another key does not open it",
    );
    check(
        open(&[], &a, &b, &record, &[0u8; 4]).is_err(),
        "malformed input is refused",
    );

    check(
        check_small(SMALL_ITEM_MAX).is_ok() && check_small(SMALL_ITEM_MAX + 1).is_err(),
        "the small-item limit",
    );
    check(
        month_key(0) == "1970-01" && month_key(1_761_955_200_000) == "2025-11",
        "month keys",
    );

    let dir = std::env::temp_dir().join(format!("rk-mesh-consumer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("budget.json");
    let now = 1_761_955_200_000;
    let mut budget = RelayBudget::open(path.clone(), 10 * MB, now).expect("budget");
    check(
        budget.reserve(now, 4 * MB).is_ok() && budget.remaining(now) == 6 * MB,
        "reserve within the limit",
    );
    check(
        budget.reserve(now, 7 * MB).is_err(),
        "a reservation over the monthly limit is refused",
    );
    check(
        budget.refund(MB).is_ok() && budget.used(now) == 3 * MB,
        "refund",
    );
    let mut reopened = RelayBudget::open(path, 10 * MB, now).expect("reopen");
    check(
        reopened.used(now) == 3 * MB,
        "the budget persists across reopen",
    );
    let _ = std::fs::remove_dir_all(&dir);
    println!("rightkit-mesh consumer: all checks passed");
}
