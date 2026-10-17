# Changelog

## 0.4.0 (2026-10-17)

- Add `qa` product report model: Pass/Fail/NotApplicable, Error/Warning, severity-aware blocking
  & generic persisted `qa::VerdictRecord` preserving CutRight report fields. Validated JSON checks
  schema, unique ids, nonzero `check_version` & aggregate status. Missing `check_version` reads as 1.
- Add `qa::policy_hash` (SHA-256 of the canonical JSON of a consumer policy) so product QA reports
  can be bound to the policy and check version that produced them (#179). Callers reuse a stored
  report only when both match.
- Add per-check allowed windows with `qa::unexpected_intervals`: whole-span containment inside one
  window of the same check id, explicit slack, required reasons, no merging of adjacent windows &
  overflow-safe signed timestamp arithmetic (#356).
- Add `delivery` checks: full-stream decode evidence (`DecodeIntegrity`), required streams,
  inclusive two-sided duration tolerance, codec/pixel-format allowlists, frame-rate tolerance,
  stream start/duration drift and caption timing, text safe zone, reading speed & tail coverage
  (#357). `delivery::DELIVERY_CHECK_VERSION` = 1.
- Add unit boundary/error tests & `tests/product_qa.rs` for public API adoption, persisted report
  compatibility & existing review behavior.
- Additive APIs only. Existing public structs, enum variants, standard rules, rule-set versions,
  check versions, content verdict record schema & behavior stay intact. No new dependencies.
  Version bump, registry ledger & publication remain release-owned.

## 0.4.0

Version not bumped here (the lockfile is not touched); release owner: this is a breaking minor for 0.x consumers. NOT COMPILED OR RUN: CI is the compiler.

- **Verdicts bound to the policy hash and the check versions (Orthic-Labs/rightkit#179, CutRight bogusyogi/CutRight#112).** A stored pass used to be reusable whenever the preset name matched, so tightening a policy under an unchanged name reused a stale pass. New `PolicyBinding { policy_hash, check_versions }`; `RuleSet::policy_binding()` / `policy_hash()` (SHA-256 over the canonical JSON of the whole rule set: id, version, every rule, `required`, thresholds, lists and extension params; map keys sorted); `check_version(&RuleKind)` with `BUILTIN_CHECK_VERSION` (1, bump when a built-in check changes how it measures) and `EXTENSION_CHECK_VERSION` (0: consumer-owned code, covered by the policy hash).
  - `VerdictRecord` gains `binding: Option<PolicyBinding>` (the repair loop fills it) and `verify_current(&RuleSet)`, which returns `ReviewError::StaleVerdict` when the binding is missing or the rule set id or version, policy hash or any check version differs. Reuse a stored pass only after it returns `Ok`.
  - Record version 1.1.0 (was 1.0.0). This build reads 1.0.0 and 1.1.0; a 1.0.0 record parses with no binding and is never current. A reader of 1.0.0 only refuses 1.1.0 records.
  - Breaking for exhaustive users: `VerdictRecord` has a new public field and `ReviewError` a new variant `StaleVerdict`; neither is `#[non_exhaustive]`.
  - Not done here: CutRight's silence and loudness checks. This crate has the black-frame (`det.black_frame`) and frozen-frame (`det.frozen_frame`) checks; silence and loudness need audio measurement types and CutRight's thresholds, which are not in this repository.
  - Tests: `tests/policy_binding.rs`; the FIPS vectors for the local SHA-256 are in `src/binding.rs`. No new dependency.

## 0.3.0

- Autonomy ladder (`ladder` module), ported from CutRight's `cutright-rules::ladder` (RightKit #236). Every (content type, platform) pairing starts at `Reviewed`.
  - Promotion: at `Reviewed`, `approvals_to_review_light` clean approved finals promote to `ReviewLight`; at `ReviewLight`, `approvals_to_autonomous` more promote to `Autonomous`. The streak restarts at each promotion.
  - Demotion: a rejected final drops to `Reviewed` at any level. A failed critic check resets the streak and, on an `Autonomous` final, drops one level to `ReviewLight`.
  - `LadderPolicy` holds the thresholds (defaults 5 and 10; omitted JSON fields keep defaults; zero and unknown fields are refused).
  - `Ladder::to_json` / `from_json` write and read a state tagged `schema` = `rightkit-review.ladder`, `version` = `1.0.0`. A snapshot with another schema or a newer version is refused. A snapshot with neither field (CutRight's original format) is read as 1.0.0.
- Additive: no existing API, rule id, or verdict record format changed. No new dependencies.

## 0.2.0

- Text rule family on `ArtifactKind::Text`. Each rule is pure and takes its thresholds and lists as data. Each returns pass, fail, or inconclusive with a stable rule id and offsets or counts in the reason:
  - `det.text.banned_phrase` (`RuleKind::BannedPhrase`): case-insensitive, whole-word match against a supplied list.
  - `det.text.dash_density` (`RuleKind::DashDensity`): U+2014 and U+2013 per 100 words above a supplied maximum.
  - `det.text.semicolon_density` (`RuleKind::SemicolonDensity`): U+003B per 100 words above a supplied maximum.
  - `det.text.sentence_length_variance` (`RuleKind::SentenceLengthVariance`): population variance of words per sentence below a supplied floor.
  - `det.text.tricolon_frequency` (`RuleKind::TricolonFrequency`): tricolon sentences per 100 sentences above a supplied maximum. The detection pattern is defined in `text.rs`.
  - `det.text.stock_opener` (`RuleKind::StockOpener`): a sentence opening with a supplied one-to-three-word opener.
  - `det.text.claim_proof` (`RuleKind::ClaimProof`): each claim annotation {value, unit, subject, date} must match a supplied approved proof on value, unit and subject, with the date inside its inclusive window. Decimals compare as exact strings after canonicalisation.
  - Every text rule is inconclusive below its supplied `min_words`, when no text body is supplied, or when its denominator is zero.
- Text input: `ArtifactDescription::text` (`TextBody { body, claims }`) and `ClaimAnnotation`.
- Open rule kinds: `RuleKind::Extension { kind, params }` runs a consumer rule registered in a `RuleRegistry` (`ExtensionRule`). An unregistered kind is inconclusive, so it blocks when required. Kind names starting with `det.` are reserved.
- `run_repair_loop_with` takes a `RuleRegistry`. `RuleSet::evaluate_with` does the same for a single evaluation.
- Existing `det.*` media rule ids, their JSON shapes, and their behaviour are unchanged. Verdict record schema and version are unchanged.

Breaking Rust API changes in 0.2.0:
- `RuleKind` is `#[non_exhaustive]`. Matches outside this crate need a wildcard arm.
- `ArtifactDescription` has a new public field `text`. Struct literals outside this crate must set `text` (use `None` for media).
- Verdict record readers are unaffected.

## 0.1.0

- First published release. Shared review capability for the content pipeline: typed deterministic checks over a media or brief description (`RuleSet::standard`), declared holds and windows, disposition rules, a hard-capped repair loop (`run_repair_loop`, cap never above 5), an advisory `Judge` interface that cannot override deterministic failures, and versioned `VerdictRecord`s.
