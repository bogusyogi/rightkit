# rightkit-assist

The noticing loop for an always-on assistant (RK-2). A signal enters, code sorts it and reads structured bookings, a model with no tools sees only what code could not place, typed facts are stored with provenance, checks run after each fact, and findings go to the attention Inbox. Pure logic and storage traits: no network, no model calls, no clock reads. The host supplies the mail, the model's reply and the time.

Not published (0.1.0, unreleased). Written, not yet compiled.

## The name

`rightkit-assist` is a proposal; the final name is Adrian's. The name is in one place: `Cargo.toml` (`name`, and `[lib] name` beside it) and the directory. No source file spells it (`rightkit_assist::NAME` reads it back). To rename: the directory, those two manifest lines, the entries in `packages/release/rightkit-versions.json` and `Cargo.lock`, then regenerate `INDEX.md`.

## Modules

| Module | Job |
|---|---|
| `signal` | One signal per event, unique on (source, account, message id); states `new -> sorted -> triaged \| no_work \| triage_failed` |
| `sort` | Sender lists the owner edits and mail headers settle travel, customer, bill, bulk. Bulk stops as `no_work` |
| `markup`, `booking` | A bounded HTML reader; hotel and flight reservations from JSON-LD and microdata |
| `triage` | The tools-off request, the output schema, the validator, one retry |
| `facts` | Typed facts with provenance and source message id; dates as local date plus zone |
| `checks`, `calendar_checks` | Rules over facts, after each insert: `trip.dates`, and with calendar entries `trip.event`, `trip.event_dates`, `calendar.conflict` |
| `calendar` | Calendar signals: `InboundEvent`, `new_signal`, `extract` (code only, no model) |
| `instruction` | Standing instructions, inert until owner-approved; occurrences keyed by digest |
| `state` | `AssistState` and the `AssistStore` trait, with an in-memory store |
| `pipeline` | Mail: `prepare`, `apply_answer`, `replay`. Calendar: `prepare_calendar`, `refresh_calendar` |

## Rules this crate keeps

1. Outside text meets no tools. The model request has `tools` empty by construction; the mail is wrapped as untrusted with `<`, `>` and `&` escaped, so it cannot close the wrapper.
2. Code before model. Bulk mail and mail with readable bookings never reach a model (`RunReport::used_model` says so).
3. Only typed facts leave a mail. The validator rejects unknown fields, unknown enum values, bad dates and over-long text. The only free text a fact holds is a short display label, cleaned and capped. Finding text is built from dates only; a name from a mail never enters it.
4. Authority comes from the owner. An instruction is inert until an `OwnerApproval` for its current revision exists; nothing here makes one from a mail, a fact or a model's output. A revision voids the approval.

## How a host drives it

```text
poller   -> state.admit(NewSignal)            (idempotent: same source, account and message id changes nothing)
step 1   -> pipeline::prepare(state, id, mail, now)
              Finished(report)   bulk, or bookings read by code: no model call
              NeedsModel(req)    send req to a tools-off model
step 2   -> pipeline::apply_answer(state, id, &req, reply, from, now)
              Retry(req2)        send once more
              Finished(report)   triaged, no_work or triage_failed
report   -> report.findings go to the attention Inbox (it dedupes), report.fired are occurrences to run
```

`pipeline::replay(state, id, mail, now, &mut |req| stored_reply)` re-runs all of it for one signal (the debugging and journey hook).

### rightkit-job-queue integration (the host's)

Nothing here depends on `rightkit-job-queue`. The host keeps one `AssistState` behind its `AssistStore` and runs the steps as journal tasks:

- one task per signal, keyed on `Signal::dedupe_key`, so a duplicate poll cannot make a second task; the task payload is the signal id only (the mail is re-fetched, never stored in the journal);
- `prepare` is one task step; the model call is its own step, and its reply is the input to `apply_answer`, so a crash between them replays the call, not the sort;
- an occurrence is a task keyed on `Occurrence::key`; the host marks it with `AssistState::finish_occurrence`, which promotes the one queued occurrence.

Storage: implement `AssistStore::update` as load, run the closure, save, under one lock. `AssistState` is `serde`-serialisable.
