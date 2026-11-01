//! rightkit-crypto: SHA-256, HMAC-SHA256, base64/base64url, hex, BLAKE3, canonical JSON and approval tokens, checked
//! against published test vectors (FIPS 180, RFC 4231 case 2, RFC 4648, BLAKE3 empty input).
use rightkit_crypto as c;
use rightkit_crypto::approval::{self, ApprovalError};
use serde_json::{json, Value};

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-crypto: {what}"))
    }
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let abc = c::hex_encode(&c::sha256(b"abc"));
    ensure(abc == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", &format!("sha256(abc)={abc}"))?;
    let empty = c::hex_encode(&c::sha256(b""));
    ensure(empty == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", &format!("sha256(empty)={empty}"))?;
    let long = c::hex_encode(&c::sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"));
    ensure(long == "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1", &format!("sha256(two-block)={long}"))?;
    let mac = c::hex_encode(&c::hmac_sha256(b"Jefe", b"what do ya want for nothing?"));
    ensure(mac == "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843", &format!("hmac RFC 4231 case 2={mac}"))?;

    ensure(c::ct_eq(b"abc", b"abc") && !c::ct_eq(b"abc", b"abd") && !c::ct_eq(b"abc", b"ab"), "ct_eq")?;

    ensure(c::base64_encode(b"foobar") == "Zm9vYmFy" && c::base64_encode(b"fo") == "Zm8=", "base64 encode")?;
    ensure(c::base64_decode("Zm9vYmFy").map_err(|e| e.to_string())? == b"foobar", "base64 decode")?;
    ensure(c::base64_encode(&[0xfb, 0xff]) == "+/8=" && c::base64url_encode(&[0xfb, 0xff]) == "-_8", "base64 alphabets")?;
    ensure(c::base64url_decode("-_8").map_err(|e| e.to_string())? == [0xfb, 0xff], "base64url decode")?;
    ensure(c::base64_decode("Zg=").is_err() && c::base64url_decode("Z").is_err(), "malformed base64 accepted")?;

    let b3 = c::blake3_text("");
    ensure(b3 == "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262", &format!("blake3(empty)={b3}"))?;

    let canon = c::canonical_json(&json!({ "b": 1, "a": [true, null, "x"], "c": { "z": 0, "y": 1 } }));
    ensure(canon == r#"{"a":[true,null,"x"],"b":1,"c":{"y":1,"z":0}}"#, &format!("canonical_json={canon}"))?;

    let key = b"e2e-approval-key";
    let token = approval::mint(key, "publish", "post-1", 1000, 60);
    ensure(approval::verify(key, "publish", "post-1", &token, 1030).is_ok(), "fresh token rejected")?;
    ensure(approval::verify(key, "publish", "post-1", &token, 1060) == Err(ApprovalError::Expired), "expired token accepted")?;
    ensure(approval::verify(key, "publish", "post-1", &token, 999) == Err(ApprovalError::NotYetValid), "early token accepted")?;
    ensure(approval::verify(key, "publish", "post-2", &token, 1030) == Err(ApprovalError::BadSignature), "wrong subject accepted")?;
    ensure(approval::verify(b"other", "publish", "post-1", &token, 1030) == Err(ApprovalError::BadSignature), "wrong key accepted")?;
    ensure(approval::verify(key, "publish", "post-1", "garbage", 1030) == Err(ApprovalError::Malformed), "garbage token")?;

    Ok(json!({ "sha256_abc": abc, "hmac_rfc4231_2": mac, "blake3_empty": b3, "canonical_json": canon, "approval_token_len": token.len() }))
}
