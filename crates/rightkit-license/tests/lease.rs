mod common;

use base64::Engine;
use ed25519_dalek::Signer;
use rightkit_license::{
    canonical_lease_envelope, verify_lease_envelope, verify_lease_envelope_fields, LeaseEnvelope,
    LeaseError,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LeaseVector {
    public_key_base64url: String,
    canonical: String,
    envelope: Value,
}

fn vector() -> (LeaseVector, [u8; 32]) {
    let vector: LeaseVector =
        serde_json::from_str(include_str!("../test-vectors/lease-envelope-v1.json"))
            .expect("lease vector must be valid JSON");
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&vector.public_key_base64url)
        .expect("public key must be base64url");
    let key: [u8; 32] = key.try_into().expect("public key must be 32 bytes");
    (vector, key)
}

fn with(envelope: &Value, field: &str, value: Value) -> String {
    let mut copy = envelope.clone();
    copy[field] = value;
    copy.to_string()
}

fn sample() -> LeaseEnvelope {
    LeaseEnvelope {
        lease_id: "lease_01HZX".into(),
        device_id_hash: "9f2c1a7be04d3f56".into(),
        pool: "pro".into(),
        issued_at: "2026-10-01T00:00:00.000Z".into(),
        expires_at: "2026-11-01T00:00:00.000Z".into(),
        grace_seconds: 604_800,
        generation: 3,
        unverified_entitlement: Some("ignored".into()),
    }
}

#[test]
fn canonical_bytes_are_the_seven_camel_case_fields_in_order() {
    let (vector, _) = vector();
    assert_eq!(
        canonical_lease_envelope(&sample()).unwrap(),
        vector.canonical
    );
    assert_eq!(
        vector.canonical,
        r#"{"leaseId":"lease_01HZX","deviceIdHash":"9f2c1a7be04d3f56","pool":"pro","issuedAt":"2026-10-01T00:00:00.000Z","expiresAt":"2026-11-01T00:00:00.000Z","graceSeconds":604800,"generation":3}"#
    );
}

#[test]
fn node_signed_vector_verifies_and_returns_the_signed_metadata() {
    let (vector, key) = vector();
    let envelope =
        verify_lease_envelope(&vector.envelope.to_string(), &key).expect("vector verifies");
    assert_eq!(
        envelope,
        sample().with_entitlement("ENTITLEMENT.TOKEN.NOT-COVERED-BY-ENVELOPE-SIGNATURE")
    );
}

trait WithEntitlement {
    fn with_entitlement(self, value: &str) -> Self;
}
impl WithEntitlement for LeaseEnvelope {
    fn with_entitlement(mut self, value: &str) -> Self {
        self.unverified_entitlement = Some(value.to_string());
        self
    }
}

#[test]
fn every_signed_field_is_covered() {
    let (vector, key) = vector();
    for (field, tampered) in [
        ("leaseId", json!("lease_other")),
        ("deviceIdHash", json!("0000")),
        ("pool", json!("plus")),
        ("issuedAt", json!("2026-10-02T00:00:00.000Z")),
        ("expiresAt", json!("2099-11-01T00:00:00.000Z")),
        ("graceSeconds", json!(604_801)),
        ("generation", json!(4)),
    ] {
        assert_eq!(
            verify_lease_envelope(&with(&vector.envelope, field, tampered), &key),
            Err(LeaseError::BadSignature),
            "{field}"
        );
    }
}

#[test]
fn entitlement_is_not_covered_by_the_envelope_signature() {
    // Documented limitation: the caller must verify the nested entitlement itself.
    let (vector, key) = vector();
    let swapped = with(&vector.envelope, "entitlement", json!("SOMETHING.ELSE"));
    let envelope = verify_lease_envelope(&swapped, &key).expect("still verifies");
    assert_eq!(
        envelope.unverified_entitlement.as_deref(),
        Some("SOMETHING.ELSE")
    );
    let absent = with(&vector.envelope, "entitlement", Value::Null);
    assert_eq!(
        verify_lease_envelope(&absent, &key)
            .unwrap()
            .unverified_entitlement,
        None
    );
}

#[test]
fn unknown_fields_are_ignored() {
    let (vector, key) = vector();
    let extra = with(&vector.envelope, "futureField", json!({"a": 1}));
    assert!(verify_lease_envelope(&extra, &key).is_ok());
}

#[test]
fn wrong_key_is_rejected() {
    let (vector, _) = vector();
    assert_eq!(
        verify_lease_envelope(&vector.envelope.to_string(), &common::other_pubkey()),
        Err(LeaseError::BadSignature)
    );
}

#[test]
fn signature_problems_fail_closed() {
    let (vector, key) = vector();
    let missing = {
        let mut copy = vector.envelope.clone();
        copy.as_object_mut().unwrap().remove("signature");
        copy.to_string()
    };
    assert_eq!(
        verify_lease_envelope(&missing, &key),
        Err(LeaseError::MissingSignature)
    );
    for blank in [json!(""), json!("   "), Value::Null] {
        assert_eq!(
            verify_lease_envelope(&with(&vector.envelope, "signature", blank), &key),
            Err(LeaseError::MissingSignature)
        );
    }
    assert_eq!(
        verify_lease_envelope(
            &with(&vector.envelope, "signature", json!("!!!not base64!!!")),
            &key
        ),
        Err(LeaseError::BadEncoding)
    );
    assert_eq!(
        verify_lease_envelope(&with(&vector.envelope, "signature", json!("AAAA")), &key),
        Err(LeaseError::MalformedSignature)
    );
}

