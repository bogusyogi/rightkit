//! `chat_read`: the user and assistant turns of one chat on this computer, for a chat the user
//! selected in Settings (`remoteChatRead`). The transcript readers are `chat::read` (built on
//! `chat::said`). What is returned may leave this computer, so every turn passes `redact`, the
//! call is capped by turns (`remoteChatReadMaxTurns`) and bytes (`remoteChatReadMaxBytes`), and
//! the Activity record keeps only that a read happened and its size, never the turns.

use super::settings::Settings;
use super::{Refusal, Result};
use serde_json::Value;

/// The chat kinds remote tools can read and name: `<kind>:<id>` with one of these kinds.
pub const CHAT_KINDS: &[&str] = &["claude", "codex", "muse"];

/// The chat named as `<id>` or `<kind>:<id>` for a kind in `CHAT_KINDS`, split.
fn parse(chat: &str) -> Result<(Option<&str>, &str)> {
    match chat.split_once(':') {
        Some((kind, id)) if CHAT_KINDS.contains(&kind) => Ok((Some(kind), id)),
        // `<device>:<session>` names a chat on a linked computer. Remote tools reach this
        // computer only, until the device link is qualified.
        Some(_) => Err(Refusal::new(
            "not_local",
            "that chat is not on this computer; remote tools reach this computer's chats only (name one as <id>, or as <kind>:<id> with kind claude, codex or muse)",
        )),
        None => Ok((None, chat)),
    }
}

/// Whether Settings selected this chat. `kind` is what is known of the chat's kind: the one
/// named in the request, or the transcript's once it is found.
pub(crate) fn selected(settings: &Settings, kind: Option<&str>, id: &str) -> bool {
    settings.chat_read.iter().any(|entry| {
        let (entry_kind, entry_id) = match entry.split_once(':') {
            Some((k, i)) if CHAT_KINDS.contains(&k) => (Some(k), i),
            _ => (None, entry.as_str()),
        };
        entry_id == id
            && match (entry_kind, kind) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    })
}

pub fn read(settings: &Settings, chat: &str, limit: Option<u64>) -> Result<Value> {
    use rightkit_chat_bridge::chat::read::{ReadError, locate, read_turns};
    use serde_json::json;

    if !settings.enabled {
        return Err(Refusal::new(
            "remote_off",
            "remote tools are turned off in the app's settings (remoteTools)",
        ));
    }
    let chat = chat.trim();
    if chat.is_empty() {
        return Err(Refusal::new("invalid_request", "a chat id is needed"));
    }
    let (kind, id) = parse(chat)?;
    let invalid = |why: String| Refusal::new("invalid_request", why);
    // Selection is checked on the id alone first, so an unselected chat never shows whether it
    // exists.
    if !selected(settings, kind, id) {
        return Err(Refusal::new(
            "not_selected",
            "that chat is not selected for remote reading; select it in the app's settings (remoteChatRead)",
        ));
    }
    let turns_cap = settings.chat_read_max_turns;
    let limit = match limit {
        Some(0) => return Err(invalid("--limit must be 1 or more".into())),
        Some(n) if n > turns_cap => {
            return Err(Refusal::new(
                "too_large",
                format!("a chat read returns at most {turns_cap} turns (remoteChatReadMaxTurns)"),
            ));
        }
        Some(n) => n,
        None => turns_cap,
    };
    let (found_kind, path) = locate(kind, id).map_err(|e| match e {
        ReadError::Invalid(why) => invalid(why),
        ReadError::NotFound => Refusal::new("not_found", "no transcript for that chat here"),
        ReadError::Failed(why) => Refusal::new("read_failed", why),
    })?;
    // The transcript's own kind may differ from what a bare id was selected as.
    if !selected(settings, Some(found_kind.as_str()), id) {
        return Err(Refusal::new(
            "not_selected",
            "that chat is not selected for remote reading; select it in the app's settings (remoteChatRead)",
        ));
    }
    let read = read_turns(&found_kind, &path, limit as usize).map_err(|e| match e {
        ReadError::Invalid(why) => invalid(why),
        ReadError::NotFound => Refusal::new("not_found", "no transcript for that chat here"),
        ReadError::Failed(why) => Refusal::new("read_failed", why),
    })?;

    let byte_cap = settings.chat_read_max_bytes as usize;
    let mut redactions = 0usize;
    let mut truncated = false;
    let mut used = 0usize;
    // Newest first, so the byte cap drops the oldest turns.
    let mut kept: Vec<Value> = Vec::new();
    for turn in read.turns.iter().rev() {
        let (mut text, count) = super::redact::redact(&turn.text);
        redactions += count;
        let mut cut = turn.truncated;
        if used + text.len() > byte_cap {
            truncated = true;
            if !kept.is_empty() {
                break;
            }
            // Even the newest turn is over the cap: keep its start.
            let mut end = byte_cap.min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            cut = true;
        }
        used += text.len();
        kept.push(json!({
            "role": turn.role.as_str(),
            "ts": turn.ts,
            "text": text,
            "truncated": cut,
        }));
    }
    kept.reverse();
    let returned = kept.len();
    Ok(json!({
        "ok": true,
        "chat": id,
        "kind": read.kind,
        "turns": kept,
        "returned": returned,
        "bytes": used,
        "truncated": truncated || read.turns.len() > returned,
        "redactions": redactions,
        "scanCut": read.scan_cut,
    }))
}

