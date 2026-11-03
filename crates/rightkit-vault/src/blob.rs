//! Blob format and crypto. Pure functions, no I/O.
//!
//! The same construction as `rightkit-sync`'s `cipher` module (ChaCha20-Poly1305 and
//! HKDF-SHA256 from `ring`, a random salt per message from which a one-use key and nonce are
//! derived, length-prefixed associated data), with one addition: the key that wraps the data
//! key mixes two inputs.
//!
//! ```text
//! dek        = 32 random bytes, fresh on every save
//! data       = salt_d(16) || ChaCha20-Poly1305(HKDF(ikm = dek, salt = salt_d, info = data label))
//! wrapped    = salt_w(16) || ChaCha20-Poly1305(HKDF(ikm = server_key || client_secret,
//!                                               salt = salt_w, info = wrap label, app, subject, key id))
//! ```
//!
//! Each HKDF call yields 44 bytes: a 32-byte key and a 12-byte nonce used once. Associated data
//! binds the format version, app, subject and (for the wrap) the key id, so a blob cannot be
//! moved to another app or account, relabelled to another key id, or downgraded.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::hkdf;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::keys::{ClientSecret, ServerKey, KEY_LEN};

pub const VERSION: u32 = 1;
const SALT_LEN: usize = 16;
const TAG_LEN: usize = 16;
const OKM_LEN: usize = 44;
const WRAP_LABEL: &[u8] = b"rightkit-vault/1/wrap";
const DATA_LABEL: &[u8] = b"rightkit-vault/1/data";
/// `salt || DEK || tag`.
const WRAPPED_LEN: usize = SALT_LEN + KEY_LEN + TAG_LEN;

/// What is stored in the cloud folder (JSON). Everything here is public by design except what
/// the two ciphertext fields protect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blob {
    pub v: u32,
    pub app: String,
    pub subject: String,
    /// Id of the server key the DEK is wrapped under.
    pub key_id: String,
    /// base64(salt || sealed DEK).
    pub wrapped_dek: String,
    /// base64(salt || sealed payload).
    pub data: String,
}

struct Len(usize);
impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| Error::Entropy)?;
    Ok(b)
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn wrap_info(app: &str, subject: &str, key_id: &str) -> Vec<u8> {
    let mut v = Vec::new();
    put(&mut v, WRAP_LABEL);
    put(&mut v, app.as_bytes());
    put(&mut v, subject.as_bytes());
    put(&mut v, key_id.as_bytes());
    v
}

fn data_info() -> Vec<u8> {
    let mut v = Vec::new();
    put(&mut v, DATA_LABEL);
    v
}

fn wrap_aad(app: &str, subject: &str, key_id: &str) -> Vec<u8> {
    let mut v = Vec::new();
    put(&mut v, WRAP_LABEL);
    v.extend_from_slice(&VERSION.to_be_bytes());
    put(&mut v, app.as_bytes());
    put(&mut v, subject.as_bytes());
    put(&mut v, key_id.as_bytes());
    v
}

/// The payload AAD omits the key id so a re-wrap leaves the payload ciphertext untouched.
fn data_aad(app: &str, subject: &str) -> Vec<u8> {
    let mut v = Vec::new();
    put(&mut v, DATA_LABEL);
    v.extend_from_slice(&VERSION.to_be_bytes());
    put(&mut v, app.as_bytes());
    put(&mut v, subject.as_bytes());
    v
}

fn derive(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<Zeroizing<[u8; OKM_LEN]>> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let parts: [&[u8]; 1] = [info];
    let okm = prk
        .expand(&parts, Len(OKM_LEN))
        .map_err(|_| Error::Crypto)?;
    let mut out = Zeroizing::new([0u8; OKM_LEN]);
    okm.fill(&mut out[..]).map_err(|_| Error::Crypto)?;
    Ok(out)
}

fn cipher(okm: &[u8; OKM_LEN]) -> Result<(LessSafeKey, Nonce)> {
    let key = LessSafeKey::new(
        UnboundKey::new(&aead::CHACHA20_POLY1305, &okm[..32]).map_err(|_| Error::Crypto)?,
    );
    let nonce = Nonce::try_assume_unique_for_key(&okm[32..]).map_err(|_| Error::Crypto)?;
    Ok((key, nonce))
}

/// `salt || ciphertext || tag`.
fn seal_with(ikm: &[u8], info: &[u8], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let salt = random::<SALT_LEN>()?;
    let okm = derive(ikm, &salt, info)?;
    let (key, nonce) = cipher(&okm)?;
    let mut buf = Zeroizing::new(Vec::with_capacity(plaintext.len() + TAG_LEN));
    buf.extend_from_slice(plaintext);
    key.seal_in_place_append_tag(nonce, Aad::from(aad), &mut *buf)
        .map_err(|_| Error::Crypto)?;
    let mut out = Vec::with_capacity(SALT_LEN + buf.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&buf);
    Ok(out)
}

