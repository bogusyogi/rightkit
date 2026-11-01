//! rightkit-sync-keys: the account data key lifecycle on a credential store, per-zone key derivation, and the device-approval
//! key transfer (X25519 + short code + AEAD) between two in-process devices, with a wrong code, a context mismatch and a
//! tampered transfer all refused.
//!
//! Needs: `rightkit-credentials` as a direct engine dependency (the key manager is built over its `Store`; the probe uses its
//! in-memory backend so no OS keychain item is created).
use std::sync::Arc;

use rightkit_credentials::{Config, MemoryBackend, Namespace, Store};
use rightkit_sync_keys::transfer::{Approver, Requester, TransferContext};
use rightkit_sync_keys::{account_scope, Error, InstallOutcome, KeyManager, KeyProvider, ZoneKeys};
use serde_json::{json, Value};

fn store() -> Result<Store, String> {
    let ns = Namespace::new("e2e.rightkit.sync-keys").map_err(|e| format!("namespace: {e}"))?;
    Ok(Store::open(
        Config::new(ns).backend(Arc::new(MemoryBackend::new())),
    ))
}

/// One full handshake up to the point where both devices show a code. The approver may use another context.
fn handshake(
    req_ctx: TransferContext,
    app_ctx: TransferContext,
) -> Result<(Requester, Approver, String, String), String> {
    let (mut req, begin) =
        Requester::begin(req_ctx, Some("e2e-new-device")).map_err(|e| format!("begin: {e}"))?;
    let (mut appr, offer) =
        Approver::receive_begin(app_ctx, &begin).map_err(|e| format!("receive_begin: {e}"))?;
    if appr.requester_label() != Some("e2e-new-device") {
        return Err(format!("approver saw label {:?}", appr.requester_label()));
    }
    let reveal = req
        .receive_offer(&offer)
        .map_err(|e| format!("receive_offer: {e}"))?;
    appr.receive_reveal(&reveal)
        .map_err(|e| format!("receive_reveal: {e}"))?;
    let rc = req.code().map_err(|e| format!("requester code: {e}"))?;
    let ac = appr.code().map_err(|e| format!("approver code: {e}"))?;
    Ok((req, appr, rc, ac))
}

