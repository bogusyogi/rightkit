//! The names of Claude accounts, shared by the Mac, the Windows PC and the phone (#454).
//!
//! One file, `claude-account-names.json` in the host app's state folder, keyed by the account's uuid
//! (`oauthAccount.accountUuid`, the id Claude Code and Claude Desktop both use):
//! `{"schema":1,"accounts":{"<uuid>":{"name":"Work","at":<ms>,"seen":<ms>}}}`.
//!
//! * `name` and `at`: an explicit rename and when it was made. An empty `name` is an explicit
//!   return to the automatic name. Merging two computers keeps, per uuid, the rename with the
//!   newest `at` (equal times: the larger name, so both computers pick the same one).
//! * `seen`: when any computer first saw the account. Merging keeps the smallest.
//! * The automatic name is `Claude A`, `Claude B`, ... by rank of `(seen, uuid)` among every uuid
//!   in the file, so both computers converge once they have merged and a rename never shifts
//!   another account's letter. An account with no entry yet is named `Claude <first 8 of uuid>`.
//!
//! The notch on each platform reads and writes the same file (`ClaudeAccountBook.swift`,
//! `windows/src/claude_accounts.rs`); the hub merges the other computer's copy from its chat
//! listing (`chat::links`) and the phone gets the resolved names in `chat.list`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const FILE: &str = "claude-account-names.json";
const FILE_MAX_BYTES: u64 = 256 * 1024;
const NAME_MAX_CHARS: usize = 40;
/// A time further ahead than this is not believed (clock skew allowance).
const FUTURE_SLACK_MS: u64 = 5 * 60 * 1000;
/// Accounts one file keeps: more than a person has, so a hostile peer cannot grow it.
const ACCOUNTS_MAX: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub at: u64,
    #[serde(default)]
    pub seen: u64,
}

pub type Names = BTreeMap<String, Entry>;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn file() -> Option<PathBuf> {
    crate::desktop::state_base()
        .ok()
        .map(|base| base.join(FILE))
}

/// A canonical 8-4-4-4-12 hex uuid.
pub fn is_account_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn clean_name(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(NAME_MAX_CHARS)
        .collect()
}

/// Whether a stamp is believable: at most five minutes ahead of this clock and within `i64`
/// (the phone and the Mac read these as signed). One bad stamp would otherwise win every merge.
fn sane(ms: u64) -> bool {
    ms <= now_ms().saturating_add(FUTURE_SLACK_MS) && ms <= i64::MAX as u64
}

/// The names kept on this computer; empty when there is no file or it cannot be read.
pub fn load() -> Names {
    load_checked().unwrap_or_default()
}

/// Like `load`, but `None` when a file is there and cannot be read or parsed: a writer must
/// leave such a file alone, never rewrite it from an empty map.
fn load_checked() -> Option<Names> {
    let path = file()?;
    let meta = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Names::new()),
        Err(_) => return None,
    };
    if meta.len() > FILE_MAX_BYTES {
        return None;
    }
    let doc = serde_json::from_slice::<Value>(&fs::read(&path).ok()?).ok()?;
    Some(parse(&doc))
}

/// Entries from a document of the file's shape (this computer's file, or the other computer's
/// `claudeAccountNames` in its chat listing). Anything that is not a uuid is dropped.
pub fn parse(doc: &Value) -> Names {
    let accounts = doc.get("accounts").unwrap_or(doc);
    let mut out = Names::new();
    if let Some(map) = accounts.as_object() {
        for (id, item) in map.iter().take(ACCOUNTS_MAX) {
            let id = id.trim().to_ascii_lowercase();
            if !is_account_id(&id) {
                continue;
            }
            let at = item.get("at").and_then(Value::as_u64).unwrap_or(0);
            let seen = item.get("seen").and_then(Value::as_u64).unwrap_or(0);
            // An entry with a stamp from the future is dropped whole, not clamped.
            if !sane(at) || !sane(seen) {
                continue;
            }
            out.insert(
                id,
                Entry {
                    name: clean_name(item.get("name").and_then(Value::as_str).unwrap_or("")),
                    at,
                    seen,
                },
            );
        }
    }
    out
}

pub fn to_json(names: &Names) -> Value {
    json!({ "schema": 1, "accounts": names })
}

fn save(names: &Names) {
    let Some(path) = file() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(bytes) = serde_json::to_vec(&to_json(names)) {
        let _ = crate::desktop::write_private(&path, &bytes, false);
    }
}

