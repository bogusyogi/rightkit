# Changelog

## 0.1.1 (unreleased; `publish = false` until the name is approved)

- Calendar signals (AG-2; additive). A calendar event is structured, so code settles it with no model call.
  - `calendar`: `InboundEvent` (the fields of `rightkit-google-apis`'s `CalEvent` plus `cancelled` and `organizer_is_self`), `new_signal` (source `calendar`, message id `<calendar id>/<event id>`, trust `Owner` when the owner organizes the event, else `Outside`), `extract` (a typed `CalendarEntry`; an all-day end is Google's exclusive date turned back one day; a cancelled event gives none; title and location are cleaned and capped).
  - `facts`: `CalendarEntry` and `FactBody::Event`; `Fact::event()`; `instruction::FactKind::Event`. A new enum variant: an exhaustive match over `FactBody` or `FactKind` in a host needs an arm.
  - `pipeline`: `prepare_calendar(state, signal_id, &InboundEvent, now) -> RunReport` (new, sorted, then triaged by code; a cancelled or unreadable event ends `no_work`; `used_model` false; no instruction fires, as instructions trigger on mail) and `refresh_calendar` (reset, drop the signal's facts, run again, for an event that moved or was cancelled).
  - `calendar_checks` (re-exported ids in `checks`): `trip.event` (a booking still ahead that no entry of the owner's covers; a flight within three days of a hotel stay belongs to that trip), `trip.event_dates` (an entry whose location names a hotel's city or name, with dates one to three days off the booking's), `calendar.conflict` (two timed entries in the same zone that overlap). Findings are built from dates and times only, never from a title or location. An entry from an outsider (provenance untrusted) does not count as covering a trip, only as a possible conflict. The booking checks stay quiet until a calendar-entry fact exists, because before that "no entry" would only mean "not read yet"; a calendar with no entries at all therefore raises nothing.
- Version 0.1.1 so a host can tell the calendar side from 0.1.0; still `publish = false`. Written, not compiled. No unit tests, per the 2026-11-03 rule.

## 0.1.0 (unreleased)

- First version (RK-2, the noticing loop; blueprint 2026-11-24). Pure logic and storage traits: no network, no model calls, no clock reads.
  - `signal`: one signal per event, unique on (source, account, message id), states `new -> sorted -> triaged | no_work | triage_failed`.
  - `sort`: the owner's sender lists and mail headers settle travel, customer, bill and bulk; bulk becomes `no_work` with no model call; a sender whose authentication failed is not matched against the lists.
  - `markup`, `booking`: a bounded HTML reader and structured booking extraction (schema.org hotel and flight reservations from JSON-LD and microdata): dates, places, reference.
  - `triage`: the request for a tools-off model (allowlisted headers and up to 8 KB of plain text inside an escaped untrusted wrapper), the output JSON schema, a strict validator, one retry then `triage_failed`. Output can only be typed facts.
  - `facts`: typed facts (booking, party, commitment, money) with provenance (`owner`, `known_sender`, `untrusted`) and the source message id; a date is a local date plus a zone.
  - `checks`: run after each fact insert; first check `trip.dates`, producing `NewInboxItem` findings for the attention Inbox. Finding text is built from dates only.
  - `instruction`: standing instructions (trigger, condition, action, revision), inert until owner-approved and voided by a revision; occurrences keyed by a digest of instruction, revision and trigger, queued once or skipped, never overlapped.
  - `state`: `AssistState` behind the `AssistStore` trait, with `MemoryAssistStore`.
  - `pipeline`: `prepare`, `apply_answer` and `replay` for one signal.
  - The triage output shape is written once, as const field tables in `triage`: `output_schema()` renders them as JSON Schema and `validate()` walks the same tables. No schema-derivation crate is a dependency, so there is no second hand-kept copy.
- Depends on `rightkit-agent-engine-attention` (the unreleased `inbox` module), `rightkit-fs` (`digest` feature), `rightkit-time`, `serde`, `serde_json`. No new external dependency.
- Written, not compiled. No unit tests, per the 2026-11-03 rule.
