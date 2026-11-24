// Duty host state and alert routing.
//
// The computer that holds duty owns the Inbox and decides where each alert goes. This module holds
// what is neutral about that: the last presence report of each computer (memory only, stale after
// `STALE_SECS`), the alerts queued for this computer's notch, which paired devices count as phone,
// computer or server, the quiet-hours line format, the content-free title of an alert, the checks
// on the wire methods (`presence.report`, `alert.show`, `alert.clear`, `duty.alert_done`) and one
// routing pass over `Inbox::poll`. The host owns the settings it reads, the device registry, the
// device link and push senders (passed in as closures), the clock (passed in) and the thread.
//
// An alert carries a fixed title and an item id and nothing else; the content is read from the
// Inbox after the device unlocks.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{json, Value};

use super::inbox::{
    DeviceKind, DevicePresence, Inbox, InboxItem, InboxState, InboxStore, PresenceState,
    RoutingSettings, ALERT_MAX_AGE_SECS,
};
use super::{QuietHoursResponse, QuietHoursWindow};

/// A presence report older than this is not a computer in use.
pub const STALE_SECS: i64 = 60;
/// Alerts queued for this computer's notch at most.
pub const ALERTS_MAX: usize = 16;
/// Longest alert title, in characters.
pub const TITLE_MAX_CHARS: usize = 120;
/// The longest snooze a notch may ask for.
pub const MAX_SNOOZE_SECS: i64 = 7 * 24 * 3600;
/// The shortest snooze a notch may ask for.
pub const MIN_SNOOZE_SECS: i64 = 60;
/// Item and device pairs already told to clear are remembered up to this many.
const CLEARED_MAX: usize = 512;

/// What each capability word means. A computer reports only these words, and the duty host keeps
/// only these words.
pub const CAPABILITIES: [&str; 5] = [
    "screen_observe",
    "screen_act",
    "browser",
    "typed_fill",
    "approvals",
];

/// The capability words of a report, cut to the ones this build knows.
pub fn known_capabilities(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|word| CAPABILITIES.contains(word))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ---- who the owner's devices are ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Phone,
    Computer,
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub device_id: String,
    pub role: Role,
}

/// The role a paired device holds in the owner's membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Server,
    Computer,
    Other,
}

/// The role of one paired device: an inactive or guest device has none; a phone or tablet counts
/// only when the owner paired it; any other device is a server by its member role, or a computer
/// when the owner paired it. `owner_paired` is asked only when the answer needs it.
pub fn classify(
    active: bool,
    guest: bool,
    mobile: bool,
    member: MemberKind,
    owner_paired: impl FnOnce() -> bool,
) -> Option<Role> {
    if !active || guest {
        return None;
    }
    if mobile {
        return owner_paired().then_some(Role::Phone);
    }
    match member {
        MemberKind::Server => Some(Role::Server),
        MemberKind::Computer => owner_paired().then_some(Role::Computer),
        MemberKind::Other => None,
    }
}

// ---- plain words and lines -------------------------------------------------------------------------

/// An id an alert may name: 1 to 128 bytes of `A-Z a-z 0-9 . _ : @ # = -`.
pub fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '@' | '#' | '=' | '-')
        })
}

/// The alert title for a finding: control characters removed, trimmed, cut to
/// [`TITLE_MAX_CHARS`].
pub fn title_of(finding: &str) -> String {
    let plain: String = finding.chars().filter(|c| !c.is_control()).collect();
    plain.trim().chars().take(TITLE_MAX_CHARS).collect()
}

/// The stored quiet-hours lines (`HH:MM-HH:MM [zone]`) as the settings the routing reads. A line
/// with no zone is read as UTC; the zone tables are `rightkit-time`'s. A line that does not parse
/// is skipped; no windows means quiet hours are off.
pub fn quiet_windows(lines: &[String]) -> QuietHoursResponse {
    let windows: Vec<QuietHoursWindow> = lines
        .iter()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (start, end) = parts.next()?.split_once('-')?;
            Some(QuietHoursWindow {
                start: start.to_string(),
                end: end.to_string(),
                timezone: Some(parts.next().unwrap_or("UTC").to_string()),
            })
        })
        .collect();
    QuietHoursResponse {
        workspace_root: String::new(),
        enabled: !windows.is_empty(),
        windows,
    }
}

