// rightkit-crypto 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves every public item of the crate through its public API, against vectors that do not come from
// the crate: the FIPS 180-4 and RFC 4231 vectors, RFC 4648 base64, the BLAKE3 reference vectors, and
// digests, MACs and approval tokens computed independently with a second implementation (the golden
// values below were produced with a Python standard-library HMAC and hash, not by this crate).
// Covered: sha256 (padding boundaries and a multi-block message), hmac_sha256 (RFC 4231 cases 1 to 4, 6
// and 7, an empty key, keys at and over the block size), ct_eq, hex_encode, base64 and base64url encode
// and decode (RFC vectors, all byte values, canonical-form and alphabet refusals, DecodeError),
// blake3_text, canonical_json, and approval::mint / approval::verify with ApprovalError (the exact
// token wire form, every refusal, the half-open validity window, saturating expiry).
// Does not prove: constant-time behaviour of ct_eq (a timing property no assertion can show), the
// behaviour on a host with a different pointer width, or any use of the crate inside an app.
// State: in-memory values only. No network, no files, no process spawn, no clock, no user data.
use rightkit_crypto::approval::{self, ApprovalError};
use rightkit_crypto::{
    base64_decode, base64_encode, base64url_decode, base64url_encode, blake3_text, canonical_json,
    ct_eq, hex_encode, hmac_sha256, sha256, DecodeError,
};
use serde_json::{json, Value};

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&sha256(data))
}

fn hmac_hex(key: &[u8], msg: &[u8]) -> String {
    hex_encode(&hmac_sha256(key, msg))
}

fn sha256_vectors() {
    // FIPS 180-4 examples.
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "sha256 of abc"
    );
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "sha256 of the empty message"
    );
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        "sha256 of the 448-bit message"
    );
    assert_eq!(
        sha256_hex(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"),
        "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
        "sha256 of the 896-bit message"
    );
    // Many blocks: one million 'a'.
    assert_eq!(
        sha256_hex(&vec![b'a'; 1_000_000]),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        "sha256 of one million a"
    );
    // Padding boundaries: the length field spills into a second block at 56 bytes of tail.
    let boundaries: [(usize, &str); 9] = [
        (
            55,
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
        ),
        (
            56,
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
        ),
        (
            63,
            "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34",
        ),
        (
            64,
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
        ),
        (
            65,
            "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0",
        ),
        (
            119,
            "31eba51c313a5c08226adf18d4a359cfdfd8d2e816b13f4af952f7ea6584dcfb",
        ),
        (
            120,
            "2f3d335432c70b580af0e8e1b3674a7c020d683aa5f73aaaedfdc55af904c21c",
        ),
        (
            127,
            "c57e9278af78fa3cab38667bef4ce29d783787a2f731d4e12200270f0c32320a",
        ),
        (
            128,
            "6836cf13bac400e9105071cd6af47084dfacad4e5e302c94bfed24e013afb73e",
        ),
    ];
    for (len, expected) in boundaries {
        assert_eq!(
            sha256_hex(&vec![b'a'; len]),
            expected,
            "sha256 of {len} bytes of a"
        );
    }
    assert_eq!(sha256(b"abc").len(), 32, "a digest is 32 bytes");
}

