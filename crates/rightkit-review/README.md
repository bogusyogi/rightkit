# rightkit-review

Shared review capability for the content pipeline: typed deterministic checks over a media or text description, declared holds, a capped repair loop, an advisory judge interface, and versioned verdict records.

- Typed rules (`RuleSet::standard`, `Rule`, `RuleKind`). Each rule returns one `Verdict`: verified, failed, unverified, inconclusive, held, or not applicable.
- Text rules (`RuleKind::BannedPhrase`, `DashDensity`, `SemicolonDensity`, `SentenceLengthVariance`, `TricolonFrequency`, `StockOpener`, `ClaimProof`). Pure checks over a supplied text body. Thresholds and lists are data in the rule. Each returns pass, fail, or inconclusive.
- Open rule kinds (`RuleKind::Extension`, `RuleRegistry`, `ExtensionRule`). A consumer registers its own rule without editing this crate. An unregistered kind is inconclusive.
- Declared holds (`Declarations`, `DeclaredWindow`, `WindowKind`). A brief can exempt a deliberate hold, silence, or static region (`WindowKind::Hold`, `Silence`, `Static`). Undeclared or partly declared defects still fail.
- Disposition (`disposition`). Failed findings block. Required unknowns block. Held findings route to a human. Everything else passes.
- Repair loop (`run_repair_loop`, `RepairCap`, `HARD_MAX_REPAIR_ATTEMPTS`). The cap never exceeds 5. When it is reached, the loop returns the last inspectable candidate.
- Advisory judge (`Judge`, `NoJudge`). Judge scores are recorded beside the disposition and cannot change it. This crate calls no model.
- Verdict records (`VerdictRecord`, `VERDICT_RECORD_SCHEMA`, `VERDICT_RECORD_VERSION`). A record is refused when its schema or version is not one this build reads.
- Policy-bound verdicts (`PolicyBinding`, `RuleSet::policy_binding`, `VerdictRecord::verify_current`, `check_version`). A record stores the SHA-256 of the whole rule set and each rule's check version; a stored pass is reusable only while `verify_current` accepts it, so tightening a policy under an unchanged preset name invalidates it. Records from before 1.1.0 carry no binding and are never current.
- Autonomy ladder (`ladder::Ladder`, `LadderKey`, `LadderPolicy`, `Level`, `Final`, `LADDER_SCHEMA`, `LADDER_VERSION`). Reviewed, review-light and autonomous per (content type, platform), with promotion and demotion, overridable thresholds (defaults 5 and 10) and a version-tagged serialised state.

Catalog capability: `content-review`. The catalog entry is declaration-only (`rightkit-ownership.json`). Consumers (GenRight G6, CutRight C10, PostRight P4) own their review policy. This crate owns the checks and verdict semantics.

## When to use it

- Use it when a content pipeline step must turn measurements of a rendered artifact into pass, hold, or block.
- Use it when a repair attempt must be bounded and its attempts must be auditable.

Do not use it to decode or measure media. Measurements arrive as data (`FrameStat`, `TextRegion`, `DeterminismProbe`). Do not use it to get a model's opinion as a verdict. The judge is advisory only. Do not use it for an unbounded retry loop.

## Text rules

Text rules run on `ArtifactKind::Text`. They read `ArtifactDescription::text` (a `TextBody` with the exact text and its claim annotations). The text is data handed in. Nothing is fetched. A text rule needs no threshold of its own: each one takes its limits and lists from its `RuleKind`, and `RuleSet::new` refuses data outside the domain (for example an empty phrase list, a NaN maximum, or an invalid proof date).

