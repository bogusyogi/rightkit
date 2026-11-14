# Changelog

## 0.2.1

- Fixed: three mock fixtures (a draft body, an inbox comment, a publish error message) named "App" instead of the host app name; they read `hostAppName()` when used now. Mock data source only (dev and QA builds).

## 0.2.0

Breaking (one export renamed).

- Fixed: four strings read the host app name when the module loaded, before a host could call `setHostAppName`, so they showed "App". They are read when shown now: the outcome-unknown explanations, the `retries_exhausted` cause, and the awaiting-approval label.
- `OUTCOME_UNKNOWN_EXPLANATION` (a constant) is now `outcomeUnknownExplanation()`.
- Fixed: the awaiting-approval label named the host app twice where it should name the producer apps. New `setHostProducerApps(names)` / `hostProducerApps()`; a host passes the text it wants in the parentheses (PostRight: "GenRight or CutRight"). With none set the label has no parenthetical.

## 0.1.1

- Fixed: `@rightkit/social-ui/data` and `@rightkit/social-ui/data/source` are importable. The README sends hosts to these two modules by subpath (`selectDataSource`, `DataSource`, `DateRange` are left out of the root entry), but the `exports` map did not list them.

## 0.1.0

- First release. Social media management UI: accounts, drafts, approvals and analytics screens with their data layer.
