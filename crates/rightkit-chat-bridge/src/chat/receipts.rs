//! Receipts back to the sender. A send receipt says what happened when the message was sent.
//! These updates say what happened later, once the receiving side learns it:
//! * `delivered`: the receiving chat had the message, but only after the sender stopped waiting
//!   for the send's answer (it was told `unknown`, #437, contract G-C2). Sent by the receiver
//!   only when its delivery ran past `LATE_AFTER_MS`;
//! * `seen`: the chat's hook claimed the message and wrote it into its output for the chat's model
//!   (persisted -> claimed -> emitted; a claim whose output was never written is recovered after
//!   120 s and handed over again, so delivery is at least once);
//! * `picked-up`: the message was in Codex's own queue and the thread took it from there;
//! * `released`: a held message was read from the chat inbox (`inbox --ack`), or the chat that
//!   held it said it let it through;
//! * `refused`: the owner refused a held message (`inbox --refuse`), or the chat said it refused
//!   it after holding it;
//! * `expired`: a held message was dropped unread from the inbox, or a queued Codex message was
//!   not taken within 24 hours, or the chat reported it expired.
//!
//! `delivered` is otherwise the send receipt itself, not an update.
//!
//! Each update is addressed to the sending chat's id. On the sender's computer it lands in that
//! chat's receipt list (`<cli> chat receipts`, and the receipt lines of `<cli> chat inbox`), and
//! a chat whose hook runs gets a short notice of new ones (`take_notice`, batched, rate-limited,
//! switch `chatReceiptNotices`). A sender on this computer is written at once. A sender on
//! another computer is queued in `receipt-outbox` and posted over the same link replies use
//! (`<cli> chat receipt-post`); the hub's background pass posts it and retries. Hooks and the
//! CLI never wait for ssh. Best effort: an update that cannot be written is dropped silently.
//!
//! As with envelopes, ssh authenticates the computer and nothing else. The update text is cut
//! to plain words, so a linked computer cannot put markup into a chat's notice.

use super::codex_hook;
use super::deliver_codex;
use super::envelope::{Envelope, now_ms, plain_label};
use super::links;
use super::store::Store;
use serde::{Deserialize, Serialize};

pub const DELIVERED: &str = "delivered";
pub const SEEN: &str = "seen";
pub const PICKED_UP: &str = "picked-up";
pub const RELEASED: &str = "released";
pub const REFUSED: &str = "refused";
pub const EXPIRED: &str = "expired";
const STATES: [&str; 6] = [DELIVERED, SEEN, PICKED_UP, RELEASED, REFUSED, EXPIRED];

const WATCH_DIR: &str = "receipt-watch";
const OUTBOX_DIR: &str = "receipt-outbox";
const SENDERS_DIR: &str = "senders";
/// A queued Codex message is watched, and an outbound update retried, for this long.
const WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
/// Posts that failed this many times are dropped.
const OUTBOX_ATTEMPTS: u32 = 50;
/// The most a chat's hook notice is held back by: at most one notice per this many ms.
pub const NOTICE_GAP_MS: u64 = 60_000;
/// The most receipts one notice names; the rest wait for a later notice.
pub const NOTICE_BATCH: usize = 10;
/// A delivery that took this long (ms) may have outlived the sender's wait for the answer (the
/// engine's first attempt waits 15 s, `MESH_FIRST_CALL_WAIT`), so the receiver also sends a
/// `delivered` update. Quicker deliveries are answered in the send receipt alone.
pub const LATE_AFTER_MS: u64 = 12_000;
const DETAIL_MAX: usize = 160;

/// One receipt update: what the chat `by` did with message `msg_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    /// Position in the sending chat's receipt list; set when stored (0 on the wire).
    #[serde(default)]
    pub seq: u64,
    /// The id the send receipt printed.
    pub msg_id: String,
    /// One of the state words above.
    pub state: String,
    /// Plain words saying why (cut to 160 characters, see `plain_label`).
    #[serde(default)]
    pub detail: String,
    /// The receiving chat as its computer shows it: `"<chat> on <computer>"`.
    #[serde(default)]
    pub by: String,
    /// The sending chat the update is for, by its id on the sender's computer.
    #[serde(default)]
    pub to: String,
    /// When it happened (ms since the epoch).
    #[serde(default)]
    pub ts: u64,
}

