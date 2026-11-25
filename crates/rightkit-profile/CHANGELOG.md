# Changelog

## 0.1.0 (unreleased)

- First version (RK-8, the profile; blueprint 2026-11-24). Pure logic and plain values: no network, no model calls, no clock reads, no files.
  - `area`: ten areas with stable slugs; health and money carry a sensitivity floor; `Area::CARD` names the identity card's areas.
  - `entry`: `Entry` with value, source (owner, repository, mail or memory), `true_from`, state (confirmed, proposed, stale), sensitivity, optional `review_by`, history of replaced values and the owner's confirmation record; checks for keys, values, references and `YYYY-MM-DD` dates.
  - `profile`: `ProfileState` with entries and proposals. `propose` stores a suggestion and never changes a value (a disagreeing suggestion turns a confirmed entry stale); `set_by_owner`, `confirm` and `reject` take an `OwnerConfirmation`; `mark_stale` turns entries past their review date stale; a changed value keeps the old one as history.
  - `interview`: `questions` returns stale entries first, then unanswered template questions, at most a limit.
  - `card`: confirmed entries of the chosen areas that the health and money `Switches` allow, with the area's floor applied.
  - `neutral`: `export` and `import`, one JSON line per entry or proposal (`rightkit.profile.neutral.v1`); import is additive, idempotent, cannot confirm and never overwrites.
- Depends on `rightkit-fs` (`digest` feature), `rightkit-time`, `serde`, `serde_json`. No new external dependency.
- Left open, because the Nucleus contract leaves them open: the on-disk file form, a retired state, the sensitivity scale beyond health and money, the task kind to area table and the sync backend. See the README.
- Written, not compiled. No unit tests, per the 2026-11-03 rule.
