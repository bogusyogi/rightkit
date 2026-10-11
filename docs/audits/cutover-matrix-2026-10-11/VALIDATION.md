# Validation receipt

Final source validation: 2026-10-11T03:07:58.829649+00:00.

- 29 app/tool columns × 97 catalog rows: 2,813 classifications; no missing rows, duplicate IDs or unresolved statuses.
- 814 cited files checked for existence & line bounds. Status-specific shared/local/owner/applicability evidence present; zero structural errors.
- All 2,813 Excel & CSV statuses match canonical JSON. All 29 app payloads match assembled inventory; all 29 handoffs exist.
- Offline browser checks passed: full matrix, component/app/status filters, evidence dialog, RightSites Swift states, 27 active columns & two retained columns. No page errors or network requests.
- Source hashes & cited excerpts preserve observed state. 22 cited files changed since initial baseline; targeted caller rechecks are recorded in reconciliation, with final evidence frozen at validation time.

Counts: Y 236; N 164; M 12; NA 2401. Totals include archive/retained columns. NA describes current feature/runtime compatibility, not overall standardization; see unsupported-host reuse notes.

Mechanical text checks retained 191 review prompts, primarily absence phrasing. They are not semantic test results. Per-app source review & reconciliation supply classification rationale. Product builds, installed binaries, performance & publication were outside this source audit; artifact validation checks report integrity only.

[Structural receipt](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/evidence/validation.json) · [Export hashes](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/evidence/artifact-validation.json) · [UI receipt](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/evidence/ui-validation.json) · [Reconciliation](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/RECONCILIATION.md)

Use [migration queue](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/MIGRATION-QUEUE.md) & per-app handoffs to assign concrete cutovers.