/// An update for a sender on another computer, kept until a link takes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outbox {
    /// The sending computer's name, as the envelope named it.
    pub to_device: String,
    pub update: Update,
    #[serde(default)]
    pub attempts: u32,
    /// Earliest time (ms) of the next post; a failed post waits longer each time.
    #[serde(default)]
    pub next_ms: u64,
}

/// A queued Codex message whose pick-up is watched. `queue_id` is Codex's own queue id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {
    pub msg_id: String,
    pub queue_id: String,
    pub from_device: String,
    pub from_session: String,
    /// The receiving chat, `"<chat> on <computer>"`.
    pub by: String,
    pub created_ms: u64,
}

fn clip(text: &str) -> String {
    plain_label(text, DETAIL_MAX)
}

/// This computer's name, or empty when it is unknown.
fn computer_of(store: &Store) -> String {
    super::local_identity(store)
        .map(|me| me.alias)
        .unwrap_or_default()
}

/// `"<chat> on <computer>"`, the way chats are shown.
pub fn by_label(chat: &str, computer: &str) -> String {
    format!("{chat} on {computer}")
}

/// The receiving chat for message `msg_id` as this computer shows it: the chat's name from its
/// route, else its id.
fn by_for(store: &Store, msg_id: &str, session: &str) -> String {
    let chat = store
        .reply_route(msg_id)
        .map(|route| {
            if route.to_name.is_empty() {
                route.to_session
            } else {
                route.to_name
            }
        })
        .unwrap_or_else(|| session.to_string());
    by_label(&chat, &computer_of(store))
}

/// Record that the chat `by` now reports `state` for message `msg_id`, which `from_device`'s
/// chat `from_session` sent. Best effort.
pub fn report(
    store: &Store,
    msg_id: &str,
    from_device: &str,
    from_session: &str,
    by: &str,
    state: &str,
    detail: &str,
) {
    let update = Update {
        seq: 0,
        msg_id: msg_id.to_string(),
        state: state.to_string(),
        detail: clip(detail),
        by: clip(by),
        to: from_session.to_string(),
        ts: now_ms(),
    };
    if from_device == computer_of(store) {
        let _ = store.append_receipt(from_session, &update);
    } else {
        let key = format!("{msg_id}-{state}-{from_session}");
        let item = Outbox {
            to_device: from_device.to_string(),
            update,
            attempts: 0,
            next_ms: 0,
        };
        let _ = store.save_record(&store.record_path(OUTBOX_DIR, &key), &item);
    }
}

/// The receiving side finished delivering `env` to chat `session` here and it took `took_ms`:
/// when that is past `LATE_AFTER_MS` the sender may have been told `unknown` already, so queue a
/// `delivered` update for it (G-C2). `state` is the send receipt's own word. Only the states in
/// which the chat has the message count (not refused, unknown or unsupported). The outbox key is
/// per message and state, so a repeat of the same id replaces the update, never doubles it.
pub fn late_delivered(
    store: &Store,
    session: &str,
    env: &Envelope,
    state: super::ReceiptState,
    took_ms: u64,
) {
    use super::ReceiptState as S;
    if took_ms < LATE_AFTER_MS || !matches!(state, S::Delivered | S::Queued | S::Held) {
        return;
    }
    let by = by_for(store, &env.id, session);
    report(
        store,
        &env.id,
        &env.from.device,
        &env.from.session,
        &by,
        DELIVERED,
        &format!(
            "answered after {} s, receipt {}",
            took_ms / 1000,
            state.as_str()
        ),
    );
}

/// The hook wrote message `msg_id` into a chat's hook output here (claimed, then emitted): it is
/// on its way to that chat's model. Reported only after the output was written; a claim whose
/// output failed goes back to the inbox and is reported when a later run emits it.
pub fn seen(store: &Store, msg_id: &str) {
    note_route(store, msg_id, SEEN, "handed to the chat as added context");
}

/// The hook set message `msg_id` aside because it was too large to hand to the chat whole.
pub fn hook_refused(store: &Store, msg_id: &str) {
    note_route(
        store,
        msg_id,
        REFUSED,
        "too large for the chat's hook to hand over whole; it was not cut",
    );
}

/// A chat's own notice about message `msg_id` (its `peer_message_status` frame, from the reply
/// hub): `delivered` after a hold means released, `refused`, `denied` and `dropped` refused, and
/// `expired` expired. Other statuses change nothing.
pub fn from_chat_status(store: &Store, msg_id: &str, status: &str, reason: &str) {
    let state = match status {
        "delivered" => RELEASED,
        "refused" | "denied" | "dropped" => REFUSED,
        "expired" => EXPIRED,
        _ => return,
    };
    let detail = if reason.trim().is_empty() {
        "the chat said so"
    } else {
        reason
    };
    note_route(store, msg_id, state, detail);
}

