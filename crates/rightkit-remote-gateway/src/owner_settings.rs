//! Owner-only changes to the security settings (`settings::PROTECTED_KEYS`).
//!
//! Any process of the same user can write a `hub-commands` file, run `pulse settings set` or edit
//! `owner app-state.json`, so none of them may change a gate. The change goes: hub
//! (`remote.settings.set`, caller's executable verified by the resident engine) -> the engine's own
//! store (`protected_store.rs`, the only place the gates are read from) -> this queue, in the
//! engine's memory -> owner app (`remote.settings.take`, executable verified as owner app), which
//! applies it to its own preferences and publishes it in `owner app-state.json` for display. A change
//! that makes a gate less safe (`loosens`) also needs the owner, checked here: first the system
//! check, a signature of an enrolled owner-check key (Windows Hello, Touch ID or the Mac login
//! password) over a single-use challenge for exactly that key and value (`challenge`,
//! `unlock::authorise_settings`), and only when that is not available the host app PIN. Never both. A
//! change that tightens a gate needs nothing. The baseline a change is judged against is the
//! engine's own value, never the published file; a baseline that cannot be read or understood counts
//! as less safe.
//!
//! The queue only keeps owner app's display in step with the engine: an entry waits until owner app
//! takes it (newest per key), and a restarted engine queues every stored value again
//! (`seed_queue`), so a owner app that missed a change or whose preferences were edited from outside
//! shows the engine's value on its next take.
//!
//! The same queue carries the hub's owner-only owner app commands (`OWNER_COMMANDS`: delete, account,
//! privileged helper, update, quit; #190) as `hubCommand.<id>` entries: the hub sends them with
//! `remote.settings.set`, the engine checks the caller as for a setting, and owner app runs them on
//! its take. They are never stored, so a restarted engine does not run one again.

use super::settings;
use super::{Refusal, Result, protected_store, unlock};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::sync::Mutex;

const MAX_QUEUED: usize = 64;
const MAX_LIST: usize = 64;
const MAX_ITEM_CHARS: usize = 1024;
/// The largest hub command, as the JSON text owner app parses.
const MAX_COMMAND_BYTES: usize = 4096;

/// The queue key of one hub command the engine verified: `hubCommand.<id>`, the id being the hub's
/// request stamp (digits only), which is also the name of the result file owner app writes
/// (`hub-commands/results/<id>.json`). Never stored (`protected_store`) and never re-queued by
/// `seed_queue`: a command runs once, on the take that delivers it, or not at all.
pub const HUB_COMMAND_PREFIX: &str = "hubCommand.";

/// The owner app commands that delete data, change an account, the privileged helper, install an
/// update or quit the host app (#190). The notches refuse them from a `hub-commands` file, which any
/// process of the user can write; they arrive only as `hubCommand.<id>` through this queue, after
/// the resident engine checked that the caller is the host app hub's executable. The Mac owner app keeps
/// the same list (`HubBridge.ownerCommands`), as does the Windows owner app (`bridge::OWNER_COMMANDS`).
/// `permissionRequest` belongs here only for the privileged helper (`is_owner_command`).
pub const OWNER_COMMANDS: &[&str] = &[
    "forgetClaudeAccount",
    "signOut",
    "forgetReading",
    "clipboardClear",
    "helperEnable",
    "helperDisable",
    "installUpdate",
    "quit",
    // The Mac owner app signs the engine's owner challenge with Touch ID (`device.owner.challenge`).
    "signOwnerChallenge",
];

/// Whether this hub command (`{"command": .., ..}`) may only come through the engine.
pub fn is_owner_command(command: &Value) -> bool {
    match command.get("command").and_then(Value::as_str) {
        Some("permissionRequest") => command.get("id").and_then(Value::as_str) == Some("helper"),
        Some(name) => OWNER_COMMANDS.contains(&name),
        None => false,
    }
}

/// The domain label of the owner channel's MAC (`owner_channel_mac`).
pub const OWNER_CHANNEL_DOMAIN: &str = "pulse.hub.command.v1";

