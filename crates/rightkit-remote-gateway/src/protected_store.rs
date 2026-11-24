//! The engine's own copy of the owner-only security settings (`settings::PROTECTED_KEYS`).
//!
//! `owner app-state.json` is a file any process of the same user can write, so a gate read from it
//! proves nothing. The engine therefore keeps the protected values itself and ignores the file for
//! them (`Settings::load` calls `apply`):
//!
//! * **Who writes.** Only this module, through `set`, which `owner_settings::request` calls after
//!   the owner path (`remote.settings.set`: the hub's or owner app's executable verified, and for a
//!   change that loosens a gate the owner check: a signature of an enrolled owner-check key, else
//!   the host app PIN). The owner app still receives each change through
//!   `remote.settings.take` and applies it to its own preferences and to the file it publishes, but
//!   that file and those preferences are a display of the value, not the gate.
//! * **Where it lives.** One item in the OS credential store (`rightkit-credentials`, service
//!   `pulse.protected-settings`), created by the engine with the store's default access control.
//!   A same-user process can still delete it or write one of its own, so the engine does not trust
//!   an item by name (`protected_guard.rs`): on macOS it refuses an item whose keychain partition
//!   list is not the one the host app's signed code gets, and on Windows the blob carries an HMAC whose key
//!   sits in a DPAPI-protected file (wrapped with PIN-derived entropy once a PIN is set). macOS
//!   carries the same HMAC seal (key in `protected-settings.seal`, wrapped with a PBKDF2 key from the
//!   PIN once one is set), because a same-user process can make a keychain of its own with the right
//!   partition list and move it onto the login keychain's path. A blob whose seal is not proven is
//!   read through `tighten_only`: closed defaults, plus only the values that are safer than the
//!   defaults. With a PIN set, a gate the PIN loosened reads off after every engine restart until
//!   the PIN has been verified once (`pin_verified`); Touch ID or Windows Hello alone does not
//!   reopen it. A gate the system check loosened (below) needs no PIN.
//! * **Owner-check proofs (blob version 3).** A value an owner-check signature loosened is stored
//!   in the blob's `owner` object as `{ "value", "proof" }`, outside the sealed `values`, so the
//!   seal's PIN rule does not touch it. The proof is the `settings` signature the engine verified
//!   when it took the change (`unlock::authorise_settings`): the device, public key, key id,
//!   challenge, client, nonce, window and signature. **Load-time rule.** A value in `owner` is
//!   accepted when it is looser than the defaults, still shaped as `check_shape` wants, and its
//!   proof passes `approval_sig::stored_owner_proofs_valid`: a key with that id and public key is
//!   enrolled now and signed kind `settings` over `owner_settings::settings_action(key, value)`
//!   (so the proof commits to exactly that key and value). Nothing else counts: an entry that fails
//!   any of this is dropped, so the gate reads at its closed default. A stored proof has no live
//!   challenge to check, so what the live check does (a single-use challenge, the signature's
//!   window, a fresh nonce) is replaced by this: the proof is good for exactly that key and value
//!   for as long as the signing key stays enrolled. Revoking or replacing the key voids every such
//!   value; whoever can write the credential store can put back a proof the owner once made (a
//!   rollback of a value the owner once chose, never a value of their own making), the same reach
//!   as putting back an older sealed blob. Tightening a key drops its proof. A value the PIN
//!   loosened stays in the sealed `values` as before. Version 2 blobs load unchanged.
//! * **A PIN gate waiting for the PIN.** With a PIN set and not entered in this process the seal key
//!   cannot be opened, so a write cannot seal. The loaded blob's loosened PIN values are then kept
//!   raw in `carried` (never trusted: they count only after the PIN is entered and their own seal
//!   verifies, `parse`), and every later write puts them back untouched, so a change confirmed by the
//!   system check never costs the owner a gate they set with the PIN. A key written since is
//!   listed in `superseded` beside the waiting values (never edited into them: that would break
//!   their seal) and is not taken back when the PIN is entered.
//! * **Owner entries that cannot be checked yet.** When the owner-check key store cannot be read or
//!   is not proven (the elevated Windows service unreachable), or policy refuses every stored proof
//!   (`approval_sig::stored_owner_proofs_valid`: Windows without the service and with a PIN set),
//!   the `owner` entries are kept raw in `pending_owner`: never trusted, written back untouched, tried
//!   again at most every 10 seconds (`revalidate`), and removed by a value written for their key.
//! * **First run.** When there is no item and no marker file, the store starts at the closed
//!   defaults, stores them once, and writes the marker file `protected-settings.marker` beside the
//!   owner app's data. A missing item with the marker present is a deleted item: nothing is stored
//!   again and the engine fails closed until the owner recovers it (`recover`, after Touch ID or
//!   Windows Hello in owner app).
//! * **Fail closed.** When the item cannot be read, is not trusted or is not valid, `values` is
//!   `None` and `apply` leaves every protected key out: switches are off, lists are empty,
//!   `chatUnlock` is on, and `phoneChatReadAll` is on (the owner's own paired phone reads every
//!   chat; only that phone gains, #579 D9).
//! * **Other processes.** The gateway and the CLI run tools in their own process and ask the
//!   resident engine (`remote.settings.protected`), after the OS has said the answering process runs
//!   the engine's executable (`approvals::ask_engine_verified`); an engine that is not running, or
//!   not the engine, is a fail closed.
//! * **The PIN marker.** The item also records that a host app PIN was set, so deleting the PIN's own
//!   credential item does not let the PIN be reset without the old one (`unlock.rs`). This holds
//!   only while the engine's item survives: deleting the item, the marker file and the PIN item together is a first run with
//!   `pin_set` false, so the PIN reads as unset and can be set again. Nothing loosens (every gate is
//!   back at its default, off), but the PIN is then unset, not lost.