| Rule id | Kind | Passes when |
|---|---|---|
| `det.text.banned_phrase` | `BannedPhrase { phrases, min_words }` | No listed phrase occurs (case-insensitive, whole word). |
| `det.text.dash_density` | `DashDensity { max_per_100_words, min_words }` | U+2014 and U+2013 per 100 words is at or below the maximum. |
| `det.text.semicolon_density` | `SemicolonDensity { max_per_100_words, min_words }` | U+003B per 100 words is at or below the maximum. |
| `det.text.sentence_length_variance` | `SentenceLengthVariance { min_variance, min_sentences, min_words }` | Population variance of words per sentence is at or above the floor. |
| `det.text.tricolon_frequency` | `TricolonFrequency { max_per_100_sentences, min_sentences, min_words }` | Tricolon sentences per 100 sentences is at or below the maximum. |
| `det.text.stock_opener` | `StockOpener { openers, min_words }` | No sentence opens with a listed one-to-three-word opener. |
| `det.text.claim_proof` | `ClaimProof { approved_proofs, min_words }` | Every claim annotation matches an approved proof on value, unit and subject, and its date is inside that proof's inclusive window. |

Every text rule is inconclusive below its `min_words`, when no body is supplied, or when its denominator is zero. Inconclusive blocks when the rule is required.

Normalisation: curly quotes fold to ASCII; a word is a run of letters, digits and U+0027; sentences end at `.`, `!` or `?` followed by whitespace or the end of text, and at line breaks that end a line holding non-whitespace. Unicode NFC is not applied here, so producers supply NFC text. Offsets are character positions. Decimal claim values compare as exact strings after removing leading and trailing zeros; they never pass through floats.

Judge scores stay advisory. A text verdict's disposition depends only on its deterministic findings.

## Consumer rules

```rust
let mut registry = RuleRegistry::new();
registry.register("acme.word_limit", |subject: &Subject, params: &serde_json::Value| {
    // return Verdict::Verified, Verdict::Failed { .. } or Verdict::Inconclusive { .. }
    Verdict::Verified
})?;
let rule = Rule {
    id: "acme.word_limit".into(),
    required: true,
    kind: RuleKind::Extension {
        kind: "acme.word_limit".into(),
        params: serde_json::json!({ "max": 120 }),
    },
};
```

Run it with `RuleSet::evaluate_with` or `run_repair_loop_with`. Kind names starting with `det.` are reserved.

## Cargo features

None. The crate has no `[features]` table.

## Platform support

Pure Rust with no platform-specific code. It needs no OS services and no network.

## Usage

Copied from `tests/repair_and_judge.rs`. `frame_mut` comes from `tests/support/mod.rs`; `black_defect` is defined at `tests/repair_and_judge.rs:33`. `Subject` is in the crate root exports. Illustrative setup is omitted:

```rust
use rightkit_review::{
    disposition, run_repair_loop, Disposition, Finding, NoJudge, RepairCap, RuleSet, StopReason,
};

fn fix_black(candidate: &Subject, _findings: &[Finding], _attempt: u32) -> Option<Subject> {
    let mut next = candidate.clone();
    frame_mut(&mut next, 5000).black = false;
    Some(next)
}

let rules = RuleSet::standard();
let mut repairer = fix_black;
let outcome = run_repair_loop(
    &rules,
    black_defect(),
    RepairCap::new(3).unwrap(),
    &mut repairer,
    &NoJudge,
)
.unwrap();
assert_eq!(outcome.stop, StopReason::Passed);
assert_eq!(outcome.record.attempts[1].disposition, Disposition::Pass);
```

`Subject`, `frame_mut` and `black_defect` come from the test file and its fixtures. See `tests/holds.rs` for declared windows and `tests/planted_defects.rs` for single-defect cases.

## Tests

- `tests/holds.rs`: declared holds and windows.
- `tests/planted_defects.rs`: one planted defect per rule.
- `tests/repair_and_judge.rs`: repair cap, stop reasons, and judge isolation.
- `tests/text_rules.rs`: each text rule at its limit, over its limit, with Unicode, empty text and malformed policy; judge isolation for text.
- `tests/extension.rs`: consumer rules through the registry, unregistered kinds, and wire compatibility.

## Version

Current source: unreleased (crate version stays 0.4.0 until the release owner bumps it). See `CHANGELOG.md`; registry staging & publication belong to release owner.

