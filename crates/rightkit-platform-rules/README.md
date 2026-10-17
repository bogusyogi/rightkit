# rightkit-platform-rules

A versioned catalog of per-platform limits and rules (caption lengths, carousel counts, video durations, quota windows, API versions), a stable rule-revision hash, and a check of planned values against the rules.

Catalog capability: `platform-rules` (to be added by the lead). Not yet published (0.1.0).

- `Catalog::bundled()`: the rules shipped in `assets/platform-rules.json`. `Catalog::from_json` loads another document; `merge` / `merge_json` add newer records.
- `Catalog::get(platform, account_type, capability, format) -> Option<&Rule>`: the latest revision of a rule. It falls back from the account type to `all` and from the format to `any`. A `Rule` carries `rule_id`, the limit (`limit()`), `source_grade`, `source_url`, `fetched()`, `revision()` and `supersedes`.
- `Catalog::revision(platform) -> RuleRevision`: a stable hash of the platform's latest rules. Store it on briefs, exports and approvals. `is_current(&revision)` says whether work bound to it is still current.
- `Catalog::check(&revision, &[Planned]) -> Result<(), Violation>` (and `check_at` with an explicit date): refuses a stale revision, a value with no rule, an unsourced or stale rule, a unit mismatch, and a value outside the limit.

## Rules enforced in code

- A record is `sourced` only when its grade is `official_fetched`, it names a source URL and fetch date, its value is stated and it has no conflict note. An `unsourced` record carries no value.
- A change is a new revision (`ruleRevision` = previous + 1, `supersedes` names the previous one). `merge` refuses an edit, a skipped revision and a renamed rule. History is kept.
- `unsourced`, `stale` (stored, or past `sunsetDate`) and absent rules block use.
- A change to a source URL or date alone does not change the revision hash.

## What the app keeps

Fetching and citing sources, the hold-or-regenerate behaviour when a revision changes, and the revision field on its own records. Editorial preferences (hashtags, posting windows) are not platform rules. `ruleRevision` on briefs and envelopes belongs to `rightkit-contracts`, not this crate.

## Bundled data

The seed rows come from PostRight's P0 and P1 research (fetch date 2026-10-08, official pages). Unit definitions (for example how a platform counts characters) are not stated by the sources; boundary tests should use ASCII text. `instagram.reel.duration` and `instagram.caption.length` are unsourced and block use. `linkedin.api_version` (202510) sunsets 2026-10-15, after which the check treats it as stale.

## Dependencies

`serde` and `serde_json` only. The revision hash is a change detector, not a cryptographic digest.

## Usage

```rust
use rightkit_platform_rules::{Catalog, Planned};

let catalog = Catalog::bundled();
let revision = catalog.revision("threads");
let planned = [Planned::number("all", "text.length", "text", 480.0).in_unit("characters")];
catalog.check(&revision, &planned)?;
```