fn open_with(ikm: &[u8], info: &[u8], aad: &[u8], envelope: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if envelope.len() < SALT_LEN + TAG_LEN {
        return Err(Error::Format("sealed field too short".into()));
    }
    let (salt, sealed) = envelope.split_at(SALT_LEN);
    let okm = derive(ikm, salt, info)?;
    let (key, nonce) = cipher(&okm)?;
    let mut buf = Zeroizing::new(sealed.to_vec());
    let n = key
        .open_in_place(nonce, Aad::from(aad), &mut buf[..])
        .map_err(|_| Error::Decrypt)?
        .len();
    buf.truncate(n);
    Ok(buf)
}

/// `server_key || client_secret`: the wrap-key input. Neither half alone is the key.
fn wrap_ikm(key: &ServerKey, client: &ClientSecret) -> Zeroizing<[u8; 2 * KEY_LEN]> {
    let mut ikm = Zeroizing::new([0u8; 2 * KEY_LEN]);
    ikm[..KEY_LEN].copy_from_slice(key.bytes());
    ikm[KEY_LEN..].copy_from_slice(client.as_bytes());
    ikm
}

fn wrap_dek(
    dek: &[u8; KEY_LEN],
    key: &ServerKey,
    client: &ClientSecret,
    app: &str,
    subject: &str,
) -> Result<String> {
    let ikm = wrap_ikm(key, client);
    let envelope = seal_with(
        &ikm[..],
        &wrap_info(app, subject, key.id()),
        &wrap_aad(app, subject, key.id()),
        dek,
    )?;
    Ok(B64.encode(envelope))
}

fn unwrap_dek(
    blob: &Blob,
    key: &ServerKey,
    client: &ClientSecret,
) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let envelope = B64
        .decode(&blob.wrapped_dek)
        .map_err(|_| Error::Format("wrapped_dek is not base64".into()))?;
    if envelope.len() != WRAPPED_LEN {
        return Err(Error::Format("wrapped_dek length".into()));
    }
    let ikm = wrap_ikm(key, client);
    let dek = open_with(
        &ikm[..],
        &wrap_info(&blob.app, &blob.subject, key.id()),
        &wrap_aad(&blob.app, &blob.subject, key.id()),
        &envelope,
    )?;
    let arr: [u8; KEY_LEN] = dek
        .as_slice()
        .try_into()
        .map_err(|_| Error::Format("dek length".into()))?;
    Ok(Zeroizing::new(arr))
}

fn check(blob: &Blob, app: &str, subject: &str) -> Result<()> {
    if blob.v != VERSION {
        return Err(Error::Version(blob.v));
    }
    if blob.app != app || blob.subject != subject {
        return Err(Error::Decrypt);
    }
    Ok(())
}

/// Seal `plaintext` under a FRESH random DEK and wrap that DEK under `key` + `client`.
pub fn seal_blob(
    plaintext: &[u8],
    key: &ServerKey,
    client: &ClientSecret,
    app: &str,
    subject: &str,
) -> Result<Blob> {
    let dek = Zeroizing::new(random::<KEY_LEN>()?);
    let data = seal_with(&dek[..], &data_info(), &data_aad(app, subject), plaintext)?;
    Ok(Blob {
        v: VERSION,
        app: app.into(),
        subject: subject.into(),
        key_id: key.id().to_string(),
        wrapped_dek: wrap_dek(&dek, key, client, app, subject)?,
        data: B64.encode(data),
    })
}

/// Open a blob. `app`/`subject` are the caller's expected identity, not the blob's claims.
pub fn open_blob(
    blob: &Blob,
    key: &ServerKey,
    client: &ClientSecret,
    app: &str,
    subject: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    check(blob, app, subject)?;
    if blob.key_id != key.id() {
        return Err(Error::Decrypt);
    }
    let dek = unwrap_dek(blob, key, client)?;
    let data = B64
        .decode(&blob.data)
        .map_err(|_| Error::Format("data is not base64".into()))?;
    open_with(&dek[..], &data_info(), &data_aad(app, subject), &data)
}

/// Rotation: unwrap the DEK with `old`, re-wrap under `new`. The payload is untouched.
pub fn rewrap(
    blob: &Blob,
    old: &ServerKey,
    new: &ServerKey,
    client: &ClientSecret,
    app: &str,
    subject: &str,
) -> Result<Blob> {
    check(blob, app, subject)?;
    if blob.key_id != old.id() {
        return Err(Error::Decrypt);
    }
    let dek = unwrap_dek(blob, old, client)?;
    let mut out = blob.clone();
    out.key_id = new.id().to_string();
    out.wrapped_dek = wrap_dek(&dek, new, client, app, subject)?;
    Ok(out)
}
