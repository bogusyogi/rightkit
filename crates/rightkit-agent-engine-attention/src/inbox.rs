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
// Zones: quiet-hours windows carry an optional zone name. The crate has no zone database, so the
// caller supplies `offset_for(zone, unix)`, the offset in seconds east of UTC; `zone` is the
// window's own zone name, or `None` when the window names none (the caller's default zone).
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

/// How sure the agent is of a finding. A finding is a candidate, not a verdict: a mismatch can
/// have an innocent explanation, and a lower confidence with `missing_information` says so.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InboxConfidence {
    Low,
    Medium,
    High,
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
    /// How sure the agent is that this finding is right. `None` when the check did not say.
    #[serde(default)]
    pub confidence: Option<InboxConfidence>,
    /// What information is missing: what the agent could not find or confirm and would need in
    /// order to be sure. Empty when nothing is known to be missing.
    #[serde(default)]
    pub missing_information: Vec<String>,
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
    pub confidence: Option<InboxConfidence>,
    #[serde(default)]
    pub missing_information: Vec<String>,
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
        confidence: new.confidence,
        missing_information: new.missing_information,
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
    /// Items whose expiry passed while quiet hours held them, never delivered: they stay open so
    /// routing delivers them once after the hold, and they expire on the tick after that.
    pub expiry_deferred: Vec<String>,
}

/// True when `item` is open, was never delivered, and its expiry instant fell inside a quiet-hours
/// hold that applied to it. Such an item is owed one delivery before it may expire.
fn expiry_fell_in_hold(
    item: &InboxItem,
    settings: &RoutingSettings,
    release_at: &dyn Fn(i64) -> Option<i64>,
) -> bool {
    let Some(expiry) = item.expires_at else {
        return false;
    };
    item.state == InboxState::Open
        && item.deliveries.is_empty()
        && !(item.urgent && settings.urgent_bypasses_quiet_hours)
        && release_at(expiry).is_some_and(|release| release > expiry)
}