/// The UTC offset of a zone at an instant, for [`quiet_windows`]'s windows; UTC when unknown.
pub fn offset_for(zone: Option<&str>, unix: i64) -> i32 {
    super::zone_offset_secs(zone, unix)
}

/// The body of `alert.show` for an item.
pub fn alert_show_body(item: &InboxItem) -> Value {
    json!({ "itemId": item.id, "title": title_of(&item.finding), "urgent": item.urgent })
}

// ---- the state -------------------------------------------------------------------------------------

/// An alert waiting for this computer's notch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub item_id: String,
    pub title: String,
    pub urgent: bool,
    pub at: i64,
}

impl Alert {
    /// The reply of `duty.alert_next` for this alert.
    pub fn to_json(&self) -> Value {
        json!({ "alert": { "itemId": self.item_id, "title": self.title, "urgent": self.urgent, "at": self.at } })
    }
}

/// What `duty.alert_done` asks the host to do after the alert left the queue.
#[derive(Debug, Clone, PartialEq)]
pub enum AlertDone {
    /// The hub's Inbox page is where "open" goes; nothing to tell the duty host.
    Open,
    /// Send `method` (`alert.dismiss` or `alert.snooze`) with `body` to the duty host.
    Forward { method: &'static str, body: Value },
}

/// Why a duty call's parameters were refused: the message.
pub type Refused = String;

struct Seen {
    device_id: String,
    presence: DevicePresence,
    at: i64,
    capabilities: Vec<String>,
}

/// One note the routing pass makes, for the host to log or publish. Ids and words only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DutyNote {
    RouteFailed(String),
    RecordFailed(String),
    Delivered { item: String, phone: bool },
    DeliveryFailed(String),
    Cleared { item: String },
    ClearSkipped(String),
}

/// Duty state in memory only: presence reports, the notch's queue, the clear history and when the
/// last pass ran.
pub struct DutyState {
    seen: Mutex<Vec<Seen>>,
    alerts: Mutex<VecDeque<Alert>>,
    cleared: Mutex<Vec<String>>,
    last_routed: Mutex<i64>,
}

impl Default for DutyState {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl DutyState {
    pub const fn new() -> Self {
        DutyState {
            seen: Mutex::new(Vec::new()),
            alerts: Mutex::new(VecDeque::new()),
            cleared: Mutex::new(Vec::new()),
            last_routed: Mutex::new(0),
        }
    }

    // -- presence

    /// Keeps the last report of `device_id` (one per computer).
    pub fn record(
        &self,
        device_id: &str,
        presence: DevicePresence,
        now: i64,
        capabilities: Vec<String>,
    ) {
        let mut seen = lock(&self.seen);
        seen.retain(|s| s.device_id != device_id);
        seen.push(Seen {
            device_id: device_id.to_string(),
            presence,
            at: now,
            capabilities,
        });
    }

    /// `presence.report {lastInputAt, presence, capabilities?}` from a paired computer. A clock that
    /// runs ahead is clamped to `now`: an input in the future would hold the alert on that computer
    /// for ever. Refused when `presence` is not a known state.
    pub fn on_report(&self, device_id: &str, params: &Value, now: i64) -> Result<(), Refused> {
        let presence: PresenceState =
            serde_json::from_value(params.get("presence").cloned().unwrap_or(Value::Null))
                .map_err(|_| "presence is not a known state".to_string())?;
        let last_input_at = params
            .get("lastInputAt")
            .and_then(Value::as_i64)
            .map(|at| at.min(now));
        self.record(
            device_id,
            DevicePresence {
                device_id: device_id.to_string(),
                kind: DeviceKind::Computer,
                last_input_at,
                presence,
            },
            now,
            known_capabilities(params.get("capabilities")),
        );
        Ok(())
    }

    /// The reports that are still fresh, one per computer.
    pub fn fresh(&self, now: i64) -> Vec<DevicePresence> {
        lock(&self.seen)
            .iter()
            .filter(|s| now.saturating_sub(s.at) <= STALE_SECS)
            .map(|s| s.presence.clone())
            .collect()
    }

    /// Whether `device_id` has a fresh presence report: the computer is awake and reporting.
    pub fn target_fresh(&self, device_id: &str, now: i64) -> bool {
        lock(&self.seen)
            .iter()
            .any(|s| s.device_id == device_id && now.saturating_sub(s.at) <= STALE_SECS)
    }

