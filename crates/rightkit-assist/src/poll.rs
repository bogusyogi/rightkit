//! The noticing poll: what a pass remembers between runs ([`PollState`]), one pass over new mail ([`poll_pass`]), the unfinished
//! signals an earlier pass left (`backlog` runs inside it), one signal from the start ([`process`]) and the model-call gate
//! ([`MODEL_CALLS_PER_DAY`], the daily cap and the "model cannot be asked now" wait).
//!
//! Neutral logic only. The host supplies the mail source, the model call, the Inbox and the log through [`PollHost`], the records
//! directory through [`Pass`], and the clock through `now`. Nothing here reads a clock, reaches a network or names a product.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::calendar::InboundEvent;
use crate::file_store::{self, POLL_FILE};
use crate::pipeline::{self, Applied, Prepared, RunReport};
use crate::signal::{dedupe_key, NewSignal, SignalState, Source};
use crate::sort::{Category, InboundMail};
use crate::state::AssistStore;
use crate::triage::TriageRequest;

/// The account label signals carry. One account is signed in per computer.
pub const ACCOUNT: &str = "primary";
/// How many of the newest messages a reconcile lists.
pub const RECONCILE_MAX: usize = 25;
/// Mail received this long before the switch was first on still counts as new.
pub const SLACK_MS: i64 = 5 * 60 * 1000;
/// A message that failed to fetch this many times stops holding the cursor back.
pub const FETCH_FAIL_MAX: u8 = 5;
/// Most unfinished signals one pass takes up again.
pub const BACKLOG_MAX: usize = 10;
/// Model calls one UTC day may make.
pub const MODEL_CALLS_PER_DAY: u32 = 200;
/// How long a sign-in the mail source refused pauses the poll.
pub const PAUSE_SIGN_IN_SECS: i64 = 30 * 60;

/// What a discovery found, in the loop's terms.
pub struct Found {
    pub cursor: String,
    pub ids: Vec<String>,
    /// The first pass: nothing is read, the cursor is the baseline.
    pub baseline: bool,
    /// False when a message failed inside the source's own delta, so the cursor stayed where it was.
    pub advanced: bool,
}

/// What the loop needs from its host. Errors are stable codes (`google_sign_in_again`, `google_not_found`, ...).
pub trait PollHost {
    /// Mail changed since `cursor` (a baseline when `None`).
    fn discover(
        &self,
        cursor: Option<&str>,
        not_before_ms: i64,
        reconcile_max: usize,
    ) -> Result<Found, String>;
    /// One message as the loop's input.
    fn fetch(&self, id: &str) -> Result<InboundMail, String>;
    /// The owner's calendar events between `from` and `to` (seconds), `owner` being his addresses.
    fn events(&self, from: i64, to: i64, owner: &[String]) -> Result<Vec<InboundEvent>, String>;
    /// Whether a request that reaches the model may be made at all.
    fn model_switch_on(&self) -> bool;
    /// The one model call (no tools). `Err` is a stable code; `agent_not_installed` and `agent_not_signed_in` mean the model
    /// cannot be asked now, any other error counts as a bad answer.
    fn answer(&self, system: &str, input: &str) -> Result<String, String>;
    /// Raises a finished run's findings and says what settled. Returns how many new Inbox items.
    fn deliver(&self, report: RunReport) -> usize;
    /// A log line that names no signal and no text.
    fn warn(&self, event: &str, message: &str);
}

/// What the poll remembers between passes. Ids, times and codes only.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollState {
    /// The mail source's history cursor.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Receive time (ms) before which mail is not read.
    #[serde(default)]
    pub not_before_ms: i64,
    #[serde(default)]
    pub last_poll_at: i64,
    #[serde(default)]
    pub last_ok_at: i64,
    /// The code the last pass ended on, when it did not end well.
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub pause_until: i64,
    /// Failed fetches by message id.
    #[serde(default)]
    pub fetch_failures: BTreeMap<String, u8>,
    /// The last fingerprint of each calendar event the loop has read, by its signal's message id (`<calendar>/<event>`): an event
    /// whose fingerprint changed is run again (`calendar_poll::calendar_pass`); one no longer listed is forgotten here (its facts stay).
    #[serde(default)]
    pub event_prints: BTreeMap<String, String>,
    /// UTC day number of `model_calls`.
    #[serde(default)]
    pub model_day: i64,
    #[serde(default)]
    pub model_calls: u32,
}

/// The poll record in `<dir>/poll.json`. A file that cannot be read is an error.
pub fn poll_read(dir: &Path) -> Result<PollState, String> {
    let _guard = file_store::lock();
    Ok(file_store::read_json(&dir.join(POLL_FILE))?.unwrap_or_default())
}

