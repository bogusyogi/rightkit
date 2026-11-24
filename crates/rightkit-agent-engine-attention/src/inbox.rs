// Agent Inbox (RK-3, the RightKit half of AG-3 and the routing part of AG-5).
//
// An Inbox item is what the agent found, why, the evidence behind it, what it proposes, and
// where it stands. This module holds: the item, dedupe on (check id + sorted fact ids), a snooze
// and an expiry that run on a caller-supplied clock, quiet hours in local civil time that HOLD
// an alert (never drop it), and the routing rules of "Alerts follow you" as pure functions.
//
// Naming: types are `Inbox*` and the module is `inbox`. Nothing here uses `chat_inbox` or
// anything that reads as Pulse's `/var/lib/pulse-inbox`; this crate never names a path outside
// its own `<state-dir>/attention/` directory. The pre-existing `AttentionState` enum is a
// different thing (typed banner status) and is untouched.
//
// Time: every instant is Unix seconds (`i64`) and always passed in; nothing here reads a clock.
//
// Storage: the decision logic is pure functions over `&mut Vec<InboxItem>`. The host persists
// through `InboxStore::update`, one atomic read-modify-write. `FileInboxStore` follows the
// crate's existing pattern (`<state-dir>/attention/inbox.json`, `lock_attention_state`,
// `atomic_write`, damaged-file set-aside); `MemoryInboxStore` is for hosts that keep items
// elsewhere. Storage failures are returned, never swallowed.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::QuietHoursResponse;
use super::QuietHoursWindow;

// ─── The item ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InboxState {
    /// Waiting for the owner; eligible for routing.
    Open,
    /// Hidden until `snooze_until`, then returns as `Open`.
    Snoozed,
    /// The owner dismissed it. Terminal; the key stays dismissed.
    Dismissed,
    /// The owner acted on it. Terminal.
    Handled,
    /// `expires_at` passed before it was resolved. Terminal.
    Expired,
}

impl InboxState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            InboxState::Dismissed | InboxState::Handled | InboxState::Expired
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalNeed {
    /// Safe to run without a signed approval.
    None,
    /// Needs a signed approval bound to the exact effect before it runs.
    Signed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InboxAction {
    pub id: String,
    pub label: String,
    pub approval: ApprovalNeed,
    /// True when the agent already did this; false when it is only proposed.
    #[serde(default)]
    pub done: bool,
}

/// One alert already sent to one device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InboxDelivery {
    pub device_id: String,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InboxItem {
    pub id: String,
    pub finding: String,
    pub rule: String,
    pub check_id: String,
    /// Sorted, de-duplicated.
    pub fact_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub proposed_actions: Vec<InboxAction>,
    pub state: InboxState,
    pub dedupe_key: String,
    #[serde(default)]
    pub urgent: bool,
    pub created_at: i64,
    #[serde(default)]
    pub snooze_until: Option<i64>,
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// When it reached a terminal state.
    #[serde(default)]
    pub resolved_at: Option<i64>,
    #[serde(default)]
    pub deliveries: Vec<InboxDelivery>,
}

/// What a check hands over to raise an item. The host mints the id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewInboxItem {
    pub id: String,
    pub finding: String,
    pub rule: String,
    pub check_id: String,
    pub fact_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub proposed_actions: Vec<InboxAction>,
    #[serde(default)]
    pub urgent: bool,
    #[serde(default)]
    pub expires_at: Option<i64>,
}

fn escape_key_part(part: &str, out: &mut String) {
    for ch in part.chars() {
        if ch == '\\' || ch == '|' || ch == ',' {
            out.push('\\');
        }
        out.push(ch);
    }
}

/// The dedupe key: the check id, then the fact ids sorted and de-duplicated, with `\`, `|` and
/// `,` escaped so two different (check, facts) pairs can never produce the same key.
pub fn dedupe_key(check_id: &str, fact_ids: &[String]) -> String {
    let sorted: BTreeSet<&str> = fact_ids.iter().map(String::as_str).collect();
    let mut key = String::new();
    escape_key_part(check_id, &mut key);
    key.push('|');
    for (i, fact) in sorted.iter().enumerate() {
        if i > 0 {
            key.push(',');
        }
        escape_key_part(fact, &mut key);
    }
    key
}