/// Folds `remote` into `local`: per uuid the newest rename, the earliest first sighting.
/// Returns whether `local` changed.
pub fn merge(local: &mut Names, remote: &Names) -> bool {
    let mut changed = false;
    for (id, theirs) in remote {
        if !sane(theirs.at) || !sane(theirs.seen) {
            continue;
        }
        match local.get_mut(id) {
            None => {
                if local.len() < ACCOUNTS_MAX {
                    local.insert(id.clone(), theirs.clone());
                    changed = true;
                }
            }
            Some(mine) => {
                if (theirs.at, &theirs.name) > (mine.at, &mine.name) {
                    mine.name = theirs.name.clone();
                    mine.at = theirs.at;
                    changed = true;
                }
                if theirs.seen != 0 && (mine.seen == 0 || theirs.seen < mine.seen) {
                    mine.seen = theirs.seen;
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Merges the other computer's copy into this computer's file. Returns whether anything changed.
pub fn merge_remote(remote: &Value) -> bool {
    let theirs = parse(remote);
    if theirs.is_empty() {
        return false;
    }
    let Some(mut mine) = load_checked() else {
        return false;
    };
    let changed = merge(&mut mine, &theirs);
    if changed {
        save(&mine);
    }
    changed
}

/// Records first sightings of accounts that have no entry yet, so each gets its place in the
/// automatic order. `seen_ms` is when it was first seen (a folder's age), 0 for now.
pub fn note_seen(ids: &[(String, u64)]) {
    let Some(mut names) = load_checked() else {
        return;
    };
    let mut changed = false;
    for (id, seen_ms) in ids {
        let id = id.trim().to_ascii_lowercase();
        if !is_account_id(&id) || names.contains_key(&id) || names.len() >= ACCOUNTS_MAX {
            continue;
        }
        let seen = if *seen_ms == 0 { now_ms() } else { *seen_ms };
        names.insert(
            id,
            Entry {
                name: String::new(),
                at: 0,
                seen,
            },
        );
        changed = true;
    }
    if changed {
        save(&names);
    }
}

/// An explicit rename (empty `name` returns the account to its automatic name).
pub fn rename(id: &str, name: &str) -> bool {
    let id = id.trim().to_ascii_lowercase();
    if !is_account_id(&id) {
        return false;
    }
    let Some(mut names) = load_checked() else {
        return false;
    };
    let now = now_ms();
    let entry = names.entry(id).or_insert_with(|| Entry {
        seen: now,
        ..Entry::default()
    });
    entry.name = clean_name(name);
    entry.at = now;
    save(&names);
    true
}

/// The `customName` the notch's account book (`claude-account-usage.json`) holds for `id`.
fn book_custom_name(id: &str) -> Option<String> {
    let base = crate::desktop::state_base().ok()?;
    let path = base.join("claude-account-usage.json");
    if fs::metadata(&path).ok()?.len() > 1024 * 1024 {
        return None;
    }
    let doc: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    doc.get("accounts")?
        .as_array()?
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(id))?
        .get("customName")?
        .as_str()
        .map(clean_name)
        .filter(|n| !n.is_empty())
}

/// The `email` of every account in the notch's account book, by account id.
fn book_emails() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(base) = crate::desktop::state_base().ok() else {
        return out;
    };
    let path = base.join("claude-account-usage.json");
    let Some(doc) = fs::metadata(&path)
        .ok()
        .filter(|m| m.len() <= 1024 * 1024)
        .and_then(|_| fs::read(&path).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return out;
    };
    for entry in doc
        .get("accounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(id), Some(email)) = (
            entry.get("id").and_then(Value::as_str),
            entry.get("email").and_then(Value::as_str),
        ) {
            out.insert(id.to_ascii_lowercase(), email.to_string());
        }
    }
    out
}

/// The name an email address implies: the organisation in its domain ("Orthiclabs" for
/// `x@orthiclabs.com`), or "Personal" for a mail provider's address. None for anything else.
fn email_base_name(email: &str) -> Option<String> {
    let domain = email.rsplit_once('@')?.1.trim().to_ascii_lowercase();
    let labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
    let org = *labels.get(labels.len().checked_sub(2)?)?;
    const PERSONAL: &[&str] = &[
        "gmail",
        "googlemail",
        "icloud",
        "me",
        "mac",
        "outlook",
        "hotmail",
        "live",
        "msn",
        "yahoo",
        "ymail",
        "proton",
        "protonmail",
        "pm",
        "aol",
        "fastmail",
        "hey",
        "gmx",
    ];
    if PERSONAL.contains(&org) {
        return Some("Personal".to_string());
    }
    let mut chars = org.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// The automatic name of `id` from its email (#454): the base name, numbered ("Orthiclabs 2")
/// when several accounts share it, in the order of first sighting.
fn email_label(names: &Names, id: &str) -> Option<String> {
    let emails = book_emails();
    let base = email_base_name(emails.get(id)?)?;
    let key = |i: &str| {
        let seen = names.get(i).map_or(0, |e| e.seen);
        (if seen == 0 { u64::MAX } else { seen }, i.to_string())
    };
    let mine = key(id);
    let rank = emails
        .iter()
        .filter(|(other, email)| {
            other.as_str() != id
                && email_base_name(email).as_deref() == Some(base.as_str())
                && key(other) < mine
        })
        .count();
    Some(if rank == 0 {
        base
    } else {
        format!("{base} {}", rank + 1)
    })
}

fn letter(rank: usize) -> String {
    match rank {
        0..=25 => format!("Claude {}", char::from(b'A' + rank as u8)),
        n => format!("Claude {}", n + 1),
    }
}

/// The name of `id` in `names`.
pub fn label_in(names: &Names, id: &str) -> String {
    let id = id.trim().to_ascii_lowercase();
    let Some(entry) = names.get(&id) else {
        return book_custom_name(&id)
            .or_else(|| email_label(names, &id))
            .unwrap_or_else(|| format!("Claude {}", id.get(..8).unwrap_or(&id)));
    };
    if !entry.name.is_empty() {
        return entry.name.clone();
    }
    // A name chosen in the hub before the shared file existed, still only in the account book.
    if entry.at == 0
        && let Some(custom) = book_custom_name(&id)
    {
        return custom;
    }
    if let Some(auto) = email_label(names, &id) {
        return auto;
    }
    let mine = (
        if entry.seen == 0 {
            u64::MAX
        } else {
            entry.seen
        },
        &id,
    );
    let rank = names
        .iter()
        .filter(|(other, e)| {
            (if e.seen == 0 { u64::MAX } else { e.seen }, *other) < (mine.0, mine.1)
        })
        .count();
    letter(rank)
}

/// What the host app calls account `id` on this computer. An account not seen before is noted now, so
/// its place in the automatic order is fixed from its first listing.
pub fn label(id: &str) -> String {
    let mut names = load();
    if !names.contains_key(&id.trim().to_ascii_lowercase()) {
        note_seen(&[(id.to_string(), 0)]);
        names = load();
    }
    label_in(&names, id)
}

/// `{uuid: name}` for every account in the file: what the phone's `chat.list` carries.
pub fn resolved() -> Value {
    let names = load();
    let map: BTreeMap<String, String> = names
        .keys()
        .map(|id| (id.clone(), label_in(&names, id)))
        .collect();
    json!(map)
}

type ConfigKey = (std::time::SystemTime, u64);
type ConfigCache = std::collections::HashMap<PathBuf, (ConfigKey, Option<String>)>;

/// The account a Claude config folder is signed in to: `<folder>/.claude.json`, or for a default
/// `~/.claude` the `~/.claude.json` beside it (`oauthAccount.accountUuid`). Nothing else in the
/// file is read. The answer is kept per file until its size or time changes: the file runs to
/// megabytes and the chat list asks every few seconds.
pub fn account_of_config(config: &Path) -> Option<String> {
    static CACHE: std::sync::Mutex<Option<ConfigCache>> = std::sync::Mutex::new(None);
    let mut candidates = vec![config.join(".claude.json")];
    if config.file_name().is_some_and(|n| n == ".claude")
        && let Some(parent) = config.parent()
    {
        candidates.push(parent.join(".claude.json"));
    }
    candidates.into_iter().find_map(|path| {
        let meta = fs::metadata(&path).ok()?;
        if meta.len() > 8 * 1024 * 1024 {
            return None;
        }
        let key = (meta.modified().ok()?, meta.len());
        let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let cache = guard.get_or_insert_with(Default::default);
        if let Some((kept, answer)) = cache.get(&path)
            && *kept == key
        {
            return answer.clone();
        }
        let answer = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|doc| {
                let id = doc
                    .pointer("/oauthAccount/accountUuid")?
                    .as_str()?
                    .to_ascii_lowercase();
                is_account_id(&id).then_some(id)
            });
        cache.insert(path, (key, answer.clone()));
        answer
    })
}
