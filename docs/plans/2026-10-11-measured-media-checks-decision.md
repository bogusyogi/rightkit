# Decision: measured media checks (gap ledger row 91)

Status: adjudication, read-only. I changed no files and ran no builds or ffmpeg. Where I could not settle something from code, I name the experiment.

## 0. Findings that change the framing

- **No second consumer of rendered-file checks exists yet.** I found none in PostRight, ViewRight or CutRight beyond `cutright-qa`.
  - PostRight has no `rightkit-review` import in any `Cargo.toml` or `.rs` file. It only plans to reuse `disposition` (`postright/docs/contracts/p4-skills-system.md:304`).
  - GenRight is the one real second consumer. `genright-studio/src/pipeline_workflows/media.rs:308-343` hand-parses `ebur128=peak=true`. `compose.rs:343-371` does decode, probe and loudness verification of an audio-only output. The shape must therefore work with no video stream and no captions.
- **`rightkit-media` already has a loudness family.** `loudnorm.rs` has `measure_loudness` (:340) and `LoudnessAcceptance::verify` (:297). It treats non-finite loudness as silent (:86-88). The contract's "nearest shared code" (section 8) omits it.
- **`rightkit-review` is unpublished.** It is staged only at 0.1.0 (`packages/release/rightkit-versions.json:343`) while `Cargo.toml` says 0.2.0. CutRight pins `=0.2.0` (`cutright-qa/Cargo.toml:25`) and resolves it by path patch (`cutright/Cargo.toml:104`).
- **`rightkit-review` documents that it never decodes media** (`subject.rs:1-4`). It also has no severity and no time slack.

## 1. Home

**Decision: a new crate `rightkit-media-checks` (lib `rightkit_media_checks`). A small `ebur128` module goes into `rightkit-media`.**

Dependency direction:
```
rightkit-media-checks -> rightkit-media (default-features = false)
                      -> rightkit-review   (optional feature `review`, adapter only)
apps -> rightkit-media-checks
```
`rightkit-review` and `rightkit-qa` are not changed structurally.

What goes where:
- **`rightkit-media`:** ebur128 command text, summary parser and typed errors (see section 2). It already owns the ffmpeg boundary, ffprobe JSON (`probe.rs:376-495`, which skips attached pictures at :387 and rejects `0/0` rationals at :366) and the runner.
- **`rightkit-media-checks`:** the six invocations, the detector parsers (`parse_blackdetect`, `parse_freezedetect`, `parse_silencedetect`, `ClipCounter`), `Policy`, `judge`, the SRT caption checks, versions and policy hash.
- **Presets:** none ship in the crate. YouTube, Shorts, Reels and TikTok values are CutRight product policy. GenRight targets -16 LUFS.

Rejected alternatives:
- **Module in `rightkit-review` depending on `rightkit-media`.**
  - It breaks the crate's "never decodes media" contract (`subject.rs:1-4`).
  - It drags `rightkit-media`, `rightkit-process` and `rightkit-fs` into PostRight's text-only use.
  - A `Failed` verdict always blocks (`verdict.rs:46-53`), so Warning would be a semantic change to a crate PostRight plans to reuse.
  - It couples an unpublished crate's release to media churn.
  - A feature flag would not fix this. Feature-gated public types still define what review is.
- **Module in `rightkit-media`.**
  - Policy, thresholds, SRT parsing and verdicts are not ffmpeg-boundary concerns.
  - Apps pin media exactly (`=0.2.6` in five CutRight crates). Every threshold or wording change would force a lockstep bump across them.
- **In `rightkit-qa`.**
  - It is a desktop-QA harness with png, getrandom, toml, control, service and framed-sidecar dependencies, and it sits in the runtime-budget class (`docs/RUNTIME-BUDGETS.md:17`).
  - Loudness there is only a convenience; its `measure` is a different algorithm.
