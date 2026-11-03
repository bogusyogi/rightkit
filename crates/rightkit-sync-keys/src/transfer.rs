//! Device-approval key transfer, protocol `rightkit-sync-keys-transfer/1`.
//!
//! A new device (the *requester*) gets the account data key only after an enrolled device (the
//! *approver*) approves it. The two talk through a relay that sees only opaque byte strings; the
//! relay is never trusted for secrecy or authenticity. The approval is the `rk-pair/1`
//! short-code pairing of `rightkit-daemon` (see `crate::pairing_sas` for the copied functions)
//! with an X25519 step and an AEAD key wrap added.
//!
//! ```text
//!   requester R (new device)                              approver A (enrolled)
//!   ephemeral X25519 (r_sk, r_pk), nonce_d
//!   1. Begin  { r_pk, commit = H(r_pk, nonce_d, label), label }  ------------->
//!                                       ephemeral X25519 (a_sk, a_pk), nonce_h
//!   <-------------------------------- 2. Offer { a_pk, nonce_h }
//!   3. Reveal { nonce_d }  --------------------------------------------------->
//!                                       A checks commit; both sides now compute
//!   shared = X25519(r_sk, a_pk)                         shared = X25519(a_sk, r_pk)
//!   code   = SAS(r_pk, nonce_d, nonce_h, label, host_binding = a_pk || app || account)
//!   wrap   = HKDF(salt = H(transcript), ikm = shared, "wrap")
//!   R shows `code`; the user reads it ->  types the code shown on R into A (A checks it) <-
//!                                         and types the code shown on A into R
//!   <------ 4. Transfer { nonce, XChaCha20-Poly1305(wrap, aad = H(transcript), version || key) }
//! ```
//!
//! Security argument:
//!
//! * Commit-reveal (inherited from `rk-pair/1`). R commits to `(r_pk, nonce_d, label)` before it
//!   sees `a_pk` and `nonce_h`; A picks `a_pk` and `nonce_h` before it sees `nonce_d`. A relay
//!   that substitutes its own keys toward both sides must fix its key toward A before it learns
//!   A's reply, and its key toward R before it learns `nonce_d`, so it cannot steer either code.
//!   The two codes then agree only by chance, 1 in 10^6 per session, and A and R each accept at
//!   most [`MAX_WRONG_CODES`] typed codes per session. The code never leaves a device except on
//!   its screen; it is not sent through the relay.
//! * Both sides must confirm. A confirms before it wraps (so a relay cannot make A release the
//!   key to a substituted key), and R confirms before it opens (so a relay that swapped A's key
//!   cannot hand R a forged data key). Neither `wrap` nor `unwrap` is reachable without the
//!   code check: [`Approver::approve`] and [`Requester::accept`] take the typed code.
//! * The X25519 public keys are in the code: R's key is the `public_key` field of the SAS, A's
//!   key rides in `host_binding`. The transcript hash used as HKDF salt and AEAD associated data
//!   covers both keys, both nonces, the label, the app and the account id, so a transfer cannot
//!   be replayed into another session, account or app.
//! * The data key is wrapped under a key that exists only after an X25519 agreement with fresh
//!   ephemeral keys on both sides (forward secrecy against later key compromise); contributory
//!   checks reject low-order public keys.
//! * The server can drop, delay or replay messages (a denial of service at worst). A replayed
//!   Transfer fails AEAD authentication in any other session.
//!
//! Limits: the code is 20 bits and is safe only because it is checked by a person, online, under
//! the small attempt budget. The relay must expire sessions (a few minutes) and rate-limit
//! `Begin`; that is the relay's job and is not enforced here. A relay can still make a session
//! fail, never succeed wrongly.

use crate::error::{Error, Result};
use crate::key::{check_component, DataKey, KEY_LEN};
use crate::pairing_sas::{
    label_bytes, pairing_code, pairing_commit, CODE_DIGITS, MAX_LABEL_CHARS, NONCE_BYTES,
};
use crate::util::{ct_eq, hex_decode, push_lp, random_array};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, SharedSecret, StaticSecret};
use zeroize::Zeroizing;

/// Name of this protocol, for documentation and relay routing.
pub const PROTOCOL: &str = "rightkit-sync-keys-transfer/1";
/// Wrong codes one device accepts before the session is burned.
pub const MAX_WRONG_CODES: u32 = 3;

