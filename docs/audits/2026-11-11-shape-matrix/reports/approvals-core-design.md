# Approvals core: one model, one rule set, three stores

Decision 3 of the shape review (`../README.md`, "Approvals"), taken on the recommended option, pending the
owner's confirmation. Source review: `family-agent-platform.md`, sections rightkit-approvals,
rightkit-agent-gateway, rightkit-remote-gateway, rightkit-agent-engine. File:line references below are to the
code as it was on the branch before this change (commit `e0f0c3b`), unless marked "now".

## What existed: five implementations

| # | Where | App | What it is |
|---|---|---|---|
| 1 | `crates/rightkit-approvals/src/lib.rs` (756 lines) + `eyes.rs` | GenRight | File queue: one challenge file per nonce, consumed by a `.used` marker; proof is an HMAC token over kind, draft id, input hash and nonce minted with a per-launch secret |
| 2 | `crates/rightkit-remote-gateway/src/approvals.rs` (1,147 lines) | Pulse | In-memory queue in the resident engine: requests, decisions, used ids; owner's click bound to a display digest; signed or PIN proof |
| 3 | `crates/rightkit-agent-gateway/src/approval*.rs` (6 files, 1,346 lines) | MailRight | SQL queue (`agent_approvals`): status strings, guarded UPDATEs, boot binding, caps, windows, decline cooldown |
| 4 | `crates/rightkit-agent-engine/src/approval.rs` `ApprovalBinding` | Pulse | Engine-side record: principal, task, effect, `args_hash`, destination, executor, scope, expiry, revision |
| 5 | `crates/rightkit-agent-engine-protocol/src/approvals.rs` `PendingApprovalContext` / `PermissionGateRequest`, with `crates/rightkit-agent-policy/src/approval_binding.rs` `PayloadDigest` | CodeRight | Engine-side record: tool, command, network host, optional `payload_digest` |

`crates/rightkit-owner-approvals` is Pulse's proof material (approver keys, signature verification,
`approval_sig.rs:483` `verify`), plus the shared `Refusal` type (`lib.rs:27-54`) the Pulse queue refused with.

Common to all three queues: a pending request with an id, a kind, a digest of the exact thing approved, an
expiry, single use, a decision only in the app's own window. Different: storage (files, memory, SQL) and the
proof of who decided (HMAC launch secret; signature or PIN; an in-process window call).

## The one data model (`rightkit-approvals` 0.2.0, `src/model.rs`)

* **Request**: `id` (opaque, 16 bytes from the OS source, `digest.rs` `new_id`), `kind` (the app's word:
  `act`, `session`, `send`, a draft kind), `scope` (tool, family, account), `origin`, `binding`, `summary`
  (what the decider is shown), `detail` (display-only, never digested), `created_ms`, `expires_ms` (the
  deadline for a decision).
* **Origin**: `door` (the way in: `local`, `gateway`, a window), `filer` (the process label that filed it),
  `client` (whom it is for: a chat, an agent). A grant never crosses any of the three.