- **Split with measurement in media and judgement in review.** Review's input is per-frame `FrameStat` samples (`subject.rs:25`). Converting detector intervals to frames loses millisecond precision and the slack semantics. Review still has no Warning.
- **Everything in media with a thin judge in CutRight.** This is the status quo and leaves GenRight duplicating the parser.

## 2. Loudness

**Placement.** Add `rightkit-media/src/ebur128.rs` with these items:
- `ebur128_filter(peak: PeakMode) -> String` (adds `framelog=quiet`).
- `parse_ebur128_summary(stderr) -> Result<Ebur128Summary, Ebur128Error>`.
- A `GateState`-aware integrated value.

`loudnorm.rs` stays for normalisation. I rejected using loudnorm JSON for checks: it is a different filter and report parity with CutRight needs the `Summary:` block.

**What happens to `rightkit-qa::loudness`:**
- Keep `measure`, `measure_file` and `Loudness`. GenRight asserts against them (`genright-e2e/src/lib.rs:383`).
- **Deprecate** `ebur128_ffmpeg` and `parse_ebur128_summary`. No external caller exists; only `tests/media_ffmpeg.rs:80,131` use them.
- Do not re-export. Do not fix in place: adding a media dependency to a desktop harness for two functions is not worth it.
- Move the pure-Rust versus ffmpeg agreement tests onto the new media parser (dev-dependency).

**The three behavioural differences, decided:**

| Difference | Shared semantics |
|---|---|
| Missing summary: rightkit-qa errors (`loudness.rs:291-293, 301-304`), CutRight returns None (`probe.rs:297-303`) | The parser returns a typed `Err(NoSummary \| NoIntegrated)`. The check layer maps it to `Unverified` with Error severity, which blocks. The outcome is identical to CutRight's blocking Fail "ebur128 produced no summary" (`run.rs:368-378`). Absence is never encoded as a number or as None. |
| One `Peak:` is stored as both sample and true peak, and defaults to -inf (`loudness.rs:305, 310-311`) | `true_peak_dbtp: Option<f64>` and `sample_peak_dbfs: Option<f64>` are separate. The parser reads the section heading ("True peak:" or "Sample peak:"), not the first `Peak:` line. Absent gives `None`, never -inf. With `peak=true`, sample peak is `None`. CutRight's evidence sample peak keeps coming from the clip scan (`probe.rs:344-346`). |
| -70 gate: rightkit-qa maps ≤ -70.0 to None (`loudness.rs:22, 308`), CutRight maps only non-finite (`probe.rs:293`) | `Integrated::{Value(f64), BelowGate}`. Any non-finite printed value is `BelowGate`. Any finite printed value is `Value`, including -70.0. Never fabricate `BelowGate` from a threshold. |

**All-gated signal: undecided from code; experiment named.** The only evidence for `-inf` is CutRight's own fixture (`probe.rs:577-580`) and a comment. rightkit-qa's comment claims -70.0 (`loudness.rs:307`). Nothing captured real output.

Run each ffmpeg build we ship (CutRight bundle, GenRight bundle, the `rightkit-media` catalog build). Use `anullsrc` for 3 s, a 0.3 s sine, and sines at -75 dBFS and -69.9 LUFS. Use two chains:
- `-af silencedetect=...,ebur128=peak=true:framelog=quiet`
- `-filter_complex ebur128=peak=true`

Record the literal `I:`, `LRA:` and `Peak:` lines and the `-version` line as committed fixtures. Assert that the combined chain's Summary equals the standalone Summary to 0.1. That also settles contract §8.4 and open question 2.

The risk is low either way. A finite -70.0 or `-inf` both fail any target window above about -68 LUFS. Only the message and evidence differ.

## 3. Verdict model