/// Writes the poll record.
pub fn poll_save(dir: &Path, poll: &PollState) -> Result<(), String> {
    let _guard = file_store::lock();
    file_store::write_json(dir, &dir.join(POLL_FILE), poll)
}

/// What a pass holds while it runs: the records directory, the poll record and whether the model is out of reach this pass.
pub struct Pass {
    pub dir: PathBuf,
    pub poll: PollState,
    /// Set when the model cannot be asked now (not installed, not signed in, switch off, cap): a message that needs it
    /// waits, and no other message tries.
    pub model_blocked: Option<String>,
}

impl Pass {
    pub fn new(dir: impl Into<PathBuf>, poll: PollState) -> Pass {
        Pass {
            dir: dir.into(),
            poll,
            model_blocked: None,
        }
    }

    /// Saves the poll record; a failure is logged, never raised.
    pub fn save<H: PollHost>(&self, host: &H) {
        if let Err(error) = poll_save(&self.dir, &self.poll) {
            host.warn("assist.poll_state_failed", &error);
        }
    }
}

enum Reply {
    Text(String),
    /// The call failed in a way that counts as a bad answer (timeout, crash, output too large).
    Missing,
    /// The model cannot be asked now; the signal waits.
    Wait(String),
}

fn ask_model<H: PollHost>(host: &H, pass: &mut Pass, request: &TriageRequest, now: i64) -> Reply {
    if let Some(code) = &pass.model_blocked {
        return Reply::Wait(code.clone());
    }
    // The crate builds every request with no tools; this is the assertion it offers.
    if !request.tools.is_empty() {
        return wait(pass, "tools_not_empty");
    }
    if !host.model_switch_on() {
        return wait(pass, "agent_off");
    }
    let day = now.div_euclid(86_400);
    if pass.poll.model_day != day {
        pass.poll.model_day = day;
        pass.poll.model_calls = 0;
    }
    if pass.poll.model_calls >= MODEL_CALLS_PER_DAY {
        return wait(pass, "daily_cap");
    }
    pass.poll.model_calls += 1;
    pass.save(host);
    let system = format!(
        "{}\n\nAnswer with one JSON object that validates against this JSON Schema:\n{}",
        request.system, request.output_schema
    );
    match host.answer(&system, &request.input) {
        Ok(text) => Reply::Text(text),
        Err(code) if matches!(code.as_str(), "agent_not_installed" | "agent_not_signed_in") => {
            wait(pass, &code)
        }
        Err(_) => Reply::Missing,
    }
}

fn wait(pass: &mut Pass, code: &str) -> Reply {
    pass.model_blocked = Some(code.to_string());
    Reply::Wait(code.to_string())
}

pub enum Outcome {
    Finished(Box<RunReport>),
    /// Left as it is; the code says why the model could not be asked.
    Waiting(String),
}

/// Runs one signal from the start for `mail`. A signal an earlier pass left half way is reset first and its facts dropped, so
/// the result is what the current rules make of the current message. The store is locked only for each step, never across
/// the model call.
pub fn process<H: PollHost, S: AssistStore>(
    host: &H,
    store: &S,
    pass: &mut Pass,
    signal_id: &str,
    mail: &InboundMail,
    now: i64,
) -> Result<Outcome, String> {
    store.update(|state| {
        if let Some(signal) = state.signals.get_mut(signal_id) {
            if signal.state != SignalState::New {
                signal.reset();
            }
        }
        state.remove_facts_of(signal_id);
    })?;
    let prepared = store.update(|state| pipeline::prepare(state, signal_id, mail, now))??;
    let mut request = match prepared {
        Prepared::Finished(report) => return Ok(Outcome::Finished(Box::new(report))),
        Prepared::NeedsModel(request) => request,
    };
    loop {
        let reply = match ask_model(host, pass, &request, now) {
            Reply::Text(text) => Some(text),
            Reply::Missing => None,
            Reply::Wait(code) => return Ok(Outcome::Waiting(code)),
        };
        let applied = store.update(|state| {
            pipeline::apply_answer(
                state,
                signal_id,
                &request,
                reply.as_deref(),
                &mail.from,
                now,
            )
        })??;
        match applied {
            Applied::Finished(report) => return Ok(Outcome::Finished(Box::new(report))),
            Applied::Retry(next) => request = next,
        }
    }
}

/// Gives up on a signal whose message cannot be read: `triage_failed`, with a fixed reason.
fn abandon<H: PollHost, S: AssistStore>(host: &H, store: &S, signal_id: &str, reason: &str) {
    let result = store.update(|state| {
        if let Some(signal) = state.signals.get_mut(signal_id) {
            signal.reset();
            let _ = signal.advance(SignalState::Sorted);
            signal.last_error = Some(reason.to_string());
            let _ = signal.advance(SignalState::TriageFailed);
        }
    });
    if let Err(error) = result {
        host.warn("assist.store_failed", &error);
    }
}