* **Binding** (the reference rule, Pulse's `ApprovalBinding`, `rightkit-agent-engine/src/approval.rs:17-40`):
  `args` (hex SHA-256 of the exact arguments), `destination`, `expires_ms`, `revision`. `Binding::check` is
  the rule at use: same destination (else `Different`), same arguments and revision (else `Changed`), not
  past the expiry (else `Expired`). `Binding::digest` folds all four into one domain-tagged digest.
* **Digest**: `bind_of(parts)`, SHA-256 over length-prefixed parts, is Pulse's `bind_of`
  (`remote-gateway/src/approvals.rs:130-137`) moved unchanged; it is byte for byte CodeRight's
  `PayloadDigest::of(tool, payload)` (`agent-policy/src/approval_binding.rs:15-22`) as `bind_of([tool,
  payload])`. `Request::display_digest(domain)` is Pulse's `request_digest`
  (`remote-gateway/src/approvals.rs:395-404`) with the domain tag as a parameter, so no app's wire tag lives
  in the core.
* **Decision and state**: `State` = pending, approved, denied, done (used), failed (claimed, effect failed or
  unknown), expired. `Grant` (set by an approving decision) = `Once {valid_ms}`, `Idle {idle_ms}`,
  `Until {at_ms}`, `Open`. `Record` = request + state + `decided_ms`, `last_used_ms`, `grant`, `reason`,
  `result`. `Record::live` says whether it can still be decided or used.
* **Scope** is the request's `scope` field plus the origin: Pulse's per-client session grants
  (`approvals.rs:1035-1118`) and MailRight's per-agent, per-account approvals map onto it without new
  fields.

## The store trait (`src/store.rs`)

`insert`, `get`, `pending(now)`, `approved(now)`, `transition(id, from, change)`, `touch`, with default
`find_pending` and `count_pending`. `transition` is the only way a record changes state and is a
compare-and-set: of two callers moving the same record from the same state exactly one gets `true` (Pulse's
single lock, MailRight's guarded UPDATE `approval_store.rs:358-388`, GenRight's `create_new` marker
`lib.rs:495-510`). Nothing in a store decides.

Stores:

* `MemoryStore` (Pulse): one `Mutex<Vec<Record>>`, `const`-constructible for a `static`; every access sweeps
  by `Retention` (pending past deadline and ended grants dropped; closed records visible for a time; caps on
  pending, decided and used), Pulse's `prune_held` (`approvals.rs:406-434`).
* `FileStore` (GenRight, feature `file`, default on): `<root>/<kind>/<destination>.<id>.json`, plus
  `.approved`, `.used`, `.closed` markers made with `create_new`; a use and a close exclude each other. The
  request file keeps GenRight's `nonce` and `input_hash` keys so `rightkit_media_gen::host::approval::
  receipt_valid` (`rightkit-media-gen/src/host/approval.rs:179`) reads it unchanged.
* SQL (MailRight): kept in `rightkit-agent-gateway` as `approval_store::SqlStore`, implementing the trait over
  the existing `agent_approvals` table. Why not a feature of the core: it needs `rusqlite` through
  `rightkit-db` (a bundled SQLite build) and one app's table and columns (`boot_id`, `target_key`,
  `agent_drafts`); as a core feature it would put a database dependency and one app's schema behind a flag
  of the shared crate. The trait is the seam; a second SQL user would move the adapter into its own crate.

## The rules (`src/rules.rs`), once for every store

* `file`: refuses an empty id or kind; answers a repeat of a waiting request (`Request::same_ask`: kind,
  scope, whole origin, arguments, destination) with that request (Pulse `approvals.rs:485-494`); refuses
  past the pending cap, overall (Pulse 16, `approvals.rs:50`) or per client (MailRight 5 per agent,
  `approval_store.rs:15`).
* `decide`: only a pending, live request; an approval carrying a `Proof` is accepted only when it verifies;
  a decision carrying the display digest is accepted only when it equals the waiting request's (`Shown`,
  Pulse's `approval_changed`, `approvals.rs:840-845`); one `transition` to approved (with its grant) or
  denied.
* `take`: an approval is spent by one act, for the same kind, scope, door, filer and destination, the same
  arguments and revision, optionally the same summary (Pulse `take_act_exact`), while its grant lives; one
  `transition` to done. With a proof, a pending request is decided and used in one move (GenRight's token
  presented with the act, `lib.rs:447-523`).
* `session`: a live session grant for this kind, scope, door and client; finding it is a use
  (`touch`), Pulse's `serve_session` (`approvals.rs:1102-1117`).

## Proof and verify (`src/proof.rs`)

`Proof::verify(&Record)`: evidence that the decider approved exactly this record. Implementations live with
their key material, not in the core:

* launch-secret token: `rightkit-media-gen-approvals` `LaunchToken` (HMAC over kind, destination = draft id,
  args = input hash, id = nonce; `rightkit_media_gen::host::approval::token_matches`);
* signed approver key: `rightkit-owner-approvals` `approval_sig::SignedApproval` (kind `approve` over the
  record's id and client and the display digest shown; `approval_sig.rs:483` `verify`);
* PIN: checked by `rightkit-remote-gateway` `unlock::confirm_pin` before the decision;
* MailRight: the decision is a call from the app's own window in the app's process; no proof, and the
  binding is checked after the claim against the hash the card showed (`approval.rs:408-415`).

## How each implementation maps onto the core

| Old | New |
|---|---|
| GenRight challenge file (`lib.rs:360-427`) | `FileStore` record: id = nonce, kind, binding {args = input hash, destination = draft id, expires = +30 min}, detail {legacy_hash} |
| GenRight `consume` (`lib.rs:447-523`) | `rules::take` with `LaunchToken` proof; errors mapped back to its codes |
| GenRight kinds, commands, eyes, Blender | unchanged code in the new crate `rightkit-media-gen-approvals` 0.1.0 |
| Pulse `Held` / `Request` / `Decision` / used list (`approvals.rs:182-265`) | one `MemoryStore` of `Record`s |
| Pulse `prune_held` (`406-434`) | `Retention` and `Grant` (act `Once` 10 min; session `Idle` 30 min; standing `Idle` 30 days; unlock `Until` or `Open`) |
| Pulse `file_request_detail` (`461-515`) | field checks, then `rules::file` with cap 16 |
| Pulse `decide_inner` / `decide_signed` (`735-874`) | unlock checks, then `rules::decide` with `SignedApproval` (sig) or after `confirm_pin` (pin) |
| Pulse `take_local` (`888-937`) | `rules::take`, errors mapped to its messages |
| Pulse `serve_session` / `serve_revoke` (`1035-1147`) | `rules::session`, `MemoryStore::remove_where` |
| MailRight `Approval` struct (`approval_store.rs:28-45`) | core `Record` (`type Approval = Record`; `MailApproval` accessors: agent = origin.client, account = scope, draft = binding.destination) |
| MailRight `claim_approve` (`379-388`) | `rules::decide` over `SqlStore` (guarded UPDATE: unexpired and same boot) |
| MailRight `insert_pending` / `count_pending` (`174-192`, `267-274`) | `rules::file` with the per-agent cap 5 |
| MailRight `finish` / `sweep` (`313-375`) | `SqlStore::move_row` / `SqlStore::sweep` (timeout = state expired, stored as `denied`/`timeout`) |
| MailRight windows, decline cooldown, drafts, attach paths | stay in `approval_store.rs` / `approval_request.rs` as mail policy |
| Pulse `ApprovalBinding` (`agent-engine/src/approval.rs:17-40`) | `ApprovalBinding::core_binding()` gives the core `Binding` |
| CodeRight `PayloadDigest::of` (`agent-policy/src/approval_binding.rs:15-22`) | is `bind_of([tool, payload])` (same bytes) |
| CodeRight `PendingApprovalContext` (`agent-engine-protocol/src/approvals.rs:120-133`) | not changed: follow-up with decision 4 |
| owner-approvals `Refusal` (`lib.rs:27-54`) | the core's `Refusal`, re-exported under the same path |

## Daemon approval workflow folded in

The review missed a sixth implementation: `crates/rightkit-daemon/src/approval.rs` (1,538 lines,
PK07-4; line references to it are at commit `aa1f9a5`), `ApprovalCoordinator` over an `ApprovalStore` with an app `ApprovalVerifier`, an
`ApprovalProfile` of per-action data and an optional `AuditSink`. No crate, recipe or app in this repo
called it. It is deleted (rightkit-daemon 0.8.0, no re-export); what it guaranteed is either in the core
now or already covered there. Pulse's digest binding stays the reference rule.

Kept, as app-neutral rules in `rightkit-approvals` 0.2.0:

* **Claim, then settle.** The daemon's `Approved -> Claimed -> Settled` (`claim`, `settle`,
  `approval.rs:1381-1487`) is `State::Claimed`, `rules::claim` (every check of `take`; one claimant
  wins the compare-and-set, the others get `Used`) and `rules::settle` with `rules::Outcome`
  (`Done`; `Failed`, reason `failed`; `Unknown`, reason `unknown_outcome`). The settled states are the
  core's existing `Done` and `Failed`, whose reason code already named `unknown_outcome` (MailRight's
  sweep). The daemon's non-`Clone` `ClaimToken` with a claim id is not kept: the store's compare-and-set
  from `Claimed` already lets exactly one settlement win, and a late settlement after a recovery is
  `Error::Unclaimed`.
* **Outcome unknown after a crash or timeout.** `recover_claims` (`approval.rs:1488-1530`) is
  `rules::recover(store, claimed_before_ms, now, reconcile)`: every claim made at or before the cut-off
  that nobody settled is settled as the app's reconciler answers, and as `unknown_outcome` when it
  cannot tell. Never retried, never read as "did not happen". `Store` gains `claimed()`; `MemoryStore`
  never sweeps a claimed record; `FileStore` writes a `.claimed` marker that excludes `.used` and
  `.closed` until they settle it. MailRight's table has no claimed status (its `approved` row is the
  send in flight, settled by its own `sweep`), so `SqlStore` refuses a move to or from `Claimed`.
* **Revoke before use.** `decide(Revoke)`, `cancel` and `revoke_account` (`approval.rs:1231-1360`) are
  `rules::revoke(store, id, reason, now)` and `rules::revoke_where(store, select, reason, now)`: a
  pending or approved request becomes `Denied` with the app's reason (`revoked`, `cancelled`). No
  proof: revoking only reduces trust (as Pulse's `remove_where`). Who may cancel (the requester) is the
  app's check. A claimed request is past revoking: its act may be running.
* **Audit before effect.** `AuditRequirement::BeforeEffect` (`approval.rs:106-115`, `1063-1075`) is a store wrapper,
  `Audited<S, A: Audit>`: the app's row is appended before every filing and every move, and a failed
  append changes nothing. A row records an attempted move (another mover can still win the
  compare-and-set), as the daemon's did. What a row holds and where it is written stays the app's.

Dropped, and why:

* **Per-revision decision binding and `StaleRevision`.** The core binds a decision to the display digest
  of what the decider was shown and moves it with a compare-and-set from `Pending`; an answer to an
  earlier view fails `Shown` or finds the request no longer pending. The thing approved has its own
  revision in `Binding`.
* **The cross-request nonce ledger** (`claim_nonce`). A `Proof` is bound to one record and a record is
  decided once; a proof that carries a nonce spends it in `Proof::verify`, where its key material lives
  (`rightkit-owner-approvals` `admin_sig.rs` already keeps its spent nonces on disk).
* **Principals, account scope and named approvers** (`VerifiedPrincipal`, `AccountScope`). Who may
  decide for what is the app's proof (an approver key enrolled for an account, a window session).
* **`ApprovalProfile` / `ActionProfile`** (per-action lifetimes, required account or client,
  reconcilable flag, `max_open`). App policy: the request carries its own deadline, `Limits` caps what
  waits, `recover` takes the reconciler as an argument.
* **`redact_detail` and `AuditEvent::line`.** The core's `detail` is display-only and never digested;
  what reaches an audit row is the app's `Audit`.
* **The injected clock.** Every core rule already takes `now_ms`.
* **`Expired` on every touch and the `Revoked` and `Cancelled` states.** `Record::live` refuses an
  expired request at every rule; revoked and cancelled are `Denied` with a reason.

## What is left (follow-up)

* CodeRight's `PendingApprovalContext.payload_digest` is optional ("for back-compat"); making it a core
  `Binding` (mandatory digest, destination, expiry, revision) changes the turn loop's permission flow in
  `rightkit-agent-engine-core` and belongs to the agent-engine merge (decision 4).
* `ApprovalBinding` should carry a core `Binding` field instead of four loose fields once the task contract
  crate is drawn (decision 4).
* MailRight records no display digest at the decision (its card re-reads the draft and the hash is compared
  after the claim). Storing the shown hash in the row would make its decision digest-bound like Pulse's; it
  needs a column in the app's schema.
* MailRight's in-flight send (`approved` row, `sweep` to `failed`/`unknown_outcome` after `STUCK_MS`) is
  the core's claim and recover; storing it as `claimed` needs a status value in the app's schema (the
  `agent_approvals` CHECK constraint), after which `SqlStore` can stop refusing `Claimed`.
* `rightkit_media_gen::host::approval` (token mint and verify, receipts) stays in `rightkit-media-gen`: the
  app shell and providers use it without the engine.
