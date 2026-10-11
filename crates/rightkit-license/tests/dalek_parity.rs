//! Cross-version parity vectors for the Ed25519 / base64 dependencies.
//!
//! `cases()` builds a deterministic set of token and lease-envelope inputs that probe the
//! behaviours which differ between Ed25519 implementations and base64 decoders:
//! non-canonical `S`, `S + L`, high bits, invalid and small-order `R`, small-order,
//! non-canonical and off-curve public keys, wrong-length signatures, base64 padding,
//! non-zero trailing bits, wrong alphabet and whitespace.
//!
//! `test-vectors/dalek-parity-v1.json` records the outcome of every case. It was recorded
//! from the 0.1.3-era dependency set (ed25519-dalek 2.2.0, base64 0.22.1) by the ignored
//! `record_dalek_parity_vectors` test BEFORE the move to ed25519-dalek 3 and base64 0.23,
//! and `recorded_outcomes_are_unchanged` asserts it. Re-record only deliberately, and say
//! why in the CHANGELOG: a changed outcome is a change in what the crate accepts.

mod common;

use base64::Engine;
use common::{signing_key, Claims, APP, DEVICE};
use ed25519_dalek::Signer;
use rightkit_license::{verify_lease_envelope, verify_token};
use serde_json::{json, Value};

const NOW: i64 = 2_000;

/// The Ed25519 group order L, little endian.
const L: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
];