**Do not touch `rightkit-review`'s `Verdict` or `disposition` (`verdict.rs:46-62`).** The shared crate has its own result type:
```
CheckStatus { Pass, Fail, NotApplicable, Unverified }
Severity    { Error, Warning }
blocks() = (Fail|Unverified) && severity == Error
```
`Unverified` is emitted exactly where CutRight emits "could not measure" Fails today, always with that check's severity: loudness no summary, clip scan not run, duration unreadable, frame rate unreadable, pixel format absent (Warning), and missing required stream.

- **CutRight release gate unchanged.** The CutRight adapter maps `Unverified` to `Fail` with the same severity. `QaCheck::blocking()` (`report.rs:54-57`) is untouched.
- **Honest improvement, same gate result.** When the decode is unclean, the black, freeze and silence checks currently pass with empty evidence (`run.rs:610-622, 641`). `av.sync` also passes when both comparisons are absent (`run.rs:427-428`). The shared crate emits `Unverified`/Error for these. The CutRight adapter maps them back to `Pass`, so reports are unchanged. `file.integrity` already blocks, so the overall verdict is unchanged. Keeping reports byte-identical avoids bumping `CHECK_VERSION`, which would void every stored pass (`receipt.rs:206-225`).
- **Review adapter (`review` feature), `into_findings()`:**

| Shared | review `Finding` |
|---|---|
| Pass | Verified |
| NotApplicable | NotApplicable |
| Fail, Error | Failed (blocks) |
| Unverified, Error | Unverified, `required = true` (blocks) |
| Unverified, Warning | Unverified, `required = false` |
| Fail, Warning | Unverified, `required = false`, reason prefixed "advisory defect:" |

This is lossy by design. The native report is the record. Review's Warning-less model stays; PostRight is unaffected.

- **Per-check version.** Each result carries `check_version: u32` from a constant table keyed by check id. The crate exposes `SUITE_FINGERPRINT` (hash of the id-to-version table). CutRight bumps its own `CHECK_VERSION` only when that fingerprint changes.
- **Policy hash.** `Policy::hash()` is SHA-256 over canonical JSON (sorted keys, fixed float rendering), using the digest already available through `rightkit-fs` (`digest` feature, already a `rightkit-media` dependency). No new hash dependency. The report carries `policy_hash` and `hash_alg`.
- **The hash is additive.** CutRight keeps its full-spec equality check (`receipt.rs:216-223`) and does not persist the new fields. `QaSpec` has `deny_unknown_fields`, so `exempt_slack_ms` stays out of it and the adapter supplies 100.

## 4. Exemptions: two models and an adapter

Review windows (`subject.rs:103-123`, `rules.rs:350-364`):
- `u64` bounds, mandatory reason, rejected when reversed.
- Hold exempts black and freeze. Silence exempts black only. Static exempts freeze only.
- Black uses point sampling, `start <= t < end`. Freeze uses a strict-end span.
- No slack.

CutRight ranges (`run.rs:82-88`, `probe.rs:191-195`):
- Per check kind, `i64`, unvalidated.
- A whole interval must lie inside one range widened by 100 ms. Edges are inclusive. Partial overlap exempts nothing.

These differ in semantics, not just representation, so one model would change verdicts. Decision:
- The shared crate owns the CutRight containment model: `AllowedRanges { black, freeze, silence: Vec<TimeRange> }`, with `slack_ms` a `Policy` field defaulting to 100.
- `from_declared_windows(&[DeclaredWindow]) -> AllowedRanges` (review feature) maps in these directions:
  - black from Hold and Silence;
  - freeze from Hold and Static;
  - silence from Silence only (review has no silence rule, and a Hold is a picture hold);
  - slack 0, because review has none.
- Shared validation rejects reversed or empty ranges with `InvalidRange`, as review does (`subject.rs:186-192`). CutRight accepted them as given, but every engine call passes empty lists today (`media_cmd.rs:1329-1338, 1855-1862`; `outputs_cmd.rs:316-323`), so nothing observable changes.
- Exemptions applied are echoed in evidence. A verifier reading only the report can then recompute them. That settles contract open question 8 for the shared crate. CutRight's persisted report need not adopt it.