fn hmac_vectors() {
    // RFC 4231 test cases 1, 2, 3, 4, 6 and 7 (case 5 is a truncated MAC, which this crate does not offer).
    assert_eq!(
        hmac_hex(&[0x0b; 20], b"Hi There"),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        "RFC 4231 case 1"
    );
    assert_eq!(
        hmac_hex(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
        "RFC 4231 case 2"
    );
    assert_eq!(
        hmac_hex(&[0xaa; 20], &[0xdd; 50]),
        "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
        "RFC 4231 case 3"
    );
    let key4: Vec<u8> = (1u8..=25).collect();
    assert_eq!(
        hmac_hex(&key4, &[0xcd; 50]),
        "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
        "RFC 4231 case 4"
    );
    assert_eq!(
        hmac_hex(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        ),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        "RFC 4231 case 6 (key longer than the block)"
    );
    assert_eq!(
        hmac_hex(
            &[0xaa; 131],
            b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm."
        ),
        "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
        "RFC 4231 case 7"
    );
    // Key edge cases from an independent implementation.
    assert_eq!(
        hmac_hex(b"", b""),
        "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad",
        "empty key and empty message"
    );
    assert_eq!(
        hmac_hex(&[b'k'; 64], b"msg"),
        "d62ad25bb128e96ab6ef43464aaf2bb91b5b85f013381e9ba6be747e8d0911b2",
        "a key exactly one block long is used as is"
    );
    assert_eq!(
        hmac_hex(&[b'k'; 65], b"msg"),
        "c65585780e821bd312cfaa440c33cb28d6af6b323d4977ae83934834ce589e38",
        "a key one byte over the block is hashed first"
    );
    assert_ne!(
        hmac_sha256(b"key-a", b"msg"),
        hmac_sha256(b"key-b", b"msg"),
        "the key changes the MAC"
    );
    assert_ne!(
        hmac_sha256(b"key", b"msg-a"),
        hmac_sha256(b"key", b"msg-b"),
        "the message changes the MAC"
    );
}

fn compare_and_hex() {
    assert!(ct_eq(b"abc", b"abc"), "equal slices compare equal");
    assert!(ct_eq(b"", b""), "two empty slices compare equal");
    assert!(!ct_eq(b"abc", b"abd"), "a last-byte difference is unequal");
    assert!(!ct_eq(b"abc", b"xbc"), "a first-byte difference is unequal");
    assert!(!ct_eq(b"abc", b"ab"), "a shorter right side is unequal");
    assert!(!ct_eq(b"ab", b"abc"), "a longer right side is unequal");
    assert!(!ct_eq(b"", b"a"), "empty against non-empty is unequal");
    let digest = sha256(b"payload");
    assert!(
        ct_eq(&digest, &sha256(b"payload")),
        "equal digests compare equal"
    );
    assert!(
        !ct_eq(&digest, &sha256(b"payloaD")),
        "different digests compare unequal"
    );

    assert_eq!(hex_encode(b""), "", "empty input encodes to empty hex");
    assert_eq!(
        hex_encode(&[0x00, 0xff, 0x0a, 0xa0]),
        "00ff0aa0",
        "hex is zero padded and lowercase"
    );
    let every: Vec<u8> = (0..=255u8).collect();
    let expected: String = every.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex_encode(&every), expected, "every byte value encodes");
    assert_eq!(hex_encode(&every).len(), 512, "two characters per byte");
}

fn base64_cases() {
    // RFC 4648 section 10.
    let cases = [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];
    for (plain, encoded) in cases {
        assert_eq!(base64_encode(plain.as_bytes()), encoded, "encode {plain:?}");
        assert_eq!(
            base64_decode(encoded).expect("decode RFC vector"),
            plain.as_bytes(),
            "decode {encoded:?}"
        );
        let unpadded = encoded.trim_end_matches('=');
        assert_eq!(
            base64url_encode(plain.as_bytes()),
            unpadded,
            "url encode {plain:?} has no padding"
        );
        assert_eq!(
            base64url_decode(unpadded).expect("decode url vector"),
            plain.as_bytes(),
            "url decode {unpadded:?}"
        );
    }
    // The two alphabets differ in exactly two characters.
    assert_eq!(base64_encode(&[0xfb, 0xff]), "+/8=", "standard alphabet");
    assert_eq!(base64url_encode(&[0xfb, 0xff]), "-_8", "url alphabet");
    assert_eq!(base64_decode("+/8=").unwrap(), vec![0xfb, 0xff]);
    assert_eq!(base64url_decode("-_8").unwrap(), vec![0xfb, 0xff]);
    assert!(
        base64_decode("-_8=").is_err(),
        "the standard decoder refuses the url alphabet"
    );
    assert!(
        base64url_decode("+/8").is_err(),
        "the url decoder refuses the standard alphabet"
    );

    // Round trip of every length from 0 to 96 over varied byte values, and of all 256 byte values.
    for len in 0..=96usize {
        let data: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8 ^ (len as u8)).collect();
        let std = base64_encode(&data);
        assert_eq!(
            std.len(),
            len.div_ceil(3) * 4,
            "padded length for {len} bytes"
        );
        assert_eq!(base64_decode(&std).unwrap(), data, "round trip {len} bytes");
        let url = base64url_encode(&data);
        assert!(
            !url.contains(['=', '+', '/']),
            "url form of {len} bytes has no padding or standard-only characters"
        );
        assert_eq!(
            base64url_decode(&url).unwrap(),
            data,
            "url round trip {len} bytes"
        );
    }
    let every: Vec<u8> = (0..=255u8).collect();
    assert_eq!(base64_decode(&base64_encode(&every)).unwrap(), every);
    assert_eq!(base64url_decode(&base64url_encode(&every)).unwrap(), every);

    // Refusals: bad length, bad padding, non-canonical trailing bits, characters outside the alphabet.
    for bad in [
        "Zg=", "Z", "Zm9", "Zm9v=", "Zh==", "Zm9=", "Z===", "====", "Zm9v====", "Zm9!", "Zm 9",
        "Zm9v\n",
    ] {
        assert!(
            base64_decode(bad).is_err(),
            "standard decode must refuse {bad:?}"
        );
    }
    for bad in [
        "Z", "Zm9v+", "Zh", "Zm9", "Zg==", "Zm+v", "Zm/v", "Zm9!", "Zm 9",
    ] {
        assert!(
            base64url_decode(bad).is_err(),
            "url decode must refuse {bad:?}"
        );
    }
    assert_eq!(
        base64url_decode("Zm8").expect("Zm8 is canonical unpadded input"),
        b"fo".to_vec(),
        "unused trailing bits must be zero, so Zm8 decodes and Zm9 does not"
    );
    assert_eq!(base64_decode("").unwrap(), Vec::<u8>::new());
    assert_eq!(base64url_decode("").unwrap(), Vec::<u8>::new());

    // DecodeError is a value type with a stable message and the standard error trait.
    let error: DecodeError = base64_decode("Zg=").unwrap_err();
    assert_eq!(error, DecodeError, "DecodeError carries no payload");
    assert_eq!(error.to_string(), "invalid base64", "DecodeError message");
    let as_dyn: &dyn std::error::Error = &error;
    assert_eq!(as_dyn.to_string(), "invalid base64");
    let copy = error;
    assert_eq!(format!("{copy:?}"), "DecodeError");
}

