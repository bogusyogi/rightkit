# Changelog

## 0.1.2 (unpublished)

- rightkit#1495: the handoff source block carries the open-items ledger. `SourceBlock` gains `open_items` (`OpenItem { id, item, owner, state, next, blocker, approval { needed, from }, evidence }`), `open_items_none_reason`, `procedures` (`Procedure { name, steps, steps_source }`) and `struck_items`; all are covered by the existing block hash, and a block without them keeps its digest. Ids are 1-64 characters of `[A-Za-z0-9_.-]`, unique; free text is secret-scanned.
- BEHAVIOUR, `bootstrap`: `--live-state` is now required, and the block must carry `open_items` or an empty list with a one-line `open_items_none_reason` (the reason is refused beside non-empty rows). `bootstrap` prefills the ledger from machine-readable sources before it hashes the block: `git status` and commits on no remote for each `--repo` (default: the workspace), the files under `--approvals-dir` (default `<workspace>/tasks/approvals`), and with `--issues-repo owner/name` (repeatable) the open issues from `gh`. The source adds rows, overrides a prefilled row by declaring its id, or strikes it in `struck_items` with a reason; a strike of an unknown id is refused. A source that was asked for and cannot be read refuses the pointer.
- `validate-handoff --context`: fails when a ledger id is missing from section 8 `### Imported open items` (6 columns: id, item, disposition, next action, approval needed, evidence), is duplicated or unknown, or lacks a disposition of carried, done, superseded or user-cancelled (done, superseded and user-cancelled need concrete evidence; carried needs a concrete next action); and fails when a step of a declared procedure is missing from, or out of order in, section 10. The messages name the ids and steps. The section 8 gap map now ends where the imported-items table starts.
- The paste block and `bootstrap --json` print the ledger, procedures and struck rows.
- No unit tests were added (Adrian: end-to-end only). The 0.6.36 replay fixture and the empty-ledger refusal are end-to-end runs against the installed Legion script; see the issue.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `legion-handoff` (engine).