/// The owner channel (#190), the route for an owner command when no resident engine is attached:
/// owner app starts the hub as its child and hands it a fresh random 32-byte key on the child's
/// stdin (never argv, the environment or a file). The hub writes the command to `hub-commands` as
/// `{"signed": <command JSON text>, "counter", "ts", "mac"}`; this is `mac`, the lowercase hex
/// HMAC-SHA256 under that key of `"<OWNER_CHANNEL_DOMAIN>\n<counter>\n<ts ms>\n<text>"`. The owner app
/// runs it only with a valid MAC, a counter above the last it took and a `ts` under 60 s old
/// (`HubLauncher.openOwnerCommand` on the Mac, `hub::open_owner_command` on Windows, which builds
/// the same message).
pub fn owner_channel_mac(key: &[u8; 32], counter: u64, ts_ms: u64, text: &str) -> String {
    let message = format!("{OWNER_CHANNEL_DOMAIN}\n{counter}\n{ts_ms}\n{text}");
    rightkit_crypto::hex_encode(&rightkit_crypto::hmac_sha256(key, message.as_bytes()))
}

/// Queues one owner command (`key` is `hubCommand.<id>`, `value` the command as JSON text) for the
/// owner app. Nothing is stored and no PIN is asked: the authority is the caller check the resident
/// bridge already made, and owner app still refuses a command older than a minute.
fn request_command(key: &str, value: &Value) -> Result<bool> {
    let id = &key[HUB_COMMAND_PREFIX.len()..];
    if id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid(
            "a hub command's id is its request stamp, digits only",
        ));
    }
    let text = value
        .as_str()
        .filter(|text| text.len() <= MAX_COMMAND_BYTES)
        .ok_or_else(|| invalid("a hub command is one short JSON text"))?;
    let command: Value =
        serde_json::from_str(text).map_err(|_| invalid("a hub command is one JSON object"))?;
    if !command.is_object() || !is_owner_command(&command) {
        return Err(invalid(
            "only the owner-only notch commands go through the engine",
        ));
    }
    queue(key, Value::from(text));
    Ok(true)
}

static QUEUE: Mutex<Vec<(String, Value)>> = Mutex::new(Vec::new());

/// Held across the whole read-baseline, check, store of one setting change, so two calls at once
/// cannot both be judged against the same old value (a lowering that lands after another lowering
/// would otherwise raise the value without the PIN).
static CHANGE: Mutex<()> = Mutex::new(());

fn invalid(why: &str) -> Refusal {
    Refusal::new("invalid_request", why)
}

fn as_list(value: &Value) -> Option<Vec<String>> {
    let items = match value {
        Value::Array(items) => items.clone(),
        Value::String(text) => match serde_json::from_str::<Value>(text).ok()? {
            Value::Array(items) => items,
            _ => return None,
        },
        _ => return None,
    };
    items
        .iter()
        .map(|item| item.as_str().map(|s| s.trim().to_string()))
        .collect()
}

fn is_list_key(key: &str) -> bool {
    matches!(
        key,
        settings::KEY_BLOCKED_APPS
            | settings::KEY_FORM_FILL_STANDING
            | settings::KEY_FORM_FILL_PROFILE
            | settings::KEY_FORM_FILL_LOGIN_MANAGERS
            | settings::KEY_ROOTS
            | settings::KEY_CHAT_READ
            | settings::KEY_TERM_ALLOW
            | settings::KEY_TERM_DENY
    )
}

/// The two payment caps (`remoteFormFillPaymentsAutoCapCents`, `...MonthCapCents`): whole cents.
fn is_payment_cap_key(key: &str) -> bool {
    matches!(
        key,
        settings::KEY_PAYMENTS_AUTO_CAP | settings::KEY_PAYMENTS_MONTH_CAP
    )
}

/// The six remote-read caps (`remoteReadMaxBytes` and the others, #579 D10): the default a missing
/// value means and the hard maximum a value is clamped to. `None` for any other key.
fn read_cap_limits(key: &str) -> Option<(u64, u64)> {
    Some(match key {
        settings::KEY_READ_MAX_BYTES => (
            settings::DEFAULT_READ_MAX_BYTES,
            settings::HARD_READ_MAX_BYTES,
        ),
        settings::KEY_FIND_MAX_RESULTS => (
            settings::DEFAULT_FIND_MAX_RESULTS,
            settings::HARD_FIND_MAX_RESULTS,
        ),
        settings::KEY_CHAT_READ_MAX_TURNS => (
            settings::DEFAULT_CHAT_READ_MAX_TURNS,
            settings::HARD_CHAT_READ_MAX_TURNS,
        ),
        settings::KEY_CHAT_READ_MAX_BYTES => (
            settings::DEFAULT_CHAT_READ_MAX_BYTES,
            settings::HARD_CHAT_READ_MAX_BYTES,
        ),
        settings::KEY_IMAGE_MAX_BYTES => (
            settings::DEFAULT_IMAGE_MAX_BYTES,
            settings::HARD_IMAGE_MAX_BYTES,
        ),
        settings::KEY_DOCS_MAX_BYTES => (
            settings::DEFAULT_DOCS_MAX_BYTES,
            settings::HARD_DOCS_MAX_BYTES,
        ),
        _ => return None,
    })
}

