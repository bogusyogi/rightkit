# rightkit-approvals

One approvals core for every app. An agent, a tool or a job asks; the user decides in the app; the act
runs once. The record and the rules live here once; apps differ only in where records are stored and how
the decider is proven.

* **Model.** `Request` (id, kind, scope, `Origin` {door, filer, client}, `Binding`, summary, display-only
  detail, created, deadline) and `Record` (the request, its `State`: pending, approved, denied, done,
  failed, expired; the `Grant` an approval gives; a reason code; a result).
* **Binding (the reference rule).** An approval is bound to a digest of the exact arguments, the
  destination, the expiry and the revision (`Binding`, `bind_of`: length-prefixed SHA-256). The act about
  to run presents its binding and must match (`Binding::check`). A decision is bound to the display
  digest of what the decider was shown (`Request::display_digest`, under the app's own domain tag).
* **Rules** (`rules`): `file` (repeat of a waiting request gets the same id; pending cap), `decide` (only
  a pending, unexpired request; proof and display digest checked; one compare-and-set), `take` (spent by
  one act for the same kind, scope, door, filer, destination, arguments and revision; with a proof a
  pending request is decided and used in one move), `session` (a live session grant, refreshed by use).
* **Stores.** The `Store` trait (atomic `transition`), `MemoryStore` (nothing on disk approves; a restart
  forgets) and `FileStore` (feature `file`, default: one owner-only file per request, a `create_new`
  marker per move). An app's database implements the trait (MailRight's SQL store does, in
  `rightkit-agent-gateway`).
* **Proof.** The `Proof` trait: a launch-secret token (`rightkit-media-gen-approvals`), a signed approver
  key (`rightkit-owner-approvals`, `approval_sig::SignedApproval`), the engine-checked PIN. A decision
  made by a call from the app's own window needs none.

Who uses it: Pulse's remote-tool queue (`rightkit-remote-gateway`, `MemoryStore`), GenRight's draft and
delivery approvals (`rightkit-media-gen-approvals`, `FileStore`), MailRight's send, forward, delete and
attach approvals (`rightkit-agent-gateway`, SQL). Pulse's `ApprovalBinding` (`rightkit-agent-engine`)
converts to the core `Binding`; CodeRight's payload digest (`rightkit-agent-policy`) is `bind_of`.

Design note: `docs/audits/2026-11-11-shape-matrix/reports/approvals-core-design.md`. Version 0.1.0 of
this crate was GenRight's approvals; they are now `rightkit-media-gen-approvals`.