#[test]
fn malformed_envelopes_are_parse_errors() {
    let (_, key) = vector();
    assert!(matches!(
        verify_lease_envelope("not json", &key),
        Err(LeaseError::Parse(_))
    ));
    assert!(matches!(
        verify_lease_envelope(r#"{"leaseId":"x"}"#, &key),
        Err(LeaseError::Parse(_))
    ));
}

#[test]
fn rust_signed_envelope_round_trips_with_a_test_key() {
    let envelope = sample();
    let canonical = canonical_lease_envelope(&envelope).unwrap();
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(common::signing_key().sign(canonical.as_bytes()).to_bytes());
    let wire = json!({
        "leaseId": envelope.lease_id,
        "deviceIdHash": envelope.device_id_hash,
        "pool": envelope.pool,
        "issuedAt": envelope.issued_at,
        "expiresAt": envelope.expires_at,
        "graceSeconds": envelope.grace_seconds,
        "generation": envelope.generation,
        "signature": signature,
    })
    .to_string();
    let verified = verify_lease_envelope(&wire, &common::pubkey()).unwrap();
    assert_eq!(verified.generation, 3);
    assert_eq!(verified.unverified_entitlement, None);
}

#[test]
fn json_string_escaping_matches_javascript_for_ascii_specials() {
    // Node's JSON.stringify and serde_json agree on these; a drift here would break
    // verification of any lease whose pool or id contains them.
    let envelope = LeaseEnvelope {
        pool: "a\"b\\c\nd\u{0001}e/é".into(),
        ..sample()
    };
    assert_eq!(
        canonical_lease_envelope(&envelope).unwrap(),
        "{\"leaseId\":\"lease_01HZX\",\"deviceIdHash\":\"9f2c1a7be04d3f56\",\"pool\":\"a\\\"b\\\\c\\nd\\u0001e/é\",\"issuedAt\":\"2026-10-01T00:00:00.000Z\",\"expiresAt\":\"2026-11-01T00:00:00.000Z\",\"graceSeconds\":604800,\"generation\":3}"
    );
}

#[test]
fn fields_entry_and_string_entry_verify_the_identical_signed_message() {
    // Sign the canonical bytes of one set of fields with a throwaway key, then present
    // the same fields through both entries.
    let envelope = sample();
    let canonical = canonical_lease_envelope(&envelope).unwrap();
    let key = common::signing_key();
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(key.sign(canonical.as_bytes()).to_bytes());
    let pubkey = common::pubkey();

    let as_json = json!({
        "leaseId": envelope.lease_id,
        "deviceIdHash": envelope.device_id_hash,
        "pool": envelope.pool,
        "entitlement": "ignored",
        "issuedAt": envelope.issued_at,
        "expiresAt": envelope.expires_at,
        "graceSeconds": envelope.grace_seconds,
        "generation": envelope.generation,
        "signature": signature,
    })
    .to_string();
    assert_eq!(
        verify_lease_envelope(&as_json, &pubkey).unwrap(),
        envelope,
        "string entry"
    );
    assert_eq!(
        verify_lease_envelope_fields(&envelope, &signature, &pubkey),
        Ok(()),
        "fields entry"
    );

    // Any change to a signed field is rejected identically by both.
    let mut tampered = envelope.clone();
    tampered.generation += 1;
    let mut tampered_json: Value = serde_json::from_str(&as_json).unwrap();
    tampered_json["generation"] = json!(tampered.generation);
    assert_eq!(
        verify_lease_envelope(&tampered_json.to_string(), &pubkey),
        Err(LeaseError::BadSignature)
    );
    assert_eq!(
        verify_lease_envelope_fields(&tampered, &signature, &pubkey),
        Err(LeaseError::BadSignature)
    );
    // The unsigned entitlement is ignored by the fields entry.
    let mut other_entitlement = envelope.clone();
    other_entitlement.unverified_entitlement = None;
    assert_eq!(
        verify_lease_envelope_fields(&other_entitlement, &signature, &pubkey),
        Ok(())
    );
}

#[test]
fn fields_entry_matches_the_node_signed_vector_and_fails_closed_like_the_string_entry() {
    let (vector, key) = vector();
    let signature = vector.envelope["signature"].as_str().unwrap().to_string();
    assert_eq!(
        verify_lease_envelope_fields(&sample(), &signature, &key),
        Ok(())
    );
    assert_eq!(
        verify_lease_envelope_fields(&sample(), "", &key),
        Err(LeaseError::MissingSignature)
    );
    assert_eq!(
        verify_lease_envelope_fields(&sample(), "!!!", &key),
        Err(LeaseError::BadEncoding)
    );
    assert_eq!(
        verify_lease_envelope_fields(&sample(), "AAAA", &key),
        Err(LeaseError::MalformedSignature)
    );
    assert_eq!(
        verify_lease_envelope_fields(&sample(), &signature, &common::other_pubkey()),
        Err(LeaseError::BadSignature)
    );
}