/// The value as owner app will take it: a bool, a whole number for the minutes, or a list of
/// short strings. Anything else is refused.
pub(super) fn check_shape(key: &str, value: &Value) -> Result<Value> {
    if key == settings::KEY_CHAT_UNLOCK_MINUTES {
        let n = value
            .as_u64()
            .or_else(|| {
                value
                    .as_f64()
                    .filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0)
                    .map(|f| f as u64)
            })
            .ok_or_else(|| invalid("chatUnlockMinutes is a whole number of minutes"))?;
        return Ok(Value::from(n.min(settings::HARD_CHAT_UNLOCK_MINUTES)));
    }
    if key == settings::KEY_AGENT_BROWSER_MINUTES {
        let n = stored_minutes(value).filter(|n| *n >= 1).ok_or_else(|| {
            invalid("agentBrowserSessionMinutes is a whole number of minutes, 1 or more")
        })?;
        return Ok(Value::from(n.min(settings::HARD_AGENT_BROWSER_MINUTES)));
    }
    // A read cap is a JSON integer, 1 or more, clamped to its hard maximum. A float (even a whole
    // one), a negative number, text, null or a bool is refused, so no cast can lose or wrap a value.
    if let Some((_, hard)) = read_cap_limits(key) {
        let n = value
            .as_u64()
            .filter(|n| *n >= 1)
            .ok_or_else(|| invalid("a read cap is a whole number, 1 or more"))?;
        return Ok(Value::from(n.min(hard)));
    }
    if is_payment_cap_key(key) {
        let n = stored_minutes(value)
            .ok_or_else(|| invalid("a payment cap is a whole number of cents"))?;
        return Ok(Value::from(n.min(settings::HARD_PAYMENTS_CAP_CENTS)));
    }
    if is_list_key(key) {
        let list = as_list(value).ok_or_else(|| invalid("this setting is a list of text lines"))?;
        if list.len() > MAX_LIST
            || list.iter().any(|item| {
                item.is_empty()
                    || item.chars().count() > MAX_ITEM_CHARS
                    || item.chars().any(char::is_control)
            })
        {
            return Err(invalid(
                "a list setting holds at most 64 short, non-empty lines",
            ));
        }
        return Ok(Value::from(list));
    }
    if key == settings::KEY_AGENT_CLI_PATH {
        let path = value
            .as_str()
            .map(str::trim)
            .ok_or_else(|| invalid("agentCliPath is a path to the claude executable"))?;
        if path.chars().count() > MAX_ITEM_CHARS || path.chars().any(char::is_control) {
            return Err(invalid(
                "agentCliPath is one short path with no control characters",
            ));
        }
        return Ok(Value::from(path));
    }
    // The lease mode is one of the three words, and the two lease times are whole seconds inside
    // their bounds (clamped to the hard ones).
    if key == settings::KEY_AGENT_FOREGROUND_MODE {
        let mode = value
            .as_str()
            .filter(|m| settings::FOREGROUND_MODES.contains(m))
            .ok_or_else(|| invalid("agentForegroundMode is ask, when-idle or task"))?;
        return Ok(Value::from(mode));
    }
    if key == settings::KEY_AGENT_FOREGROUND_SECONDS {
        let n = settings::whole_seconds(value)
            .filter(|n| *n >= 1)
            .ok_or_else(|| {
                invalid("agentForegroundSeconds is a whole number of seconds, 1 or more")
            })?;
        return Ok(Value::from(n.min(settings::HARD_AGENT_FOREGROUND_SECONDS)));
    }
    if key == settings::KEY_AGENT_FOREGROUND_IDLE_SECONDS {
        let n = settings::whole_seconds(value)
            .ok_or_else(|| invalid("agentForegroundIdleSeconds is a whole number of seconds"))?;
        return Ok(Value::from(n.clamp(
            settings::MIN_AGENT_FOREGROUND_IDLE_SECONDS,
            settings::HARD_AGENT_FOREGROUND_IDLE_SECONDS,
        )));
    }
    if value.is_boolean() {
        return Ok(value.clone());
    }
    Err(invalid("this setting is on or off"))
}