    /// Whether some computer reports itself in use now: available, with real input within the
    /// routing rule's recent-input window.
    pub fn computer_in_use(&self, now: i64) -> bool {
        let window = RoutingSettings::default().recent_input_secs;
        self.fresh(now).iter().any(|d| {
            d.kind == DeviceKind::Computer
                && d.presence == PresenceState::Available
                && d.last_input_at
                    .is_some_and(|at| now.saturating_sub(at) <= window)
        })
    }

    /// What each computer last reported (`deviceId`, `capabilities`, `lastInputAt`, `presence`,
    /// `ageSeconds`, `fresh`).
    pub fn status_devices(&self, now: i64) -> Vec<Value> {
        lock(&self.seen)
            .iter()
            .map(|s| {
                json!({
                    "deviceId": s.device_id,
                    "capabilities": s.capabilities,
                    "lastInputAt": s.presence.last_input_at,
                    "presence": serde_json::to_value(s.presence.presence).unwrap_or(Value::Null),
                    "ageSeconds": now.saturating_sub(s.at),
                    "fresh": now.saturating_sub(s.at) <= STALE_SECS,
                })
            })
            .collect()
    }

    // -- the notch's queue

    /// Queues an alert for this computer's notch: a second alert for the item replaces the first,
    /// and the queue keeps the newest [`ALERTS_MAX`].
    pub fn queue_alert(&self, alert: Alert) {
        let mut alerts = lock(&self.alerts);
        alerts.retain(|a| a.item_id != alert.item_id);
        alerts.push_back(alert);
        while alerts.len() > ALERTS_MAX {
            alerts.pop_front();
        }
    }

    /// Queues the alert of `item` for this computer's notch.
    pub fn queue_item(&self, item: &InboxItem, now: i64) {
        self.queue_alert(Alert {
            item_id: item.id.clone(),
            title: title_of(&item.finding),
            urgent: item.urgent,
            at: now,
        });
    }

    /// `alert.show {itemId, title, urgent}` from the paired server: queue it.
    pub fn on_show(&self, params: &Value, now: i64) -> Result<(), Refused> {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .filter(|id| plain_id(id))
            .ok_or_else(|| "itemId is not a plain id".to_string())?;
        let title = title_of(params.get("title").and_then(Value::as_str).unwrap_or(""));
        self.queue_alert(Alert {
            item_id: item_id.to_string(),
            title,
            urgent: params.get("urgent").and_then(Value::as_bool) == Some(true),
            at: now,
        });
        Ok(())
    }

    /// `alert.clear {itemId}` from the paired server: the item was handled somewhere, so drop it here.
    pub fn on_clear(&self, params: &Value) -> Result<(), Refused> {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .filter(|id| plain_id(id))
            .ok_or_else(|| "itemId is not a plain id".to_string())?;
        self.drop_alert(item_id);
        Ok(())
    }

    /// Drops the queued alert of `item_id`.
    pub fn drop_alert(&self, item_id: &str) {
        lock(&self.alerts).retain(|a| a.item_id != item_id);
    }

    /// The oldest alert waiting for this computer's notch. Taking it removes it.
    pub fn take_next_alert(&self) -> Option<Alert> {
        lock(&self.alerts).pop_front()
    }

    /// The reply of `duty.alert_next`: the oldest alert, or `{"alert": null}`.
    pub fn next_alert_json(&self) -> Value {
        match self.take_next_alert() {
            Some(a) => a.to_json(),
            None => json!({ "alert": null }),
        }
    }

    pub fn pending_alerts(&self) -> usize {
        lock(&self.alerts).len()
    }

    pub fn last_routed(&self) -> i64 {
        *lock(&self.last_routed)
    }

    /// `duty.alert_done {itemId, action: dismiss|snooze|open, forSeconds?}`: the owner answered an
    /// alert on this computer. The alert leaves the queue first, whatever the action; then the
    /// action is read. `Err` is a bad-input message.
    pub fn alert_done(&self, params: &Value) -> Result<AlertDone, Refused> {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .filter(|id| plain_id(id))
            .ok_or_else(|| "param `itemId` is not a plain id".to_string())?
            .to_string();
        let action = params.get("action").and_then(Value::as_str).unwrap_or("");
        self.drop_alert(&item_id);
        match action {
            "open" => Ok(AlertDone::Open),
            "dismiss" => Ok(AlertDone::Forward {
                method: "alert.dismiss",
                body: json!({ "id": item_id }),
            }),
            "snooze" => {
                let secs = params
                    .get("forSeconds")
                    .and_then(Value::as_i64)
                    .filter(|s| (MIN_SNOOZE_SECS..=MAX_SNOOZE_SECS).contains(s))
                    .ok_or_else(|| "param `forSeconds` is 60 to a week".to_string())?;
                Ok(AlertDone::Forward {
                    method: "alert.snooze",
                    body: json!({ "id": item_id, "forSeconds": secs }),
                })
            }
            _ => Err("param `action` is dismiss, snooze or open".to_string()),
        }
    }

