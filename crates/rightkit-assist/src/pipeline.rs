//! The loop for one mail signal: sort, extract, triage-validate, store facts, run checks, fire instructions.
//!
//! The host drives it in steps because the model call is the host's: [`prepare`] does everything code can do and
//! either finishes or returns the tools-off request; the host sends it and calls [`apply_answer`] with the reply
//! (possibly once more, for the retry). [`replay`] drives the same steps in one call from a stored answer.
//! No step calls a model, reads a clock or touches the network. A host runs each step as a
//! `rightkit-job-queue` task keyed on the signal's dedupe key (see the README).

use crate::booking;
use crate::checks;
use crate::facts::{Fact, FactBody, Origin};
use crate::instruction::{self, FireOutcome};
use crate::signal::{SettledBy, Signal, SignalState, Source};
use crate::sort::{self, address_of, Category, InboundMail};
use crate::state::AssistState;
use crate::triage::{self, TriageRequest, TriageStep};
use rightkit_agent_engine_attention::inbox::NewInboxItem;

/// What one run did.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub signal_id: String,
    pub state: SignalState,
    pub category: Option<Category>,
    /// Ids of facts this run inserted (a fact already held is not listed).
    pub facts_added: Vec<String>,
    /// Findings from the checks, for the attention Inbox. The Inbox dedupes them by check and facts.
    pub findings: Vec<NewInboxItem>,
    /// Instruction firings this signal caused.
    pub fired: Vec<FireOutcome>,
    /// Whether a model was needed at all. `false` for bulk mail and for mail code settled.
    pub used_model: bool,
}

pub enum Prepared {
    Finished(RunReport),
    /// Code could not place this signal. Send the request to a model with no tools.
    NeedsModel(TriageRequest),
}

pub enum Applied {
    Finished(RunReport),
    /// The answer was rejected. Send this request once more.
    Retry(TriageRequest),
}

fn signal_mut<'a>(state: &'a mut AssistState, signal_id: &str) -> Result<&'a mut Signal, String> {
    state
        .signals
        .get_mut(signal_id)
        .ok_or_else(|| "no such signal".to_string())
}

/// Insert facts one at a time, running the checks after each insert.
fn store_facts(
    state: &mut AssistState,
    signal_id: &str,
    origin: Origin,
    bodies: Vec<FactBody>,
    now: i64,
    added: &mut Vec<String>,
    findings: &mut Vec<NewInboxItem>,
) -> Result<(), String> {
    for body in bodies {
        let fact = {
            let signal = state
                .signals
                .get(signal_id)
                .ok_or_else(|| "no such signal".to_string())?;
            Fact::new(signal, origin, body, now)
        };
        let id = fact.id.clone();
        if state.insert_fact(fact) {
            added.push(id);
            for item in checks::run_all(now, &state.all_facts()) {
                if !findings.iter().any(|f| f.id == item.id) {
                    findings.push(item);
                }
            }
        }
    }
    Ok(())
}

fn finish_report(
    state: &mut AssistState,
    signal_id: &str,
    sender_address: &str,
    added: Vec<String>,
    findings: Vec<NewInboxItem>,
    now: i64,
    used_model: bool,
) -> Result<RunReport, String> {
    let signal = state
        .signals
        .get(signal_id)
        .cloned()
        .ok_or_else(|| "no such signal".to_string())?;
    let mut fired = Vec::new();
    if signal.state == SignalState::Triaged {
        let ids: Vec<String> = state
            .instructions
            .values()
            .filter(|i| i.matches_mail(&signal, sender_address))
            .map(|i| i.id.clone())
            .collect();
        for id in ids {
            let produced = state.facts_of(signal_id);
            let ok = match state.instructions.get(&id) {
                Some(i) => condition_holds(&i.condition, &produced),
                None => false,
            };
            if ok {
                fired.push(state.fire_instruction(&id, &instruction::mail_trigger_key(&signal), now));
            }
        }
    }
    Ok(RunReport {
        signal_id: signal.id,
        state: signal.state,
        category: signal.category,
        facts_added: added,
        findings,
        fired,
        used_model,
    })
}

fn condition_holds(condition: &instruction::Condition, produced: &[&Fact]) -> bool {
    if let Some(floor) = condition.provenance_at_least {
        // Provenance orders Owner < KnownSender < Untrusted, so "at least as trusted" is "not greater".
        if produced.iter().any(|f| f.provenance > floor) {
            return false;
        }
    }
    if let Some(kind) = condition.requires_fact {
        if !produced.iter().any(|f| instruction::FactKind::of(&f.body) == kind) {
            return false;
        }
    }
    true
}

