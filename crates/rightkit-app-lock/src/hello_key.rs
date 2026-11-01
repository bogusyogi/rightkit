//! Windows Hello as the owner check for adding a device or pairing a phone (Windows only).
//!
//! The application holds a Hello `KeyCredential` (RSA-2048) under a name it chooses. The engine,
//! not this module, decides: it issues a single-use challenge (`device.owner.challenge`), this
//! module has Hello sign the engine's `approval_sig` v1 lines (kind `device`, RSASSA-PKCS1-v1_5
//! SHA-256, which Hello applies to the bytes it is given) and the engine verifies the signature
//! against the enrolled key. Nothing here is trusted: a failure only means the caller falls back
//! to the PIN. The key is enrolled once; creating it prompts Hello, and every signature prompts
//! Hello again (face, fingerprint or Hello PIN).

use serde_json::Value;

/// The engine calls this module makes.
pub trait OwnerEngine {
    /// Calls one engine method; `Err` carries the reason.
    fn call(&self, method: &str, params: Value) -> Result<Value, String>;
}

/// What this module needs from the application.
pub struct Ctx<'a> {
    /// The engine connection.
    pub engine: &'a dyn OwnerEngine,
    /// The name of the Hello credential, such as `app-owner-v1`.
    pub credential: &'a str,
}

use rightkit_owner_approvals::approval_sig::{
    bind_action, enroll_action, fresh_nonce, key_id_of, message, HELLO_DEVICE,
};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use windows::core::HSTRING;
use windows::Security::Credentials::{
    KeyCredential, KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialStatus,
};
use windows::Security::Cryptography::Core::CryptographicPublicKeyBlobType;
use windows::Storage::Streams::{DataReader, DataWriter, IBuffer};

/// How long a signature stays valid, seconds (the engine allows 120 at most).
const LIFETIME: u64 = 100;

fn err(e: windows::core::Error) -> String {
    format!("Windows Hello: {}", e.message())
}

fn buffer(bytes: &[u8]) -> Result<IBuffer, String> {
    let writer = DataWriter::new().map_err(err)?;
    writer.WriteBytes(bytes).map_err(err)?;
    writer.DetachBuffer().map_err(err)
}

fn bytes(buffer: &IBuffer) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; buffer.Length().map_err(err)? as usize];
    let reader = DataReader::FromBuffer(buffer).map_err(err)?;
    reader.ReadBytes(&mut out).map_err(err)?;
    Ok(out)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn nonce() -> Result<String, String> {
    fresh_nonce().ok_or_else(|| "no random nonce".to_string())
}

fn public_key(credential: &KeyCredential) -> Result<Vec<u8>, String> {
    let key = credential
        .RetrievePublicKeyWithBlobType(CryptographicPublicKeyBlobType::Pkcs1RsaPublicKey)
        .map_err(err)?;
    bytes(&key)
}

/// Has Hello sign one `approval_sig` envelope with `credential`; returns the `sig` the engine reads.
fn sign(
    credential: &KeyCredential,
    public: &[u8],
    kind: &str,
    request: &str,
    action: &str,
    client: &str,
) -> Result<Value, String> {
    let key_id = key_id_of(public);
    let nonce = nonce()?;
    let issued = now();
    let expires = issued + LIFETIME;
    let text = message(
        kind,
        request,
        action,
        client,
        HELLO_DEVICE,
        &key_id,
        &nonce,
        issued,
        expires,
    );
    let signed = credential
        .RequestSignAsync(&buffer(&text)?)
        .map_err(err)?
        .join()
        .map_err(err)?;
    if signed.Status().map_err(err)? != KeyCredentialStatus::Success {
        return Err("Windows Hello did not confirm".to_string());
    }
    let signature = bytes(&signed.Result().map_err(err)?)?;
    Ok(json!({
        "keyId": key_id, "nonce": nonce, "issued": issued, "expires": expires,
        "signature": rightkit_fs::digest::hex(&signature),
    }))
}

fn open(ctx: &Ctx) -> Result<KeyCredential, String> {
    let opened = KeyCredentialManager::OpenAsync(&HSTRING::from(ctx.credential))
        .map_err(err)?
        .join()
        .map_err(err)?;
    if opened.Status().map_err(err)? != KeyCredentialStatus::Success {
        return Err("no Windows Hello key".to_string());
    }
    opened.Credential().map_err(err)
}

/// Whether this computer can use Hello keys.
fn supported() -> bool {
    KeyCredentialManager::IsSupportedAsync()
        .and_then(|a| a.join())
        .unwrap_or(false)
}

/// The key id of the Hello key the engine has enrolled, if any, and whether the engine holds its
/// binding to this computer's device.
fn enrolled_key(ctx: &Ctx) -> Option<(String, bool)> {
    let status = ctx.engine.call("remote.owner.key.status", json!({})).ok()?;
    status["keys"].as_array()?.iter().find_map(|k| {
        (k["device"] == HELLO_DEVICE)
            .then(|| {
                k["keyId"]
                    .as_str()
                    .map(|id| (id.to_string(), k["bound"] == true))
            })
            .flatten()
    })
}