// ─── Raise, dismiss, snooze, expire (pure over the item list) ────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaiseOutcome {
    /// A new open item was stored.
    Created { id: String },
    /// An open or snoozed item with this key exists; nothing was added.
    AlreadyOpen { id: String },
    /// An item with this key was dismissed, handled or expired; nothing was added. A dismissed
    /// key stays dismissed.
    Suppressed { id: String, state: InboxState },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Applied,
    /// The item is not in a state this change applies to, or the argument is not usable.
    NoChange,
    NotFound,
}

/// Raises an item. The same key never yields a second non-terminal item, and a key whose item
/// reached any terminal state is never raised again (so a re-run check cannot nag).
///
/// A candidate whose `expires_at` is already past is stored as an `Expired` record, so the key
/// is remembered, and reported as `Suppressed`.
pub fn raise(
    items: &mut Vec<InboxItem>,
    new: NewInboxItem,
    now: i64,
) -> Result<RaiseOutcome, String> {
    if new.id.trim().is_empty() {
        return Err("inbox item id is empty".to_string());
    }
    if new.check_id.trim().is_empty() {
        return Err("inbox item check id is empty".to_string());
    }
    let key = dedupe_key(&new.check_id, &new.fact_ids);
    if let Some(existing) = items.iter().find(|i| i.dedupe_key == key) {
        return Ok(if existing.state.is_terminal() {
            RaiseOutcome::Suppressed {
                id: existing.id.clone(),
                state: existing.state,
            }
        } else {
            RaiseOutcome::AlreadyOpen {
                id: existing.id.clone(),
            }
        });
    }
    if items.iter().any(|i| i.id == new.id) {
        return Err(format!("inbox item id '{}' is already in use", new.id));
    }
    let id = new.id.clone();
    let facts: BTreeSet<String> = new.fact_ids.into_iter().collect();
    let expired = new.expires_at.is_some_and(|at| at <= now);
    items.push(InboxItem {
        id: new.id,
        finding: new.finding,
        rule: new.rule,
        check_id: new.check_id,
        fact_ids: facts.into_iter().collect(),
        evidence_ids: new.evidence_ids,
        proposed_actions: new.proposed_actions,
        state: if expired {
            InboxState::Expired
        } else {
            InboxState::Open
        },
        dedupe_key: key,
        urgent: new.urgent,
        created_at: now,
        snooze_until: None,
        expires_at: new.expires_at,
        resolved_at: if expired { Some(now) } else { None },
        deliveries: Vec::new(),
    });
    Ok(if expired {
        RaiseOutcome::Suppressed {
            id,
            state: InboxState::Expired,
        }
    } else {
        RaiseOutcome::Created { id }
    })
}

fn resolve(items: &mut [InboxItem], id: &str, to: InboxState, now: i64) -> Transition {
    let Some(item) = items.iter_mut().find(|i| i.id == id) else {
        return Transition::NotFound;
    };
    if item.state.is_terminal() {
        return Transition::NoChange;
    }
    item.state = to;
    item.snooze_until = None;
    item.resolved_at = Some(now);
    Transition::Applied
}

/// The owner dismissed the item on any device. It clears everywhere because routing reads
/// state, and the key stays dismissed.
pub fn dismiss(items: &mut [InboxItem], id: &str, now: i64) -> Transition {
    resolve(items, id, InboxState::Dismissed, now)
}

/// The owner acted on the item on any device.
pub fn mark_handled(items: &mut [InboxItem], id: &str, now: i64) -> Transition {
    resolve(items, id, InboxState::Handled, now)
}