/// Report through the route saved for `msg_id` (the sender and the chat it went to).
fn note_route(store: &Store, msg_id: &str, state: &str, detail: &str) {
    let Some(route) = store.reply_route(msg_id) else {
        return;
    };
    let chat = if route.to_name.is_empty() {
        route.to_session.clone()
    } else {
        route.to_name.clone()
    };
    let by = by_label(&chat, &computer_of(store));
    report(
        store,
        msg_id,
        &route.from_device,
        &route.from_session,
        &by,
        state,
        detail,
    );
}

/// A held message was read from `session`'s inbox (`inbox --ack`).
pub fn released(store: &Store, session: &str, env: &Envelope) {
    let by = by_for(store, &env.id, session);
    report(
        store,
        &env.id,
        &env.from.device,
        &env.from.session,
        &by,
        RELEASED,
        "read from the chat inbox by its owner",
    );
}

/// A held message was refused by its owner (`inbox --refuse`).
pub fn refused_by_owner(store: &Store, session: &str, env: &Envelope, reason: &str) {
    let by = by_for(store, &env.id, session);
    report(
        store,
        &env.id,
        &env.from.device,
        &env.from.session,
        &by,
        REFUSED,
        reason,
    );
}

/// A held message was dropped unread from `session`'s inbox to keep the inbox small.
pub fn expired(store: &Store, session: &str, env: &Envelope) {
    let by = by_for(store, &env.id, session);
    report(
        store,
        &env.id,
        &env.from.device,
        &env.from.session,
        &by,
        EXPIRED,
        "dropped unread from the chat inbox to keep it small",
    );
}

/// A message went into Codex's own queue (`queue_id`, which the queue row carries): watch it
/// until the thread takes it. `chat_name` is the receiving Codex chat.
pub fn watch_codex_queue(store: &Store, env: &Envelope, queue_id: &str, chat_name: &str) {
    let watch = Watch {
        msg_id: env.id.clone(),
        queue_id: queue_id.to_string(),
        from_device: env.from.device.clone(),
        from_session: env.from.session.clone(),
        by: by_label(chat_name, &computer_of(store)),
        created_ms: now_ms(),
    };
    let _ = store.save_record(&store.record_path(WATCH_DIR, &env.id), &watch);
}

/// The hub's background pass: report picked-up or expired watches, then post queued updates.
pub fn background(store: &Store) {
    check_watches(store);
    drain_outbox(store);
}

fn check_watches(store: &Store) {
    let now = now_ms();
    for (path, watch) in store.list_records::<Watch>(WATCH_DIR) {
        let (state, detail) = if deliver_codex::queue_row_pending(&watch.queue_id) == Some(false) {
            (
                PICKED_UP,
                "the Codex thread took the message from its queue",
            )
        } else if now.saturating_sub(watch.created_ms) > WINDOW_MS {
            (EXPIRED, "not taken from the Codex queue within 24 hours")
        } else {
            continue;
        };
        report(
            store,
            &watch.msg_id,
            &watch.from_device,
            &watch.from_session,
            &watch.by,
            state,
            detail,
        );
        store.remove_record(&path);
    }
}

fn drain_outbox(store: &Store) {
    let Ok(me) = super::local_identity(store) else {
        return;
    };
    for (path, mut item) in store.list_records::<Outbox>(OUTBOX_DIR) {
        if item.next_ms > now_ms() {
            continue;
        }
        let link = super::reply_address(store, &me, &item.to_device, &item.update.to)
            .ok()
            .and_then(|address| {
                address
                    .split_once(':')
                    .map(|(device, _)| device.to_string())
            })
            .and_then(|device| links::find(store, &device));
        let posted = match link {
            Some(link) => links::post_receipt(&link, &item.update).is_ok(),
            None => false,
        };
        if posted {
            store.remove_record(&path);
            continue;
        }
        item.attempts += 1;
        item.next_ms = now_ms() + u64::from(item.attempts.min(30)) * 60_000;
        let stale = now_ms().saturating_sub(item.update.ts) > WINDOW_MS;
        if item.attempts >= OUTBOX_ATTEMPTS || stale {
            store.remove_record(&path);
        } else {
            let _ = store.save_record(&path, &item);
        }
    }
}