## 5. API sketch

```rust
pub struct Policy { pub container: ContainerPolicy, pub video: Option<VideoPolicy>,
    pub audio: Option<AudioPolicy>, pub captions: Option<CaptionPolicy>, pub slack_ms: i64 }
pub struct Expectations { pub duration_ms: Option<i64>, pub fps: Option<f64>,
    pub captions_srt: Option<String>, pub allowed: AllowedRanges }   // all optional; Default = none
pub enum Pass<T> { Done(T), Skipped(SkipReason) }   // SkipReason: NoStream | DecodeUnclean
pub struct Measurements { pub probe: ProbeFacts, pub decode: DecodeResult,
    pub black: Pass<Vec<Interval>>, pub freeze: Pass<Vec<Interval>>, pub silence: Pass<Vec<Interval>>,
    pub loudness: Pass<Result<Ebur128Summary, Ebur128Error>>, pub clip: Pass<ClipScan>,
    pub ffmpeg_identity: String }
pub struct MeasureOptions { pub run: RunOptions, pub clip_stream: bool }  // timeout and cancel mandatory
pub fn measure(ffmpeg: &dyn FfmpegBinary, ffprobe: &dyn FfmpegBinary, file: &Path,
    policy: &Policy, opts: &MeasureOptions) -> Result<Measurements, MeasureError>;
pub fn judge(policy: &Policy, expect: &Expectations, m: &Measurements) -> Vec<CheckResult>;
pub fn captions_checks(policy: &CaptionPolicy, srt: &str, duration_ms: Option<i64>) -> Vec<CheckResult>; // pure
pub struct CheckResult { pub id: &'static str, pub status: CheckStatus, pub severity: Severity,
    pub message: String, pub evidence: serde_json::Value, pub check_version: u32 }
```

**Error versus None:**
- `Err(MeasureError)` only for whole-run failures, which CutRight also turns into "no report": a tool cannot start, a pass exits non-zero, timeout or cancel, the render is not a regular file. An ffprobe failure is the exception. CutRight writes a one-check `file.integrity` report (`run.rs:594-605`), so the CutRight adapter keeps that mapping.
- `Pass::Skipped` means the pass did not run. `judge` turns this into `Unverified`.
- `Option` fields mean "the tool ran and did not print this" (absent true peak gives NotApplicable "no signal above the gate", as at `run.rs:365`).
- Optional inputs: `captions_srt` is None, so the caption checks follow `required` (NotApplicable or Fail/Error). `allowed` is empty by default. `duration_ms` None falls back to "probed value > 0". `fps` None falls back to "probed > 0".
- Message strings and evidence keys are frozen to CutRight's current text (golden-tested), so the adapter is a type mapping only. Later wording changes bump that check's `check_version`.

**Stays in CutRight:**
- `QaReport`, `QaReceipt`, `MediaFacts`, `RenderIdentity` and `deny_unknown_fields` (`receipt.rs:24-43`).
- BLAKE3 render and caption hashing, and the receipt self-hash.
- `qa-presets.json`, `QaSpec`, `for_timeline_preset`, and `platforms.json` with packaging.
- `verify_receipt`'s current-policy comparison, and the package manifest.
- The `Unverified` to `Fail` and unmeasured-pass legacy mapping.

## 6. Acceptance before CutRight's local checks are removed

1. Golden parity: old `judge` and new `judge` produce the same `(id, status, severity)` for the same measurements. Include message and evidence equality for every case in `run.rs:794-926`, and keep the unit tests in `probe.rs:545-634`. Run the engine and `gate.rs` suites unchanged (contract §9.1).
2. New automated end-to-end cases (the contract admits these have unit coverage only):
   - planted silence of 3 s at 2 to 5 s, failing `audio.silence`;
   - a frozen segment (looped still), failing `video.frozen_frames`;
   - loudness at -30 and at -6, failing `audio.loudness`;
   - a true-peak overshoot.
