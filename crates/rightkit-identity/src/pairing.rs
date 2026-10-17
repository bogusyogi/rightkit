//! Device pairing (Orthic-Labs/rightkit#409): signed-transcript confirmation, owner approval,
//! per-device effect grants, resume tickets and a redelivery-safe replay store.
//!
//! Key agreement is out of scope: the caller supplies the shared secret both devices derived
//! (for example from `rightkit_sync_keys::pairing_sas`). Both sides MAC the same canonical
//! transcript under a role label, so a reflected MAC fails. The owner approves only after the
//! peer's MAC has verified.

use crate::util::{ct_eq, hex, hmac_sha256, random_bytes, unhex};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Role::Initiator => "initiator",
            Role::Responder => "responder",
        }
    }
    pub fn peer(self) -> Role {
        match self {
            Role::Initiator => Role::Responder,
            Role::Responder => Role::Initiator,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTranscript {
    pub session_id: String,
    /// The device being paired (it receives the grant).
    pub initiator_device: String,
    pub responder_device: String,
    pub initiator_nonce: Vec<u8>,
    pub responder_nonce: Vec<u8>,
    pub requested_effects: Vec<String>,
}

fn put(out: &mut Vec<u8>, tag: &str, value: &[u8]) {
    out.extend_from_slice(&(tag.len() as u32).to_be_bytes());
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

impl PairingTranscript {
    /// Length-prefixed, tagged, order-fixed encoding. Effects are sorted and de-duplicated so the
    /// encoding does not depend on request order.
    pub fn canonical(&self) -> Vec<u8> {
        let effects: BTreeSet<&str> = self.requested_effects.iter().map(String::as_str).collect();
        let mut out = b"rightkit-identity/transcript/v1".to_vec();
        put(&mut out, "session", self.session_id.as_bytes());
        put(&mut out, "initiator", self.initiator_device.as_bytes());
        put(&mut out, "responder", self.responder_device.as_bytes());
        put(&mut out, "initiator_nonce", &self.initiator_nonce);
        put(&mut out, "responder_nonce", &self.responder_nonce);
        put(
            &mut out,
            "effect_count",
            &(effects.len() as u32).to_be_bytes(),
        );
        for e in effects {
            put(&mut out, "effect", e.as_bytes());
        }
        out
    }
}

/// The MAC `role` sends to confirm it saw this transcript.
pub fn transcript_mac(secret: &[u8], role: Role, t: &PairingTranscript) -> [u8; 32] {
    let mut msg = format!("rightkit-identity/pairing/v1/{}\0", role.label()).into_bytes();
    msg.extend_from_slice(&t.canonical());
    hmac_sha256(secret, &msg)
}

/// Six-digit code both screens show (HOTP-style truncation of an HMAC over the transcript).
pub fn confirmation_code(secret: &[u8], t: &PairingTranscript) -> String {
    let mut msg = b"rightkit-identity/pairing/v1/code\0".to_vec();
    msg.extend_from_slice(&t.canonical());
    let h = hmac_sha256(secret, &msg);
    let off = (h[31] & 0x0f) as usize;
    let n = u32::from_be_bytes([h[off], h[off + 1], h[off + 2], h[off + 3]]) & 0x7fff_ffff;
    format!("{:06}", n % 1_000_000)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingState {
    Proposed,
    TranscriptConfirmed,
    Approved,
    Denied,
    Expired,
    Complete,
}

/// Effects a device may perform, decided by the owner at approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceGrant {
    pub device_id: String,
    pub effects: BTreeSet<String>,
}

impl DeviceGrant {
    pub fn allows(&self, effect: &str) -> bool {
        self.effects.contains(effect)
    }
}

pub struct PairingSession {
    transcript: PairingTranscript,
    secret: Vec<u8>,
    local_role: Role,
    expires_unix: u64,
    state: PairingState,
    granted: BTreeSet<String>,
}

impl PairingSession {
    pub fn new(
        transcript: PairingTranscript,
        secret: Vec<u8>,
        local_role: Role,
        now_unix: u64,
        ttl_secs: u64,
    ) -> Self {
        PairingSession {
            transcript,
            secret,
            local_role,
            expires_unix: now_unix.saturating_add(ttl_secs),
            state: PairingState::Proposed,
            granted: BTreeSet::new(),
        }
    }

    pub fn state(&self) -> &PairingState {
        &self.state
    }

    /// The MAC to send to the peer.
    pub fn local_mac(&self) -> [u8; 32] {
        transcript_mac(&self.secret, self.local_role, &self.transcript)
    }

    pub fn code(&self) -> String {
        confirmation_code(&self.secret, &self.transcript)
    }

    fn live(&mut self, now: u64) -> Result<()> {
        match self.state {
            PairingState::Denied | PairingState::Expired | PairingState::Complete => {
                Err(Error::Refused("pairing session is closed"))
            }
            _ if now >= self.expires_unix => {
                self.state = PairingState::Expired;
                Err(Error::Refused("pairing session expired"))
            }
            _ => Ok(()),
        }
    }

    /// Verify the peer's transcript MAC. A wrong MAC leaves the state unchanged.
    pub fn confirm_peer(&mut self, peer_mac: &[u8], now: u64) -> Result<()> {
        self.live(now)?;
        if self.state != PairingState::Proposed {
            return Err(Error::Refused("transcript already confirmed"));
        }
        let want = transcript_mac(&self.secret, self.local_role.peer(), &self.transcript);
        if !ct_eq(&want, peer_mac) {
            return Err(Error::Refused("transcript confirmation mismatch"));
        }
        self.state = PairingState::TranscriptConfirmed;
        Ok(())
    }

    /// Owner approval, narrowed to `granted` (must be a subset of the requested effects).
    pub fn owner_approve(
        &mut self,
        granted: impl IntoIterator<Item = String>,
        now: u64,
    ) -> Result<()> {
        self.live(now)?;
        if self.state != PairingState::TranscriptConfirmed {
            return Err(Error::Refused(
                "owner approval needs a confirmed transcript",
            ));
        }
        let requested: BTreeSet<&String> = self.transcript.requested_effects.iter().collect();
        let granted: BTreeSet<String> = granted.into_iter().collect();
        if !granted.iter().all(|g| requested.contains(g)) {
            return Err(Error::Refused("grant exceeds requested effects"));
        }
        self.granted = granted;
        self.state = PairingState::Approved;
        Ok(())
    }

    pub fn owner_deny(&mut self, now: u64) -> Result<()> {
        self.live(now)?;
        if self.state != PairingState::TranscriptConfirmed {
            return Err(Error::Refused("only a confirmed transcript can be decided"));
        }
        self.state = PairingState::Denied;
        Ok(())
    }

    /// Finish an approved session and return the device's grant.
    pub fn complete(&mut self, now: u64) -> Result<DeviceGrant> {
        self.live(now)?;
        if self.state != PairingState::Approved {
            return Err(Error::Refused("pairing is not approved"));
        }
        self.state = PairingState::Complete;
        Ok(DeviceGrant {
            device_id: self.transcript.initiator_device.clone(),
            effects: std::mem::take(&mut self.granted),
        })
    }
}

/// A signed, expiring ticket a paired device presents to resume without re-pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTicket {
    pub ticket_id: String,
    pub device_id: String,
    pub expires_unix: u64,
}

impl ResumeTicket {
    /// Returns the ticket and its wire token `v1.<id>.<expires>.<hex device>.<hex mac>`.
    pub fn issue(
        key: &[u8],
        device_id: &str,
        now_unix: u64,
        ttl_secs: u64,
    ) -> Result<(ResumeTicket, String)> {
        let t = ResumeTicket {
            ticket_id: hex(&random_bytes::<16>()?),
            device_id: device_id.to_string(),
            expires_unix: now_unix.saturating_add(ttl_secs),
        };
        let body = t.body();
        let token = format!("{body}.{}", hex(&hmac_sha256(key, body.as_bytes())));
        Ok((t, token))
    }

    fn body(&self) -> String {
        format!(
            "v1.{}.{}.{}",
            self.ticket_id,
            self.expires_unix,
            hex(self.device_id.as_bytes())
        )
    }

    /// Verify signature and expiry.
    pub fn verify(key: &[u8], token: &str, now_unix: u64) -> Result<ResumeTicket> {
        let bad = Error::Refused("invalid resume ticket");
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 5 || parts[0] != "v1" {
            return Err(bad);
        }
        let body = parts[..4].join(".");
        let mac = unhex(parts[4]).ok_or(Error::Refused("invalid resume ticket"))?;
        if !ct_eq(&hmac_sha256(key, body.as_bytes()), &mac) {
            return Err(bad);
        }
        let expires_unix: u64 = parts[2]
            .parse()
            .map_err(|_| Error::Refused("invalid resume ticket"))?;
        if now_unix >= expires_unix {
            return Err(Error::Refused("resume ticket expired"));
        }
        let device = unhex(parts[3])
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or(Error::Refused("invalid resume ticket"))?;
        Ok(ResumeTicket {
            ticket_id: parts[1].to_string(),
            device_id: device,
            expires_unix,
        })
    }

    /// Verify and consume: a ticket redeems once. A redelivery returns `Refused`.
    pub fn redeem(
        key: &[u8],
        token: &str,
        now_unix: u64,
        replay: &mut ReplayStore,
    ) -> Result<ResumeTicket> {
        let t = ResumeTicket::verify(key, token, now_unix)?;
        match replay.observe(
            &t.ticket_id,
            now_unix,
            t.expires_unix.saturating_sub(now_unix),
        ) {
            Observation::New => Ok(t),
            Observation::Duplicate => Err(Error::Refused("resume ticket already used")),
            Observation::Full => Err(Error::Refused("replay store full")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    New,
    /// Already seen: acknowledge the redelivery without applying it again.
    Duplicate,
    /// No room after pruning expired ids. Fail closed: the caller refuses the message.
    Full,
}

/// Bounded set of seen message ids with expiry. Serialisable so a caller can persist it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayStore {
    capacity: usize,
    seen: BTreeMap<String, u64>,
}

impl ReplayStore {
    pub fn new(capacity: usize) -> Self {
        ReplayStore {
            capacity,
            seen: BTreeMap::new(),
        }
    }

    /// Record `id` until `now + ttl_secs`. Expired ids are pruned first.
    pub fn observe(&mut self, id: &str, now_unix: u64, ttl_secs: u64) -> Observation {
        self.seen.retain(|_, exp| *exp > now_unix);
        if self.seen.contains_key(id) {
            return Observation::Duplicate;
        }
        if self.seen.len() >= self.capacity {
            return Observation::Full;
        }
        self.seen
            .insert(id.to_string(), now_unix.saturating_add(ttl_secs));
        Observation::New
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript() -> PairingTranscript {
        PairingTranscript {
            session_id: "s1".into(),
            initiator_device: "phone".into(),
            responder_device: "desktop".into(),
            initiator_nonce: vec![1; 16],
            responder_nonce: vec![2; 16],
            requested_effects: vec!["write".into(), "read".into(), "read".into()],
        }
    }

    #[test]
    fn effect_order_does_not_change_encoding() {
        let mut b = transcript();
        b.requested_effects = vec!["read".into(), "write".into()];
        assert_eq!(transcript().canonical(), b.canonical());
        b.responder_nonce = vec![3; 16];
        assert_ne!(transcript().canonical(), b.canonical());
    }

    #[test]
    fn full_flow() {
        let secret = b"shared-secret".to_vec();
        let mut desk = PairingSession::new(transcript(), secret.clone(), Role::Responder, 100, 60);
        let phone = PairingSession::new(transcript(), secret, Role::Initiator, 100, 60);
        assert_eq!(desk.code(), phone.code());
        assert!(desk.owner_approve(vec!["read".to_string()], 101).is_err());
        assert!(
            desk.confirm_peer(&desk.local_mac(), 101).is_err(),
            "reflected mac fails"
        );
        desk.confirm_peer(&phone.local_mac(), 101).unwrap();
        assert!(desk.owner_approve(vec!["admin".to_string()], 102).is_err());
        desk.owner_approve(vec!["read".to_string()], 102).unwrap();
        let g = desk.complete(103).unwrap();
        assert_eq!(g.device_id, "phone");
        assert!(g.allows("read") && !g.allows("write"));
        assert!(desk.complete(104).is_err());
    }

    #[test]
    fn expiry_and_denial() {
        let mut s = PairingSession::new(transcript(), b"k".to_vec(), Role::Responder, 0, 10);
        assert!(s.confirm_peer(&[0; 32], 11).is_err());
        assert_eq!(s.state(), &PairingState::Expired);
        let mut s = PairingSession::new(transcript(), b"k".to_vec(), Role::Responder, 0, 10);
        let peer = PairingSession::new(transcript(), b"k".to_vec(), Role::Initiator, 0, 10);
        s.confirm_peer(&peer.local_mac(), 1).unwrap();
        s.owner_deny(2).unwrap();
        assert!(s.complete(3).is_err());
    }

    #[test]
    fn tickets_expire_and_redeem_once() {
        let key = b"ticket-key";
        let (_, token) = ResumeTicket::issue(key, "phone", 100, 50).unwrap();
        let mut replay = ReplayStore::new(8);
        let t = ResumeTicket::redeem(key, &token, 110, &mut replay).unwrap();
        assert_eq!(t.device_id, "phone");
        assert!(ResumeTicket::redeem(key, &token, 111, &mut replay).is_err());
        assert!(ResumeTicket::verify(key, &token, 150).is_err());
        assert!(ResumeTicket::verify(b"other", &token, 110).is_err());
        let tampered = token.replacen(&hex(b"phone"), &hex(b"other"), 1);
        assert!(ResumeTicket::verify(key, &tampered, 110).is_err());
    }

    #[test]
    fn replay_store_bounds() {
        let mut r = ReplayStore::new(2);
        assert_eq!(r.observe("a", 0, 10), Observation::New);
        assert_eq!(r.observe("a", 1, 10), Observation::Duplicate);
        assert_eq!(r.observe("b", 1, 10), Observation::New);
        assert_eq!(r.observe("c", 2, 10), Observation::Full);
        assert_eq!(r.observe("c", 11, 10), Observation::New);
    }
}
