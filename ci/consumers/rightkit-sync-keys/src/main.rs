// rightkit-sync-keys 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves the in-memory half of the public API: the account data key (`DataKey::generate`, its version and
// key id), `KeyId::parse` and its refusals, per-zone keys (`DataKey::zone_key`: deterministic, distinct per
// app, zone and data key, and carrying the data key's id and version), `account_scope` (deterministic,
// 64 lower-case hex characters, changes with every input), and `StaticKeys` as a `KeyProvider`
// (`current_key` is the highest version; `key_by_id` and `key_by_version` find a held key and answer
// `None` for one not held).
// Does not prove: `KeyManager` and `ZoneKeys` (they keep the key in the OS credential store, which a
// registry consumer must not write), the device-approval transfer, wipe on revoke, or the `sync` feature.
// State: in-memory values only. No network, no files, no process spawn, no user data.
use rightkit_sync_keys::{account_scope, DataKey, KeyId, KeyProvider, StaticKeys};

fn is_lower_hex(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn data_keys() {
    let first = DataKey::generate(1).expect("generate version 1");
    assert_eq!(first.version(), 1, "the version is the one asked for");
    assert!(DataKey::generate(0).is_err(), "version 0 is refused");
    let id = first.key_id().as_str().to_string();
    assert!(
        id.len() == 17 && id.starts_with('k') && is_lower_hex(&id[1..]),
        "a key id is k and 16 lower-case hex characters"
    );
    let second = DataKey::generate(1).expect("generate again");
    assert_ne!(
        first.key_id(),
        second.key_id(),
        "two random keys have different ids"
    );
}

fn key_ids() {
    let parsed = KeyId::parse("k0123456789abcdef").expect("a well-formed id parses");
    assert_eq!(parsed.as_str(), "k0123456789abcdef");
    assert_eq!(parsed.to_string(), "k0123456789abcdef");
    for bad in [
        "",
        "k",
        "0123456789abcdef0",
        "k0123456789abcde",
        "k0123456789abcdeff",
        "K0123456789abcdef",
        "k0123456789ABCDEF",
        "k0123456789abcdeg",
    ] {
        assert!(KeyId::parse(bad).is_err(), "refused: {bad:?}");
    }
}

fn zone_keys() {
    let key = DataKey::generate(3).expect("generate");
    let a = key.zone_key("app.one", "zone-a").expect("zone key");
    let again = key.zone_key("app.one", "zone-a").expect("zone key again");
    assert_eq!(
        a.as_bytes(),
        again.as_bytes(),
        "the same inputs give the same key"
    );
    assert_eq!(a.version(), 3, "a zone key carries the data key's version");
    assert_eq!(a.key_id(), key.key_id(), "and the data key's id");
    let other_zone = key.zone_key("app.one", "zone-b").expect("other zone");
    assert_ne!(
        a.as_bytes(),
        other_zone.as_bytes(),
        "another zone gives another key"
    );
    let other_app = key.zone_key("app.two", "zone-a").expect("other app");
    assert_ne!(
        a.as_bytes(),
        other_app.as_bytes(),
        "another app gives another key"
    );
    let other_key = DataKey::generate(3).expect("generate");
    let theirs = other_key.zone_key("app.one", "zone-a").expect("zone key");
    assert_ne!(
        a.as_bytes(),
        theirs.as_bytes(),
        "another data key gives another key"
    );
    assert_ne!(
        a.as_bytes(),
        key.expose_bytes(),
        "a zone key is not the data key"
    );
}

fn scopes() {
    let id = KeyId::parse("k0123456789abcdef").expect("id");
    let other = KeyId::parse("k0123456789abcdee").expect("id");
    let base = account_scope("app.one", "zone-a", "account-1", &id).expect("scope");
    assert_eq!(base.len(), 64, "a scope is a SHA-256 in hex");
    assert!(is_lower_hex(&base), "lower-case hex");
    assert_eq!(
        base,
        account_scope("app.one", "zone-a", "account-1", &id).expect("scope again"),
        "deterministic"
    );
    for changed in [
        account_scope("app.two", "zone-a", "account-1", &id),
        account_scope("app.one", "zone-b", "account-1", &id),
        account_scope("app.one", "zone-a", "account-2", &id),
        account_scope("app.one", "zone-a", "account-1", &other),
    ] {
        assert_ne!(
            base,
            changed.expect("scope"),
            "every input changes the scope"
        );
    }
}

fn static_keys() {
    let one = DataKey::generate(1).expect("v1");
    let two = DataKey::generate(2).expect("v2");
    let id_one = one.key_id().clone();
    let id_two = two.key_id().clone();
    let keys = StaticKeys::new(vec![one, two], "app.one", "zone-a").expect("static keys");
    let current = keys.current_key().expect("current");
    assert_eq!(current.version(), 2, "the highest version is current");
    assert_eq!(current.key_id(), &id_two);
    let by_id = keys.key_by_id(&id_one).expect("lookup").expect("held");
    assert_eq!(by_id.version(), 1, "an older key is found by its id");
    let by_version = keys.key_by_version(2).expect("lookup").expect("held");
    assert_eq!(by_version.key_id(), &id_two, "and by its version");
    assert!(
        keys.key_by_version(9).expect("lookup").is_none(),
        "an unheld version is None"
    );
    let unheld = KeyId::parse("k0123456789abcdef").expect("id");
    assert!(
        keys.key_by_id(&unheld).expect("lookup").is_none(),
        "an unheld id is None"
    );
    assert!(
        StaticKeys::new(Vec::new(), "app.one", "zone-a")
            .expect("empty set")
            .current_key()
            .is_err(),
        "no key means not enrolled"
    );
}

fn main() {
    data_keys();
    key_ids();
    zone_keys();
    scopes();
    static_keys();
    println!("rightkit-sync-keys registry consumer: ok");
}