const WIRE_VERSION: u8 = 1;
const T_BEGIN: u8 = 1;
const T_OFFER: u8 = 2;
const T_REVEAL: u8 = 3;
const T_TRANSFER: u8 = 4;
const AEAD_NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const PAYLOAD_BYTES: usize = 4 + KEY_LEN;

const HOST_DOMAIN: &[u8] = b"rightkit-sync-keys-transfer/1 host";
const TRANSCRIPT_DOMAIN: &[u8] = b"rightkit-sync-keys-transfer/1 transcript";
const WRAP_INFO: &[u8] = b"rightkit-sync-keys-transfer/1 wrap";

/// What a transfer is for. Both devices must pass the same values; they are bound into the code
/// and the key wrap, so a mismatch fails as a code mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferContext {
    app: String,
    account_id: String,
}

impl TransferContext {
    pub fn new(app: impl Into<String>, account_id: impl Into<String>) -> Result<Self> {
        let (app, account_id) = (app.into(), account_id.into());
        check_component("app", &app)?;
        check_component("account id", &account_id)?;
        Ok(Self { app, account_id })
    }

    fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        push_lp(&mut b, self.app.as_bytes());
        push_lp(&mut b, self.account_id.as_bytes());
        b
    }
}

/// Group a six-digit code for display, `123 456`. [`Approver::approve`] and
/// [`Requester::accept`] accept the grouped or the plain form.
pub fn format_code(code: &str) -> String {
    if code.len() == CODE_DIGITS {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

// ---- shared pieces ----------------------------------------------------------

struct Keys {
    code: Zeroizing<String>,
    wrap: Zeroizing<[u8; KEY_LEN]>,
    transcript: [u8; 32],
}

fn host_binding(a_pk: &[u8; 32], ctx: &TransferContext) -> Vec<u8> {
    let mut b = Vec::new();
    push_lp(&mut b, HOST_DOMAIN);
    push_lp(&mut b, a_pk);
    push_lp(&mut b, &ctx.bytes());
    b
}

fn new_keypair() -> Result<(StaticSecret, [u8; 32])> {
    let seed = Zeroizing::new(random_array::<32>()?);
    let secret = StaticSecret::from(*seed);
    let public = PublicKey::from(&secret).to_bytes();
    Ok((secret, public))
}

fn agree(secret: &StaticSecret, peer: &[u8; 32]) -> Result<SharedSecret> {
    let shared = secret.diffie_hellman(&PublicKey::from(*peer));
    if !shared.was_contributory() {
        return Err(Error::WeakPeerKey);
    }
    Ok(shared)
}

#[allow(clippy::too_many_arguments)]
fn derive(
    r_pk: &[u8; 32],
    a_pk: &[u8; 32],
    nonce_d: &[u8; NONCE_BYTES],
    nonce_h: &[u8; NONCE_BYTES],
    label: Option<&str>,
    ctx: &TransferContext,
    shared: &SharedSecret,
) -> Result<Keys> {
    let code = pairing_code(r_pk, nonce_d, nonce_h, label, &host_binding(a_pk, ctx));
    let mut t = Vec::new();
    push_lp(&mut t, TRANSCRIPT_DOMAIN);
    push_lp(&mut t, r_pk);
    push_lp(&mut t, a_pk);
    push_lp(&mut t, nonce_d);
    push_lp(&mut t, nonce_h);
    push_lp(&mut t, &label_bytes(label));
    push_lp(&mut t, &ctx.bytes());
    let transcript: [u8; 32] = Sha256::digest(&t).into();
    let mut wrap = Zeroizing::new([0u8; KEY_LEN]);
    Hkdf::<Sha256>::new(Some(&transcript[..]), shared.as_bytes())
        .expand(WRAP_INFO, &mut wrap[..])
        .map_err(|_| Error::Protocol("key derivation failed"))?;
    Ok(Keys {
        code: Zeroizing::new(code),
        wrap,
        transcript,
    })
}

/// Digits only, then exactly `CODE_DIGITS`, then constant-time compare.
fn code_matches(want: &str, typed: &str) -> bool {
    let got: String = typed.chars().filter(char::is_ascii_digit).collect();
    got.len() == CODE_DIGITS && ct_eq(got.as_bytes(), want.as_bytes())
}

fn check_label(label: Option<&str>) -> Result<()> {
    if let Some(l) = label {
        if l.chars().count() > MAX_LABEL_CHARS || l.chars().any(char::is_control) {
            return Err(Error::Invalid {
                what: "device label",
                reason: format!("at most {MAX_LABEL_CHARS} characters, no control characters"),
            });
        }
    }
    Ok(())
}

fn seal(key: &[u8; KEY_LEN], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| Error::Authentication)?;
    let nonce = random_array::<AEAD_NONCE_BYTES>()?;
    let ct = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| Error::Authentication)?;
    let mut out = Vec::with_capacity(nonce.len() + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open(
    key: &[u8; KEY_LEN],
    aad: &[u8],
    nonce: &[u8; AEAD_NONCE_BYTES],
    ct: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| Error::Authentication)?;
    cipher
        .decrypt(&XNonce::from(*nonce), Payload { msg: ct, aad })
        .map(Zeroizing::new)
        .map_err(|_| Error::Authentication)
}

// ---- wire encoding ----------------------------------------------------------

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(Error::Protocol("truncated message"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
    fn header(&mut self, ty: u8) -> Result<()> {
        let [version, kind] = self.array::<2>()?;
        if version != WIRE_VERSION {
            return Err(Error::Protocol("unsupported protocol version"));
        }
        if kind != ty {
            return Err(Error::Protocol("unexpected message type"));
        }
        Ok(())
    }
    fn finish(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::Protocol("trailing bytes"))
        }
    }
}

