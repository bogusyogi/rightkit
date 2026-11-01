//! rightkit-send: a LocalSend identity (a fresh TLS certificate) and its pinned fingerprint, the sanitising of names a peer sends
//! (no traversal, no separators, no reserved names), and unique reservation of an incoming file's path on disk.
use rightkit_send::net::{valid_pin, Identity};
use rightkit_send::proto::{reserve_unique, sanitize_component, sanitize_relative};
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let id = Identity::generate("rke2e-probe").map_err(|e| format!("identity: {e}"))?;
    if !valid_pin(&id.fingerprint) || id.cert_der.is_empty() {
        return Err(format!("a generated identity's fingerprint {:?} is not a valid pin", id.fingerprint));
    }
    let evil = sanitize_relative("../../etc/passwd");
    if evil.iter().any(|p| p == ".." || p.contains('/') || p.contains('\\')) {
        return Err(format!("a traversing name survived sanitising: {evil:?}"));
    }
    let comp = sanitize_component("a/b\\c:d");
    if comp.contains('/') || comp.contains('\\') {
        return Err(format!("a separator survived sanitising: {comp}"));
    }
    let dir = ctx.dir("send")?;
    let first = reserve_unique(&dir, &["photo.jpg".to_string()]).map_err(|e| format!("reserve: {e}"))?;
    std::fs::write(&first, b"x").map_err(|e| e.to_string())?;
    let second = reserve_unique(&dir, &["photo.jpg".to_string()]).map_err(|e| format!("reserve again: {e}"))?;
    if first == second || !second.starts_with(&dir) {
        return Err(format!("a second incoming file with the same name reused {}", second.display()));
    }
    Ok(json!({ "fingerprint": id.fingerprint, "sanitized": evil, "unique": [first, second] }))
}