/// Moves time forward. Expiry is checked first, so an item whose expiry and snooze both passed
/// is expired, not woken. A woken item returns as `Open` with its delivery history cleared, so
/// it alerts again.
///
/// A held alert is never lost to expiry: an open item that was never delivered and whose expiry
/// instant fell inside a quiet-hours hold is not expired. It stays open, is listed in
/// `expiry_deferred`, is delivered once by routing after the hold ends, and expires on the first
/// tick after that delivery is recorded. A snoozed item is not held by quiet hours (the owner
/// hid it), so it expires normally.
///
/// `release_at(t)` is the release instant of the quiet-hours hold in force at `t`, or `None` when
/// quiet hours are not in force at `t` (see [`quiet_hours_release_at`]). It is asked about the
/// item's expiry instant, so changing the quiet-hours setting afterwards can change the answer.
pub fn tick(
    items: &mut [InboxItem],
    now: i64,
    settings: &RoutingSettings,
    release_at: &dyn Fn(i64) -> Option<i64>,
) -> TickReport {
    let mut report = TickReport::default();
    for item in items.iter_mut() {
        if item.state.is_terminal() {
            continue;
        }
        if item.expires_at.is_some_and(|at| at <= now) {
            if expiry_fell_in_hold(item, settings, release_at) {
                report.expiry_deferred.push(item.id.clone());
                continue;
            }
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

/// Seconds since local midnight at `unix` in the zone whose offset is `offset_secs`. An offset
/// `rightkit-time` rejects is treated as UTC rather than failing.
fn local_second_of_day(unix: i64, offset_secs: i32) -> i64 {
    let civil = rightkit_time::DateTime::from_unix(unix, offset_secs)
        .or_else(|| rightkit_time::DateTime::from_unix(unix, 0));
    match civil {
        Some(c) => c.hour as i64 * 3600 + c.minute as i64 * 60 + c.second as i64,
        None => unix.rem_euclid(86_400),
    }
}

/// Start and end, in seconds from local midnight, of a usable window: both times parse and
/// `start != end` (equal is empty).
fn window_bounds(w: &QuietHoursWindow) -> Option<(i64, i64)> {
    let (s, e) = (hhmm_minutes(&w.start)? * 60, hhmm_minutes(&w.end)? * 60);
    (s != e).then_some((s, e))
}

fn second_inside(start: i64, end: i64, sod: i64) -> bool {
    if start < end {
        start <= sod && sod < end
    } else {
        sod >= start || sod < end
    }
}

/// True when `unix` is inside any of `windows`, each read in its own zone (see
/// [`quiet_release_at`] for `offset_for`). Unlike [`quiet_release_at`] it does not care whether
/// the windows ever end, so windows that cover the whole day are all-day active here.
pub fn quiet_windows_contain(
    unix: i64,
    windows: &[QuietHoursWindow],
    offset_for: &dyn Fn(Option<&str>, i64) -> i32,
) -> bool {
    windows.iter().any(|w| {
        window_bounds(w).is_some_and(|(start, end)| {
            let sod = local_second_of_day(unix, offset_for(w.timezone.as_deref(), unix));
            second_inside(start, end, sod)
        })
    })
}

/// If `now` is inside quiet hours, the Unix second at which they end (the first instant outside
/// every window); `None` when quiet hours are not in force.
///
/// `windows` are `HH:MM` local-time windows; `start > end` crosses midnight and `start == end`
/// is empty (the same rules as the crate's existing quiet-hours predicate). Windows with an
/// unparsable time are ignored. Each window is read in its own zone: `offset_for(zone, unix)`
/// returns that zone's UTC offset in seconds, east positive, at that instant (`zone` is the
/// window's `timezone`, `None` when it names none), so the caller's zone database handles DST.
/// Windows that together cover the whole day hold nothing: a window that never ends would drop
/// alerts, not hold them.
pub fn quiet_release_at(
    now: i64,
    windows: &[QuietHoursWindow],
    offset_for: &dyn Fn(Option<&str>, i64) -> i32,
) -> Option<i64> {
    let parsed: Vec<(&QuietHoursWindow, i64, i64)> = windows
        .iter()
        .filter_map(|w| window_bounds(w).map(|(s, e)| (w, s, e)))
        .collect();
    if parsed.is_empty() {
        return None;
    }
    let mut t = now;
    for _ in 0..(parsed.len() * 2 + 2) {
        let mut next: Option<i64> = None;
        for &(w, start, end) in &parsed {
            let sod = local_second_of_day(t, offset_for(w.timezone.as_deref(), t));
            if second_inside(start, end, sod) {
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
    offset_for: &dyn Fn(Option<&str>, i64) -> i32,
) -> Option<i64> {
    if !settings.enabled {
        return None;
    }
    quiet_release_at(now, &settings.windows, offset_for)
}

// ─── Routing: "Alerts follow you" ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DeviceKind {
    Computer,
    Phone,
}

/// What a desktop's presence reports. Anything but `Available` keeps the notch quiet.
/// `Locked` (a locked screen) is not in use, like the others: routing never counts it as a computer in use.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PresenceState {
    Available,
    InMeeting,
    Fullscreen,
    DoNotDisturb,
    Locked,
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
    /// quiet-hours setting; `offset_for` is the zone offset function (see [`quiet_release_at`]).
    /// An item whose expiry fell inside a quiet-hours hold is delivered once after the hold and
    /// expires on a later pass (see [`tick`]). Nothing is marked sent: the host sends, then calls
    /// [`Inbox::record_delivery`], so a failed send is offered again.
    pub fn poll(
        &self,
        now: i64,
        devices: &[DevicePresence],
        settings: &RoutingSettings,
        quiet: &QuietHoursResponse,
        offset_for: &dyn Fn(Option<&str>, i64) -> i32,
    ) -> Result<PollReport, String> {
        let release_at = |t: i64| quiet_hours_release_at(quiet, t, offset_for);
        let release = release_at(now);
        self.store.update(|items| {
            let ticked = tick(items, now, settings, &release_at);
            let decisions = items
                .iter()
                .filter(|i| i.state == InboxState::Open)
                .map(|i| (i.id.clone(), route_item(i, devices, settings, release, now)))
                .collect();
            PollReport { ticked, decisions }
        })
    }
}

// ─── Host helpers: reads, list views and the alert pass ──────

/// The longest id (item, action, evidence, fact) an act or a reply accepts.
pub const MAX_ID_LEN: usize = 128;

/// An item older than this that was never alerted is not alerted now: a phone that registers late
/// must not get a flood of old items. A snoozed item that wakes is the exception (its delivery
/// history is cleared on purpose).
pub const ALERT_MAX_AGE_SECS: i64 = 24 * 3600;

/// The item's state as of `now`: a snoozed item whose time has passed is open, an unresolved item
/// past its expiry is expired. Read only; [`tick`] is what writes that change back.
pub fn state_at(item: &InboxItem, now: i64) -> InboxState {
    if item.state.is_terminal() {
        return item.state;
    }
    if item.expires_at.is_some_and(|at| at <= now) {
        return InboxState::Expired;
    }
    if item.state == InboxState::Snoozed && item.snooze_until.is_none_or(|at| at <= now) {
        return InboxState::Open;
    }
    item.state
}

/// The state's word as a reply carries it.
pub fn state_word(state: InboxState) -> &'static str {
    match state {
        InboxState::Open => "open",
        InboxState::Snoozed => "snoozed",
        InboxState::Dismissed => "dismissed",
        InboxState::Handled => "handled",
        InboxState::Expired => "expired",
    }
}

/// An id (item, action, evidence, fact) is a plain token: 1 to [`MAX_ID_LEN`] bytes of
/// `A-Z a-z 0-9 . _ : @ # = -`.
pub fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '@' | '#' | '=' | '-')
        })
}

