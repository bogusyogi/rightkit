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

Crate version: see [INDEX.md](../../INDEX.md) and `CHANGELOG.md`. 0.1.0 is staged, not published, in the registry record (`../../packages/release/rightkit-versions.json`). Current source: 0.3.0 (adds the autonomy ladder; 0.2.0 added text rules and open rule kinds).
