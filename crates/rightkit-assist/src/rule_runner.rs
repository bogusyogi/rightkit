//! What a standing-instruction firing does, and how a rule reads to the owner.
//!
//! The record and the firing rules are [`crate::instruction`]; this module is the chain a host runs around them:
//!
//! * [`fire_schedule`] fires the instructions triggered by one claimed minute of a schedule, [`run_outcome`] runs a firing
//!   that is ready, [`run_finished`] ends a run the host started and carries the queued firing behind it on. Each firing is an
//!   occurrence keyed on the instruction, its revision and the trigger (so the same firing twice changes nothing); a second firing
//!   while the first still runs waits, once, and is otherwise skipped.
//! * **Actions.** `notify {text}` raises an Inbox item with the owner's own wording (never text from a mail) and is done.
//!   `run_recipe` is handed to the host ([`RuleHost::start_run`]), which owns what a recipe is and how a run is submitted; the
//!   occurrence is `running` while the run lasts and ends with it ([`run_finished`]). A run that has not ended after
//!   [`STALE_RUN_SECS`] is taken for lost: its occurrence ends `uncertain`.
//! * **Change detection.** [`brief_change_key`] and [`brief_unchanged`] let a host make no model call for a daily brief when
//!   nothing it could report changed.
//! * **Proposals.** [`checked_trigger`] and [`checked_action`] say what a proposed rule may contain; [`sentence`] and
//!   [`instruction_json`] show a rule the same way on every surface.
//!
//! The host supplies the store, the clock, the Inbox, the run submission and the logs through [`RuleHost`]; nothing here reads a
//! file, a clock or a network.

use rightkit_agent_engine_attention::inbox::{InboxItem, InboxState, NewInboxItem, RaiseOutcome};
use serde_json::{json, Value};

use crate::facts::Provenance;
use crate::instruction::{
    schedule_trigger_key, Action, FactKind, FireOutcome, Occurrence, OccurrenceState,
    StandingInstruction, Trigger,
};
use crate::sort::Category;
use crate::state::AssistStore;

/// Most rules a host holds.
pub const MAX_RULES: usize = 50;
/// Longest notify text, in characters.
pub const NOTIFY_MAX_CHARS: usize = 300;
/// Most parameters a recipe action takes.
pub const RECIPE_PARAMS_MAX: usize = 8;
/// Longest recipe parameter value, in characters.
pub const PARAM_MAX_CHARS: usize = 200;
/// A rule's Inbox item expires after this long.
pub const ITEM_LIFETIME_SECS: i64 = 7 * 86_400;
/// A run that has not ended after this long is taken for lost: its occurrence ends `uncertain`, so the instruction's lane opens
/// again. (A cancelled run or one lost to a restart never reaches [`run_finished`].)
pub const STALE_RUN_SECS: i64 = 2 * 3600;

/// How a `run_recipe` occurrence stands after the host tried to start it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStart {
    /// The run was handed over; the occurrence is `running` and ends with [`run_finished`].
    Started,
    /// No run was made; the occurrence ends skipped.
    Skipped,
}

/// Everything a firing needs from its host.
pub trait RuleHost {
    type Store: AssistStore;

    /// The store the instructions and occurrences live in.
    fn store(&self) -> Self::Store;
    /// Now, in Unix seconds.
    fn now(&self) -> i64;
    /// Raises one Inbox item.
    fn raise_notice(&self, item: NewInboxItem) -> Result<RaiseOutcome, String>;
    /// An Inbox item a rule raised was created (the host tells its surfaces).
    fn notice_raised(&self, id: &str);
    /// True when this `run_recipe` instruction would report what it reported last time, so no run is made. The host owns which
    /// recipes this applies to and records what it saw ([`brief_unchanged`]). Default: never.
    fn skip_unchanged_run(&self, _instruction: &StandingInstruction) -> bool {
        false
    }
    /// Starts one run of the instruction's recipe for occurrence `key`. `mark_running` marks the occurrence `running` (true when
    /// it is stored) and must be called after the run is built and before it is submitted, so the run's own start-up check finds
    /// an open occurrence; when it returns false the run must not be submitted.
    fn start_run(
        &self,
        instruction: &StandingInstruction,
        key: &str,
        mark_running: &mut dyn FnMut() -> bool,
    ) -> RunStart;
    /// A warning for the host's log.
    fn warn(&self, event: &str, fields: Value);
}

enum Exec {
    Finished(OccurrenceState),
    Started,
}