3. Allowed ranges, each against real renders:
   - a black segment fully inside a range passes;
   - a segment ending 101 ms past a range's end still fails;
   - a segment ending exactly 100 ms past a range's end is exempt;
   - a partial overlap fails with the full length compared;
   - a reversed range returns `InvalidRange`;
   - `from_declared_windows` mapping per kind.
4. Captions: one case per problem in contract §2.2 (start < 0, end ≤ start, end past duration + tolerance, out of order, overlap, blank cue, too many lines, over-long line, zero cues). Add reading-speed boundary (equal passes, greater fails) and tail-gap boundary. Add an unparsable SRT, which emits only `captions.sanity` as Fail. Add no SRT with `required` true and false.
5. Loudness fixtures from the section 2 experiment, committed with the `-version` line. Include missing summary, missing `I:`, and the combined-versus-standalone chain equality.
6. Unmeasured states: decode-unclean render gives `Unverified` shared and legacy-pass mapped. Output truncated by the stderr cap gives `Unverified`, never Pass.
7. Stale invalidation (contract §9.2 says no test exists): a `CHECK_VERSION` change and a preset change both make `verify_receipt` refuse.
8. Exit and timeout: a hung or cancelled pass returns `Err`, not a report.

## 7. Build order (5 lanes)

| Lane | Scope | Verification class | Model |
|---|---|---|---|
| L1 | `rightkit-media`: `ebur128` module, runner stderr tail, streaming-stdout helper | `rightkit cargo test -p rightkit-media`, plus the committed-fixture experiment (needs ffmpeg) | **Stronger (Sonnet):** the experiment's result sets the gate semantics, and the runner change touches shared process code |
| L2 | `rightkit-media-checks`: scaffold, the six passes, the detector parsers, `ClipCounter` port, timeouts | Offline unit tests, then ffmpeg-gated tests that print `SKIP:` | Cheapest (Haiku) is viable: a port of reference code with its tests |
| L3 | `judge`, `Policy`, `AllowedRanges`, captions, versions and policy hash, frozen golden messages | Golden table test against `run.rs` outputs | **Stronger (Sonnet):** a wrong verdict still compiles and could weaken a release gate |
| L4 | `review` adapter, `rightkit-qa` deprecation, READMEs and CHANGELOGs, `rightkit-versions.json` staging, ledger row 91 | `rightkit cargo test` for review and qa, plus `pnpm --dir rightkit test:release` | Cheapest (Haiku) |
| L5 | CutRight migration and parity run, in the CutRight repo | Old-versus-new report comparison on a render corpus, then `gate.rs` and `finishing_e2e` unchanged | **Stronger (Sonnet):** release-gate parity and receipt compatibility |

Sequence: L1, then L2, and L3 in parallel once the `Measurements` types are frozen, with L4 alongside L3, then L5. Publication order is media, then media-checks, then review if the adapter feature is to ship.

Per `docs/rules/rightkit.md`, builds are subject to the 30-minute idle rule and the local-build limits. Verification runs through `rightkit cargo`.

## Contract problems (ambiguous or wrong against the code)

1. **Contract §1 overstates the boundary.** The code uses `cutright_project::worker` spawn functions (`probe.rs:7-9, 371-381, 401-419, 498-531`), not `rightkit-media`'s runner. There is no timeout or cancellation. The clip scan inherits stdin (`probe.rs:502`) while the contract lists `-nostdin`.
2. **`rightkit-media`'s capture cannot run these passes as is.**
   - `run_capture` keeps only the first 4 MiB of stderr (`runner.rs:30, 571`). The ebur128 Summary is at the end, so a long log would silently lose it and look like "no summary".
   - It buffers stdout whole (`runner.rs:505-509`), which is unusable for the f32 clip stream.
   - Lane L1 must fix both.