fn blake3_cases() {
    // BLAKE3 reference vectors for the inputs [], [0], [0, 1] and [0, 1, 2] (all valid UTF-8).
    assert_eq!(
        blake3_text(""),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    assert_eq!(
        blake3_text("\u{0}"),
        "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213"
    );
    assert_eq!(
        blake3_text("\u{0}\u{1}"),
        "7b7015bb92cf0b318037702a6cdd81dee41224f734684c2c122cd6359cb1ee63"
    );
    assert_eq!(
        blake3_text("\u{0}\u{1}\u{2}"),
        "e1be4d7a8ab5560aa4199eea339849ba8e293d55ca0a81006726d184519e647f"
    );
    // Shape and sensitivity over text that spans several 1024-byte chunks and non-ASCII characters.
    let long = "caf\u{e9} \u{2603} ".repeat(700);
    let digest = blake3_text(&long);
    assert_eq!(digest.len(), 64, "a hex digest of 32 bytes");
    assert!(
        digest
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "lowercase hex only"
    );
    assert_eq!(digest, blake3_text(&long), "the hash is deterministic");
    assert_ne!(
        digest,
        blake3_text(&format!("{long}x")),
        "one more character changes it"
    );
    assert_ne!(
        blake3_text("e"),
        blake3_text("\u{e9}"),
        "the UTF-8 bytes are hashed"
    );
    assert_ne!(
        blake3_text("abc"),
        sha256_hex(b"abc"),
        "BLAKE3 and SHA-256 are different functions"
    );
}

fn canonical_json_cases() {
    let parsed: Value =
        serde_json::from_str(r#"{"b":[1,{"z":null,"a":true}],"a":"x\ny"}"#).expect("parse fixture");
    assert_eq!(
        canonical_json(&parsed),
        r#"{"a":"x\ny","b":[1,{"a":true,"z":null}]}"#,
        "keys sorted at every depth, output compact"
    );
    // Byte order, not locale or code-point-folding order.
    let keyed = json!({"b": 4, "\u{e9}": 5, "a": 2, "B": 1, "aa": 3, "": 0});
    assert_eq!(
        canonical_json(&keyed),
        "{\"\":0,\"B\":1,\"a\":2,\"aa\":3,\"b\":4,\"\u{e9}\":5}",
        "object keys are ordered by byte value"
    );
    // The same value built in a different key order has the same canonical form.
    assert_eq!(
        canonical_json(&json!({"x": 1, "y": [true, false], "z": {"q": 1, "p": 2}})),
        canonical_json(&json!({"z": {"p": 2, "q": 1}, "y": [true, false], "x": 1})),
        "insertion order does not matter"
    );
    // Arrays keep their order; empty containers and scalars use compact forms.
    assert_eq!(canonical_json(&json!([3, 1, 2])), "[3,1,2]");
    assert_eq!(canonical_json(&json!({})), "{}");
    assert_eq!(canonical_json(&json!([])), "[]");
    assert_eq!(canonical_json(&json!(null)), "null");
    assert_eq!(canonical_json(&json!(true)), "true");
    assert_eq!(canonical_json(&json!(false)), "false");
    assert_eq!(canonical_json(&json!(0)), "0");
    assert_eq!(canonical_json(&json!(-17)), "-17");
    assert_eq!(canonical_json(&json!(10_000_000_000u64)), "10000000000");
    assert_eq!(canonical_json(&json!(1.5)), "1.5");
    assert_eq!(
        canonical_json(&json!([{"b": [], "a": {}}])),
        r#"[{"a":{},"b":[]}]"#
    );
    // String escapes use the serde_json forms; non-ASCII text is written as is.
    assert_eq!(
        canonical_json(&json!("a\"b\\c\td\u{1}\u{e9}")),
        "\"a\\\"b\\\\c\\td\\u0001\u{e9}\"",
        "string escaping"
    );
    // Canonical output parses back to a value with the same canonical form.
    let once = canonical_json(&parsed);
    let again: Value = serde_json::from_str(&once).expect("canonical output is JSON");
    assert_eq!(
        again, parsed,
        "canonical output parses back to the same value"
    );
    assert_eq!(
        canonical_json(&again),
        once,
        "canonicalisation is idempotent"
    );
}

fn approval_tokens() {
    let key: &[u8] = b"app-held-secret";

    // Exact wire form, computed independently: v1.<issued>.<expires>.<base64url HMAC-SHA256 of the
    // length-prefixed encoding>.
    let token = approval::mint(key, "publish", "post-1", 1000, 60);
    assert_eq!(
        token, "v1.1000.1060.FL-xHjDam8x6ZExATuH9pZO_ZZ0WBWqKhUFzmKES02c",
        "token wire form"
    );
    assert_eq!(
        approval::mint(key, "pub\u{e9}", "post \u{2603}", 1_700_000_000, 3600),
        "v1.1700000000.1700003600.Ua0J7m4JuNmGf6ZqH8RNS8R_srrGLXzPE-ZmZRf6VOs",
        "non-ASCII purpose and subject are length-prefixed as UTF-8 bytes"
    );
    assert_eq!(
        approval::mint(b"", "", "", 0, 0),
        "v1.0.0.f3lQMzBdzoMnX81wX0EYLmyp-7FP5egntpDJeJUfXdI",
        "empty key, purpose and subject"
    );
    assert_eq!(
        approval::mint(&[b'k'; 70], "ab", "c", 1, 10),
        "v1.1.11.y_DSK6gHo6jjVZWBg7MLzLSom3saku7c1eM_Um0wxNc",
        "a key longer than the HMAC block"
    );
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(segments.len(), 4, "four dot-separated segments");
    assert_eq!(segments[3].len(), 43, "an unpadded base64url 32-byte MAC");
    assert!(
        !segments[3].contains(['=', '+', '/']),
        "the MAC segment is url-safe and unpadded"
    );
    assert_eq!(
        base64url_decode(segments[3])
            .expect("MAC segment decodes")
            .len(),
        32,
        "the MAC is 32 bytes"
    );
    assert!(
        !token.contains("app-held-secret"),
        "the token never contains the key"
    );

    // Validity window is [issued, expires).
    assert_eq!(
        approval::verify(key, "publish", "post-1", &token, 1000),
        Ok(())
    );
    assert_eq!(
        approval::verify(key, "publish", "post-1", &token, 1059),
        Ok(())
    );
    assert_eq!(
        approval::verify(key, "publish", "post-1", &token, 1060),
        Err(ApprovalError::Expired),
        "valid until the second before expiry"
    );
    assert_eq!(
        approval::verify(key, "publish", "post-1", &token, u64::MAX),
        Err(ApprovalError::Expired)
    );
    assert_eq!(
        approval::verify(key, "publish", "post-1", &token, 999),
        Err(ApprovalError::NotYetValid)
    );
    assert_eq!(
        approval::verify(key, "publish", "post-1", &token, 0),
        Err(ApprovalError::NotYetValid)
    );

    // A zero lifetime is expired at once; a lifetime that overflows saturates at the largest second.
    let instant = approval::mint(key, "publish", "post-1", 500, 0);
    assert_eq!(
        approval::verify(key, "publish", "post-1", &instant, 500),
        Err(ApprovalError::Expired),
        "ttl 0 never validates"
    );
    let saturated = approval::mint(key, "publish", "post-1", u64::MAX - 5, 100);
    assert_eq!(
        saturated,
        "v1.18446744073709551610.18446744073709551615.9vy--WR2R2ch-tVX80YBfEyxoVJZ1Pds9mn8o4Ic9KU",
        "expiry saturates at u64::MAX"
    );
    assert_eq!(
        approval::verify(key, "publish", "post-1", &saturated, u64::MAX - 5),
        Ok(())
    );
    assert_eq!(
        approval::verify(key, "publish", "post-1", &saturated, u64::MAX - 1),
        Ok(())
    );
    assert_eq!(
        approval::verify(key, "publish", "post-1", &saturated, u64::MAX),
        Err(ApprovalError::Expired)
    );

    // Binding: purpose, subject and key are all part of the MAC.
    let other_key: &[u8] = b"other";
    for (k, purpose, subject) in [
        (key, "delete", "post-1"),
        (key, "publish", "post-2"),
        (key, "Publish", "post-1"),
        (other_key, "publish", "post-1"),
    ] {
        assert_eq!(
            approval::verify(k, purpose, subject, &token, 1001),
            Err(ApprovalError::BadSignature),
            "a changed binding must not verify: {purpose}/{subject}"
        );
    }
    let ab_c = approval::mint(key, "ab", "c", 1, 10);
    assert_eq!(
        approval::verify(key, "a", "bc", &ab_c, 2),
        Err(ApprovalError::BadSignature),
        "field boundaries are unambiguous"
    );
    assert_eq!(approval::verify(key, "ab", "c", &ab_c, 2), Ok(()));
    // Tampering with the times is a signature failure, not a window decision.
    let extended = token.replacen(".1060.", ".9999.", 1);
    assert_eq!(
        approval::verify(key, "publish", "post-1", &extended, 1001),
        Err(ApprovalError::BadSignature),
        "an extended expiry is refused"
    );
    let mac = segments[3];
    let flipped = format!(
        "v1.1000.1060.{}{}",
        &mac[..42],
        if mac.ends_with('A') { 'B' } else { 'A' }
    );
    assert_eq!(
        approval::verify(key, "publish", "post-1", &flipped, 1001),
        Err(ApprovalError::BadSignature),
        "a changed MAC is refused"
    );

    // Malformed tokens are refused before any key use, including non-canonical numbers.
    for malformed in [
        "",
        "v1",
        "v1.1.2",
        "v2.1.2.AA",
        "V1.1.2.AA",
        "v1.x.2.AA",
        "v1.1.y.AA",
        "v1.1.2.AA.AA",
        "v1.1.2.!!",
        "v1..1060.AA",
        "v1.1000..AA",
        "v1.-5.1060.AA",
        "v1.1000.-5.AA",
        "v1.01000.1060.AA",
        "v1.+1000.1060.AA",
        "v1.1000.+1060.AA",
        "v1.1000.01060.AA",
        "v1. 1000.1060.AA",
        "v1.18446744073709551616.18446744073709551617.AA",
    ] {
        assert_eq!(
            approval::verify(key, "publish", "post-1", malformed, 1),
            Err(ApprovalError::Malformed),
            "malformed token {malformed:?}"
        );
    }
    for noncanonical in ["v1.01000.1060.", "v1.+1000.1060.", "v1.1000.01060."] {
        let with_real_mac = format!("{noncanonical}{mac}");
        assert_eq!(
            approval::verify(key, "publish", "post-1", &with_real_mac, 1001),
            Err(ApprovalError::Malformed),
            "{with_real_mac}"
        );
    }

    // The error type: copyable value, stable messages, standard error trait, no key material.
    let expected = [
        (ApprovalError::Malformed, "malformed approval token"),
        (
            ApprovalError::BadSignature,
            "approval token signature mismatch",
        ),
        (ApprovalError::Expired, "approval token expired"),
        (ApprovalError::NotYetValid, "approval token not yet valid"),
    ];
    for (error, message) in expected {
        let copy = error;
        assert_eq!(copy, error);
        assert_eq!(error.to_string(), message);
        let as_dyn: &dyn std::error::Error = &error;
        assert_eq!(as_dyn.to_string(), message);
        let rendered = format!("{error} {error:?}");
        assert!(
            !rendered.contains("app-held-secret") && !rendered.contains(mac),
            "an error never carries key or MAC material"
        );
    }
}

fn main() {
    sha256_vectors();
    hmac_vectors();
    compare_and_hex();
    base64_cases();
    blake3_cases();
    canonical_json_cases();
    approval_tokens();
    println!("OK rightkit-crypto 0.1.0 (registry consumer)");
}