fn wrong_code(code: &str) -> String {
    let mut c: Vec<char> = code.chars().collect();
    c[0] = if c[0] == '9' {
        '0'
    } else {
        ((c[0] as u8) + 1) as char
    };
    c.into_iter().collect()
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    // ---- key lifecycle on a credential store --------------------------------------------------------------------
    let a = KeyManager::new(store()?, "e2e-account").map_err(|e| format!("manager: {e}"))?;
    if a.is_enrolled().map_err(|e| e.to_string())? {
        return Err("a fresh store reports enrolled".into());
    }
    if a.require_current().err() != Some(Error::NotEnrolled) {
        return Err("require_current on a fresh store is not NotEnrolled".into());
    }
    let (k1, created) = a.create_if_absent().map_err(|e| e.to_string())?;
    let (k1b, created2) = a.create_if_absent().map_err(|e| e.to_string())?;
    if !created || created2 || k1.expose_bytes() != k1b.expose_bytes() || k1.version() != 1 {
        return Err("create_if_absent is not idempotent".into());
    }
    let k2 = a.rotate().map_err(|e| e.to_string())?;
    if k2.version() != 2 || k2.key_id() == k1.key_id() {
        return Err("rotation did not produce a new version and id".into());
    }
    if a.find_by_id(k1.key_id())
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err("the previous key vanished at rotation".into());
    }
    let zk = ZoneKeys::new(a.clone(), "e2eapp", "notes").map_err(|e| e.to_string())?;
    let z2 = zk.current_key().map_err(|e| e.to_string())?;
    let z1 = zk
        .key_by_id(k1.key_id())
        .map_err(|e| e.to_string())?
        .ok_or("zone key for the old id missing")?;
    if z1.as_bytes() == z2.as_bytes() || z2.as_bytes() == k2.expose_bytes() {
        return Err("zone keys are not separated from each other or from the root".into());
    }
    let other_zone = k2.zone_key("e2eapp", "other").map_err(|e| e.to_string())?;
    if other_zone.as_bytes() == z2.as_bytes() {
        return Err("two zones derived the same key".into());
    }
    let scope1 =
        account_scope("e2eapp", "notes", "e2e-account", k1.key_id()).map_err(|e| e.to_string())?;
    let scope2 = zk.account_scope().map_err(|e| e.to_string())?;
    if scope1 == scope2 || scope2.len() != 64 {
        return Err("rotation did not change the account scope".into());
    }
    if !a.retire(1).map_err(|e| e.to_string())?
        || a.find_by_id(k1.key_id())
            .map_err(|e| e.to_string())?
            .is_some()
    {
        return Err("retire did not remove the old key".into());
    }
    if a.retire(2).is_ok() {
        return Err("the current version was retirable".into());
    }

    // ---- device-approval transfer: happy path -------------------------------------------------------------------
    let ctx = TransferContext::new("e2eapp", "e2e-account").map_err(|e| e.to_string())?;
    let (mut req, mut appr, rc, ac) = handshake(ctx.clone(), ctx.clone())?;
    if rc != ac || rc.len() != 6 {
        return Err(format!(
            "the two devices computed different codes ({rc} vs {ac})"
        ));
    }
    let wire = appr
        .approve(&rc, &k2)
        .map_err(|e| format!("approve: {e}"))?;
    if wire.windows(32).any(|w| w == k2.expose_bytes().as_slice()) {
        return Err("the transfer message carries the key in the clear".into());
    }
    let got = req.accept(&ac, &wire).map_err(|e| format!("accept: {e}"))?;
    if got.expose_bytes() != k2.expose_bytes() || got.version() != 2 || got.key_id() != k2.key_id()
    {
        return Err("the requester recovered a different key".into());
    }
    // The new device installs it into its own store and derives the same zone key.
    let b = KeyManager::new(store()?, "e2e-account").map_err(|e| e.to_string())?;
    if b.install(&got, false).map_err(|e| e.to_string())? != InstallOutcome::Advanced {
        return Err("install on the new device did not advance".into());
    }
    let zb = ZoneKeys::new(b, "e2eapp", "notes").map_err(|e| e.to_string())?;
    if zb.current_key().map_err(|e| e.to_string())?.as_bytes() != z2.as_bytes() {
        return Err("the new device derives another zone key".into());
    }
    // A spent session cannot be approved twice.
    if appr.approve(&rc, &k2).is_ok() {
        return Err("a spent approver session approved again".into());
    }

    // ---- wrong code: refused, nothing wrapped; burns after MAX_WRONG_CODES ---------------------------------------
    let (_req, mut appr, rc, _ac) = handshake(ctx.clone(), ctx.clone())?;
    let bad = wrong_code(&rc);
    match appr.approve(&bad, &k2) {
        Err(Error::SasMismatch { remaining: 2 }) => {}
        other => {
            return Err(format!(
                "wrong code was not SasMismatch(2): {:?}",
                other.map(|v| v.len())
            ))
        }
    }
    let mut burned = false;
    for _ in 0..4 {
        if matches!(appr.approve(&bad, &k2), Err(Error::TooManyAttempts)) {
            burned = true;
            break;
        }
    }
    if !burned || appr.approve(&rc, &k2).is_ok() {
        return Err("the session survived too many wrong codes".into());
    }

    // ---- context mismatch (another account) shows as different codes, so approval is refused ---------------------
    let other_ctx = TransferContext::new("e2eapp", "someone-else").map_err(|e| e.to_string())?;
    let (_req, mut appr, rc, ac) = handshake(ctx.clone(), other_ctx)?;
    let _ = &ac;
    if rc == ac {
        return Err("a mismatched account context produced the same code".into());
    }
    if !matches!(appr.approve(&rc, &k2), Err(Error::SasMismatch { .. })) {
        return Err("approver accepted across an account mismatch".into());
    }

    // ---- tampered transfer: correct code, flipped ciphertext byte => Authentication -----------------------------
    let (mut req, mut appr, rc, ac) = handshake(ctx.clone(), ctx.clone())?;
    let mut wire = appr.approve(&rc, &k2).map_err(|e| e.to_string())?;
    let last = wire.len() - 1;
    wire[last] ^= 0x01;
    match req.accept(&ac, &wire) {
        Err(Error::Authentication) => {}
        Err(e) => return Err(format!("tampered transfer gave {e}, not Authentication")),
        Ok(_) => return Err("a tampered transfer was accepted".into()),
    }

    // ---- replay into another session fails ---------------------------------------------------------------------------
    let (mut req1, mut appr1, rc1, ac1) = handshake(ctx.clone(), ctx.clone())?;
    let w1 = appr1.approve(&rc1, &k2).map_err(|e| e.to_string())?;
    drop(req1.accept(&ac1, &w1));
    let (mut req2, _appr2, _rc2, ac2) = handshake(ctx.clone(), ctx.clone())?;
    if req2.accept(&ac2, &w1).is_ok() {
        return Err("a transfer from another session was accepted".into());
    }

    Ok(json!({
        "key_versions": 2,
        "zone_keys_separated": true,
        "scope_len": scope2.len(),
        "transfer": "happy path, wrong code (burn after 3), account mismatch, tamper, replay all as specified",
    }))
}