fn encode_begin(r_pk: &[u8; 32], commit: &[u8; 32], label: Option<&str>) -> Vec<u8> {
    let mut m = vec![WIRE_VERSION, T_BEGIN];
    m.extend_from_slice(r_pk);
    m.extend_from_slice(commit);
    match label {
        None => m.push(0),
        Some(l) => {
            m.push(1);
            m.extend_from_slice(&(l.len() as u16).to_be_bytes());
            m.extend_from_slice(l.as_bytes());
        }
    }
    m
}

struct Begin {
    r_pk: [u8; 32],
    commit: [u8; 32],
    label: Option<String>,
}

fn decode_begin(bytes: &[u8]) -> Result<Begin> {
    let mut r = Reader(bytes);
    r.header(T_BEGIN)?;
    let r_pk = r.array::<32>()?;
    let commit = r.array::<32>()?;
    let label = match r.array::<1>()?[0] {
        0 => None,
        1 => {
            let len = u16::from_be_bytes(r.array::<2>()?) as usize;
            let raw = r.take(len)?;
            Some(
                std::str::from_utf8(raw)
                    .map_err(|_| Error::Protocol("label is not UTF-8"))?
                    .to_string(),
            )
        }
        _ => return Err(Error::Protocol("bad label flag")),
    };
    r.finish()?;
    if check_label(label.as_deref()).is_err() {
        return Err(Error::Protocol("invalid device label"));
    }
    Ok(Begin {
        r_pk,
        commit,
        label,
    })
}

fn encode_offer(a_pk: &[u8; 32], nonce_h: &[u8; NONCE_BYTES]) -> Vec<u8> {
    let mut m = vec![WIRE_VERSION, T_OFFER];
    m.extend_from_slice(a_pk);
    m.extend_from_slice(nonce_h);
    m
}

fn encode_reveal(nonce_d: &[u8; NONCE_BYTES]) -> Vec<u8> {
    let mut m = vec![WIRE_VERSION, T_REVEAL];
    m.extend_from_slice(nonce_d);
    m
}

fn encode_transfer(sealed: &[u8]) -> Vec<u8> {
    let mut m = vec![WIRE_VERSION, T_TRANSFER];
    m.extend_from_slice(sealed);
    m
}

fn encode_payload(key: &DataKey) -> Zeroizing<Vec<u8>> {
    let mut p = Zeroizing::new(Vec::with_capacity(PAYLOAD_BYTES));
    p.extend_from_slice(&key.version().to_be_bytes());
    p.extend_from_slice(key.expose_bytes());
    p
}

fn decode_payload(p: &[u8]) -> Result<DataKey> {
    if p.len() != PAYLOAD_BYTES {
        return Err(Error::Protocol("wrapped payload has the wrong length"));
    }
    let version = u32::from_be_bytes([p[0], p[1], p[2], p[3]]);
    let mut bytes = Zeroizing::new([0u8; KEY_LEN]);
    bytes.copy_from_slice(&p[4..]);
    DataKey::from_bytes(version, bytes)
}

// ---- requester (the new device) -------------------------------------------