3. **Contract §2.2, `video.frozen_frames`:** "closed at 0 when the probe has no duration" hides that an open freeze with unknown duration becomes zero-length and passes (`probe.rs:246` uses `duration_ms.max(start)`, `run.rs:608` uses `unwrap_or(0)`).
4. **False passes on unmeasured data** (contract §2.3 states them but does not flag them as defects): `run.rs:610-622, 641` and `run.rs:427-428`. See section 3.
5. **Contract §8.4:** rightkit-qa also differs by using `-filter_complex` with no `-vn` (`loudness.rs:260-261`), not only by chain composition. The experiment covers it.
6. **Contract §8 omits `rightkit-media/loudnorm.rs`,** which has its own silence rule (:86-88) and a ±0.5 LU acceptance (:31).
7. **Contract §2.2, `audio.true_peak`:** with `peak=true` ebur128 prints no sample peak. The ledger's "separate sample and true peak" therefore needs `peak=sample+true`, or the clip scan as the sample-peak source. CutRight uses the clip scan.
8. **Contract §4 versus review:** the ranges are unvalidated in CutRight, but review rejects reversed windows (`subject.rs:186-192`). The adapter needs an explicit rule, which section 4 gives.
9. **Policy hash and `QaSpec`:** `QaSpec` has `deny_unknown_fields` (`spec.rs:28-62`). Putting `slack_ms` or a policy hash in it would break stored specs and receipts (`receipt.rs:216-223`).
10. **Contract §10 open question 1** is closed only by the section 2 experiment. Questions 6 and 7 (multi-stream, channel-count mismatch) are out of scope for this decision, and the shared crate keeps CutRight's first-stream and probed-channel behaviour.

---
Source: Sage adjudication, 2026-10-11, against the CutRight contract `cutright/docs/contracts/measured-media-checks.md`. Third wave; not started. Gap ledger row 91.

## Addendum: GenRight fit check (2026-10-11, accepted by the RightKit lead)

GenRight checked section 5 against `genright-studio/src/pipeline_workflows/media.rs:293-343` and `compose.rs:343-371`. Five adjustments, all accepted; none changes the home or the verdict model. They bind lanes L1 to L3.

1. **ffprobe is optional.** `measure(ffmpeg, ffprobe: Option<&dyn FfmpegBinary>, ..)`. With `None`, `ProbeFacts` is derived from `ffmpeg -i` stderr (GenRight's fallback, media.rs:201-227) and marked as such in evidence; facts that source cannot give are `None`, and checks that need them are `Unverified`. Reason: no catalogued Windows ffprobe today (ledger #27).
2. **Passes follow the policy.** `video: None` runs no black or freeze pass; an `AudioPolicy` enables loudness, silence and clip individually; `clip_stream = false` means no clip pass. A pass that policy did not ask for is absent from `Measurements` (`Pass::NotRequested`), never run and never judged.
3. **`Ebur128Summary` fields:** integrated (`Integrated::{Value, BelowGate}`), `lra_lu: Option<f64>`, `true_peak_dbtp: Option<f64>`, `sample_peak_dbfs: Option<f64>`.
4. **A failed pass does not sink the run when the caller says so.** `MeasureOptions.on_pass_failure: {FailRun (default, CutRight's behaviour), Record}`; with `Record` a pass that exits non-zero is `Pass::Failed(reason)` and the remaining passes run. Likewise `after_unclean_decode: {SkipDependent (default), KeepMeasuring}`. Whole-run `Err` is then only: tool cannot start, timeout, cancel, input is not a regular file. Decode-clean rule, shared: exit 0 and empty stderr under `-v error`.
5. **No stream is per pass, not an error:** audio passes are `Pass::Skipped(NoStream)` on a video-only file and video passes on an audio-only file.

Confirmed fine as sketched: mandatory timeout and cancel, `&dyn FfmpegBinary`, `ffmpeg_identity` in the result, `judge()` optional for a caller that applies its own rule.