#[derive(Default)]
pub struct Counts {
    pub admitted: usize,
    pub settled: usize,
    pub waiting: usize,
    pub skipped: usize,
    pub new_items: usize,
}

fn finish<H: PollHost>(host: &H, counts: &mut Counts, outcome: Outcome, pass: &mut Pass) {
    match outcome {
        Outcome::Finished(report) => {
            counts.settled += 1;
            counts.new_items += host.deliver(*report);
        }
        Outcome::Waiting(code) => {
            counts.waiting += 1;
            pass.poll.last_error = Some(code);
        }
    }
}

/// Counts one failed fetch of `id`. True when it should stop holding the cursor back: the source no longer has the message, or
/// it failed [`FETCH_FAIL_MAX`] times.
fn give_up(pass: &mut Pass, id: &str, code: &str) -> bool {
    let failures = pass.poll.fetch_failures.entry(id.to_string()).or_insert(0);
    *failures = failures.saturating_add(1);
    let give_up = code == "google_not_found" || *failures >= FETCH_FAIL_MAX;
    if give_up {
        pass.poll.fetch_failures.remove(id);
    }
    give_up
}

/// One pass over mail: discover what changed, admit and run each new message, move the cursor once everything found is admitted
/// (or given up on), then take up the backlog. `Err` is a stable code from the host.
pub fn poll_pass<H: PollHost, S: AssistStore>(
    host: &H,
    store: &S,
    pass: &mut Pass,
    now: i64,
) -> Result<Counts, String> {
    let now_ms = now.saturating_mul(1000);
    let found = host.discover(
        pass.poll.cursor.as_deref(),
        pass.poll.not_before_ms,
        RECONCILE_MAX,
    )?;
    let mut counts = Counts::default();
    if found.baseline {
        pass.poll.cursor = Some(found.cursor.clone());
        pass.poll.not_before_ms = now_ms.saturating_sub(SLACK_MS);
    }
    let snapshot = store.snapshot()?;
    let known: HashSet<String> = snapshot
        .signals
        .values()
        .map(|s| s.dedupe_key.clone())
        .collect();
    let lists = snapshot.sender_lists;
    let mut held_back = false;
    let mut attempted: HashSet<String> = HashSet::new();
    for id in &found.ids {
        if known.contains(&dedupe_key(Source::Mail, ACCOUNT, id)) {
            continue;
        }
        let mail = match host.fetch(id) {
            Ok(mail) => mail,
            Err(code) if code == "google_sign_in_again" => return Err(code),
            Err(code) => {
                if give_up(pass, id, &code) {
                    counts.skipped += 1;
                } else {
                    held_back = true;
                }
                continue;
            }
        };
        pass.poll.fetch_failures.remove(id);
        let admission = store.update(|state| {
            state.admit(&NewSignal {
                source: Source::Mail,
                account: ACCOUNT.to_string(),
                message_id: id.clone(),
                seen_at: now,
                trust: lists.trust_of(&mail),
            })
        })?;
        if !admission.created {
            continue;
        }
        counts.admitted += 1;
        attempted.insert(admission.signal_id.clone());
        match process(host, store, pass, &admission.signal_id, &mail, now) {
            Ok(outcome) => finish(host, &mut counts, outcome, pass),
            Err(error) => host.warn("assist.process_failed", &error),
        }
        pass.save(host);
    }
    // Everything found is now admitted (or given up on), so the cursor may move.
    if !held_back && found.advanced && !found.baseline {
        pass.poll.cursor = Some(found.cursor);
    }
    backlog(host, store, pass, &mut counts, &attempted, now)?;
    Ok(counts)
}

/// Takes up unfinished signals an earlier pass left (a restart, a model that was not reachable).
fn backlog<H: PollHost, S: AssistStore>(
    host: &H,
    store: &S,
    pass: &mut Pass,
    counts: &mut Counts,
    attempted: &HashSet<String>,
    now: i64,
) -> Result<(), String> {
    let waiting: Vec<(String, String, SignalState)> = store
        .snapshot()?
        .signals
        .values()
        .filter(|s| s.source == Source::Mail && !s.state.is_final() && !attempted.contains(&s.id))
        .take(BACKLOG_MAX)
        .map(|s| (s.id.clone(), s.message_id.clone(), s.state))
        .collect();
    for (signal_id, message_id, state) in waiting {
        // A signal that already reached the model step waits for it; nothing else about it is worth a fetch.
        if pass.model_blocked.is_some() && state != SignalState::New {
            continue;
        }
        let mail = match host.fetch(&message_id) {
            Ok(mail) => mail,
            Err(code) if code == "google_sign_in_again" => return Err(code),
            Err(code) => {
                if give_up(pass, &message_id, &code) {
                    abandon(host, store, &signal_id, "message_unavailable");
                    counts.skipped += 1;
                }
                continue;
            }
        };
        pass.poll.fetch_failures.remove(&message_id);
        match process(host, store, pass, &signal_id, &mail, now) {
            Ok(outcome) => finish(host, counts, outcome, pass),
            Err(error) => host.warn("assist.process_failed", &error),
        }
        pass.save(host);
    }
    Ok(())
}

