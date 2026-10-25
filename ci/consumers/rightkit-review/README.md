# rightkit-review registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-review` to the exact version in its
`Cargo.toml` with default features (the crate has none). It also depends on `serde_json = "1"`,
which is already in the crate's own dependency closure, for `Check` evidence and extension
parameters. It is built from the sealed archive through the offline directory registry. Nothing in it
is platform-gated, so Windows, macOS and Linux run the same assertions. Measurements are fixtures:
the crate never decodes media or calls a model.

It proves:

- Media rules: `RuleSet::standard` has the nine documented rule ids, all required; a clean video
  verifies every rule; each planted defect (black frame, frozen run, text overflow, low contrast,
  flash rate, differing renders, licence, duration, resolution) fails its own rule and no other;
  declared `Hold`, `Silence` and `Static` windows exempt exactly what they declare, a partly declared
  run still fails, an ambiguous frame holds, missing measurements are unverified or inconclusive,
  video-only rules are not applicable to a still, and `Subject::validate` refuses unusable input.
- `disposition`: failed blocks, a required unknown blocks, an advisory unknown passes, held holds, a
  block outranks a hold. `RuleSet::new` refuses empty ids, version 0, duplicates and thresholds
  outside their domain.
- Policy binding: `policy_hash` equals an independently computed SHA-256 of the canonical JSON,
  changes with any threshold or version, and `check_versions` follow the built-in and extension
  constants.
- Repair loop: `RepairCap` is limited to 5; the loop stops `Passed`, `RepairCapReached`,
  `RepairUnavailable` or `Held`, calls the repairer no more than the cap, and keeps the last
  candidate and every attempt; the advisory judge is recorded and cannot change the disposition;
  out-of-range scores are dropped, never clamped.
- Verdict records: JSON round trip; a 1.0.0 record without a binding is readable but never current;
  `verify_current` refuses a record once the policy, rule set version or a check version changes;
  another schema, a newer minor and a malformed version are refused.
- Text rules: banned phrase (whole word, case-insensitive, curly quotes), dash and semicolon density
  at and over the limit, sentence-length variance, tricolon frequency, stock opener and claim proof
  (exact decimal match, unit, subject, inclusive date window); inconclusive below `min_words` or
  without a body; not applicable to a non-text artifact; policy data outside its domain refused.
- Extension rules: `RuleRegistry` refuses empty, reserved `det.` and duplicate kinds, runs a
  consumer rule through `evaluate_with` and `run_repair_loop_with`; without a registry the kind is
  inconclusive.
- `ladder`: promotion at the default and an overridden threshold, demotion on a rejected final or a
  failed critic check, the versioned snapshot round trip, a legacy untagged snapshot, and each
  refusal.
- `qa` and `delivery`: `Check`, `overall_status`, the QA `policy_hash` (independently computed),
  the snake_case `VerdictRecord` wire shape, `verify_current` and `passed_under`, `with_check`,
  `unexpected_intervals`; `decode_integrity`, `required_streams`, `DurationRule`, `FrameRateRule`,
  `FormatRule`, `StreamSyncRule` and `CaptionRule` at and beyond their inclusive limits.

It does not prove decoding or measuring media, any real judge or model, or a consumer's own review
policy.

State: in-memory values only. No network, no files, no process spawn, no clock, no user data. Run
only by the CI consumer lane.