/// Snoozes an open or snoozed item until `until` (must be after `now`). Re-snoozing replaces
/// the time.
pub fn snooze(items: &mut [InboxItem], id: &str, until: i64, now: i64) -> Transition {
    let Some(item) = items.iter_mut().find(|i| i.id == id) else {
        return Transition::NotFound;
    };
    if item.state.is_terminal() || until <= now {
        return Transition::NoChange;
    }
    item.state = InboxState::Snoozed;
    item.snooze_until = Some(until);
    Transition::Applied
}

/// Records that an alert for `id` was actually sent to `device_id`. The host calls this after a
/// successful send, so a failed send is re-offered by the next routing pass.
pub fn record_delivery(items: &mut [InboxItem], id: &str, device_id: &str, now: i64) -> Transition {
    let Some(item) = items.iter_mut().find(|i| i.id == id) else {
        return Transition::NotFound;
    };
    if item.state != InboxState::Open {
        return Transition::NoChange;
    }
    if item.deliveries.iter().any(|d| d.device_id == device_id) {
        return Transition::NoChange;
    }
    item.deliveries.push(InboxDelivery {
        device_id: device_id.to_string(),
        at: now,
    });
    Transition::Applied
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReport {
    pub expired: Vec<String>,
    pub woken: Vec<String>,
}

/// Moves time forward. Expiry is checked first, so an item whose expiry and snooze both passed
/// is expired, not woken. A woken item returns as `Open` with its delivery history cleared, so
/// it alerts again.
pub fn tick(items: &mut [InboxItem], now: i64) -> TickReport {
    let mut report = TickReport::default();
    for item in items.iter_mut() {
        if item.state.is_terminal() {
            continue;
        }
        if item.expires_at.is_some_and(|at| at <= now) {
            item.state = InboxState::Expired;
            item.snooze_until = None;
            item.resolved_at = Some(now);
            report.expired.push(item.id.clone());
            continue;
        }
        if item.state == InboxState::Snoozed && item.snooze_until.is_none_or(|at| at <= now) {
            item.state = InboxState::Open;
            item.snooze_until = None;
            item.deliveries.clear();
            report.woken.push(item.id.clone());
        }
    }
    report
}

// ─── Quiet hours in local civil time ─────────────────────────────────────────

fn hhmm_minutes(value: &str) -> Option<i64> {
    if !super::valid_hhmm(value) {
        return None;
    }
    let (h, m) = value.split_once(':')?;
    Some(h.parse::<i64>().ok()? * 60 + m.parse::<i64>().ok()?)
}

/// Seconds since local midnight at `unix`, using the zone offset in force at that instant. An
/// offset `rightkit-time` rejects is treated as UTC rather than failing.
fn local_second_of_day(unix: i64, offset_secs_at: &dyn Fn(i64) -> i32) -> i64 {
    let civil = rightkit_time::DateTime::from_unix(unix, offset_secs_at(unix))
        .or_else(|| rightkit_time::DateTime::from_unix(unix, 0));
    match civil {
        Some(c) => c.hour as i64 * 3600 + c.minute as i64 * 60 + c.second as i64,
        None => unix.rem_euclid(86_400),
    }
}

/// If `now` is inside quiet hours, the Unix second at which they end (the first instant outside
/// every window); `None` when quiet hours are not in force.
///
/// `windows` are `HH:MM` local-time windows; `start > end` crosses midnight and `start == end`
/// is empty (the same rules as the crate's existing quiet-hours predicate). Windows with an
/// unparsable time are ignored. `offset_secs_at(unix)` returns the zone's UTC offset in seconds,
/// east positive, at that instant, so the caller's zone database handles DST. Windows that
/// together cover the whole day hold nothing: a window that never ends would drop alerts, not
/// hold them.
pub fn quiet_release_at(
    now: i64,
    windows: &[QuietHoursWindow],
    offset_secs_at: &dyn Fn(i64) -> i32,
) -> Option<i64> {
    let parsed: Vec<(i64, i64)> = windows
        .iter()
        .filter_map(|w| Some((hhmm_minutes(&w.start)? * 60, hhmm_minutes(&w.end)? * 60)))
        .filter(|(s, e)| s != e)
        .collect();
    if parsed.is_empty() {
        return None;
    }
    let mut t = now;
    for _ in 0..(parsed.len() * 2 + 2) {
        let sod = local_second_of_day(t, offset_secs_at);
        let mut next: Option<i64> = None;
        for &(start, end) in &parsed {
            let inside = if start < end {
                start <= sod && sod < end
            } else {
                sod >= start || sod < end
            };
            if inside {
                let candidate = t + (end - sod).rem_euclid(86_400);
                next = Some(next.map_or(candidate, |n| n.max(candidate)));
            }
        }
        match next {
            None => return if t == now { None } else { Some(t) },
            Some(n) => t = n,
        }
    }
    None
}

/// As [`quiet_release_at`], for the crate's stored quiet-hours settings (`enabled` honoured).
pub fn quiet_hours_release_at(
    settings: &QuietHoursResponse,
    now: i64,
    offset_secs_at: &dyn Fn(i64) -> i32,
) -> Option<i64> {
    if !settings.enabled {
        return None;
    }
    quiet_release_at(now, &settings.windows, offset_secs_at)
}

// ─── Routing: "Alerts follow you" ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DeviceKind {
    Computer,
    Phone,
}