/// The Hello key's binding to this computer's device: the key itself signs (kind `bind`) the device
/// id, the device's identity key and its own public key (`approval_sig::bind_action`), so no other
/// device can claim it. `None` when the engine cannot say which device this is (the key then stays
/// unbound and every receiver ignores it).
fn binding(ctx: &Ctx, credential: &KeyCredential, public: &[u8]) -> Result<Option<Value>, String> {
    let Ok(identity) = ctx.engine.call("device.identity", json!({})) else {
        return Ok(None);
    };
    let (Some(device_id), Some(device_key)) = (
        identity["deviceId"].as_str(),
        identity["endpointId"].as_str(),
    ) else {
        return Ok(None);
    };
    let action = bind_action(
        device_id,
        device_key,
        HELLO_DEVICE,
        &rightkit_fs::digest::hex(public),
    );
    let mut signed = sign(credential, public, "bind", "bind", &action, "")?;
    signed["deviceId"] = json!(device_id);
    signed["deviceKey"] = json!(device_key);
    Ok(Some(signed))
}

/// Makes (replacing any earlier) the Hello key (creating it prompts Hello), signs its proof of
/// possession and enrols it with the engine (`remote.owner.key.enrol.local`: no PIN; the engine
/// checks that this process is the application's trusted shell). Returns the credential.
fn enrol(ctx: &Ctx) -> Result<KeyCredential, String> {
    let created = KeyCredentialManager::RequestCreateAsync(
        &HSTRING::from(ctx.credential),
        KeyCredentialCreationOption::ReplaceExisting,
    )
    .map_err(err)?
    .join()
    .map_err(err)?;
    if created.Status().map_err(err)? != KeyCredentialStatus::Success {
        return Err("Windows Hello did not create the key".to_string());
    }
    let credential = created.Credential().map_err(err)?;
    let public = public_key(&credential)?;
    let action = enroll_action(&public);
    let proof = sign(&credential, &public, "enroll", "enroll", &action, "")?;
    ctx.engine.call(
        "remote.owner.key.enrol.local",
        json!({
            "publicKey": rightkit_fs::digest::hex(&public),
            "device": HELLO_DEVICE,
            "proof": proof,
            "binding": binding(ctx, &credential, &public)?,
        }),
    )?;
    Ok(credential)
}

/// Gives an already enrolled Hello key (one enrolled before bindings existed) its binding to this
/// computer, with one Hello prompt and no operation: `remote.owner.key.bind.local`. Nothing
/// happens when the key is bound or none is enrolled. The engine verifies the signature.
pub fn bind_key(ctx: &Ctx) -> Result<Value, String> {
    if !supported() {
        return Err("Windows Hello is not set up on this computer".to_string());
    }
    let Some((id, bound)) = enrolled_key(ctx) else {
        return Ok(json!({ "bound": false, "reason": "no_key" }));
    };
    if bound {
        return Ok(json!({ "bound": true }));
    }
    let credential = open(ctx)?;
    let public = public_key(&credential)?;
    if key_id_of(&public) != id {
        return Err("The enrolled key is not this computer's Hello key".to_string());
    }
    let params = json!({
        "publicKey": rightkit_fs::digest::hex(&public),
        "device": HELLO_DEVICE,
        "binding": binding(ctx, &credential, &public)?
            .ok_or("The engine cannot say which device this is".to_string())?,
    });
    ctx.engine.call("remote.owner.key.bind.local", params)?;
    Ok(json!({ "bound": true }))
}

/// The owner proof for `op` (`join`, or `approve` with `pairing_id`): `{challenge, sig}` for the
/// device command, from Hello prompts: one to sign, and on first use one more to create and enrol
/// the key. Err when Hello is unavailable or did not confirm; the caller then sends no proof and
/// the page asks for the PIN.
pub fn proof(ctx: &Ctx, op: &str, pairing_id: Option<&str>, scope: Value) -> Result<Value, String> {
    if !supported() {
        return Err("Windows Hello is not set up on this computer".to_string());
    }
    let enrolled = enrolled_key(ctx);
    let existing = open(ctx).ok().filter(|c| {
        public_key(c).is_ok_and(|p| {
            enrolled
                .as_ref()
                .is_some_and(|(id, _)| *id == key_id_of(&p))
        })
    });
    let bound = enrolled.as_ref().is_some_and(|(_, bound)| *bound);
    let fresh = existing.is_none();
    let credential = match existing {
        Some(c) => c,
        None => enrol(ctx)?,
    };
    let public = public_key(&credential)?;
    if !bound && !fresh {
        // A key enrolled before bindings existed: this owner check creates its binding once.
        if let Ok(Some(binding)) = binding(ctx, &credential, &public) {
            let _ = ctx.engine.call(
                "remote.owner.key.bind.local",
                json!({
                    "publicKey": rightkit_fs::digest::hex(&public),
                    "device": HELLO_DEVICE,
                    "binding": binding,
                }),
            );
        }
    }
    let issued = ctx.engine.call(
        "device.owner.challenge",
        json!({ "op": op, "pairingId": pairing_id, "guest": scope["guest"], "leaseMs": scope["leaseMs"] }),
    )?;
    let text = |k: &str| {
        issued[k]
            .as_str()
            .map(str::to_string)
            .ok_or("bad challenge")
    };
    let (challenge, action, client) = (text("challenge")?, text("action")?, text("client")?);
    let sig = sign(&credential, &public, "device", &challenge, &action, &client)?;
    Ok(json!({ "challenge": challenge, "sig": sig }))
}
