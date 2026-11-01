//! rightkit-identity: installation identity (persisted, clone detection, startup claim), device pairing (transcript MACs,
//! owner approval, resume tickets, replay store), per-device bearer tokens with the per-IP rate limit, build identity.
use rightkit_identity::{
    confirmation_code, host_display_name, machine_binding_hash, transcript_mac, AuthError, BearerRegistry, BuildIdentity,
    ClaimOutcome, InstallationStore, Observation, PairingSession, PairingState, PairingTranscript, RateLimiter, ReplayStore,
    ResumeTicket, Role, Startup,
};
use serde_json::{json, Value};
use std::net::IpAddr;

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-identity: {what}"))
    }
}

fn e<T: std::fmt::Display>(what: &str) -> impl FnOnce(T) -> String + '_ {
    move |err| format!("rightkit-identity: {what}: {err}")
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("identity")?;
    let now = 1_800_000_000u64;

    // Installation identity: mint, persist, resume, clone detection.
    let binding_a = machine_binding_hash("e2e-machine-a");
    ensure(binding_a == machine_binding_hash("  e2e-machine-a \n"), "machine binding is not trim-stable")?;
    ensure(binding_a.len() == 64 && !binding_a.contains("e2e-machine"), "machine binding leaks the raw id or is not a sha256 hex")?;
    let store = InstallationStore::new(&dir);
    let fresh = match store.load_or_create(&binding_a, now).map_err(e("first load_or_create"))? {
        Startup::Fresh(id) => id,
        other => return Err(format!("rightkit-identity: first run was not Fresh: {other:?}")),
    };
    ensure(fresh.generation == 1 && fresh.created_unix == now, "fresh identity fields")?;
    ensure(dir.join("installation-identity.json").is_file(), "identity file was not persisted")?;
    let resumed = match InstallationStore::new(&dir).load_or_create(&binding_a, now + 5).map_err(e("second load"))? {
        Startup::Resumed(id) => id,
        other => return Err(format!("rightkit-identity: second run was not Resumed: {other:?}")),
    };
    ensure(resumed.installation_id == fresh.installation_id, "resumed identity changed id")?;

    // Startup claim: acquire, held by live, released, unclean exit after a dead holder.
    let me = std::process::id();
    let other = me.wrapping_add(1_000_003);
    let acquired = store.claim_startup(&fresh.installation_id, me, now, |_| false).map_err(e("claim"))?;
    ensure(acquired == ClaimOutcome::Acquired, &format!("first claim was {acquired:?}"))?;
    let claim = store.read_claim().map_err(e("read_claim"))?.ok_or("rightkit-identity: claim not on disk")?;
    ensure(claim.pid == me && claim.installation_id == fresh.installation_id && !claim.nonce.is_empty(), "claim contents")?;
    match store.claim_startup(&fresh.installation_id, other, now + 1, |p| p == me).map_err(e("live-holder claim"))? {
        ClaimOutcome::HeldByLiveProcess(c) if c.pid == me => {}
        o => return Err(format!("rightkit-identity: live holder was not respected: {o:?}")),
    }
    ensure(!store.release_claim_nonce(me, "wrong-nonce").map_err(e("release wrong nonce"))?, "a wrong nonce released the claim")?;
    ensure(store.release_claim_nonce(me, &claim.nonce).map_err(e("release nonce"))?, "the right nonce did not release")?;
    ensure(store.read_claim().map_err(e("read after release"))?.is_none(), "claim remained after release")?;
    store.claim_startup(&fresh.installation_id, other, now + 2, |_| false).map_err(e("claim by other"))?;
    match store.claim_startup(&fresh.installation_id, me, now + 3, |_| false).map_err(e("claim after crash"))? {
        ClaimOutcome::PreviousUncleanExit(c) if c.pid == other => {}
        o => return Err(format!("rightkit-identity: an unreleased dead claim was not reported: {o:?}")),
    }
    ensure(store.release_claim(me).map_err(e("release_claim"))?, "release_claim by owner pid")?;

    let binding_b = machine_binding_hash("e2e-machine-b");
    match store.load_or_create(&binding_b, now + 10).map_err(e("clone load"))? {
        Startup::CloneDetected { previous_installation_id, identity, .. } => {
            ensure(previous_installation_id == fresh.installation_id, "clone reported the wrong previous id")?;
            ensure(identity.installation_id != fresh.installation_id && identity.generation == 2, "clone did not re-mint")?;
        }
        o => return Err(format!("rightkit-identity: moved machine binding was not CloneDetected: {o:?}")),
    }
    ensure(host_display_name("  My\tMac.local ") == "My Mac" && host_display_name("\u{7}") == "This device", "host_display_name")?;

    // Pairing: both sides MAC the transcript, reflected MAC refused, owner narrows the grant.
    let secret = b"shared-secret-from-sas".to_vec();
    let transcript = PairingTranscript {
        session_id: "s-1".into(),
        initiator_device: "phone".into(),
        responder_device: "desktop".into(),
        initiator_nonce: vec![1; 16],
        responder_nonce: vec![2; 16],
        requested_effects: vec!["read".into(), "write".into(), "read".into()],
    };
    ensure(confirmation_code(&secret, &transcript).len() == 6, "confirmation code is not six digits")?;
    let mut desktop = PairingSession::new(transcript.clone(), secret.clone(), Role::Responder, now, 60);
    let phone = PairingSession::new(transcript.clone(), secret.clone(), Role::Initiator, now, 60);
    ensure(desktop.code() == phone.code(), "the two screens show different codes")?;
    ensure(
        desktop.confirm_peer(&desktop.local_mac(), now + 1).is_err() && *desktop.state() == PairingState::Proposed,
        "a reflected MAC was accepted",
    )?;
    ensure(desktop.confirm_peer(&phone.local_mac(), now + 1).is_ok(), "the peer MAC did not verify")?;
    ensure(
        desktop.owner_approve(vec!["delete".to_string()], now + 2).is_err(),
        "a grant beyond the requested effects was approved",
    )?;
    desktop.owner_approve(vec!["read".to_string()], now + 2).map_err(e("owner_approve"))?;
    let grant = desktop.complete(now + 3).map_err(e("complete"))?;
    ensure(grant.device_id == "phone" && grant.allows("read") && !grant.allows("write"), "grant contents")?;
    ensure(desktop.complete(now + 4).is_err(), "a completed session completed twice")?;
    ensure(
        transcript_mac(&secret, Role::Initiator, &transcript) != transcript_mac(&secret, Role::Responder, &transcript),
        "role labels do not separate MACs",
    )?;
    let mut expiring = PairingSession::new(transcript, secret, Role::Responder, now, 10);
    ensure(expiring.confirm_peer(&[0; 32], now + 10).is_err() && *expiring.state() == PairingState::Expired, "session did not expire")?;

    // Resume tickets: verify, redeem once, tamper, expiry.
    let key = b"resume-signing-key";
    let (ticket, token) = ResumeTicket::issue(key, "phone", now, 100).map_err(e("issue ticket"))?;
    ensure(ResumeTicket::verify(key, &token, now + 1).map_err(e("verify ticket"))?.device_id == "phone", "ticket device")?;
    ensure(ResumeTicket::verify(b"other", &token, now + 1).is_err(), "ticket verified under another key")?;
    ensure(ResumeTicket::verify(key, &token, now + 100).is_err(), "expired ticket verified")?;
    let mut replay = ReplayStore::new(4);
    ResumeTicket::redeem(key, &token, now + 2, &mut replay).map_err(e("first redeem"))?;
    ensure(ResumeTicket::redeem(key, &token, now + 3, &mut replay).is_err(), "ticket redeemed twice")?;
    ensure(replay.observe("m1", now, 10) == Observation::New && replay.observe("m1", now + 1, 10) == Observation::Duplicate, "replay store")?;
    ensure(ticket.expires_unix == now + 100, "ticket expiry")?;

    // Bearer tokens: stored as hashes, per-device, revoke, per-IP rate limit.
    let mut reg = BearerRegistry::new();
    let mut limiter = RateLimiter::new(3, 1.0, 100);
    let ip: IpAddr = "10.0.0.1".parse().map_err(|x| format!("ip: {x}"))?;
    let tok = reg.mint("phone", now).map_err(e("mint"))?;
    ensure(tok.starts_with("rkb_"), "token prefix")?;
    ensure(reg.authenticate(&tok, ip, &mut limiter, 0) == Ok("phone"), "valid token rejected")?;
    ensure(reg.authenticate("rkb_nope", ip, &mut limiter, 0) == Err(AuthError::Invalid), "bogus token accepted")?;
    ensure(
        matches!(reg.authenticate("rkb_nope", ip, &mut limiter, 0), Ok(_)) == false
            && matches!(reg.authenticate("rkb_nope", ip, &mut limiter, 0), Err(AuthError::RateLimited { .. })),
        "the per-IP limiter did not engage after capacity attempts",
    )?;
    let other_ip: IpAddr = "10.0.0.2".parse().map_err(|x| format!("ip: {x}"))?;
    ensure(reg.authenticate(&tok, other_ip, &mut limiter, 0) == Ok("phone"), "another IP was limited by the first")?;
    ensure(reg.revoke("phone") && reg.active_devices().is_empty(), "revoke")?;
    ensure(reg.authenticate(&tok, other_ip, &mut limiter, 0) == Err(AuthError::Invalid), "revoked token accepted")?;

    // Build identity: compile-time values, runtime override, display.
    let bi = BuildIdentity::from_compile_time(Some("0123456789abcdef"), Some("false"), Some("release"), Some("beta"), Some("2026-10-18T09:00:00Z"), "1.2.3");
    ensure(bi.display() == "1.2.3+0123456 (release, beta, 2026-10-18T09:00:00Z)", &format!("display was {:?}", bi.display()))?;
    let bi = bi.with_runtime_override(|k| (k == "RIGHTKIT_CHANNEL").then(|| "stable".to_string()));
    ensure(bi.channel == "stable" && bi.git_rev == "0123456789abcdef", "runtime override")?;

    Ok(json!({
        "installation_id": fresh.installation_id,
        "clone_generation": 2,
        "grant_effects": grant.effects,
        "pairing_code_digits": 6,
        "build": bi.display(),
    }))
}