/// Pauses the poll for [`PAUSE_SIGN_IN_SECS`] from `now` (a sign-in the mail source refused).
pub fn pause_for_sign_in(pass: &mut Pass, now: i64) {
    pass.poll.pause_until = now.saturating_add(PAUSE_SIGN_IN_SECS);
}

/// The poll's cursor as the poll last kept it. `None` before the first pass.
pub fn mail_cursor(dir: &Path) -> Option<String> {
    poll_read(dir).ok().and_then(|poll| poll.cursor)
}

/// The poll's resume point as one sync row: the cursor and the receive time before which mail is not read, versioned by when the
/// last pass succeeded (milliseconds). `None` before the first successful pass. Ids and times only.
pub fn cursor_row(dir: &Path) -> Result<Option<(i64, Vec<u8>)>, String> {
    let poll = poll_read(dir)?;
    if poll.last_ok_at == 0 {
        return Ok(None);
    }
    let body = serde_json::json!({
        "cursor": poll.cursor,
        "notBeforeMs": poll.not_before_ms,
        "lastOkAt": poll.last_ok_at,
    });
    Ok(Some((
        poll.last_ok_at.saturating_mul(1000),
        body.to_string().into_bytes(),
    )))
}

/// Takes the resume point another of the owner's computers synced, only when it is newer than this one's. A computer that takes
/// duty over then reads on from where the last one stopped instead of starting a full reconcile. A failed write is passed to
/// `warn` (event name, message), not raised.
pub fn cursor_apply(dir: &Path, body: &[u8], warn: impl FnOnce(&str, &str)) -> Result<(), String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "a cursor row could not be read".to_string())?;
    let last_ok_at = value["lastOkAt"].as_i64().unwrap_or(0);
    let mut poll = poll_read(dir)?;
    if last_ok_at <= poll.last_ok_at {
        return Ok(());
    }
    poll.cursor = value["cursor"].as_str().map(str::to_owned);
    poll.not_before_ms = value["notBeforeMs"].as_i64().unwrap_or(poll.not_before_ms);
    poll.last_ok_at = last_ok_at;
    if let Err(error) = poll_save(dir, &poll) {
        warn("assist.poll_state_failed", &error);
    }
    Ok(())
}

// ---- words ---------------------------------------------------------------------------------------

pub fn state_word(state: SignalState) -> &'static str {
    match state {
        SignalState::New => "new",
        SignalState::Sorted => "sorted",
        SignalState::Triaged => "triaged",
        SignalState::NoWork => "no_work",
        SignalState::TriageFailed => "triage_failed",
    }
}

pub fn category_word(category: Category) -> &'static str {
    match category {
        Category::Travel => "travel",
        Category::Customer => "customer",
        Category::Bill => "bill",
        Category::Bulk => "bulk",
    }
}

pub fn category_of(word: &str) -> Option<Category> {
    match word {
        "travel" => Some(Category::Travel),
        "customer" => Some(Category::Customer),
        "bill" => Some(Category::Bill),
        "bulk" => Some(Category::Bulk),
        _ => None,
    }
}

// ---- argument helpers for a command line ---------------------------------------------------------

/// Why [`duration_secs`] refused a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationError {
    /// The unit is not `s`, `m`, `h` or `d`.
    Unit,
    /// The number is not a whole number, or too large.
    Number,
}

/// `30`, `30s`, `45m`, `2h`, `1d`: seconds.
pub fn duration_secs(text: &str) -> Result<i64, DurationError> {
    let (digits, unit) = match text.char_indices().last() {
        Some((at, c)) if c.is_ascii_alphabetic() => (&text[..at], c),
        _ => (text, 's'),
    };
    let unit = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return Err(DurationError::Unit),
    };
    digits
        .parse::<i64>()
        .ok()
        .and_then(|n| n.checked_mul(unit))
        .ok_or(DurationError::Number)
}

/// The one positional argument, when there is exactly one and it is not a flag.
pub fn one_pattern(arguments: &[String]) -> Option<&String> {
    match arguments {
        [pattern] if !pattern.starts_with("--") => Some(pattern),
        _ => None,
    }
}