/// Sort, then read structured bookings. Finishes bulk mail as `no_work` and mail whose bookings code read as
/// `triaged` (settled by code), with no model call. Anything else needs the model.
pub fn prepare(
    state: &mut AssistState,
    signal_id: &str,
    mail: &InboundMail,
    now: i64,
) -> Result<Prepared, String> {
    {
        let signal = signal_mut(state, signal_id)?;
        if signal.source != Source::Mail {
            return Err("only mail signals go through the mail loop".to_string());
        }
        if signal.state != SignalState::New {
            return Err("the signal was already processed; use replay to run it again".to_string());
        }
    }
    let sorting = sort::sort(&state.sender_lists, mail);
    let sender_address = address_of(&mail.from);
    {
        let signal = signal_mut(state, signal_id)?;
        signal.category = sorting.category;
        signal.sender_known = sorting.sender_known;
        signal.advance(SignalState::Sorted)?;
        if sorting.category == Some(Category::Bulk) {
            signal.advance(SignalState::NoWork)?;
        }
    }
    let mut added = Vec::new();
    let mut findings = Vec::new();
    if sorting.category == Some(Category::Bulk) {
        let report = finish_report(state, signal_id, &sender_address, added, findings, now, false)?;
        return Ok(Prepared::Finished(report));
    }

    let bookings = match &mail.html {
        Some(html) => booking::extract(html),
        None => Vec::new(),
    };
    if !bookings.is_empty() {
        let bodies: Vec<FactBody> = bookings.into_iter().map(FactBody::Booking).collect();
        store_facts(state, signal_id, Origin::Code, bodies, now, &mut added, &mut findings)?;
        let signal = signal_mut(state, signal_id)?;
        if signal.category.is_none() {
            signal.category = Some(Category::Travel);
        }
        signal.settled_by = Some(SettledBy::Code);
        signal.advance(SignalState::Triaged)?;
        let report = finish_report(state, signal_id, &sender_address, added, findings, now, false)?;
        return Ok(Prepared::Finished(report));
    }

    let signal = state
        .signals
        .get(signal_id)
        .ok_or_else(|| "no such signal".to_string())?;
    Ok(Prepared::NeedsModel(triage::build_request(signal, mail)))
}

/// Judge the model's reply. `answer` is `None` when the host got no usable reply. On acceptance the typed facts
/// are stored (checks run after each); on a first rejection a retry request comes back; on a second the signal is
/// `triage_failed`. `sender_address` is the mail's From address, for instruction triggers.
pub fn apply_answer(
    state: &mut AssistState,
    signal_id: &str,
    request: &TriageRequest,
    answer: Option<&str>,
    sender_address: &str,
    now: i64,
) -> Result<Applied, String> {
    let step = {
        let signal = signal_mut(state, signal_id)?;
        if signal.state != SignalState::Sorted {
            return Err("the signal is not waiting for a triage answer".to_string());
        }
        triage::judge_answer(signal, request, answer)
    };
    let sender_address = address_of(sender_address);
    match step {
        TriageStep::Retry(next) => Ok(Applied::Retry(next)),
        TriageStep::Failed => {
            signal_mut(state, signal_id)?.advance(SignalState::TriageFailed)?;
            let report = finish_report(state, signal_id, &sender_address, Vec::new(), Vec::new(), now, true)?;
            Ok(Applied::Finished(report))
        }
        TriageStep::Accepted(output) => {
            let mut added = Vec::new();
            let mut findings = Vec::new();
            if output.category == triage::TriageCategory::Nothing {
                signal_mut(state, signal_id)?.advance(SignalState::NoWork)?;
            } else {
                store_facts(
                    state,
                    signal_id,
                    Origin::Model,
                    output.facts,
                    now,
                    &mut added,
                    &mut findings,
                )?;
                let signal = signal_mut(state, signal_id)?;
                if signal.category.is_none() {
                    signal.category = output.category.to_category();
                }
                signal.settled_by = Some(SettledBy::Model);
                signal.advance(SignalState::Triaged)?;
            }
            let report = finish_report(state, signal_id, &sender_address, added, findings, now, true)?;
            Ok(Applied::Finished(report))
        }
    }
}

/// Re-run sort, extraction, triage validation and checks for one signal. The host supplies the mail again and
/// the model's answer through `answer`, which is called with each request (at most twice) and may return a
/// stored reply. Facts the signal produced before are removed first, so the result is what the current rules
/// make of the current inputs. The debugging hook and the journey hook.
pub fn replay(
    state: &mut AssistState,
    signal_id: &str,
    mail: &InboundMail,
    now: i64,
    answer: &mut dyn FnMut(&TriageRequest) -> Option<String>,
) -> Result<RunReport, String> {
    signal_mut(state, signal_id)?.reset();
    state.remove_facts_of(signal_id);
    let mut request = match prepare(state, signal_id, mail, now)? {
        Prepared::Finished(report) => return Ok(report),
        Prepared::NeedsModel(request) => request,
    };
    loop {
        let reply = answer(&request);
        match apply_answer(state, signal_id, &request, reply.as_deref(), &mail.from, now)? {
            Applied::Finished(report) => return Ok(report),
            Applied::Retry(next) => request = next,
        }
    }
}
