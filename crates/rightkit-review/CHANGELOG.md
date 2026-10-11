# Changelog

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