/// Why a receipt was not stored: a code the sending computer can log, and the reason in plain words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub error: String,
}

/// SSH receipts authenticate the complete update with the peer's pinned chat key.
/// Legacy unsigned receipts are deliberately rejected; mesh receipts retain transport authentication.
#[derive(Debug, Serialize, Deserialize)]
pub struct SignedUpdate {
    pub update: Update,
    pub recipient: String,
    pub signature: String,
}

impl SignedUpdate {
    fn input(update: &Update, recipient: &str) -> Vec<u8> {
        // A fixed tuple and struct field order provide unambiguous, domain-separated framing.
        serde_json::to_vec(&(
            super::brand::brand().receipt_domain.as_str(),
            recipient,
            update,
        ))
        .unwrap_or_default()
    }

    pub fn seal(store: &Store, link: &links::Link, update: &Update) -> Result<Self, String> {
        let recipient = link
            .device_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or("receipt requires a pinned recipient identity")?;
        let key = super::chat_key(store).ok_or("receipt signing key unavailable")?;
        Ok(Self {
            update: update.clone(),
            recipient: recipient.to_string(),
            signature: rightkit_fs::digest::hex(key.sign(&Self::input(update, recipient)).as_ref()),
        })
    }
}

pub fn apply_remote(store: &Store, signed: SignedUpdate) -> Result<bool, Refusal> {
    use ring::signature::{ED25519, UnparsedPublicKey};
    let update = signed.update;
    let device = store
        .read_record::<SentNote>(&store.record_path(SENT_DIR, &update.msg_id))
        .map(|sent| sent.device);
    let identity = device.and_then(|d| links::ssh_link_identity(store, &d));
    let Some((link, id)) = identity else {
        return Err(refuse(
            store,
            &update,
            "unknown-sender",
            "no pinned receipt sender",
        ));
    };
    let verified = link
        .chat_key
        .as_deref()
        .and_then(rightkit_fs::digest::unhex)
        .zip(rightkit_fs::digest::unhex(&signed.signature))
        .is_some_and(|(key, sig)| {
            UnparsedPublicKey::new(&ED25519, key)
                .verify(&SignedUpdate::input(&update, &signed.recipient), &sig)
                .is_ok()
        });
    if signed.recipient != super::local_device_id(store) || !verified {
        return Err(refuse(
            store,
            &update,
            "unverified-receipt",
            "receipt signature or recipient does not match",
        ));
    }
    let caller = super::mesh::Caller {
        device_id: id,
        name: link.device,
        owner: false,
        role: rightkit_device_link_types::MemberRole::Computer,
    };
    apply_checked(store, update, &caller, true)
}

/// Most receipt updates stored for one message (R17); more are refused.
const RECEIPTS_PER_MESSAGE: usize = 16;

/// The order receipt states may advance in. `delivered` (a late send answer) < `seen` < `picked-up` < `released`; `refused` and
/// `expired` end a message (nothing may follow them, and they may follow any state).
fn state_rank(state: &str) -> u8 {
    match state {
        DELIVERED => 0,
        SEEN => 1,
        PICKED_UP => 2,
        RELEASED => 3,
        _ => 4,
    }
}

/// What this computer remembers about a message it sent to another computer: the sending chat,
/// the computer it went to, and when. Written when the post starts (`note_outbound`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SentNote {
    msg_id: String,
    from_session: String,
    /// The linked computer's name, as the link shows it.
    device: String,
    /// The paired device-link id when the message went by (or could go by) the device link.
    #[serde(default)]
    device_id: String,
    ts: u64,
}

const SENT_DIR: &str = "sent-messages";

/// Remember that `env` is being posted over `link`, so only that computer can later send a
/// receipt for it. Best effort: a note that cannot be written leaves the message's receipts
/// refused (fail closed).
pub fn note_outbound(store: &Store, env: &Envelope, link: &links::Link) {
    if !codex_hook::safe_component(&env.id) {
        return;
    }
    let note = SentNote {
        msg_id: env.id.clone(),
        from_session: env.from.session.clone(),
        device: link.device.clone(),
        device_id: super::mesh::mesh_id(link)
            .map(str::to_string)
            .or_else(|| link.device_id.clone())
            .unwrap_or_default(),
        ts: now_ms(),
    };
    let _ = store.save_record(&store.record_path(SENT_DIR, &env.id), &note);
}

