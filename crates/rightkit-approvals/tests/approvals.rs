use rightkit_approvals::approval_sig::{self, Signed};
use rightkit_approvals::redact::redact;
use rightkit_approvals::{Refusal, audit};
use serde_json::json;

#[test]
fn redact_masks_a_token_assembled_at_run_time() {
    let token = format!("gh{}{}", "p_", "a".repeat(36));
    let (clean, count) = redact(&format!("key {token} end"));
    assert_eq!(count, 1);
    assert_eq!(clean, "key [redacted] end");
    let (same, none) = redact("nothing secret here");
    assert_eq!((same.as_str(), none), ("nothing secret here", 0));
}

#[test]
fn redact_masks_a_password_assignment() {
    let (clean, count) = redact("password=hunter2 next");
    assert_eq!(count, 1);
    assert!(!clean.contains("hunter2"));
}

#[test]
fn the_signed_message_has_the_fixed_layout() {
    let bytes = approval_sig::message(
        "approve", "r1", "a1", "chat:x", "dev", "key", "nonce", 10, 20,
    );
    let text = String::from_utf8(bytes).expect("utf-8");
    assert!(text.starts_with("pulse.approval.v1\nkind:approve\nrequest:r1\n"));
    assert!(text.ends_with("issued:10\nexpires:20"));
}

#[test]
fn a_malformed_signature_is_refused_with_a_code() {
    let err: Refusal = match Signed::from_value(&json!({"keyId": "k"})) {
        Ok(_) => panic!("a signature without fields must be refused"),
        Err(err) => err,
    };
    assert_eq!(err.code, "approval_signature_invalid");
    assert_eq!(err.to_json()["code"], "approval_signature_invalid");
}

#[test]
fn endorse_action_depends_on_every_line() {
    let a = approval_sig::endorse_action(&["one".to_string(), "two".to_string()]);
    let b = approval_sig::endorse_action(&["one".to_string(), "three".to_string()]);
    assert_eq!(a.len(), 64);
    assert_ne!(a, b);
}

#[test]
fn audit_helpers_clean_what_they_return() {
    assert_eq!(audit::client("bad name!"), "badname");
    assert_eq!(audit::client("!!"), "unknown");
    assert_eq!(audit::extension("a/b/File.TXT"), "txt");
    assert_eq!(audit::extension("noext"), "");
    assert_eq!(audit::short_id("abcdef123456-xyz"), "abcdef12");
}

mod admin {
    use rightkit_approvals::admin_sig;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

    #[test]
    fn a_phone_signature_verifies_only_over_the_bytes_it_signed() {
        let rng = SystemRandom::new();
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).expect("a key");
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .expect("a key pair");
        let public = pair.public_key().as_ref().to_vec();
        let message = admin_sig::approval_message("comp", "phone", "req", "hash", "nonce", 99);
        let signature = pair.sign(&rng, &message).expect("sign");
        assert!(admin_sig::verify(&public, &message, signature.as_ref()));
        let other = admin_sig::approval_message("comp", "phone", "req2", "hash", "nonce", 99);
        assert!(!admin_sig::verify(&public, &other, signature.as_ref()));
        assert_eq!(admin_sig::key_id(&public).len(), 32);
        assert_eq!(admin_sig::fingerprint("abcdefgh"), "abcd efgh");
    }

    #[test]
    fn public_keys_are_65_byte_points() {
        let good = format!("04{}", "ab".repeat(64));
        assert!(admin_sig::parse_public(&good).is_some());
        assert!(admin_sig::parse_public("04ab").is_none());
        assert!(admin_sig::parse_public(&format!("05{}", "ab".repeat(64))).is_none());
    }
}
