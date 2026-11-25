//! Wipe on revoke (Y3): when a device is revoked, that device erases its own synced copy and
//! destroys its sync keys the next time it starts or is reached.
//!
//! # Flow
//!
//! 1. On a device that holds the current key, [`issue_revoke_notice`] makes an opaque notice for
//!    the revoked device's id. Issue it **before** [`crate::KeyManager::rotate`]: the revoked
//!    device holds only the old versions and must be able to read the notice.
//! 2. Rotate, and approve every remaining device again (`transfer`). The revoked device cannot
//!    read anything written under the new key.
//! 3. Carry the notice to the revoked device over any channel, and keep carrying it at every
//!    contact until the device answers that it wiped. This crate has no relay and no channel; the
//!    host sends the bytes (for example over the device link, beside the "stop trusting me"
//!    notice).
//! 4. On the revoked device the host calls [`RevokeWipe::resume`] at every start, **before**
//!    opening its stores or the sync engine, and [`RevokeWipe::handle_notice`] for every notice
//!    that reaches it. A good notice for this device records a durable "wipe pending" mark,
//!    erases every registered [`LocalCopy`], destroys every key version with
//!    [`crate::KeyManager::forget_all`], and clears the mark. A crash in between leaves the mark,
//!    and the next `resume` finishes the job. While [`RevokeWipe::is_pending`] is true the host
//!    must not read, show or sync that data.
//!
//! # Limits, stated plainly
//!
//! * A device that is switched off, offline, stolen, or never started again is not reached, so
//!   it keeps its readable local copy. No design can force an erase on a machine that is not
//!   running. Y3 holds for a device that is reachable at revoke time or reconnects. (A "deadman"
//!   that wipes after N days without contact would narrow this; it changes behaviour for a laptop
//!   left closed, so it is not built until the owner asks for it.)
//! * Registering every copy is the host's job: Cortex, Nucleus, Inbox, facts, schedules, and the
//!   sync ledger files (a ledger holds not-yet-applied downloads in plaintext). Whatever is not
//!   registered survives. [`LocalCopy::erase`] must be idempotent (it runs again after a crash)
//!   and must treat "already gone" as success.
//! * The notice is authenticated with the account key, a symmetric secret. A revoked device holds
//!   the old key, so it can forge a notice. A device accepts a notice only when it names the key
//!   version the device currently holds, so a device already on the new key refuses a forged
//!   notice under the old one. A device not yet re-approved still holds the old key as current and
//!   could be wiped by a forgery that also names its device id. That costs the owner a re-approval,
//!   not any data, and is why the notice should travel over the authenticated device link.
//! * The notice names a device id. Give each enrolment of a device a fresh id, or a notice kept
//!   from an earlier enrolment could name the new one while an old key is still held.

use crate::error::{Error, Result};
use crate::key::{check_component, DataKey, KEY_LEN};
use crate::manager::KeyManager;
use crate::util::{push_lp, random_array};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

const NOTICE_FORMAT: u8 = 1;
const NOTICE_INFO: &[u8] = b"rightkit-sync-keys/v1/revoke-notice";
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 1 + 4 + NONCE_LEN;
/// Upper bound on a notice, so a hostile peer cannot make this parse a large buffer.
const MAX_NOTICE_LEN: usize = 1024;

/// One local copy of synced data that a revoked device must erase.
pub trait LocalCopy: Send + Sync {
    /// Short name for errors and logs (never content).
    fn name(&self) -> &str;
    /// Erase this copy. Must be idempotent and must succeed when the copy is already gone.
    fn erase(&self) -> std::io::Result<()>;
}

/// What [`RevokeWipe::handle_notice`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeOutcome {
    /// The notice is for another device; nothing was touched.
    NotForThisDevice,
    /// The notice names a key version this device does not hold as current (an older or newer
    /// one), or this device holds no key; nothing was touched.
    Ignored,
    /// This device erased its copies and destroyed its keys.
    Wiped,
}

fn notice_key(key: &DataKey, account_id: &str) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let mut info = Vec::new();
    info.extend_from_slice(NOTICE_INFO);
    push_lp(&mut info, account_id.as_bytes());
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    Hkdf::<Sha256>::new(None, &key.expose_bytes()[..])
        .expand(&info, &mut out[..])
        .map_err(|_| Error::Invalid {
            what: "revoke notice key",
            reason: "derivation failed".into(),
        })?;
    Ok(out)
}

fn notice_aad(key: &DataKey, account_id: &str) -> Vec<u8> {
    let mut aad = Vec::new();
    push_lp(&mut aad, NOTICE_INFO);
    push_lp(&mut aad, account_id.as_bytes());
    push_lp(&mut aad, key.key_id().as_str().as_bytes());
    aad.extend_from_slice(&key.version().to_be_bytes());
    aad
}