    // -- routing

    /// One routing pass of the duty host: `Inbox::poll` over `devices` (expire and wake by time,
    /// then `route_item` for every open item), then each alert the pass owes is sent with
    /// `deliver(role, device_id, item)` and, once it answered `Ok`, recorded so it is not offered
    /// again (a failed send is offered on the next pass). An item older than a day is not
    /// announced as news unless it was just woken from a snooze. An empty device id is skipped;
    /// `role_of` says whether a device is a phone (anything else is a computer). Afterwards an
    /// item no longer open stops being shown: its queued alert is dropped and `clear(item,
    /// device)` runs once for each other device that was sent it. `own` is this computer's device id.
    #[allow(clippy::too_many_arguments)]
    pub fn route_once<S: InboxStore>(
        &self,
        store: S,
        now: i64,
        devices: &[DevicePresence],
        routing: &RoutingSettings,
        quiet: &QuietHoursResponse,
        own: Option<&str>,
        role_of: &dyn Fn(&str) -> Role,
        deliver: &mut dyn FnMut(Role, &str, &InboxItem) -> Result<(), String>,
        clear: &mut dyn FnMut(&str, &str) -> Result<(), String>,
        note: &mut dyn FnMut(DutyNote),
    ) {
        let inbox = Inbox::new(store);
        let report = match inbox.poll(now, devices, routing, quiet, &offset_for) {
            Ok(report) => report,
            Err(error) => {
                note(DutyNote::RouteFailed(error));
                return;
            }
        };
        *lock(&self.last_routed) = now;
        let items = inbox.list().unwrap_or_default();
        for (id, decision) in &report.decisions {
            let Some(item) = items.iter().find(|i| i.id == *id) else {
                continue;
            };
            let fresh = item.created_at >= now - ALERT_MAX_AGE_SECS
                || report.ticked.woken.iter().any(|w| w == id);
            if !fresh {
                continue;
            }
            for device_id in &decision.deliver_now {
                if device_id.is_empty() {
                    continue;
                }
                let role = role_of(device_id);
                match deliver(role, device_id, item) {
                    Ok(()) => {
                        if let Err(error) = inbox.record_delivery(id, device_id, now) {
                            note(DutyNote::RecordFailed(error));
                        }
                        note(DutyNote::Delivered {
                            item: id.clone(),
                            phone: role == Role::Phone,
                        });
                    }
                    Err(code) => note(DutyNote::DeliveryFailed(code)),
                }
            }
        }
        self.clear_closed(&items, own, clear, note);
    }

    /// An item that is no longer open stops being shown on the computers that were shown it.
    fn clear_closed(
        &self,
        items: &[InboxItem],
        own: Option<&str>,
        clear: &mut dyn FnMut(&str, &str) -> Result<(), String>,
        note: &mut dyn FnMut(DutyNote),
    ) {
        for item in items
            .iter()
            .filter(|i| i.state != InboxState::Open && i.state != InboxState::Snoozed)
        {
            self.drop_alert(&item.id);
            for delivery in &item.deliveries {
                let key = format!("{}|{}", item.id, delivery.device_id);
                if lock(&self.cleared).contains(&key) || own == Some(delivery.device_id.as_str()) {
                    continue;
                }
                match clear(&item.id, &delivery.device_id) {
                    Ok(()) => {
                        let mut cleared = lock(&self.cleared);
                        cleared.push(key);
                        // The set stays small: an old pair is forgotten first.
                        if cleared.len() > CLEARED_MAX {
                            cleared.remove(0);
                        }
                        drop(cleared);
                        note(DutyNote::Cleared {
                            item: item.id.clone(),
                        });
                    }
                    // A phone has no `alert.clear`: its push cannot be recalled, and its Inbox
                    // reads the item as closed.
                    Err(code) => note(DutyNote::ClearSkipped(code)),
                }
            }
        }
    }
}