/// Fires the instructions triggered by one claimed minute of schedule `schedule_id`, and runs what became ready. Returns how many
/// occurrences were made.
pub fn fire_schedule<H: RuleHost>(host: &H, schedule_id: &str, unix_seconds: i64) -> usize {
    let key = schedule_trigger_key(schedule_id, unix_seconds);
    let now = host.now();
    let fired = host.store().update(|state| {
        let ids: Vec<String> = state
            .instructions
            .values()
            .filter(
                |i| matches!(&i.trigger, Trigger::Schedule { schedule_id: s } if s == schedule_id),
            )
            .map(|i| i.id.clone())
            .collect();
        ids.into_iter()
            .map(|id| state.fire_instruction(&id, &key, now))
            .collect::<Vec<FireOutcome>>()
    });
    match fired {
        Ok(outcomes) => outcomes
            .iter()
            .map(|o| usize::from(run_outcome(host, o)))
            .sum(),
        Err(error) => {
            host.warn("rules.store_failed", json!({ "message": error }));
            0
        }
    }
}

/// Runs a firing that is ready now. True when it made an occurrence (ready or queued).
pub fn run_outcome<H: RuleHost>(host: &H, outcome: &FireOutcome) -> bool {
    match outcome {
        FireOutcome::Ready(occurrence) => {
            run_chain(host, occurrence);
            true
        }
        // It runs when the one ahead of it finishes (`run_chain`, `run_finished`).
        FireOutcome::Queued(_) => true,
        FireOutcome::Duplicate(_) | FireOutcome::Skipped(_) | FireOutcome::Inert => false,
    }
}

/// A run the host started has ended: `idempotency_key` is the run's (`engine:<occurrence key>`). A key without that prefix does
/// nothing. The occurrence ends as `outcome`; the queued occurrence behind it, if any, runs now.
pub fn run_finished<H: RuleHost>(host: &H, idempotency_key: &str, outcome: OccurrenceState) {
    let Some(key) = idempotency_key.strip_prefix("engine:") else {
        return;
    };
    if let Some((next_key, _instruction_id)) = finish_and_next(host, key, outcome) {
        let next = host
            .store()
            .snapshot()
            .ok()
            .and_then(|s| s.occurrences.get(&next_key).cloned());
        if let Some(occurrence) = next {
            run_chain(host, &occurrence);
        }
    }
}

fn reap_stale<H: RuleHost>(host: &H, now: i64) {
    let result = host.store().update(|state| {
        let stale: Vec<String> = state
            .occurrences
            .values()
            .filter(|o| {
                o.state == OccurrenceState::Running
                    && now.saturating_sub(o.created_at) > STALE_RUN_SECS
            })
            .map(|o| o.key.clone())
            .collect();
        for key in &stale {
            let _ = state.finish_occurrence(key, OccurrenceState::Uncertain);
        }
        stale.len()
    });
    if let Err(error) = result {
        host.warn("rules.store_failed", json!({ "message": error }));
    }
}

/// Runs `first`, then each queued occurrence a finish makes ready. A run handed to the host ends the chain here: its end
/// ([`run_finished`]) carries it on.
fn run_chain<H: RuleHost>(host: &H, first: &Occurrence) {
    reap_stale(host, host.now());
    let mut key = first.key.clone();
    let mut instruction_id = first.instruction_id.clone();
    loop {
        let outcome = match execute(host, &key, &instruction_id) {
            Exec::Finished(outcome) => outcome,
            Exec::Started => return,
        };
        match finish_and_next(host, &key, outcome) {
            Some((next_key, id)) => {
                key = next_key;
                instruction_id = id;
            }
            None => return,
        }
    }
}

/// Marks `key` finished and returns the queued occurrence that became ready, with its instruction's id.
fn finish_and_next<H: RuleHost>(
    host: &H,
    key: &str,
    outcome: OccurrenceState,
) -> Option<(String, String)> {
    let next = host
        .store()
        .update(|state| state.finish_occurrence(key, outcome));
    match next {
        Ok(Some(next_key)) => host.store().snapshot().ok().and_then(|s| {
            s.occurrences
                .get(&next_key)
                .map(|o| (next_key.clone(), o.instruction_id.clone()))
        }),
        Ok(None) => None,
        Err(error) => {
            host.warn("rules.store_failed", json!({ "message": error }));
            None
        }
    }
}