const URL_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// (label, 32-byte encoding as hex)
const PUBKEYS: [(&str, &str); 14] = [
    (
        "identity",
        "0100000000000000000000000000000000000000000000000000000000000000",
    ),
    (
        "minus_one",
        "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    ),
    (
        "y_zero",
        "0000000000000000000000000000000000000000000000000000000000000000",
    ),
    (
        "y_zero_sign",
        "0000000000000000000000000000000000000000000000000000000000000080",
    ),
    (
        "order8_a",
        "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
    ),
    (
        "order8_a_sign",
        "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85",
    ),
    (
        "order8_b",
        "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
    ),
    (
        "order8_b_sign",
        "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa",
    ),
    (
        "noncanonical_y_p",
        "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    ),
    (
        "noncanonical_identity",
        "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    ),
    (
        "identity_negative_zero_x",
        "0100000000000000000000000000000000000000000000000000000000000080",
    ),
    (
        "off_curve_2",
        "0200000000000000000000000000000000000000000000000000000000000000",
    ),
    (
        "off_curve_7",
        "0700000000000000000000000000000000000000000000000000000000000000",
    ),
    (
        "off_curve_8",
        "0800000000000000000000000000000000000000000000000000000000000000",
    ),
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> [u8; 32] {
    let mut out = [0_u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).expect("hex");
    }
    out
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `s + k * L` over 256 bits (little endian, wrapping).
fn add_l(s: &[u8], times: u32) -> [u8; 32] {
    let mut out = [0_u8; 32];
    out.copy_from_slice(s);
    for _ in 0..times {
        let mut carry = 0_u16;
        for (index, byte) in out.iter_mut().enumerate() {
            let sum = u16::from(*byte) + u16::from(L[index]) + carry;
            *byte = (sum & 0xff) as u8;
            carry = sum >> 8;
        }
    }
    out
}

/// R = identity, S = 0: verifies for any message under the identity public key when
/// verification is cofactorless and does not reject weak keys.
fn zero_signature() -> Vec<u8> {
    let mut out = vec![0_u8; 64];
    out[..32].copy_from_slice(&unhex(PUBKEYS[0].1));
    out
}

fn with_s(sig: &[u8; 64], s: [u8; 32]) -> Vec<u8> {
    let mut out = sig.to_vec();
    out[32..].copy_from_slice(&s);
    out
}

fn with_r(sig: &[u8; 64], r: [u8; 32]) -> Vec<u8> {
    let mut out = sig.to_vec();
    out[..32].copy_from_slice(&r);
    out
}

/// Signature byte-level variants of one genuine signature.
fn signature_variants(sig: &[u8; 64]) -> Vec<(&'static str, Vec<u8>)> {
    let s: [u8; 32] = sig[32..].try_into().expect("32 bytes");
    let identity = unhex(PUBKEYS[0].1);
    let noncanonical_identity = unhex(PUBKEYS[9].1);
    let off_curve = unhex(PUBKEYS[11].1);
    let mut r_sign_flipped: [u8; 32] = sig[..32].try_into().expect("32 bytes");
    r_sign_flipped[31] ^= 0x80;
    let mut s_top_bit = s;
    s_top_bit[31] |= 0x80;
    let mut s_high_three = s;
    s_high_three[31] |= 0xe0;
    let mut s_one_flip = s;
    s_one_flip[0] ^= 1;
    let mut truncated = sig.to_vec();
    truncated.pop();
    let mut extended = sig.to_vec();
    extended.push(0);
    vec![
        ("genuine", sig.to_vec()),
        ("s_plus_l", with_s(sig, add_l(&s, 1))),
        ("s_plus_2l", with_s(sig, add_l(&s, 2))),
        ("s_equals_l", with_s(sig, L)),
        ("s_zero", with_s(sig, [0; 32])),
        ("s_top_bit", with_s(sig, s_top_bit)),
        ("s_high_three_bits", with_s(sig, s_high_three)),
        ("s_one_bit_flipped", with_s(sig, s_one_flip)),
        ("r_identity", with_r(sig, identity)),
        (
            "r_noncanonical_identity",
            with_r(sig, noncanonical_identity),
        ),
        ("r_off_curve", with_r(sig, off_curve)),
        ("r_sign_flipped", with_r(sig, r_sign_flipped)),
        ("zero_signature_identity_r", zero_signature()),
        ("truncated_63", truncated),
        ("extended_65", extended),
        ("empty_signature", Vec::new()),
    ]
}

/// A trailing-bits variant of a base64url string: same data bits, different padding bits.
fn tweak_trailing_bits(encoded: &str) -> Option<String> {
    let last = encoded.as_bytes().last().copied()?;
    let value = URL_ALPHABET.iter().position(|c| *c == last)? as u8;
    let unused_bits = match encoded.len() % 4 {
        2 => 4,
        3 => 2,
        _ => return None,
    };
    let mask = (1_u8 << unused_bits) - 1;
    let tweaked = if value & mask == mask {
        value & !mask
    } else {
        value | mask
    };
    let mut out = encoded[..encoded.len() - 1].to_string();
    out.push(URL_ALPHABET[usize::from(tweaked)] as char);
    Some(out)
}

pub struct Case {
    pub id: String,
    pub kind: &'static str,
    pub pubkey: [u8; 32],
    pub input: String,
}

fn token_payload() -> String {
    Claims::legacy(1_000, 9_999_999_999).payload()
}

const LEASE_FIELDS: &str = r#"{"leaseId":"lease_01HZX","deviceIdHash":"9f2c1a7be04d3f56","pool":"pro","issuedAt":"2026-10-01T00:00:00.000Z","expiresAt":"2026-11-01T00:00:00.000Z","graceSeconds":604800,"generation":3}"#;

fn lease_envelope(signature_b64: &str) -> String {
    let mut value: Value = serde_json::from_str(LEASE_FIELDS).expect("lease fields");
    value["entitlement"] = json!("ENTITLEMENT.NOT.COVERED");
    value["signature"] = json!(signature_b64);
    value.to_string()
}

pub fn cases() -> Vec<Case> {
    let key = signing_key();
    let real_pubkey = key.verifying_key().to_bytes();
    let payload = token_payload();
    let token_sig = key.sign(payload.as_bytes()).to_bytes();
    let lease_sig = key.sign(LEASE_FIELDS.as_bytes()).to_bytes();
    let payload_b64 = b64(payload.as_bytes());

    let token = |sig_bytes: &[u8]| format!("{payload_b64}.{}", b64(sig_bytes));
    let mut out = Vec::new();
    let mut push = |id: String, kind: &'static str, pubkey: [u8; 32], input: String| {
        out.push(Case {
            id,
            kind,
            pubkey,
            input,
        });
    };

    // Signature byte variants against the genuine key.
    for (label, bytes) in signature_variants(&token_sig) {
        push(
            format!("token/sig/{label}"),
            "token",
            real_pubkey,
            token(&bytes),
        );
    }
    for (label, bytes) in signature_variants(&lease_sig) {
        push(
            format!("lease/sig/{label}"),
            "lease",
            real_pubkey,
            lease_envelope(&b64(&bytes)),
        );
    }

    // Unusual public keys, with the genuine signature and with an all-zero-S identity-R one.
    let zero_sig = zero_signature();
    for (label, hex_key) in PUBKEYS {
        let pubkey = unhex(hex_key);
        push(
            format!("token/key/{label}/genuine_sig"),
            "token",
            pubkey,
            token(&token_sig),
        );
        push(
            format!("token/key/{label}/zero_sig"),
            "token",
            pubkey,
            token(&zero_sig),
        );
        push(
            format!("lease/key/{label}/genuine_sig"),
            "lease",
            pubkey,
            lease_envelope(&b64(&lease_sig)),
        );
        push(
            format!("lease/key/{label}/zero_sig"),
            "lease",
            pubkey,
            lease_envelope(&b64(&zero_sig)),
        );
    }

    // base64 decoding behaviour (token carries two encoded parts; lease carries one).
    let sig_b64 = b64(&token_sig);
    let mut token_encodings: Vec<(&str, String)> = vec![
        ("sig_padded", format!("{payload_b64}.{sig_b64}==")),
        ("sig_padded_single", format!("{payload_b64}.{sig_b64}=")),
        ("payload_padded", format!("{payload_b64}=.{sig_b64}")),
        (
            "sig_plus_char",
            format!("{payload_b64}.{}+{}", &sig_b64[..10], &sig_b64[11..]),
        ),
        (
            "sig_slash_char",
            format!("{payload_b64}.{}/{}", &sig_b64[..10], &sig_b64[11..]),
        ),
        (
            "sig_newline",
            format!("{payload_b64}.{}\n{}", &sig_b64[..10], &sig_b64[10..]),
        ),
        ("sig_leading_space", format!("{payload_b64}. {sig_b64}")),
        ("sig_trailing_space", format!("{payload_b64}.{sig_b64} ")),
        (
            "sig_invalid_char",
            format!("{payload_b64}.{}!{}", &sig_b64[..10], &sig_b64[11..]),
        ),
        (
            "sig_dropped_last_char",
            format!("{payload_b64}.{}", &sig_b64[..sig_b64.len() - 1]),
        ),
        ("extra_dot", format!("{payload_b64}.{sig_b64}.x")),
        ("empty_payload", format!(".{sig_b64}")),
        ("empty_sig", format!("{payload_b64}.")),
        ("no_dot", format!("{payload_b64}{sig_b64}")),
    ];
    if let Some(tweaked) = tweak_trailing_bits(&sig_b64) {
        token_encodings.push(("sig_trailing_bits", format!("{payload_b64}.{tweaked}")));
    }
    if let Some(tweaked) = tweak_trailing_bits(&payload_b64) {
        token_encodings.push(("payload_trailing_bits", format!("{tweaked}.{sig_b64}")));
    }
    for (label, input) in token_encodings {
        push(format!("token/b64/{label}"), "token", real_pubkey, input);
    }

    let mut lease_encodings: Vec<(&str, String)> = vec![
        ("sig_padded", lease_envelope(&format!("{sig_b64}=="))),
        (
            "sig_plus_char",
            lease_envelope(&format!("{}+{}", &sig_b64[..10], &sig_b64[11..])),
        ),
        (
            "sig_standard_alphabet_slash",
            lease_envelope(&format!("{}/{}", &sig_b64[..10], &sig_b64[11..])),
        ),
        (
            "sig_newline",
            lease_envelope(&format!("{}\n{}", &sig_b64[..10], &sig_b64[10..])),
        ),
        (
            "sig_surrounding_space",
            lease_envelope(&format!(" {sig_b64} ")),
        ),
        ("sig_blank", lease_envelope("   ")),
    ];
    let lease_sig_b64 = b64(&lease_sig);
    if let Some(tweaked) = tweak_trailing_bits(&lease_sig_b64) {
        lease_encodings.push(("sig_trailing_bits", lease_envelope(&tweaked)));
    }
    for (label, input) in lease_encodings {
        push(format!("lease/b64/{label}"), "lease", real_pubkey, input);
    }

    // Payload-level mutations that must stay rejected.
    push(
        "token/payload/tampered".to_string(),
        "token",
        real_pubkey,
        format!(
            "{}.{}",
            b64(payload.replace("pro", "plu").as_bytes()),
            b64(&token_sig)
        ),
    );
    out
}

/// The variant name of an error, or "Ok".
fn variant<T, E: std::fmt::Debug>(result: Result<T, E>) -> String {
    match result {
        Ok(_) => "Ok".to_string(),
        Err(error) => {
            let text = format!("{error:?}");
            let end = text.find(['(', ' ', '{']).unwrap_or(text.len());
            format!("Err:{}", &text[..end])
        }
    }
}

pub fn run_case(kind: &str, pubkey: &[u8; 32], input: &str) -> String {
    match kind {
        "token" => variant(verify_token(input, pubkey, APP, DEVICE, NOW)),
        "lease" => variant(verify_lease_envelope(input, pubkey)),
        other => panic!("unknown case kind {other}"),
    }
}

#[test]
#[ignore = "run once to record test-vectors/dalek-parity-v1.json; prints JSON to stdout"]
fn record_dalek_parity_vectors() {
    let recorded: Vec<Value> = cases()
        .iter()
        .map(|case| {
            json!({
                "id": case.id,
                "kind": case.kind,
                "pubkeyHex": hex(&case.pubkey),
                "input": case.input,
                "expected": run_case(case.kind, &case.pubkey, &case.input),
            })
        })
        .collect();
    let document = json!({
        "schemaVersion": 1,
        "recordedWith": { "ed25519-dalek": "2.2.0", "base64": "0.22.1" },
        "nowUnix": NOW,
        "app": APP,
        "deviceId": DEVICE,
        "cases": recorded,
    });
    println!(
        "DALEK-PARITY-BEGIN\n{}\nDALEK-PARITY-END",
        serde_json::to_string_pretty(&document).expect("json")
    );
}

fn recorded() -> Value {
    serde_json::from_str(include_str!("../test-vectors/dalek-parity-v1.json"))
        .expect("dalek parity vectors must be valid JSON")
}

/// The recorded file is the contract: replay its own inputs and demand its own outcomes.
#[test]
fn recorded_outcomes_are_unchanged() {
    let document = recorded();
    assert_eq!(document["nowUnix"], NOW);
    assert_eq!(document["app"], APP);
    assert_eq!(document["deviceId"], DEVICE);
    let recorded_cases = document["cases"].as_array().expect("cases array");
    assert!(recorded_cases.len() >= 100);
    let mut mismatches = Vec::new();
    for case in recorded_cases {
        let id = case["id"].as_str().expect("id");
        let kind = case["kind"].as_str().expect("kind");
        let pubkey = unhex(case["pubkeyHex"].as_str().expect("pubkeyHex"));
        let input = case["input"].as_str().expect("input");
        let expected = case["expected"].as_str().expect("expected");
        let actual = run_case(kind, &pubkey, input);
        if actual != expected {
            mismatches.push(format!("{id}: recorded {expected}, now {actual}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "verification outcomes changed:\n{}",
        mismatches.join("\n")
    );
}

/// The builder must still produce exactly the recorded inputs. Ed25519 signing is
/// deterministic, so this also proves the current dependency signs the same bytes as the
/// one the vectors were recorded with, and that no case was dropped or weakened.
#[test]
fn builder_reproduces_the_recorded_inputs() {
    let document = recorded();
    let recorded_cases = document["cases"].as_array().expect("cases array");
    let built = cases();
    assert_eq!(built.len(), recorded_cases.len(), "case count drifted");
    for (case, recorded_case) in built.iter().zip(recorded_cases) {
        assert_eq!(recorded_case["id"], case.id.as_str());
        assert_eq!(recorded_case["kind"], case.kind);
        assert_eq!(
            recorded_case["pubkeyHex"],
            hex(&case.pubkey).as_str(),
            "{}",
            case.id
        );
        assert_eq!(recorded_case["input"], case.input.as_str(), "{}", case.id);
    }
}

#[test]
fn case_ids_are_unique_and_the_set_is_substantial() {
    let all = cases();
    let mut ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "duplicate case ids");
    assert!(all.len() >= 100, "only {} cases", all.len());
}

#[test]
fn the_genuine_cases_verify() {
    for case in cases() {
        if case.id == "token/sig/genuine" || case.id == "lease/sig/genuine" {
            assert_eq!(
                run_case(case.kind, &case.pubkey, &case.input),
                "Ok",
                "{}",
                case.id
            );
        }
    }
}