enum RState {
    /// Begin sent; waiting for the offer.
    Begun,
    /// Offer processed, reveal produced; waiting for the code check and the transfer.
    Offered(Keys),
    Spent,
}

/// The new device's half. One value is one session; it cannot be reused.
pub struct Requester {
    ctx: TransferContext,
    label: Option<String>,
    secret: StaticSecret,
    public: [u8; 32],
    nonce_d: [u8; NONCE_BYTES],
    wrong: u32,
    state: RState,
}

impl Requester {
    /// Start a session. Returns the requester and the `Begin` message for the relay. `label` is
    /// the device name the approver sees (at most 64 characters).
    pub fn begin(ctx: TransferContext, label: Option<&str>) -> Result<(Requester, Vec<u8>)> {
        check_label(label)?;
        let (secret, public) = new_keypair()?;
        let nonce_d = random_array::<NONCE_BYTES>()?;
        let commit_hex = pairing_commit(&public, &nonce_d, label);
        let mut commit = [0u8; 32];
        commit.copy_from_slice(&hex_decode(&commit_hex).ok_or(Error::Protocol("commit"))?);
        let msg = encode_begin(&public, &commit, label);
        Ok((
            Requester {
                ctx,
                label: label.map(str::to_string),
                secret,
                public,
                nonce_d,
                wrong: 0,
                state: RState::Begun,
            },
            msg,
        ))
    }

    /// Process the approver's `Offer`; returns the `Reveal` message to send back. After this,
    /// [`Requester::code`] is available.
    pub fn receive_offer(&mut self, offer: &[u8]) -> Result<Vec<u8>> {
        if !matches!(self.state, RState::Begun) {
            return Err(Error::Protocol("offer out of order"));
        }
        let result = self.process_offer(offer);
        if result.is_err() {
            self.state = RState::Spent;
        }
        result
    }

    fn process_offer(&mut self, offer: &[u8]) -> Result<Vec<u8>> {
        let mut r = Reader(offer);
        r.header(T_OFFER)?;
        let a_pk = r.array::<32>()?;
        let nonce_h = r.array::<NONCE_BYTES>()?;
        r.finish()?;
        let shared = agree(&self.secret, &a_pk)?;
        let keys = derive(
            &self.public,
            &a_pk,
            &self.nonce_d,
            &nonce_h,
            self.label.as_deref(),
            &self.ctx,
            &shared,
        )?;
        self.state = RState::Offered(keys);
        Ok(encode_reveal(&self.nonce_d))
    }

    /// The six-digit code to show the user, to be compared with the approver's screen.
    /// Available after [`Requester::receive_offer`]. Show it only on this device's screen.
    pub fn code(&self) -> Result<String> {
        match &self.state {
            RState::Offered(k) => Ok(k.code.to_string()),
            _ => Err(Error::Protocol("no code yet")),
        }
    }

    /// The user typed the code shown on the approver's screen; if it matches, open the
    /// `Transfer` message and return the data key. A wrong code returns
    /// [`Error::SasMismatch`] without touching the transfer, and [`MAX_WRONG_CODES`] of them
    /// burn the session. Once the code matches the session is spent whatever the transfer
    /// holds: a transfer that fails authentication is [`Error::Authentication`] and a new
    /// session must be started.
    pub fn accept(&mut self, typed_code: &str, transfer: &[u8]) -> Result<DataKey> {
        let matched = match &self.state {
            RState::Offered(k) => code_matches(&k.code, typed_code),
            _ => return Err(Error::Protocol("no offer received or session spent")),
        };
        if !matched {
            self.wrong += 1;
            if self.wrong >= MAX_WRONG_CODES {
                self.state = RState::Spent;
                return Err(Error::TooManyAttempts);
            }
            return Err(Error::SasMismatch {
                remaining: MAX_WRONG_CODES - self.wrong,
            });
        }
        let RState::Offered(keys) = std::mem::replace(&mut self.state, RState::Spent) else {
            return Err(Error::Protocol("no offer received or session spent"));
        };
        let mut r = Reader(transfer);
        r.header(T_TRANSFER)?;
        let nonce = r.array::<AEAD_NONCE_BYTES>()?;
        let ct = r.take(PAYLOAD_BYTES + TAG_BYTES)?;
        r.finish()?;
        let plain = open(&keys.wrap, &keys.transcript, &nonce, ct)?;
        decode_payload(&plain)
    }
}

