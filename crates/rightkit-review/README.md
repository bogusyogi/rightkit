# rightkit-review

Shared review capability for the content pipeline: typed deterministic checks over a media or brief description, declared holds, a capped repair loop, an advisory judge interface, and versioned verdict records.

- Typed rules (`RuleSet::standard`, `Rule`, `RuleKind`). Each rule returns one `Verdict`: verified, failed, unverified, inconclusive, held, or not applicable.
- Declared holds (`Declarations`, `DeclaredWindow`, `WindowKind`). A brief can exempt a deliberate pause, silence, static region, or hold. Undeclared or partly declared defects still fail.
- Disposition (`disposition`). Failed findings block. Required unknowns block. Held findings route to a human. Everything else passes.
- Repair loop (`run_repair_loop`, `RepairCap`, `HARD_MAX_REPAIR_ATTEMPTS`). The cap never exceeds 5. When it is reached, the loop returns the last inspectable candidate.
- Advisory judge (`Judge`, `NoJudge`). Judge scores are recorded beside the disposition and cannot change it. This crate calls no model.
- Verdict records (`VerdictRecord`, `VERDICT_RECORD_SCHEMA`, `VERDICT_RECORD_VERSION`). A record is refused when its schema or version is not one this build reads.

Catalog capability: `content-review`. The catalog entry is declaration-only (`rightkit-ownership.json`). Consumers (GenRight G6, CutRight C10, PostRight P4) own their review policy. This crate owns the checks and verdict semantics.

## When to use it

- Use it when a content pipeline step must turn measurements of a rendered artifact into pass, hold, or block.
- Use it when a repair attempt must be bounded and its attempts must be auditable.

Do not use it to decode or measure media. Measurements arrive as data (`FrameStat`, `TextRegion`, `DeterminismProbe`). Do not use it to get a model's opinion as a verdict. The judge is advisory only. Do not use it for an unbounded retry loop.

## Cargo features

None. The crate has no `[features]` table.

## Platform support

Pure Rust with no platform-specific code. It needs no OS services and no network.

## Usage

Copied from `tests/repair_and_judge.rs`. `black_defect` and `frame_mut` come from `tests/support/mod.rs`. `Subject` is in the crate root exports. Illustrative setup is omitted:

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

## Version

Crate version: see [INDEX.md](../../INDEX.md). The crate is not published (`publish = false`). It has no CHANGELOG.