/// What a desktop's presence reports. Anything but `Available` keeps the notch quiet.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PresenceState {
    Available,
    InMeeting,
    Fullscreen,
    DoNotDisturb,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DevicePresence {
    pub device_id: String,
    pub kind: DeviceKind,
    /// Unix seconds of the last real (human) input; `None` when never or unknown.
    #[serde(default)]
    pub last_input_at: Option<i64>,
    pub presence: PresenceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingSettings {
    /// A computer counts as in use when its last real input is at most this many seconds old.
    /// Blueprint default 120 (2 minutes).
    pub recent_input_secs: i64,
    /// An item shown on a computer and still untouched this many seconds later also goes to the
    /// phone. Blueprint default 180 (3 minutes).
    pub escalate_to_phone_secs: i64,
    /// Quiet hours hold urgent items too unless this is true. The blueprint lists no exception,
    /// so the default is false.
    pub urgent_bypasses_quiet_hours: bool,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        RoutingSettings {
            recent_input_secs: 120,
            escalate_to_phone_secs: 180,
            urgent_bypasses_quiet_hours: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteReason {
    /// Snoozed, dismissed, handled or expired: nothing to send, and it clears everywhere.
    NotOpen,
    /// Quiet hours; `hold_until` says when it is released.
    HeldQuietHours,
    /// Urgent: the computer in use (if any) and the phone together.
    Urgent,
    /// A computer is in use; it gets the item and the phone follows at `escalate_phone_at`.
    ComputerInUse,
    /// A computer got it and sat untouched past the escalation time; the phone gets it too.
    EscalatedToPhone,
    /// No computer is in use; the phone gets it.
    NoComputerInUse,
    /// Nothing reachable is listed. The item stays open and is routed again on the next pass.
    NoReachableDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    pub reason: RouteReason,
    /// Devices that should be sent the alert now and have not been yet.
    pub deliver_now: Vec<String>,
    /// When the phone is due to be added if the item is still untouched; route again then.
    pub escalate_phone_at: Option<i64>,
    /// While held by quiet hours: when to route again.
    pub hold_until: Option<i64>,
}

impl RouteDecision {
    fn empty(reason: RouteReason) -> Self {
        RouteDecision {
            reason,
            deliver_now: Vec::new(),
            escalate_phone_at: None,
            hold_until: None,
        }
    }
}

/// Decides where `item` goes at `now`. Pure: the same inputs always give the same answer, so the
/// host can call it on every pass and on every device report.
///
/// `quiet_release_at` is the result of [`quiet_release_at`] for `now` (`Some(end)` while quiet
/// hours are in force).
///
/// Rules ("Alerts follow you"):
/// - Not open: nothing.
/// - Quiet hours: held (urgent too, unless the setting says otherwise); released by calling
///   again once `now` reaches `hold_until`.
/// - Urgent: the computer in use and the phone at once.
/// - A computer with real input within `recent_input_secs` and `Available` presence is in use;
///   the one with the newest input gets it. The phone is added once the item has sat on a
///   computer for `escalate_to_phone_secs` (measured from the first send to a computer).
/// - No computer in use: the phone.
///
/// Devices already sent this item are never listed again.
pub fn route_item(
    item: &InboxItem,
    devices: &[DevicePresence],
    settings: &RoutingSettings,
    quiet_release_at: Option<i64>,
    now: i64,
) -> RouteDecision {
    if item.state != InboxState::Open {
        return RouteDecision::empty(RouteReason::NotOpen);
    }
    if let Some(release) = quiet_release_at {
        if release > now && !(item.urgent && settings.urgent_bypasses_quiet_hours) {
            let mut held = RouteDecision::empty(RouteReason::HeldQuietHours);
            held.hold_until = Some(release);
            return held;
        }
    }

    let phones: Vec<&DevicePresence> = devices
        .iter()
        .filter(|d| d.kind == DeviceKind::Phone)
        .collect();
    // Newest input wins; ties go to the smaller device id, so the answer is deterministic.
    let computer_in_use = devices
        .iter()
        .filter(|d| d.kind == DeviceKind::Computer && d.presence == PresenceState::Available)
        .filter_map(|d| d.last_input_at.map(|at| (at, d)))
        .filter(|(at, _)| now.saturating_sub(*at) <= settings.recent_input_secs)
        .max_by(|(a_at, a), (b_at, b)| a_at.cmp(b_at).then_with(|| b.device_id.cmp(&a.device_id)))
        .map(|(_, d)| d);

    let mut wanted: Vec<&str> = Vec::new();
    let mut reason = RouteReason::NoComputerInUse;
    let mut escalate_phone_at = None;

    if item.urgent {
        reason = RouteReason::Urgent;
        if let Some(c) = computer_in_use {
            wanted.push(c.device_id.as_str());
        }
        wanted.extend(phones.iter().map(|p| p.device_id.as_str()));
    } else if let Some(c) = computer_in_use {
        reason = RouteReason::ComputerInUse;
        wanted.push(c.device_id.as_str());
        let shown_at = item
            .deliveries
            .iter()
            .filter(|d| !phones.iter().any(|p| p.device_id == d.device_id))
            .map(|d| d.at)
            .min()
            .unwrap_or(now);
        let due = shown_at.saturating_add(settings.escalate_to_phone_secs);
        if now >= due {
            reason = RouteReason::EscalatedToPhone;
            wanted.extend(phones.iter().map(|p| p.device_id.as_str()));
        } else {
            escalate_phone_at = Some(due);
        }
    } else {
        wanted.extend(phones.iter().map(|p| p.device_id.as_str()));
    }

    if wanted.is_empty() {
        return RouteDecision::empty(RouteReason::NoReachableDevice);
    }
    let deliver_now = wanted
        .into_iter()
        .filter(|id| !item.deliveries.iter().any(|d| d.device_id == *id))
        .map(str::to_string)
        .collect();
    RouteDecision {
        reason,
        deliver_now,
        escalate_phone_at,
        hold_until: None,
    }
}

// ─── Storage ─────────────────────────────────────────────────────────────────

/// The host's persistence. `update` is one atomic read-modify-write over the whole item list:
/// load, run `f`, save if `f` changed anything, return `f`'s result. Raising, dismissing and
/// routing each run inside one `update`, so two requests cannot lose each other's change.
pub trait InboxStore {
    fn update<R>(&self, f: impl FnOnce(&mut Vec<InboxItem>) -> R) -> Result<R, String>;
}

/// Items kept in memory; for hosts that persist elsewhere, and for embedding.
#[derive(Default)]
pub struct MemoryInboxStore {
    items: std::sync::Mutex<Vec<InboxItem>>,
}

impl InboxStore for MemoryInboxStore {
    fn update<R>(&self, f: impl FnOnce(&mut Vec<InboxItem>) -> R) -> Result<R, String> {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&mut items))
    }
}

/// Items kept in `<state-dir>/attention/inbox.json` under a workspace root, with the same
/// serialisation lock, atomic write and damaged-file set-aside as the rest of the crate.
pub struct FileInboxStore {
    pub workspace_root: String,
}

impl FileInboxStore {
    pub fn new(workspace_root: impl Into<String>) -> Self {
        FileInboxStore {
            workspace_root: workspace_root.into(),
        }
    }

    fn path(&self) -> std::path::PathBuf {
        super::attention_dir(&self.workspace_root).join("inbox.json")
    }
}

impl InboxStore for FileInboxStore {
    fn update<R>(&self, f: impl FnOnce(&mut Vec<InboxItem>) -> R) -> Result<R, String> {
        let _guard = super::lock_attention_state();
        let path = self.path();
        let mut items = super::read_json::<Vec<InboxItem>>(&path).unwrap_or_default();
        let before = serde_json::to_string(&items).map_err(|e| e.to_string())?;
        let result = f(&mut items);
        let after = serde_json::to_string(&items).map_err(|e| e.to_string())?;
        if after != before {
            super::write_json(&path, &items)?;
        }
        Ok(result)
    }
}

// ─── The engine over a store ─────────────────────────────────────────────────

/// What one pass found: time-driven changes, and where each open item goes now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollReport {
    pub ticked: TickReport,
    /// One entry per open item, in store order. Entries with an empty `deliver_now` need no send.
    pub decisions: Vec<(String, RouteDecision)>,
}