/// A stored whole number of minutes; `None` when the stored value is not one.
fn stored_minutes(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0)
            .map(|f| f as u64)
    })
}

/// Whether `new` makes the gate behind `key` less safe than `current`, the engine's own stored
/// value (`None`: the engine holds none, so the default applies). A stored value that cannot be
/// understood is an unknown baseline, which counts as less safe.
pub(super) fn loosens(key: &str, new: &Value, current: Option<&Value>) -> bool {
    match key {
        // Any path the engine would start is more reach than automatic (the default, empty): a
        // non-empty path loosens unless it is the stored one. Clearing it tightens.
        settings::KEY_AGENT_CLI_PATH => {
            let text = |v: &Value| v.as_str().map(|s| s.trim().to_string());
            let new = text(new).unwrap_or_default();
            !new.is_empty() && current.and_then(text).as_deref() != Some(new.as_str())
        }
        settings::KEY_CHAT_UNLOCK => {
            new == &Value::Bool(false) && current != Some(&Value::Bool(false))
        }
        // On by default, so on is the default and only turning it back on from an explicit off
        // loosens; off tightens.
        settings::KEY_PHONE_CHAT_READ_ALL => {
            new == &Value::Bool(true) && current == Some(&Value::Bool(false))
        }
        settings::KEY_CHAT_UNLOCK_MINUTES => {
            let effective = |n: u64| if n == 0 { u64::MAX } else { n };
            let before = match current {
                None => 0,
                Some(value) => match stored_minutes(value) {
                    Some(n) => n,
                    None => return true,
                },
            };
            effective(stored_minutes(new).unwrap_or(0)) > effective(before)
        }
        // A longer agent browser session is more reach; no stored value means the default.
        settings::KEY_AGENT_BROWSER_MINUTES => {
            let before = match current {
                None => settings::DEFAULT_AGENT_BROWSER_MINUTES,
                Some(value) => match stored_minutes(value) {
                    Some(n) => n,
                    None => return true,
                },
            };
            stored_minutes(new).is_none_or(|n| n > before)
        }
        // The lease modes, tightest first: ask, when-idle, task. A looser mode, or one that cannot be
        // read, loosens. No stored value means the default, ask.
        settings::KEY_AGENT_FOREGROUND_MODE => {
            let rank = |v: &Value| {
                v.as_str()
                    .and_then(|m| settings::FOREGROUND_MODES.iter().position(|x| *x == m))
            };
            let before = match current {
                None => 0,
                Some(value) => match rank(value) {
                    Some(n) => n,
                    None => return true,
                },
            };
            rank(new).is_none_or(|n| n > before)
        }
        // A longer lease is more reach.
        settings::KEY_AGENT_FOREGROUND_SECONDS => {
            let before = match current {
                None => settings::DEFAULT_AGENT_FOREGROUND_SECONDS,
                Some(value) => match settings::whole_seconds(value) {
                    Some(n) => n,
                    None => return true,
                },
            };
            settings::whole_seconds(new).is_none_or(|n| n > before)
        }
        // A shorter idle time makes `when-idle` grant sooner: looser.
        settings::KEY_AGENT_FOREGROUND_IDLE_SECONDS => {
            let before = match current {
                None => settings::DEFAULT_AGENT_FOREGROUND_IDLE_SECONDS,
                Some(value) => match settings::whole_seconds(value) {
                    Some(n) => n,
                    None => return true,
                },
            };
            settings::whole_seconds(new).is_none_or(|n| n < before)
        }
        // A higher cap is more spending power and needs the PIN; a lower one tightens. No stored
        // value means the default; one that cannot be read as cents is an unknown baseline.
        settings::KEY_PAYMENTS_AUTO_CAP | settings::KEY_PAYMENTS_MONTH_CAP => {
            let default = if key == settings::KEY_PAYMENTS_AUTO_CAP {
                settings::DEFAULT_PAYMENTS_AUTO_CAP_CENTS
            } else {
                settings::DEFAULT_PAYMENTS_MONTH_CAP_CENTS
            };
            let before = match current {
                None => default,
                Some(value) => match stored_minutes(value) {
                    Some(n) => n,
                    None => return true,
                },
            };
            stored_minutes(new).is_none_or(|n| n > before)
        }
        // A higher read cap is more reach for a remote caller and needs the PIN; a lower one
        // tightens and an equal one changes nothing. No stored value means the default; a stored
        // value that is not a whole number, or a new one that is not, counts as less safe.
        key if read_cap_limits(key).is_some() => {
            let default = read_cap_limits(key).map_or(0, |(default, _)| default);
            let before = match current {
                None => default,
                Some(value) => match value.as_u64() {
                    Some(n) => n,
                    None => return true,
                },
            };
            new.as_u64().is_none_or(|n| n > before)
        }
        // The master read-only switch is the one switch that is safer on: turning it off loosens.
        settings::KEY_READ_ONLY => {
            new == &Value::Bool(false) && current != Some(&Value::Bool(false)) && current.is_some()
        }
        // An allow list restricts while it has entries: adding one, or emptying it, adds reach.
        settings::KEY_TERM_ALLOW => {
            let before: HashSet<String> = match current {
                None => HashSet::new(),
                Some(value) => match as_list(value) {
                    Some(list) => list.into_iter().collect(),
                    None => return true,
                },
            };
            let now = as_list(new).unwrap_or_default();
            (!before.is_empty() && now.is_empty())
                || (!before.is_empty() && now.iter().any(|item| !before.contains(item)))
        }
        settings::KEY_BLOCKED_APPS | settings::KEY_TERM_DENY => {
            let before = match current {
                None => Vec::new(),
                Some(value) => match as_list(value) {
                    Some(list) => list,
                    None => return true,
                },
            };
            let now: HashSet<String> = as_list(new).unwrap_or_default().into_iter().collect();
            before.iter().any(|app| !now.contains(app))
        }
        // Adding a line adds reach: a folder, a chat, a standing grant, a profile detail.
        settings::KEY_FORM_FILL_STANDING
        | settings::KEY_FORM_FILL_PROFILE
        | settings::KEY_FORM_FILL_LOGIN_MANAGERS
        | settings::KEY_ROOTS
        | settings::KEY_CHAT_READ => {
            let before: HashSet<String> = match current {
                None => HashSet::new(),
                Some(value) => match as_list(value) {
                    Some(list) => list.into_iter().collect(),
                    None => return true,
                },
            };
            as_list(new)
                .unwrap_or_default()
                .iter()
                .any(|item| !before.contains(item))
        }
        // Every other protected key is a switch that is off by default: on is less safe.
        _ => new != &Value::Bool(false),
    }
}