/// Does one occurrence's action.
fn execute<H: RuleHost>(host: &H, key: &str, instruction_id: &str) -> Exec {
    let snapshot = host.store().snapshot().ok();
    let instruction = snapshot
        .as_ref()
        .and_then(|s| s.instructions.get(instruction_id).cloned());
    let Some(instruction) = instruction else {
        return Exec::Finished(OccurrenceState::Skipped);
    };
    match &instruction.action {
        Action::Notify { text } => {
            let now = host.now();
            let digest: String = key
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(16)
                .collect();
            let item = NewInboxItem {
                id: format!("rule-{digest}"),
                finding: text.clone(),
                rule: "standing_instruction".to_string(),
                check_id: format!("instruction.{instruction_id}"),
                fact_ids: vec![key.to_string()],
                evidence_ids: Vec::new(),
                proposed_actions: Vec::new(),
                confidence: None,
                missing_information: Vec::new(),
                urgent: false,
                expires_at: Some(now.saturating_add(ITEM_LIFETIME_SECS)),
            };
            match host.raise_notice(item) {
                Ok(RaiseOutcome::Created { id }) => {
                    host.notice_raised(&id);
                    Exec::Finished(OccurrenceState::Done)
                }
                Ok(_) => Exec::Finished(OccurrenceState::Done),
                Err(error) => {
                    host.warn("rules.raise_failed", json!({ "message": error }));
                    Exec::Finished(OccurrenceState::Skipped)
                }
            }
        }
        Action::RunRecipe { .. } => {
            if host.skip_unchanged_run(&instruction) {
                return Exec::Finished(OccurrenceState::Skipped);
            }
            let mut mark_running = || {
                host.store()
                    .update(|state| {
                        if let Some(occurrence) = state.occurrences.get_mut(key) {
                            occurrence.state = OccurrenceState::Running;
                        }
                    })
                    .is_ok()
            };
            match host.start_run(&instruction, key, &mut mark_running) {
                RunStart::Started => Exec::Started,
                RunStart::Skipped => Exec::Finished(OccurrenceState::Skipped),
            }
        }
    }
}

// ---- change detection ---------------------------------------------------------------------------------------

/// Ids of the Inbox items that are open or snoozed, sorted: what a brief looks at besides the mail cursor.
pub fn open_item_ids(items: &[InboxItem]) -> Vec<String> {
    let mut open: Vec<String> = items
        .iter()
        .filter(|i| matches!(i.state, InboxState::Open | InboxState::Snoozed))
        .map(|i| i.id.clone())
        .collect();
    open.sort();
    open
}

/// The key of what a brief looks at: the mail history cursor and the open Inbox item ids. An equal key means nothing it could
/// report changed. Lowercase hex SHA-256.
pub fn brief_change_key(cursor: &str, open_ids: &[String]) -> String {
    let text = format!("brief-key\n{cursor}\n{}", open_ids.join(","));
    rightkit_fs::digest::sha256_bytes(text.as_bytes())
}

/// Where a host remembers the last change key a brief saw, per instruction.
pub trait BriefMemory {
    /// The key recorded last for `instruction_id`, as stored.
    fn last_key(&self, instruction_id: &str) -> Option<String>;
    /// Records `key` for `instruction_id`. A failure is not an error: the next brief simply runs.
    fn record_key(&self, instruction_id: &str, key: &str);
}

/// True when the last brief of this instruction saw exactly `now_key`. Otherwise records the new key and says false.
pub fn brief_unchanged<M: BriefMemory>(memory: &M, instruction_id: &str, now_key: &str) -> bool {
    let same = memory
        .last_key(instruction_id)
        .is_some_and(|last| last.trim() == now_key);
    if !same {
        memory.record_key(instruction_id, now_key);
    }
    same
}

// ---- what a proposal may say --------------------------------------------------------------------------------

/// A plain id: 1 to 64 characters, ASCII letters and digits, `-`, `_` and `.`.
pub fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Text that is not blank, is at most `max` characters and has no control character.
pub fn plain_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// What a proposal's action may say: the owner's wording and typed parameters, never anything that could carry a mail's text.
/// `known_recipe` is the host's list of recipes. The error is the sentence to give the caller.
pub fn checked_action(action: &Action, known_recipe: impl Fn(&str) -> bool) -> Result<(), String> {
    match action {
        Action::Notify { text } if plain_text(text, NOTIFY_MAX_CHARS) => Ok(()),
        Action::Notify { .. } => Err(format!(
            "a notify text is plain text of at most {NOTIFY_MAX_CHARS} characters"
        )),
        Action::RunRecipe { recipe, params } => {
            if !plain_id(recipe) || !known_recipe(recipe) {
                return Err("a recipe is brief or prepare".to_string());
            }
            if params.len() > RECIPE_PARAMS_MAX
                || params
                    .iter()
                    .any(|(k, v)| !plain_id(k) || !plain_text(v, PARAM_MAX_CHARS))
            {
                return Err(format!(
                    "a recipe takes at most {RECIPE_PARAMS_MAX} plain parameters"
                ));
            }
            Ok(())
        }
    }
}