/// One item as a list reply shows it (camelCase): no evidence ids, only their count.
pub fn summary(item: &InboxItem, now: i64) -> serde_json::Value {
    serde_json::json!({
        "id": item.id,
        "finding": item.finding,
        "rule": item.rule,
        "state": state_word(state_at(item, now)),
        "urgent": item.urgent,
        "confidence": item.confidence,
        "createdAt": item.created_at,
        "snoozeUntil": item.snooze_until,
        "expiresAt": item.expires_at,
        "evidenceCount": item.evidence_ids.len(),
        "actions": item.proposed_actions.iter().map(|a| serde_json::json!({
            "id": a.id,
            "label": a.label,
            "approval": a.approval,
            "done": a.done,
        })).collect::<Vec<_>>(),
    })
}

/// The states a list may ask for.
pub fn is_list_state(word: &str) -> bool {
    matches!(
        word,
        "active" | "all" | "open" | "snoozed" | "dismissed" | "handled" | "expired"
    )
}

/// The list view: keeps the items whose state as of `now` matches `which` (`active` is open and
/// snoozed, `all` is every one, otherwise one state's word), sorts urgent first then newest first
/// then by id, and cuts to `limit`. Returns the kept items and whether more were cut.
pub fn list_view(
    mut items: Vec<InboxItem>,
    which: &str,
    limit: usize,
    now: i64,
) -> (Vec<InboxItem>, bool) {
    items.retain(|item| {
        let state = state_at(item, now);
        match which {
            "all" => true,
            "active" => matches!(state, InboxState::Open | InboxState::Snoozed),
            other => state_word(state) == other,
        }
    });
    items.sort_by(|a, b| {
        b.urgent
            .cmp(&a.urgent)
            .then(b.created_at.cmp(&a.created_at))
            .then(a.id.cmp(&b.id))
    });
    let truncated = items.len() > limit;
    items.truncate(limit);
    (items, truncated)
}

/// The item with `id`, if the store holds one.
pub fn find_item<S: InboxStore>(store: &S, id: &str) -> Result<Option<InboxItem>, String> {
    store.update(|items| items.iter().find(|i| i.id == id).cloned())
}

/// The longest item id an alert can name (`PhoneAlert`): 128 bytes.
const ALERT_ID_MAX: usize = 128;

/// An item id inside the set a phone alert can name, `A-Z a-z 0-9 . _ : -` and at most 128 bytes,
/// so no item is ever left without an alert for its id. An id already inside the set is returned
/// as it is. Any other is mapped: each character outside the set becomes `-`, the text is cut to
/// leave room, and the first 8 hex digits of the SHA-256 of the original are appended, so two
/// different raw ids cannot meet. The same raw id always maps to the same id.
pub fn alert_safe_id(raw: &str) -> String {
    let inside = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-');
    if !raw.is_empty() && raw.len() <= ALERT_ID_MAX && raw.chars().all(inside) {
        return raw.to_string();
    }
    let digest = rightkit_crypto::hex_encode(&rightkit_crypto::sha256(raw.as_bytes()));
    let mapped: String = raw
        .chars()
        .map(|c| if inside(c) { c } else { '-' })
        .take(ALERT_ID_MAX - 9)
        .collect();
    format!("{mapped}-{}", &digest[..8])
}

/// An alert one open item owes one phone.
pub struct OwedAlert {
    pub item_id: String,
    pub urgent: bool,
    pub device_id: String,
}

/// One pass of the phone-only poll: moves time forward ([`tick`]: snoozes end, expiries fall due,
/// an expiry inside a quiet-hours hold is deferred) and routes each open item with [`route_item`]
/// over `phones`, the owner's own paired phones that hold an alert token. No computer is taken as
/// in use, so the phone is the target; quiet hours hold an alert as the stored setting of
/// `workspace_root` says. Nothing is marked sent here: the sender calls
/// [`Inbox::record_delivery`] after a send, so a failed one is offered again on the next pass.
pub fn route_pass<S: InboxStore>(
    store: &S,
    workspace_root: &str,
    now: i64,
    phones: &[String],
) -> Result<Vec<OwedAlert>, String> {
    let settings = RoutingSettings::default();
    let release = |at: i64| {
        let seconds = u64::try_from(at).ok()?;
        super::quiet_hours_release_for_workspace(workspace_root, seconds)
            .and_then(|end| i64::try_from(end).ok())
    };
    let devices: Vec<DevicePresence> = phones
        .iter()
        .map(|id| DevicePresence {
            device_id: id.clone(),
            kind: DeviceKind::Phone,
            last_input_at: None,
            presence: PresenceState::Available,
        })
        .collect();
    store.update(|items| {
        let ticked = tick(items, now, &settings, &release);
        let release_now = release(now);
        let mut owed = Vec::new();
        for item in items.iter().filter(|i| i.state == InboxState::Open) {
            let fresh =
                item.created_at >= now - ALERT_MAX_AGE_SECS || ticked.woken.contains(&item.id);
            if !fresh {
                continue;
            }
            let decision = route_item(item, &devices, &settings, release_now, now);
            for device_id in decision.deliver_now {
                owed.push(OwedAlert {
                    item_id: item.id.clone(),
                    urgent: item.urgent,
                    device_id,
                });
            }
        }
        owed
    })
}