## Product QA & delivery (unreleased)

`qa` provides `CheckStatus::{Pass, Fail, NotApplicable}`, `Severity::{Error,
Warning}`, `Check` & `overall_status`. Only `Error` + `Fail` blocks; a failed
warning remains visible. These APIs leave existing `Verdict`, `Finding`,
`disposition`, `Subject` & repair records unchanged.

`qa::policy_hash(&spec)` gives the SHA-256 of a consumer policy; store it with `check_version`.
`qa::VerdictRecord<Spec, Render, Media>` stores product reports with snake_case
fields: `schema_version`, `preset`, `status`, `render`, `media`,
`captions_blake3`, `spec`, `check_version`, `checks`, `ffmpeg_identity` &
`generated_at`. Generic payloads keep consumer policy & identity formats intact.
`from_json`/`to_json` validate schema version 1, unique check ids, nonzero check
version & aggregate status. Missing legacy `check_version` reads as 1.
`with_check` replaces by id & recomputes status. Receipt verification still
binds render bytes, captions & full policy; parsing alone does not prove freshness.
This envelope is separate from root `VerdictRecord` (content pipeline schema 1.1.0).

`qa::unexpected_intervals` takes measured `Interval`s & `AllowedWindow`s.
Each declaration names exactly one check, carries a reason & sets its own
`slack_ms`. Only whole-span containment inside one matching window exempts a
measurement; boundary equality passes, adjacent windows never merge. Signed
millisecond times & widened arithmetic handle negative probe offsets safely.
Existing `DeclaredWindow` semantics remain unchanged.

`delivery` owns pure checks over supplied measurements:

| API | Rule |
|---|---|
| `decode_integrity` | Full decode must complete, exit successfully & report no error-level diagnostics. Missing input fails. |
| `required_streams` | Video must exist; audio must exist when required. |
| `DurationRule::check` | Positive duration within inclusive two-sided tolerance; no target still requires positive duration. |
| `FrameRateRule::check` | Finite positive rate within max(absolute tolerance, expected × relative tolerance). |
| `FormatRule::check` | Exact allowlist for codecs or pixel format, with caller-selected severity. |
| `StreamSyncRule::check` | Absolute start offset & duration drift within independent inclusive limits; compare readable pairs. |
| `CaptionRule::check` | Cue timing/order/overlap, render end & text safe zone block; reading speed & tail coverage warn. |

Policies & measurements implement serde. Invalid policy returns `ReviewError`;
missing required measurements produce failed checks. Optional absent streams
should use `Check::not_applicable`. Sync is not applicable without both streams
or any comparable pair; malformed present timestamps fail. Caption absence is
not applicable only when optional; supplied empty cues fail. Unknown render
duration skips caption end-bound & tail checks; check duration independently.
Safe zone uses nonempty line count & Unicode scalar values per line, matching
CutRight's text burn-in box. Parsing & rendered pixel bounds stay with caller.

`DecodeIntegrity` must describe decoding every selected stream through EOF.
Sampling, cancellation & timeouts set `completed = false`. Keep full error-level
diagnostics in `errors`: only display evidence truncates to eight lines. This
crate does not launch FFmpeg or verify a caller's measurement provenance.
`DELIVERY_CHECK_VERSION` versions these checks; include it in consumer check-code
binding. Bump consumer check version during adoption so existing receipts expire.

```rust
use rightkit_review::{delivery::DurationRule, qa::CheckStatus};

let rule = DurationRule { expected_ms: Some(10_000), tolerance_ms: 100 };
assert_eq!(rule.check(Some(9_900)).unwrap().status, CheckStatus::Pass);
assert!(rule.check(Some(9_899)).unwrap().blocking());
```

`tests/product_qa.rs` covers public adoption, warning persistence, old report
wire shape & legacy review isolation. Unit tests cover rule boundaries, missing
measurements, malformed policies, Unicode captions & timestamp overflow.