pub struct Inbox<S: InboxStore> {
    store: S,
}

impl<S: InboxStore> Inbox<S> {
    pub fn new(store: S) -> Self {
        Inbox { store }
    }

    pub fn raise(&self, new: NewInboxItem, now: i64) -> Result<RaiseOutcome, String> {
        self.store.update(|items| raise(items, new, now))?
    }

    pub fn dismiss(&self, id: &str, now: i64) -> Result<Transition, String> {
        self.store.update(|items| dismiss(items, id, now))
    }

    pub fn mark_handled(&self, id: &str, now: i64) -> Result<Transition, String> {
        self.store.update(|items| mark_handled(items, id, now))
    }

    pub fn snooze(&self, id: &str, until: i64, now: i64) -> Result<Transition, String> {
        self.store.update(|items| snooze(items, id, until, now))
    }

    pub fn record_delivery(
        &self,
        id: &str,
        device_id: &str,
        now: i64,
    ) -> Result<Transition, String> {
        self.store
            .update(|items| record_delivery(items, id, device_id, now))
    }

    pub fn list(&self) -> Result<Vec<InboxItem>, String> {
        self.store.update(|items| items.clone())
    }

    /// One pass: expire and wake by time, then route every open item. `quiet` is the stored
    /// quiet-hours setting; `offset_secs_at` is the zone offset function (see
    /// [`quiet_release_at`]). Nothing is marked sent: the host sends, then calls
    /// [`Inbox::record_delivery`], so a failed send is offered again.
    pub fn poll(
        &self,
        now: i64,
        devices: &[DevicePresence],
        settings: &RoutingSettings,
        quiet: &QuietHoursResponse,
        offset_secs_at: &dyn Fn(i64) -> i32,
    ) -> Result<PollReport, String> {
        let release = quiet_hours_release_at(quiet, now, offset_secs_at);
        self.store.update(|items| {
            let ticked = tick(items, now);
            let decisions = items
                .iter()
                .filter(|i| i.state == InboxState::Open)
                .map(|i| (i.id.clone(), route_item(i, devices, settings, release, now)))
                .collect();
            PollReport { ticked, decisions }
        })
    }
}