fn refuse(store: &Store, update: &Update, code: &'static str, error: &str) -> Refusal {
    let refusal = Refusal {
        code,
        error: error.to_string(),
    };
    log_refusal(store, update, &refusal);
    refusal
}

/// Store an update a linked computer sent for one of this computer's sending chats. Refused
/// while Chat is off here (`chat-off`), for a malformed update (`invalid-receipt`), and (R17)
/// unless it matches a message this computer sent: `unknown-message` (no send record),
/// `wrong-sender` (it did not come from the computer the message went to; checked when the
/// mesh `caller` is known), `wrong-chat` (not the chat that sent it), `bad-transition` (a state
/// that does not move forward, or anything after a final one) and `too-many` (over
/// `RECEIPTS_PER_MESSAGE`). A caller's registry name replaces the payload's `by`. Returns
/// whether the update was new.
pub fn apply_remote_from(
    store: &Store,
    update: Update,
    caller: Option<&super::mesh::Caller>,
) -> Result<bool, Refusal> {
    match caller {
        Some(caller) => apply_checked(store, update, caller, false),
        // No proven caller: the ssh route posts a `SignedUpdate` through `apply_remote`; an unsigned
        // update with no caller is never applied (legacy receipts fail closed, R17).
        None => Err(refuse(
            store,
            &update,
            "unknown-sender",
            "receipt without a proven sender or signature",
        )),
    }
}

fn apply_checked(
    store: &Store,
    mut update: Update,
    caller: &super::mesh::Caller,
    via_ssh: bool,
) -> Result<bool, Refusal> {
    if let Some(off) = super::chat_off(store, "", "") {
        return Err(Refusal {
            code: "chat-off",
            error: off.detail,
        });
    }
    if !codex_hook::safe_component(&update.msg_id) || !codex_hook::safe_component(&update.to) {
        return Err(Refusal {
            code: "invalid-receipt",
            error: "the receipt names no valid message or chat".to_string(),
        });
    }
    if !STATES.contains(&update.state.as_str()) {
        return Err(Refusal {
            code: "invalid-receipt",
            error: format!("unknown receipt state {}", clip(&update.state)),
        });
    }
    let Some(sent) = store.read_record::<SentNote>(&store.record_path(SENT_DIR, &update.msg_id))
    else {
        return Err(refuse(
            store,
            &update,
            "unknown-message",
            "this computer sent no message with that id",
        ));
    };
    if sent.msg_id != update.msg_id || sent.from_session != update.to {
        return Err(refuse(
            store,
            &update,
            "wrong-chat",
            "that message was not sent by that chat",
        ));
    }
    {
        let same_id = !sent.device_id.is_empty() && sent.device_id == caller.device_id;
        // An ssh link is bound by the name of the link the message went over (a message sent by
        // the device link may be answered over its ssh twin).
        let same_name = (via_ssh || sent.device_id.is_empty())
            && !sent.device.is_empty()
            && links::same_computer_key(&sent.device) == links::same_computer_key(&caller.name);
        if !same_id && !same_name {
            return Err(refuse(
                store,
                &update,
                "wrong-sender",
                "that message did not go to the computer that sent this receipt",
            ));
        }
        update.by = clip(&caller.name);
    }
    let earlier: Vec<Update> = store
        .receipts_after(&update.to, 0)
        .into_iter()
        .filter(|u| u.msg_id == update.msg_id)
        .collect();
    if earlier.iter().any(|u| u.state == update.state) {
        return Ok(false);
    }
    // A late `delivered` is only news to a sender that heard nothing: once any other update for
    // the message is stored the sender knows it arrived, and the update (which may be posted
    // after a later one, the outbox has no order) is dropped without a refusal.
    if update.state == DELIVERED && !earlier.is_empty() {
        return Ok(false);
    }
    if earlier.len() >= RECEIPTS_PER_MESSAGE {
        return Err(refuse(
            store,
            &update,
            "too-many",
            "too many receipts for that message",
        ));
    }
    let top = earlier.iter().map(|u| state_rank(&u.state)).max();
    let ended = earlier.iter().any(|u| state_rank(&u.state) == 4);
    if ended || top.is_some_and(|top| state_rank(&update.state) <= top) {
        return Err(refuse(
            store,
            &update,
            "bad-transition",
            "that receipt does not move the message forward",
        ));
    }
    update.detail = clip(&update.detail);
    update.seq = 0;
    store
        .append_receipt(&update.to, &update)
        .map_err(|e| Refusal {
            code: "not-stored",
            error: e.to_string(),
        })
}