/// The `approval_sig` kind of an owner-check signature of a protected-setting change.
pub const SETTINGS_PROOF_KIND: &str = "settings";

/// The `action` a `settings` signature commits to: SHA-256 hex of
/// `pulse.settings.v1\nkey:<key>\nvalue:<SHA-256 hex of the checked value's JSON text>`. `checked`
/// is the value as [`check_shape`] returns it (a bool, a whole number, a text or a list of texts,
/// never an object), whose compact JSON text is therefore canonical. So a signature made for one
/// key cannot change another, and one for one value cannot set another.
pub fn settings_action(key: &str, checked: &Value) -> String {
    let value = rightkit_fs::digest::hex(&rightkit_crypto::sha256(checked.to_string().as_bytes()));
    rightkit_fs::digest::hex(&rightkit_crypto::sha256(
        format!("pulse.settings.v1\nkey:{key}\nvalue:{value}").as_bytes(),
    ))
}

/// The engine's `remote.settings.challenge` with `key` and `value`: `{"needed": false}` when nothing
/// needs confirming, else `{"needed": true, "kind", "challenge", "action", "client", "expires"}`: a
/// fresh single-use challenge for the owner's device to sign (`unlock::settings_challenge`) before
/// `remote.settings.set`. Needed when the change makes a gate less safe than the engine's value
/// (required), and also when the new value is still looser than the defaults and the engine's copy
/// is not proven by the PIN seal in this process, so that without a stored proof the value would
/// read as the closed default after the next restart (a partial tightening of a value the system
/// check loosened; that one is optional). A change to the defaults or tighter never prompts. It
/// grants nothing by itself. Engine only, after the resident bridge checked the caller.
pub fn challenge(key: &str, value: &Value) -> Result<Value> {
    if key.starts_with(HUB_COMMAND_PREFIX) || !settings::is_protected(key) {
        return Err(invalid("that setting is not an owner-only setting"));
    }
    let new = check_shape(key, value)?;
    let stored: Map<String, Value> = protected_store::all()?.into_iter().collect();
    let required = loosens(key, &new, stored.get(key));
    let optional = loosens(key, &new, None) && !protected_store::sealed_by_pin()?;
    if !required && !optional {
        return Ok(serde_json::json!({ "needed": false }));
    }
    let mut issued = unlock::settings_challenge(&settings_action(key, &new))?;
    issued["needed"] = Value::Bool(true);
    Ok(issued)
}