// ---- approver (the enrolled device) ----------------------------------------

enum AState {
    /// Offer produced; waiting for the reveal.
    Offered,
    /// Commitment verified; waiting for the user's code check.
    Revealed(Keys),
    Spent,
}

/// The enrolled device's half. One value is one session; it cannot be reused.
pub struct Approver {
    ctx: TransferContext,
    r_pk: [u8; 32],
    commit_hex: String,
    label: Option<String>,
    secret: StaticSecret,
    public: [u8; 32],
    nonce_h: [u8; NONCE_BYTES],
    wrong: u32,
    state: AState,
}

impl Approver {
    /// Process the requester's `Begin`; returns the approver and the `Offer` message.
    pub fn receive_begin(ctx: TransferContext, begin: &[u8]) -> Result<(Approver, Vec<u8>)> {
        let b = decode_begin(begin)?;
        let (secret, public) = new_keypair()?;
        let nonce_h = random_array::<NONCE_BYTES>()?;
        let offer = encode_offer(&public, &nonce_h);
        Ok((
            Approver {
                ctx,
                r_pk: b.r_pk,
                commit_hex: crate::util::hex_encode(&b.commit),
                label: b.label,
                secret,
                public,
                nonce_h,
                wrong: 0,
                state: AState::Offered,
            },
            offer,
        ))
    }

    /// The device name the requester announced. Untrusted text for display only; the user's
    /// approval rests on the code, not on this name.
    pub fn requester_label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Process the requester's `Reveal`: check it against the commitment and compute the code.
    /// A mismatch burns the session ([`Error::CommitmentMismatch`]).
    pub fn receive_reveal(&mut self, reveal: &[u8]) -> Result<()> {
        if !matches!(self.state, AState::Offered) {
            return Err(Error::Protocol("reveal out of order"));
        }
        let result = self.process_reveal(reveal);
        match result {
            Ok(keys) => {
                self.state = AState::Revealed(keys);
                Ok(())
            }
            Err(e) => {
                self.state = AState::Spent;
                Err(e)
            }
        }
    }

    fn process_reveal(&self, reveal: &[u8]) -> Result<Keys> {
        let mut r = Reader(reveal);
        r.header(T_REVEAL)?;
        let nonce_d = r.array::<NONCE_BYTES>()?;
        r.finish()?;
        let expected = pairing_commit(&self.r_pk, &nonce_d, self.label.as_deref());
        if !ct_eq(expected.as_bytes(), self.commit_hex.as_bytes()) {
            return Err(Error::CommitmentMismatch);
        }
        let shared = agree(&self.secret, &self.r_pk)?;
        derive(
            &self.r_pk,
            &self.public,
            &nonce_d,
            &self.nonce_h,
            self.label.as_deref(),
            &self.ctx,
            &shared,
        )
    }

    /// The six-digit code to show the user, to be compared with the requester's screen.
    /// Available after [`Approver::receive_reveal`]. Show it only on this device's screen.
    pub fn code(&self) -> Result<String> {
        match &self.state {
            AState::Revealed(k) => Ok(k.code.to_string()),
            _ => Err(Error::Protocol("no code yet")),
        }
    }

    /// The user typed the code shown on the requester's screen; if it matches, wrap `key` for
    /// the requester and return the `Transfer` message. A wrong code returns
    /// [`Error::SasMismatch`] and wraps nothing; [`MAX_WRONG_CODES`] of them burn the session.
    /// A successful call spends the session: one transfer per session.
    pub fn approve(&mut self, typed_code: &str, key: &DataKey) -> Result<Vec<u8>> {
        let matched = match &self.state {
            AState::Revealed(k) => code_matches(&k.code, typed_code),
            _ => return Err(Error::Protocol("no reveal received or session spent")),
        };
        if !matched {
            self.wrong += 1;
            if self.wrong >= MAX_WRONG_CODES {
                self.state = AState::Spent;
                return Err(Error::TooManyAttempts);
            }
            return Err(Error::SasMismatch {
                remaining: MAX_WRONG_CODES - self.wrong,
            });
        }
        let AState::Revealed(keys) = std::mem::replace(&mut self.state, AState::Spent) else {
            return Err(Error::Protocol("no reveal received or session spent"));
        };
        let payload = encode_payload(key);
        let sealed = seal(&keys.wrap, &keys.transcript, &payload)?;
        Ok(encode_transfer(&sealed))
    }
}