/// The last refusals of a linked computer's updates, kept for the owner to read.
const REFUSALS_FILE: &str = "receipt-refusals.json";
const REFUSALS_KEPT: usize = 20;

/// Log a refused update: one line on stderr and an entry (time, code, message id and the chat id
/// it named, plain words only) in `receipt-refusals.json`, the newest `REFUSALS_KEPT`. A write
/// that fails changes nothing: the refusal already stands.
fn log_refusal(store: &Store, update: &Update, refusal: &Refusal) {
    let entry = serde_json::json!({
        "ts": now_ms(),
        "code": refusal.code,
        "msgId": clip(&update.msg_id),
        "chat": clip(&update.to),
    });
    eprintln!(
        "{}: refused a receipt update ({}) for chat {}",
        super::brand::brand().chat_marker(),
        refusal.code,
        clip(&update.to)
    );
    let _ = store.update_state(REFUSALS_FILE, |list: &mut Vec<serde_json::Value>| {
        list.push(entry);
        let extra = list.len().saturating_sub(REFUSALS_KEPT);
        list.drain(..extra);
    });
}

/// The send record of a chat that sent a message from this computer. Written once per chat.
#[derive(Serialize)]
struct SenderNote {
    session: String,
    ts: u64,
}

/// Note that chat `session` sent a message from this computer, so receipts for it are accepted.
pub fn note_sender(store: &Store, session: &str) {
    let path = store.record_path(SENDERS_DIR, session);
    if !path.is_file() {
        let note = SenderNote {
            session: session.to_string(),
            ts: now_ms(),
        };
        let _ = store.save_record(&path, &note);
    }
}

/// Drop queued updates older than the 24-hour window without posting them.
fn drop_stale_outbox(store: &Store) {
    for (path, item) in store.list_records::<Outbox>(OUTBOX_DIR) {
        if now_ms().saturating_sub(item.update.ts) > WINDOW_MS {
            store.remove_record(&path);
        }
    }
}

/// Forget send records older than a week: their receipts are long past `WINDOW_MS`.
fn drop_old_sent_notes(store: &Store) {
    for (path, note) in store.list_records::<SentNote>(SENT_DIR) {
        if now_ms().saturating_sub(note.ts) > 7 * 24 * 60 * 60 * 1000 {
            store.remove_record(&path);
        }
    }
}

/// Housekeeping, run when the hub starts: report watches that are done or past their window,
/// drop queued updates past theirs, and remove receipt lists past their limits.
pub fn prune(store: &Store) {
    check_watches(store);
    drop_stale_outbox(store);
    drop_old_sent_notes(store);
    store.prune_receipt_files();
}

/// One receipt update as a line for people and chats, for example
/// `"receipt: <id> delivered to <chat> on <device> (seen)"`.
pub fn describe(update: &Update) -> String {
    let by = if update.by.is_empty() {
        "a chat"
    } else {
        update.by.as_str()
    };
    let id = update.msg_id.as_str();
    let detail = update.detail.as_str();
    match update.state.as_str() {
        DELIVERED => format!(
            "receipt: {id} delivered to {by} after a slow send that may have been reported unknown ({detail})"
        ),
        SEEN => format!("receipt: {id} delivered to {by} (seen)"),
        PICKED_UP => format!("receipt: {id} picked up by {by} (picked-up)"),
        RELEASED => format!("receipt: {id} released by {by} (released)"),
        REFUSED => format!("receipt: {id} refused by {by} (refused: {detail})"),
        EXPIRED => format!("receipt: {id} expired before {by} took it (expired: {detail})"),
        other => format!("receipt: {id} {other} from {by} ({detail})"),
    }
}

/// The notice a chat's hook run shows its owner: the chat's new receipt updates, batched.
/// None when the switch (`chatReceiptNotices`) is off, nothing is new, or the last notice was
/// less than `NOTICE_GAP_MS` ago (the updates wait for a later run).
pub fn take_notice(store: &Store, session: &str) -> Option<String> {
    if !store.receipt_notices() {
        return None;
    }
    let updates = store
        .take_receipt_notice(session, now_ms(), NOTICE_GAP_MS, NOTICE_BATCH)
        .ok()?;
    if updates.is_empty() {
        return None;
    }
    let lines: Vec<String> = updates.iter().map(describe).collect();
    Some(lines.join("\n"))
}