/// What a proposal's trigger may say. The error is the sentence to give the caller.
pub fn checked_trigger(trigger: &Trigger) -> Result<(), String> {
    match trigger {
        Trigger::Schedule { schedule_id } if plain_id(schedule_id) => Ok(()),
        Trigger::Schedule { .. } => Err("a schedule is named by a plain id".to_string()),
        Trigger::Check { check_id } if plain_id(check_id) => Ok(()),
        Trigger::Check { .. } => Err("a check is named by a plain id".to_string()),
        Trigger::Mail { sender, .. } => match sender {
            Some(pattern) if !plain_text(pattern, 254) || pattern.contains(char::is_whitespace) => {
                Err("a sender is an address or @domain".to_string())
            }
            _ => Ok(()),
        },
    }
}

// ---- how a rule reads ---------------------------------------------------------------------------------------

/// A piece of the owner's or a sender's wording as it is shown in a sentence: control characters become spaces and it is cut at
/// `limit` characters.
fn shown(text: &str, limit: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() > limit {
        format!("{}…", clean.chars().take(limit).collect::<String>())
    } else {
        clean
    }
}

fn category_word(category: &Category) -> &'static str {
    match category {
        Category::Travel => "travel",
        Category::Customer => "customer",
        Category::Bill => "bill",
        Category::Bulk => "bulk",
    }
}

/// The standing instruction as one sentence, the same for every surface that shows it (the phone, the notch, the hub): when it
/// fires, what must hold, and what it does. It reads the typed record, so nothing in it is a model's paraphrase. A sender pattern,
/// a schedule id, a recipe name and the owner's notice text are quoted, control characters removed and long ones cut.
pub fn sentence(instruction: &StandingInstruction) -> String {
    let when = match &instruction.trigger {
        Trigger::Mail { category, sender } => {
            let kind = category.as_ref().map_or("any mail".to_string(), |c| {
                format!("a {} mail", category_word(c))
            });
            match sender {
                Some(sender) => format!("When {kind} arrives from \"{}\"", shown(sender, 80)),
                None => format!("When {kind} arrives"),
            }
        }
        Trigger::Schedule { schedule_id } => {
            format!("Each time the schedule \"{}\" runs", shown(schedule_id, 80))
        }
        Trigger::Check { check_id } => {
            format!("When the check \"{}\" finds something", shown(check_id, 80))
        }
    };
    let mut holds: Vec<String> = Vec::new();
    if let Some(provenance) = instruction.condition.provenance_at_least {
        holds.push(
            match provenance {
                Provenance::Owner => "everything it learned came from you",
                Provenance::KnownSender => {
                    "everything it learned came from you or a sender you know"
                }
                Provenance::Untrusted => "it learned something, whoever it came from",
            }
            .to_string(),
        );
    }
    if let Some(kind) = instruction.condition.requires_fact {
        holds.push(format!(
            "it found {}",
            match kind {
                FactKind::Booking => "a booking",
                FactKind::Party => "a person or company",
                FactKind::Commitment => "a commitment",
                FactKind::Money => "an amount of money",
                FactKind::Event => "a calendar entry",
            }
        ));
    }
    let condition = if holds.is_empty() {
        String::new()
    } else {
        format!(", and only if {}", holds.join(" and "))
    };
    let does = match &instruction.action {
        Action::Notify { text } => format!(
            "put a notice in your Inbox that says \"{}\"",
            shown(text, 160)
        ),
        Action::RunRecipe { recipe, params } => {
            if params.is_empty() {
                format!("start one run of the recipe \"{}\"", shown(recipe, 80))
            } else {
                let listed: Vec<String> = params
                    .iter()
                    .take(RECIPE_PARAMS_MAX)
                    .map(|(k, v)| format!("{} = \"{}\"", shown(k, 40), shown(v, 60)))
                    .collect();
                format!(
                    "start one run of the recipe \"{}\" with {}",
                    shown(recipe, 80),
                    listed.join(", ")
                )
            }
        }
    };
    format!("{when}{condition}: {does}.")
}

/// One instruction as the JSON a surface shows.
pub fn instruction_json(instruction: &StandingInstruction) -> Value {
    json!({
        "id": instruction.id,
        "revision": instruction.revision,
        "sentence": sentence(instruction),
        "trigger": instruction.trigger,
        "condition": instruction.condition,
        "action": instruction.action,
        "builtin": instruction.builtin,
        "enabled": instruction.enabled,
        "active": instruction.is_active(),
        "approvedRevision": instruction.approval.as_ref().map(|a| a.revision),
    })
}