/// Most bytes of snippet text one `ledger.history` answer carries; the rest waits for the next pull.
const HISTORY_ANSWER_BYTES: usize = 96 * 1024;
/// Most chats one `ledger.history` answer reads.
const HISTORY_CHATS: usize = 50;
/// Most turns of one chat one `ledger.history` answer reads.
const HISTORY_TURNS: usize = 500;
/// The size one snippet aims at (the Ledger's own chunk size).
const HISTORY_CHUNK: usize = 4000;

/// `ledger.history`: redacted snippets of the chats `remoteChatRead` selects, for a paired
/// computer that pulls them into its own search index. Answered only while `ledgerTranscriptShare`
/// is on (`not_granted` otherwise, which is the caller's signal to delete its copy). `cursor` maps a
/// session id to the stable id of the last turn the caller holds; turns after it are served, and
/// when that turn is not in the newest `HISTORY_TURNS` of the chat the whole window is served (the
/// caller dedupes). Each chunk carries `session`, `kind`, `turn` (the turn's stable id, a hash of
/// role, timestamp and text, never a window position), `part` (the chunk's index inside the turn),
/// `role`, `ts` and `text`; (session, turn, part) is the caller's dedupe key. The answer also
/// carries `sessions`, every session id the chat selection allows now, so the caller can delete
/// what is no longer allowed. Snippets only: no file, no path, no title, and only turns that pass
/// `redact`. The grant never widens the per-chat selection.
pub fn history(settings: &Settings, cursor: &serde_json::Map<String, Value>) -> Result<Value> {
    use rightkit_chat_bridge::chat::read::{locate, read_turns};
    use serde_json::json;

    if !settings.ledger_share {
        return Err(Refusal::new(
            "not_granted",
            "this computer does not share chat history (the app's settings, ledgerTranscriptShare)",
        ));
    }
    let mut chunks: Vec<Value> = Vec::new();
    let mut next = serde_json::Map::new();
    let mut seen: Vec<String> = Vec::new();
    let mut allowed: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut more = false;
    for entry in settings.chat_read.iter() {
        let Ok((kind, id)) = parse(entry) else {
            continue;
        };
        if id.is_empty() || allowed.iter().any(|done| done == id) {
            continue;
        }
        allowed.push(id.to_string());
        if seen.len() >= HISTORY_CHATS {
            more = true;
            continue;
        }
        let Ok((found_kind, path)) = locate(kind, id) else {
            continue;
        };
        // The transcript's own kind may differ from what a bare id was selected as.
        if !selected(settings, Some(found_kind.as_str()), id) {
            continue;
        }
        seen.push(id.to_string());
        let Ok(read) = read_turns(&found_kind, &path, HISTORY_TURNS) else {
            continue;
        };
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let ids: Vec<String> = read
            .turns
            .iter()
            .map(|turn| {
                let base = rightkit_fs::digest::sha256_bytes(
                    format!(
                        "{}\0{}\0{}",
                        turn.role.as_str(),
                        turn.ts.as_deref().unwrap_or(""),
                        turn.text
                    )
                    .as_bytes(),
                );
                let n = counts.entry(base.clone()).or_insert(0);
                *n += 1;
                format!("{}-{}", &base[..16], n)
            })
            .collect();
        let start = cursor
            .get(id)
            .and_then(Value::as_str)
            .and_then(|last| ids.iter().position(|turn| turn == last))
            .map_or(0, |at| at + 1);
        let mut last = cursor.get(id).and_then(Value::as_str).map(str::to_string);
        for (index, turn) in read.turns.iter().enumerate().skip(start) {
            let (text, _) = super::redact::redact(&turn.text);
            let parts = history_parts(&text);
            let size: usize = parts.iter().map(|part| part.len()).sum();
            if used + size > HISTORY_ANSWER_BYTES && used > 0 {
                more = true;
                break;
            }
            used += size;
            for (part, snippet) in parts.iter().enumerate() {
                chunks.push(json!({
                    "session": id,
                    "kind": read.kind,
                    "turn": ids[index],
                    "part": part,
                    "role": turn.role.as_str(),
                    "ts": turn.ts,
                    "text": snippet,
                }));
            }
            last = Some(ids[index].clone());
        }
        if let Some(last) = last {
            next.insert(id.to_string(), json!(last));
        }
    }
    Ok(json!({
        "ok": true, "chunks": chunks, "cursor": next, "sessions": allowed, "more": more,
    }))
}