use super::approval_sig::{self, OwnerCheckProof};
use super::protected_guard::{self, Seal};
use super::{Refusal, Result, approvals, owner_settings, settings};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SERVICE: &str = "pulse.protected-settings";
const ACCOUNT: &str = "values-v2";
const MARKER_FILE: &str = "protected-settings.marker";
/// The most the item may hold; a larger or malformed item is not trusted.
const MAX_BYTES: usize = 256 * 1024;
/// The most `owner` entries read from one blob (there are fewer protected keys than this).
const MAX_OWNER_ENTRIES: usize = 64;
/// The longest text of one proof field (a hex public key or signature is well under this).
const MAX_PROOF_FIELD: usize = 600;
const DAMAGED: &str = "the stored settings are damaged";
/// The longest a `superseded` list read from a blob may be.
const MAX_SUPERSEDED: usize = 256;
/// How often owner entries that could not be checked are tried again while the engine runs.
const REVALIDATE_EVERY: Duration = Duration::from_secs(10);

/// The loosened values the PIN set, as the last blob held them, kept raw while the PIN is not
/// entered in this process: not trusted, put back untouched by every write, taken back by `parse`
/// once the PIN key is open and `mac` (the seal over exactly these values) verifies.
#[derive(Clone, Debug)]
struct Carried {
    pin_set: bool,
    values: Map<String, Value>,
    mac: String,
    /// The keys written since this part was set aside. They are kept beside `values` (editing
    /// `values` would break `mac`) and never taken back, so a value the owner replaced is not
    /// resurrected when the PIN is entered.
    superseded: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct Stored {
    /// Every value in effect, the owner-checked ones included.
    values: Map<String, Value>,
    pin_set: bool,
    /// The blob's seal was not proven, so `values` went through `tighten_only` (macOS, Windows).
    unproven: bool,
    /// The proof of each value in `values` that an owner-check signature loosened; those values are
    /// written to the blob's `owner` object, not to the sealed `values`.
    proofs: BTreeMap<String, OwnerCheckProof>,
    /// PIN-loosened values waiting for the PIN (see `Carried`).
    carried: Option<Carried>,
    /// This load settled a `carried` part, so the blob is written again once the PIN key can seal
    /// (`settle`).
    rewrite: bool,
    /// `owner` entries that could not be checked (the owner-check key store was unreadable or not
    /// proven, or policy refused every proof): the entries as the blob held them. Never trusted,
    /// written back untouched, and tried again by `revalidate`. A value written since removes its
    /// entry.
    pending_owner: Map<String, Value>,
    /// When `pending_owner` was last tried, to space out the retries.
    checked_at: Option<Instant>,
}

/// The engine's copy, once loaded. Only the engine process fills it.
static CACHE: Mutex<Option<Stored>> = Mutex::new(None);

fn unavailable(why: impl std::fmt::Display) -> Refusal {
    Refusal::new(
        "settings_unavailable",
        format!(
            "the app's own copy of the security settings cannot be read ({why}), so every protected gate stays off until it can"
        ),
    )
}

fn open() -> std::result::Result<rightkit_credentials::Store, String> {
    let namespace =
        rightkit_credentials::Namespace::new(SERVICE.to_string()).map_err(|e| e.to_string())?;
    let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
    if !store.is_durable() {
        return Err("the OS credential store is guarded or unavailable".into());
    }
    Ok(store)
}

fn marker_path() -> Option<std::path::PathBuf> {
    rightkit_app_host::state_directory()
        .ok()
        .map(|dir| dir.join(MARKER_FILE))
}

/// Keeps only the values that are no looser than the defaults (gates off, `chatUnlock` on, no
/// roots, no agent path). What a tampered, recreated or migrated blob can say is bounded by this.
fn tighten_only(values: Map<String, Value>) -> Map<String, Value> {
    values
        .into_iter()
        .filter(|(key, value)| !owner_settings::loosens(key, value, None))
        .collect()
}

/// The bytes the seal covers: the version, the PIN flag and every value, keys in sorted order.
fn seal_message(pin_set: bool, values: &Map<String, Value>) -> String {
    let mut keys: Vec<&String> = values.keys().collect();
    keys.sort();
    let mut out = format!("pulse.protected.v2|{pin_set}\n");
    for key in keys {
        out.push_str(&format!("{key}={}\n", values[key]));
    }
    out
}

/// A set of values as the blob holds them, every key shape-checked: one key that is not protected
/// or not well formed is an error (the whole set is dropped).
fn read_values(object: &Value) -> std::result::Result<Map<String, Value>, String> {
    let mut values = Map::new();
    for (key, raw) in object.as_object().ok_or_else(|| DAMAGED.to_string())? {
        if !settings::is_protected(key) {
            return Err(DAMAGED.to_string());
        }
        let checked = owner_settings::check_shape(key, raw).map_err(|_| DAMAGED.to_string())?;
        values.insert(key.clone(), checked);
    }
    Ok(values)
}

/// The blob's `carried` part, when it has a well formed one.
fn read_carried(blob: &Value) -> Option<Carried> {
    let raw = blob.get("carried")?;
    Some(Carried {
        pin_set: raw["pinSet"].as_bool() == Some(true),
        values: read_values(&raw["values"]).ok()?,
        mac: raw["mac"].as_str()?.to_string(),
        superseded: raw["superseded"]
            .as_array()
            .map(|keys| {
                keys.iter()
                    .filter_map(|k| k.as_str())
                    .take(MAX_SUPERSEDED)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn proof_json(proof: &OwnerCheckProof) -> Value {
    json!({
        "device": proof.device,
        "publicKey": proof.public_key,
        "challenge": proof.challenge,
        "client": proof.client,
        "keyId": proof.key_id,
        "nonce": proof.nonce,
        "issued": proof.issued,
        "expires": proof.expires,
        "signature": proof.signature,
    })
}

/// One stored proof, when every field is there and none is oversized. Nothing here trusts it:
/// `approval_sig::stored_owner_proofs_valid` does that.
fn read_proof(value: &Value) -> Option<OwnerCheckProof> {
    let text = |k: &str| {
        value[k]
            .as_str()
            .filter(|t| t.len() <= MAX_PROOF_FIELD)
            .map(str::to_string)
    };
    Some(OwnerCheckProof {
        device: text("device")?,
        public_key: text("publicKey")?,
        challenge: text("challenge")?,
        client: text("client")?,
        key_id: text("keyId")?,
        nonce: text("nonce")?,
        issued: value["issued"].as_u64()?,
        expires: value["expires"].as_u64()?,
        signature: text("signature")?,
        binding: None,
    })
}

/// One `owner` entry that could count, with the entry as the blob held it.
struct Owned {
    key: String,
    value: Value,
    proof: OwnerCheckProof,
    raw: Value,
}

/// The blob's `owner` entries that could count: a protected key, a value that is well formed and
/// looser than the defaults, and a proof with every field. An entry that is anything else is
/// dropped on its own (it reads as the closed default).
fn read_owner(blob: &Value) -> Vec<Owned> {
    let Some(entries) = blob["owner"].as_object() else {
        return Vec::new();
    };
    entries
        .iter()
        .take(MAX_OWNER_ENTRIES)
        .filter_map(|(key, entry)| {
            if !settings::is_protected(key) {
                return None;
            }
            let value = owner_settings::check_shape(key, &entry["value"]).ok()?;
            if !owner_settings::loosens(key, &value, None) {
                return None;
            }
            Some(Owned {
                key: key.clone(),
                value,
                proof: read_proof(&entry["proof"])?,
                raw: entry.clone(),
            })
        })
        .collect()
}

/// Judges `owned` (the load-time rule): an entry whose proof holds goes into `values` and `proofs`;
/// one whose key was revoked or replaced is dropped; when the proofs cannot be judged now
/// (`approval_sig::stored_owner_proofs_valid` says `None`) every entry waits, raw, in `pending`.
fn admit_owned(
    owned: Vec<Owned>,
    values: &mut Map<String, Value>,
    proofs: &mut BTreeMap<String, OwnerCheckProof>,
    pending: &mut Map<String, Value>,
) {
    if owned.is_empty() {
        return;
    }
    let verdict = {
        let actions: Vec<String> = owned
            .iter()
            .map(|o| owner_settings::settings_action(&o.key, &o.value))
            .collect();
        let pairs: Vec<(&OwnerCheckProof, &str)> = owned
            .iter()
            .zip(&actions)
            .map(|(o, action)| (&o.proof, action.as_str()))
            .collect();
        approval_sig::stored_owner_proofs_valid(owner_settings::SETTINGS_PROOF_KIND, &pairs)
    };
    match verdict {
        Some(holds) => {
            for (o, holds) in owned.into_iter().zip(holds) {
                if holds {
                    values.insert(o.key.clone(), o.value);
                    proofs.insert(o.key, o.proof);
                }
            }
        }
        None => {
            for o in owned {
                pending.insert(o.key, o.raw);
            }
        }
    }
}

/// Tries the `pending_owner` entries again, at most every `REVALIDATE_EVERY`: the key store may be
/// readable now. A value written since removed its entry (`set`), so it is never overwritten here.
fn revalidate(stored: &mut Stored) {
    if stored.pending_owner.is_empty()
        || stored
            .checked_at
            .is_some_and(|at| at.elapsed() < REVALIDATE_EVERY)
    {
        return;
    }
    stored.checked_at = Some(Instant::now());
    let blob = json!({ "owner": Value::Object(std::mem::take(&mut stored.pending_owner)) });
    admit_owned(
        read_owner(&blob),
        &mut stored.values,
        &mut stored.proofs,
        &mut stored.pending_owner,
    );
}

fn parse(text: &str) -> std::result::Result<Stored, String> {
    if text.len() > MAX_BYTES {
        return Err("the stored settings are too large".into());
    }
    let value: Value = serde_json::from_str(text).map_err(|_| DAMAGED.to_string())?;
    // Version 2 (no owner checks) still loads; every write is version 3.
    if !matches!(value["v"].as_u64(), Some(2 | 3)) {
        return Err("the stored settings are a version this the app does not know".into());
    }
    let mut values = read_values(&value["values"])?;
    let pin_set = value["pinSet"].as_bool() == Some(true);
    let message = seal_message(pin_set, &values);
    let mac = value["mac"].as_str();
    // A gate the PIN loosened exists only under a PIN (`owner_settings::request`), so the seal
    // counts only when its key came from the PIN in this process. A clear-key seal file planted by
    // another process of the user (with `pinSet` false and the PIN item deleted) never does.
    let pin_proven = matches!(
        protected_guard::check_seal(message.as_bytes(), mac),
        Seal::Valid
    ) && protected_guard::key_from_pin();
    let mut carried = read_carried(&value);
    if !pin_proven {
        // With a PIN set the key opens only once the PIN is entered, so the loosened values wait,
        // raw and untrusted, in `carried` instead of being lost with the next write.
        if carried.is_none()
            && protected_guard::pin_bound()
            && values
                .iter()
                .any(|(k, v)| owner_settings::loosens(k, v, None))
            && let Some(mac) = mac
        {
            carried = Some(Carried {
                pin_set,
                values: values.clone(),
                mac: mac.to_string(),
                superseded: BTreeSet::new(),
            });
        }
        values = tighten_only(values);
    }
    let mut rewrite = false;
    if protected_guard::key_from_pin()
        && let Some(raw) = carried.take()
    {
        // The PIN key is open: the waiting values count if their own seal verifies, and a value
        // written since (present in `values`) always wins over a waiting one. Either way the blob
        // is written again without the waiting part (`settle`).
        rewrite = true;
        let sealed = matches!(
            protected_guard::check_seal(
                seal_message(raw.pin_set, &raw.values).as_bytes(),
                Some(raw.mac.as_str())
            ),
            Seal::Valid
        );
        if sealed {
            for (key, waiting) in raw.values {
                if !raw.superseded.contains(&key) {
                    values.entry(key).or_insert(waiting);
                }
            }
        }
    }
    // The owner-checked values: each counts only through its own proof (the load-time rule).
    let mut proofs = BTreeMap::new();
    let mut pending_owner = Map::new();
    admit_owned(
        read_owner(&value),
        &mut values,
        &mut proofs,
        &mut pending_owner,
    );
    let checked_at = (!pending_owner.is_empty()).then(Instant::now);
    Ok(Stored {
        values,
        // A PIN-bound seal also proves a PIN was set, whatever an unsealed blob says.
        pin_set: pin_set || protected_guard::pin_bound(),
        unproven: !pin_proven,
        proofs,
        carried,
        rewrite,
        pending_owner,
        checked_at,
    })
}

/// The blob as text. The sealed `values` hold every value no owner check authorised; an
/// owner-checked value lives only in `owner`, with its proof, so the seal (which needs the PIN key)
/// never decides it. `carried` goes back exactly as it was loaded.
fn blob_text(stored: &Stored, with_carried: bool) -> String {
    let mut sealed = stored.values.clone();
    // Entries that could not be checked go back as they were; a proven one replaces its own.
    let mut owner = stored.pending_owner.clone();
    for (key, proof) in &stored.proofs {
        if let Some(value) = sealed.remove(key) {
            owner.insert(
                key.clone(),
                json!({ "value": value, "proof": proof_json(proof) }),
            );
        }
    }
    let message = seal_message(stored.pin_set, &sealed);
    let mut blob = json!({ "v": 3, "values": sealed, "pinSet": stored.pin_set });
    if let Some(mac) = protected_guard::make_seal(message.as_bytes(), stored.pin_set) {
        blob["mac"] = Value::from(mac);
    }
    if !owner.is_empty() {
        blob["owner"] = Value::Object(owner);
    }
    if with_carried && let Some(carried) = &stored.carried {
        blob["carried"] = json!({
            "values": carried.values,
            "pinSet": carried.pin_set,
            "mac": carried.mac,
            "superseded": carried.superseded,
        });
    }
    blob.to_string()
}

fn persist(
    store: &rightkit_credentials::Store,
    stored: &Stored,
) -> std::result::Result<(), String> {
    let mut text = blob_text(stored, true);
    if text.len() > MAX_BYTES && stored.carried.is_some() {
        // The waiting values are the one part that can go: they count only after the PIN is
        // entered, and dropping them only makes things safer.
        text = blob_text(stored, false);
    }
    if text.len() > MAX_BYTES {
        return Err("the settings are too large to store".into());
    }
    store.set_str(ACCOUNT, &text).map_err(|e| e.to_string())
}

/// Writes the blob again when this load settled a `carried` part (`rewrite`): the PIN key is open,
/// so it seals now, with the taken-back values in the sealed part. A failed write leaves the blob as
/// it is, and the part is settled again at the next load.
fn settle(stored: &mut Stored) {
    if stored.rewrite
        && let Ok(store) = open()
        && persist(&store, stored).is_ok()
    {
        stored.rewrite = false;
        stored.unproven = false;
    }
}

/// Reads the item; a first run stores the closed defaults. An error is a fail closed.
fn load_stored() -> std::result::Result<Stored, String> {
    let store = open()?;
    // On macOS the read itself is pinned to the login keychain and the item is checked before and
    // after it (`protected_guard::read_item`); elsewhere it goes through the credential store.
    let found = match protected_guard::read_item(SERVICE, ACCOUNT) {
        Some(read) => read?,
        None => match store.get(ACCOUNT) {
            Ok(Some(secret)) => Some(secret.expose_str().map_err(|e| e.to_string())?.to_owned()),
            Ok(None) => None,
            Err(e) => return Err(e.to_string()),
        },
    };
    match found {
        Some(text) => parse(&text),
        None => {
            // No item. With the marker present it was deleted, which is not a first run.
            let marker = marker_path().ok_or("the data folder is unknown")?;
            if marker.exists() {
                return Err("the engine's stored settings were deleted".into());
            }
            let stored = Stored {
                pin_set: super::unlock::pin_item_present(),
                ..Stored::default()
            };
            persist(&store, &stored)?;
            protected_guard::check_item(SERVICE, ACCOUNT)?;
            std::fs::write(&marker, b"1")
                .map_err(|e| format!("the marker was not written: {e}"))?;
            Ok(stored)
        }
    }
}

/// Runs `f` on the engine's copy, loading it first when needed. Engine only.
fn with_loaded<T>(f: impl FnOnce(&mut Option<Stored>) -> Result<T>) -> Result<T> {
    if !approvals::is_engine_process() {
        return Err(Refusal::new(
            "approval_unavailable",
            "the security settings are held by the engine, and this process is not the engine",
        ));
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.is_none() {
        let mut loaded = load_stored().map_err(unavailable)?;
        settle(&mut loaded);
        *cache = Some(loaded);
    }
    if let Some(stored) = cache.as_mut() {
        revalidate(stored);
    }
    f(&mut cache)
}

/// The protected values, as the engine holds them. `None`: they cannot be read (fail closed). In
/// the engine this is its own copy; in any other process it asks the engine, which must be the
/// engine's own executable (`approvals::ask_engine_verified`).
pub fn values() -> Option<Map<String, Value>> {
    if approvals::is_engine_process() {
        return with_loaded(|cache| {
            Ok(cache.as_ref().map(|s| s.values.clone()).unwrap_or_default())
        })
        .ok();
    }
    let answer = approvals::ask_engine_verified("remote.settings.protected", json!({})).ok()?;
    answer["values"].as_object().cloned()
}

/// The engine's answer to `remote.settings.protected`: `{"values": {...}}`, or the refusal.
pub fn serve() -> Result<Value> {
    with_loaded(|cache| {
        let values = cache.as_ref().map(|s| s.values.clone()).unwrap_or_default();
        Ok(json!({ "values": values }))
    })
}

/// Replaces the protected keys of `published` (a `settings` object) with the engine's values.
/// Anything the file said about a protected key is dropped first, so an unreadable engine copy
/// leaves them at their defaults (off).
pub fn apply(published: &mut Value) {
    if !published.is_object() {
        *published = Value::Object(Map::new());
    }
    let Some(object) = published.as_object_mut() else {
        return;
    };
    object.retain(|key, _| !settings::is_protected(key));
    if let Some(values) = values() {
        for (key, value) in values {
            if settings::is_protected(&key) {
                object.insert(key, value);
            }
        }
    }
}

/// Stores one protected value. Engine only, after the owner path has accepted it
/// (`owner_settings::request`). `proof` is the owner-check proof that authorised it
/// (`unlock::authorise_settings`), or `None` for a change the PIN confirmed or one that needed
/// nothing; it is kept with the value only when the value is looser than the defaults (anything
/// else loads as it is), and any proof an earlier value of this key had is dropped. The credential
/// store is written first; the engine's copy changes only when that succeeded.
pub fn set(key: &str, value: &Value, proof: Option<OwnerCheckProof>) -> Result<()> {
    if !settings::is_protected(key) {
        return Err(Refusal::new(
            "invalid_request",
            "that setting is not an owner-only setting",
        ));
    }
    with_loaded(|cache| {
        let mut next = cache.clone().unwrap_or_default();
        next.values.insert(key.to_string(), value.clone());
        // What is written now wins over anything that waits: an owner entry that could not be
        // checked, and a PIN-loosened value set aside for the PIN.
        next.pending_owner.remove(key);
        if let Some(carried) = next.carried.as_mut() {
            carried.superseded.insert(key.to_string());
        }
        let unchanged = cache.as_ref().and_then(|s| s.values.get(key)) == Some(value);
        match proof.filter(|_| owner_settings::loosens(key, value, None)) {
            Some(proof) => {
                next.proofs.insert(key.to_string(), proof);
            }
            // The same value again with no new proof (a form save): its stored proof stays.
            None if unchanged => {}
            None => {
                next.proofs.remove(key);
            }
        }
        let store = open().map_err(unavailable)?;
        persist(&store, &next).map_err(unavailable)?;
        *cache = Some(next);
        Ok(())
    })
}

/// Whether the engine's copy was proven by the seal under the PIN key in this process, so a value
/// stored now is held by the PIN rule (until the next restart) without an owner-check proof.
/// Engine only.
pub fn sealed_by_pin() -> Result<bool> {
    with_loaded(|cache| Ok(cache.as_ref().is_some_and(|s| !s.unproven)))
}

/// Every protected value the engine holds, for `owner_settings::seed_queue`. Engine only.
pub fn all() -> Result<Vec<(String, Value)>> {
    with_loaded(|cache| {
        Ok(cache
            .as_ref()
            .map(|s| {
                s.values
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default())
    })
}

/// Whether a host app PIN was ever set, as the engine recorded it. Engine only.
pub fn pin_was_set() -> Result<bool> {
    with_loaded(|cache| Ok(cache.as_ref().is_some_and(|s| s.pin_set)))
}

/// Records that a host app PIN is set or changed, and re-seals the item under the new PIN (macOS, Windows).
/// Engine only.
pub fn mark_pin_set(pin: &str) -> Result<()> {
    with_loaded(|cache| {
        let mut next = cache.clone().unwrap_or_default();
        next.pin_set = true;
        // A PIN is changed with the old one entered, or reset after a lost one; either way the seal
        // key moves on, so the values waiting for the old PIN can never verify again.
        next.carried = None;
        protected_guard::pin_changed(pin);
        let store = open().map_err(unavailable)?;
        persist(&store, &next).map_err(unavailable)?;
        *cache = Some(next);
        Ok(())
    })
}

/// The owner's PIN was just verified. On macOS and Windows that unwraps the seal key, and a copy that was read
/// as closed defaults because its seal could not be checked is read again; PIN-loosened values that
/// waited in `carried` are taken back and the blob is sealed again (`settle`). Engine only; a
/// failure leaves the copy as it was.
pub fn pin_verified(pin: &str) {
    if !approvals::is_engine_process() {
        return;
    }
    protected_guard::pin_entered(pin);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.as_ref().is_some_and(|s| s.unproven)
        && let Ok(mut reloaded) = load_stored()
    {
        settle(&mut reloaded);
        if !reloaded.unproven {
            *cache = Some(reloaded);
        }
    }
}

/// The owner recovers an unreadable or untrusted item (deleted, replaced from outside, or a marker
/// without an item): the item and the marker are removed and the store starts again from the
/// closed defaults, and every loosened gate is lost, so this can only make things safer. The caller must have proved the owner's presence to the system
/// (Touch ID, Windows Hello) in owner app. Engine only.
pub fn recover() -> Result<()> {
    if !approvals::is_engine_process() {
        return Err(Refusal::new(
            "approval_unavailable",
            "the security settings are held by the engine, and this process is not the engine",
        ));
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let store = open().map_err(unavailable)?;
    store
        .delete(ACCOUNT)
        .map_err(|e| unavailable(format!("the stored item could not be removed: {e}; remove the keychain or credential item {SERVICE} by hand, then try again")))?;
    if let Some(marker) = marker_path() {
        let _ = std::fs::remove_file(marker);
    }
    protected_guard::reset_seal();
    let fresh = Stored {
        pin_set: super::unlock::pin_item_present(),
        ..Stored::default()
    };
    persist(&store, &fresh).map_err(unavailable)?;
    protected_guard::check_item(SERVICE, ACCOUNT).map_err(unavailable)?;
    let marker = marker_path().ok_or_else(|| unavailable("the data folder is unknown"))?;
    std::fs::write(marker, b"1")
        .map_err(|e| unavailable(format!("the marker was not written: {e}")))?;
    *cache = Some(fresh);
    Ok(())
}
