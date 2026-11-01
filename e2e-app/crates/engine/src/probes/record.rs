//! rightkit-record: the integrity primitives every recording bundle is sealed with (BLAKE3 content hashes, HMAC-SHA256 against RFC
//! 4231, file hashing of a real file). A live capture needs the OS screen-recording permission and a window: the app-level e2e's.
use rightkit_record::hash::{blake3_text, file_hash, hex_lower, hmac_sha256, is_blake3_text};
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let empty = blake3_text(b"");
    if !is_blake3_text(&empty) || !empty.contains("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262") {
        return Err(format!("BLAKE3(\"\") came out {empty}"));
    }
    let mac = hex_lower(&hmac_sha256(b"Jefe", b"what do ya want for nothing?"));
    if mac != "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843" {
        return Err(format!("HMAC-SHA256 RFC 4231 case 2 came out {mac}"));
    }
    let f = ctx.dir("record")?.join("frame.bin");
    std::fs::write(&f, b"").map_err(|e| e.to_string())?;
    let (h, len) = file_hash(&f).map_err(|e| format!("file hash: {e}"))?;
    if h != empty || len != 0 {
        return Err(format!("the hash of an empty file ({h}, {len} bytes) differs from BLAKE3(\"\")"));
    }
    Ok(json!({ "blake3_empty": empty, "hmac_rfc4231": mac }))
}