/// The engine's `remote.settings.set`: stores one protected change in the engine's own store and
/// queues it for owner app. A change that loosens a gate is confirmed by the owner first: a
/// `sig` over a `challenge` from [`challenge`] (an enrolled owner-check key: Windows Hello, Touch ID
/// or the Mac login password), which is checked before anything else, else the host app `pin`. The
/// signature's proof is stored with the value (`protected_store::set`). A caller's word that the
/// system accepted the owner is never a proof. Engine only, after the resident bridge checked the
/// caller.
pub fn request(
    key: &str,
    value: &Value,
    pin: Option<&str>,
    challenge: Option<&str>,
    sig: Option<&Value>,
) -> Result<bool> {
    // An owner-only owner app command from the verified hub (`HUB_COMMAND_PREFIX`), not a setting.
    if key.starts_with(HUB_COMMAND_PREFIX) {
        return request_command(key, value);
    }
    if !settings::is_protected(key) {
        return Err(invalid("that setting is not an owner-only setting"));
    }
    let new = check_shape(key, value)?;
    let _one_change_at_a_time = CHANGE.lock().unwrap_or_else(|e| e.into_inner());
    // The baseline is the engine's own copy; when that cannot be read nothing changes.
    let stored: Map<String, Value> = protected_store::all()?.into_iter().collect();
    let loosened = loosens(key, &new, stored.get(key));
    let mut proof = None;
    let mut method = "none";
    let challenge = challenge.filter(|c| !c.is_empty());
    let sig = sig.filter(|s| !s.is_null());
    if loosened {
        if let (Some(challenge), Some(sig)) = (challenge, sig) {
            proof = Some(unlock::authorise_settings(
                &settings_action(key, &new),
                challenge,
                sig,
            )?);
            method = "owner-check";
        } else {
            match pin.filter(|p| !p.is_empty()) {
                Some(pin) => {
                    unlock::confirm_pin(pin)?;
                    method = "pin";
                }
                None => {
                    // With no host app PIN there is nothing to type: say where to set one. A PIN state
                    // that cannot be read keeps the ordinary refusal.
                    if matches!(unlock::pin_is_set(), Ok(false)) {
                        return Err(Refusal::new(
                            "pin_not_set",
                            "this change makes a gate less safe, so it needs the system check (Windows Hello, Touch ID or your Mac password) or a host app PIN, and neither was available; set a PIN in the app's settings (Remote access), then turn this on again",
                        ));
                    }
                    return Err(Refusal::new(
                        "unlock_needs_auth",
                        "this change makes a gate less safe, so it needs the system check (Windows Hello, Touch ID or your Mac password), which was not available or not confirmed, or your host app PIN",
                    ));
                }
            }
        }
    } else if loosens(key, &new, None)
        && let (Some(challenge), Some(sig)) = (challenge, sig)
        && let Ok(checked) = unlock::authorise_settings(&settings_action(key, &new), challenge, sig)
    {
        // A change that stays looser than the defaults needs nothing, but a signature given for it
        // keeps it across a restart; one that does not verify is ignored (nothing is loosened).
        proof = Some(checked);
        method = "owner-check";
    }
    protected_store::set(key, &new, proof)?;
    // Which gate changed, whether it loosened and how the owner confirmed it, never the value (#580,
    // settings D7).
    super::audit::record_security(
        "settings.owner_changed",
        serde_json::json!({ "key": key, "loosened": loosened, "method": method }),
    );
    queue(key, new);
    Ok(true)
}

fn queue(key: &str, value: Value) {
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    queue.retain(|(k, _)| k != key);
    if queue.len() >= MAX_QUEUED {
        queue.remove(0);
    }
    queue.push((key.to_string(), value));
}

/// Queues every value the engine holds, so owner app takes the engine's values and shows them. The
/// resident engine calls this once after it starts serving; an unreadable store queues nothing.
pub fn seed_queue() {
    if let Ok(values) = protected_store::all() {
        for (key, value) in values {
            queue(&key, value);
        }
    }
}

/// The engine's `remote.settings.take`: the queued changes, oldest first, once. The resident
/// bridge allows only owner app (verified by its executable).
pub fn take() -> Vec<(String, Value)> {
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    queue.drain(..).collect()
}