/// Make the notice that tells `target_device_id` to wipe. Uses the current key, so call it
/// before `rotate`. `issued_at` is a Unix time in seconds, for the record only. The notice is
/// opaque bytes for any channel.
pub fn issue_revoke_notice(
    manager: &KeyManager,
    issuer_device_id: &str,
    target_device_id: &str,
    issued_at: i64,
) -> Result<Vec<u8>> {
    check_component("issuer device id", issuer_device_id)?;
    check_component("target device id", target_device_id)?;
    if issuer_device_id == target_device_id {
        return Err(Error::Invalid {
            what: "target device id",
            reason: "a device cannot issue a wipe notice for itself".into(),
        });
    }
    let key = manager.require_current()?;
    let mut body = Vec::new();
    push_lp(&mut body, issuer_device_id.as_bytes());
    push_lp(&mut body, target_device_id.as_bytes());
    body.extend_from_slice(&issued_at.to_be_bytes());
    let subkey = notice_key(&key, manager.account_id())?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(&subkey[..]).map_err(|_| Error::Authentication)?;
    let nonce = random_array::<NONCE_LEN>()?;
    let ct = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &body,
                aad: &notice_aad(&key, manager.account_id()),
            },
        )
        .map_err(|_| Error::Authentication)?;
    let mut out = Vec::with_capacity(HEADER_LEN + ct.len());
    out.push(NOTICE_FORMAT);
    out.extend_from_slice(&key.version().to_be_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// The wipe handler of one device. Build it at start with every local copy registered.
pub struct RevokeWipe {
    manager: KeyManager,
    device_id: String,
    copies: Vec<Box<dyn LocalCopy>>,
}

impl RevokeWipe {
    /// `device_id` is this device's id as the issuer knows it.
    pub fn new(
        manager: KeyManager,
        device_id: impl Into<String>,
        copies: Vec<Box<dyn LocalCopy>>,
    ) -> Result<Self> {
        let device_id = device_id.into();
        check_component("device id", &device_id)?;
        Ok(Self {
            manager,
            device_id,
            copies,
        })
    }

    /// A wipe was started and not finished. The host must not read, show or sync the data, and
    /// must call [`RevokeWipe::resume`].
    pub fn is_pending(&self) -> Result<bool> {
        self.manager.wipe_pending()
    }

    /// Call at every start, before any store or the sync engine is opened. Finishes a wipe that
    /// was started and interrupted. `Ok(true)` when it finished one, `Ok(false)` when none was
    /// pending. On an error the mark stays and the next call tries again.
    pub fn resume(&self) -> Result<bool> {
        if !self.manager.wipe_pending()? {
            return Ok(false);
        }
        self.finish()?;
        Ok(true)
    }

    /// Handle a notice that reached this device. Authentication failure is an error and touches
    /// nothing. A notice for another device, or for a key version this device does not hold as
    /// current, is ignored.
    ///
    /// A wipe notice must reach this method only over the authenticated device link (the paired,
    /// already-authenticated channel between the user's devices). It is sealed under the current
    /// key, but its transport gives no replay protection: a notice captured on an open channel and
    /// replayed to the target device would wipe it again. Do not relay notices through the sync
    /// server, a push payload or any unauthenticated path.
    pub fn handle_notice(&self, notice: &[u8]) -> Result<NoticeOutcome> {
        if notice.len() < HEADER_LEN + TAG_LEN || notice.len() > MAX_NOTICE_LEN {
            return Err(Error::Protocol("revoke notice has a bad length"));
        }
        if notice[0] != NOTICE_FORMAT {
            return Err(Error::Protocol("unknown revoke notice format"));
        }
        if self.manager.wipe_pending()? {
            // A wipe is already under way; finish it first. The notice adds nothing.
            self.finish()?;
            return Ok(NoticeOutcome::Wiped);
        }
        let version = u32::from_be_bytes([notice[1], notice[2], notice[3], notice[4]]);
        let Some(current) = self.manager.current()? else {
            return Ok(NoticeOutcome::Ignored);
        };
        if current.version() != version {
            return Ok(NoticeOutcome::Ignored);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&notice[5..HEADER_LEN]);
        let subkey = notice_key(&current, self.manager.account_id())?;
        let cipher =
            XChaCha20Poly1305::new_from_slice(&subkey[..]).map_err(|_| Error::Authentication)?;
        let body = cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &notice[HEADER_LEN..],
                    aad: &notice_aad(&current, self.manager.account_id()),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| Error::Authentication)?;
        let (target, issued_at) = parse_body(&body)?;
        if target != self.device_id {
            return Ok(NoticeOutcome::NotForThisDevice);
        }
        // Durable intent first, so a crash from here on is finished by `resume`.
        self.manager.mark_wipe_pending(issued_at)?;
        self.finish()?;
        Ok(NoticeOutcome::Wiped)
    }

    /// Erase every copy, then destroy the keys, then clear the mark. Data goes before keys so a
    /// failure leaves the keys in place for the retry; every step is safe to repeat.
    fn finish(&self) -> Result<()> {
        for copy in &self.copies {
            copy.erase().map_err(|_| Error::WipeFailed {
                copy: copy.name().to_string(),
            })?;
        }
        self.manager.forget_all()?;
        self.manager.clear_wipe_pending()?;
        Ok(())
    }
}

fn parse_body(body: &[u8]) -> Result<(String, i64)> {
    fn take<'a>(buf: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
        if buf.len() < n {
            return Err(Error::Protocol("revoke notice is truncated"));
        }
        let (head, rest) = buf.split_at(n);
        *buf = rest;
        Ok(head)
    }
    fn lp<'a>(buf: &mut &'a [u8]) -> Result<&'a [u8]> {
        let n = take(buf, 4)?;
        let n = u32::from_be_bytes([n[0], n[1], n[2], n[3]]) as usize;
        take(buf, n)
    }
    let mut rest = body;
    let _issuer = lp(&mut rest)?;
    let target = lp(&mut rest)?;
    let at = take(&mut rest, 8)?;
    if !rest.is_empty() {
        return Err(Error::Protocol("revoke notice has trailing bytes"));
    }
    let target = std::str::from_utf8(target)
        .map_err(|_| Error::Protocol("revoke notice names a bad device id"))?
        .to_string();
    let mut raw = [0u8; 8];
    raw.copy_from_slice(at);
    Ok((target, i64::from_be_bytes(raw)))
}
