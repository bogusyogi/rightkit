//! The calendar half of the poll: read the owner's calendar a day back and 90 days ahead, and run each event the loop has not
//! seen, or has seen change, through the calendar loop (code only, never a model). Same neutral rules as [`crate::poll`]: the host
//! supplies the events and takes the findings.

use std::collections::BTreeMap;

use crate::calendar::{new_signal, InboundEvent};
use crate::pipeline;
use crate::poll::{Pass, PollHost, ACCOUNT};
use crate::state::AssistStore;

/// A day back.
pub const CALENDAR_BACK_SECS: i64 = 86_400;
/// 90 days ahead.
pub const CALENDAR_AHEAD_SECS: i64 = 90 * 86_400;

/// What changes an event's facts: its title, place, times, all-day flag and the owner's answer.
pub fn event_print(event: &InboundEvent) -> String {
    let text = format!(
        "event\n{}\n{}\n{}\n{}\n{}\n{}",
        event.summary, event.location, event.start, event.end, event.all_day, event.my_response
    );
    rightkit_fs::digest::sha256_bytes(text.as_bytes())
}

/// Reads the owner's calendar and runs each event the loop has not seen, or has seen change: one signal per event, the event as a
/// typed fact, and the checks over every fact the loop holds. Findings go to the host's Inbox as for mail. Returns how many events
/// were run.
pub fn calendar_pass<H: PollHost, S: AssistStore>(
    host: &H,
    store: &S,
    pass: &mut Pass,
    now: i64,
) -> Result<usize, String> {
    let owner = store.snapshot()?.sender_lists.owner_addresses;
    let found = host.events(
        now.saturating_sub(CALENDAR_BACK_SECS),
        now.saturating_add(CALENDAR_AHEAD_SECS),
        &owner,
    )?;
    let mut prints = BTreeMap::new();
    let mut run = 0;
    for event in &found {
        let new = new_signal(ACCOUNT, event, now);
        let print = event_print(event);
        prints.insert(new.message_id.clone(), print.clone());
        let admission = store.update(|state| state.admit(&new))?;
        let report = if admission.created {
            store.update(|state| {
                pipeline::prepare_calendar(state, &admission.signal_id, event, now)
            })??
        } else if pass.poll.event_prints.get(&new.message_id) != Some(&print) {
            store.update(|state| {
                pipeline::refresh_calendar(state, &admission.signal_id, event, now)
            })??
        } else {
            continue;
        };
        run += 1;
        host.deliver(report);
    }
    pass.poll.event_prints = prints;
    Ok(run)
}