/// A turn's text in pieces of about `HISTORY_CHUNK` bytes, cut at whitespace where one exists and
/// always at a character boundary.
fn history_parts(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + HISTORY_CHUNK).min(text.len());
        if end < text.len() {
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if let Some(space) = text[start..end].rfind(char::is_whitespace)
                && space > HISTORY_CHUNK / 2
            {
                end = start + space;
            }
        }
        if end == start {
            end = (start + 1).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
        }
        let piece = text[start..end].trim();
        if !piece.is_empty() {
            out.push(piece);
        }
        start = end;
    }
    out
}

/// `chat_peers`: the chats on THIS computer that `remoteChatRead` selects, so the caller learns
/// which chats it may read and nothing about the others. Each carries `chat` (`<kind>:<id>`),
/// the value `chat_read` takes. Chats on linked computers are never listed (`not_local` for
/// their ids). A chat's title is transcript text, so it passes `redact`.
pub fn peers(settings: &Settings) -> Result<Value> {
    use rightkit_chat_bridge::chat::store::Store;
    use serde_json::json;

    if !settings.enabled {
        return Err(Refusal::new(
            "remote_off",
            "remote tools are turned off in the app's settings (remoteTools)",
        ));
    }
    let store = Store::open_default().map_err(|e| {
        Refusal::new(
            "read_failed",
            format!("could not open the app's chat store: {}", e.kind()),
        )
    })?;
    let me = rightkit_chat_bridge::chat::local_identity(&store)
        .map_err(|e| Refusal::new("read_failed", e.to_string()))?;
    let rows: Vec<Value> = rightkit_chat_bridge::chat::local_peers(&store, &me)
        .iter()
        .filter(|peer| peer.local && selected(settings, Some(peer.kind.as_str()), &peer.session))
        .map(|peer| {
            let folder = std::path::Path::new(&peer.cwd)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            json!({
                "chat": format!("{}:{}", peer.kind, peer.session),
                "id": peer.session,
                "kind": peer.kind,
                "name": super::redact::redact(&peer.name).0,
                "account": peer.account,
                "state": peer.status,
                "liveness": peer.liveness,
                "updatedMs": peer.updated_ms,
                "folder": super::redact::redact(&folder).0,
            })
        })
        .collect();
    let returned = rows.len();
    Ok(json!({
        "ok": true,
        "peers": rows,
        "returned": returned,
    }))
}
