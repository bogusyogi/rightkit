Reviewed `adf73414`—HEAD at start. Subsequent `9fcd7518` leaves reviewed route unchanged; uncommitted edits excluded. No builds, tests or dispatches run.

| Prior finding | Status | Evidence |
|---|---|---|
| 1. Build code receives OIDC | **CLOSED** | Unprivileged jobs & conditional fresh upload job: [publish.yml:270](/Volumes/D/claude/rightkit/.github/workflows/publish.yml:270). |
| 2. Cargo verification receives token | **CLOSED** | Packaging runs separately; token reaches HTTP upload only: [publish.mjs:466](/Volumes/D/claude/rightkit/scripts/ci/publish.mjs:466), [upload.mjs:338](/Volumes/D/claude/rightkit/scripts/ci/upload.mjs:338). |
| 3. Repository secrets bypass environment | **PARTLY** | npm closed by supplied environment configuration; Cargo remains accepted residual risk: [CI.md:132](/Volumes/D/claude/rightkit/docs/CI.md:132). |
| 4. Missing host/test execution passes | **PARTLY** | Explicit exclusions & evidence added; skip detection remains incomplete: [select.mjs:133](/Volumes/D/claude/rightkit/scripts/ci/select.mjs:133), [gate.mjs:146](/Volumes/D/claude/rightkit/scripts/ci/gate.mjs:146). |
| 5. Published npm bytes unverified | **PARTLY** | Hash/manifest checks added; behavioral tests still exercise workspace builds: [publish.mjs:577](/Volumes/D/claude/rightkit/scripts/ci/publish.mjs:577). |
| 6. npm provenance metadata | **CLOSED** | Repository metadata corrected & packed manifests checked in both modes: [upload.mjs:74](/Volumes/D/claude/rightkit/scripts/ci/upload.mjs:74), [i18n/package.json:29](/Volumes/D/claude/rightkit/packages/i18n/package.json:29). |
| 7. Cargo dependency parsing | **CLOSED** | Cargo metadata replaces handwritten TOML parsing: [publish.mjs:119](/Volumes/D/claude/rightkit/scripts/ci/publish.mjs:119). |
| 8. Cargo wildcard semantics | **CLOSED** | Wildcards receive separate bounded ranges: [publish.mjs:61](/Volumes/D/claude/rightkit/scripts/ci/publish.mjs:61). |
| 9. Security contract tests | **PARTLY** | Structural checks, mutations & subprocess interception added; bypasses remain: [workflow-contract.mjs:81](/Volumes/D/claude/rightkit/scripts/ci/workflow-contract.mjs:81). |
| 10. Misleading protection documentation | **PARTLY** | No-reviewer behavior corrected, but secret paragraph still calls both tokens environment secrets & npm migration pending: [CI.md:127](/Volumes/D/claude/rightkit/docs/CI.md:127). Fix to supplied current configuration. |

Remaining/new findings:

11. **P2 — Receipts overstate verification of shipped behavior.** [publish.mjs:577](/Volumes/D/claude/rightkit/scripts/ci/publish.mjs:577) checks npm metadata/file existence; [gate.mjs:226](/Volumes/D/claude/rightkit/scripts/ci/gate.mjs:226) rebuilds & tests workspace code. Broken packed JS can pass. Cargo verification similarly ignores normalized `Cargo.toml` & `Cargo.lock` at [publish.mjs:536](/Volumes/D/claude/rightkit/scripts/ci/publish.mjs:536); only macOS packaging compiles packaged contents. **Fix:** install/extract sealed artifacts into isolated consumer fixtures & test them on both hosts; validate packaged Cargo metadata against upload JSON.

12. **P2 — Required-test skips still become passes.** [gate.mjs:146](/Volumes/D/claude/rightkit/scripts/ci/gate.mjs:146) misses Rust `ignored` summaries, prefixed `[browser] SKIP` output & markers split across stream chunks. Existing examples: [cft_close.rs:67](/Volumes/D/claude/rightkit/crates/rightkit-browser/tests/cft_close.rs:67). Command execution alone satisfies `ranTests`, including zero executed tests. **Fix:** consume structured test results, require positive executed counts & reject unapproved ignored/skipped cases; buffer complete lines for remaining textual reporters.

13. **P2 — Contract accepts edits that expose credentials during dry runs.** [workflow-contract.mjs:89](/Volumes/D/claude/rightkit/scripts/ci/workflow-contract.mjs:89) accepts upload condition with appended `|| true`; uploader then runs without `--dry-run`. Secret scanning at [line 81](/Volumes/D/claude/rightkit/scripts/ci/workflow-contract.mjs:81) misses `${{ secrets['CARGO_REGISTRY_TOKEN'] }}` in pack/verify environments. Both are valid [GitHub expression syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/expressions#operators). **Fix:** require exact upload predicate, detect all secret-context access forms & add these negative mutations. Current workflow contains neither bypass.

14. **P2 — YAML parser accepts different executable text than GitHub.** [workflow-yaml.mjs:63](/Volumes/D/claude/rightkit/scripts/ci/workflow-yaml.mjs:63) ignores explicit indentation indicators & indiscriminately joins folded lines. Replacing upload’s `run` value with this shape passes its exact-command check:

    ```yaml
    run: >1
      node scripts/ci/upload.mjs
      run
    ```

    Parser produces `node scripts/ci/upload.mjs run`; YAML preserves newlines because content is more indented than specified, causing separate commands & immediate usage failure. [YAML folding rules](https://yaml.org/spec/1.2.2/#813-folded-style). **Fix:** use conforming parser or reject folded scalars/indentation indicators until implemented correctly.

15. **P2 — Strict verification lacks required fixtures.** [gate.mjs:162](/Volumes/D/claude/rightkit/scripts/ci/gate.mjs:162) enables `RIGHTKIT_REQUIRE_ASSETS=1`, but workflow provisions neither sibling product checkouts nor Whisper library. Consequently `@rightkit/release` contracts deliberately run against missing paths ([right-suite-contract.test.mjs:35](/Volumes/D/claude/rightkit/packages/release/right-suite-contract.test.mjs:35)); ASR panics without `RIGHTKIT_WHISPER_LIB` ([cpp.rs:668](/Volumes/D/claude/rightkit/crates/rightkit-asr/src/whisper/cpp.rs:668)). **Fix:** provision pinned fixtures/assets before verification or separate consumer qualification into explicit required jobs. These selections cannot finish successfully as configured.

16. **P3 — Registry response handling is incomplete.** [upload.mjs:338](/Volumes/D/claude/rightkit/scripts/ci/upload.mjs:338) omits documented `Accept: application/json`; [line 340](/Volumes/D/claude/rightkit/scripts/ci/upload.mjs:340) treats HTTP 200 containing `errors` as publication success, then waits for index timeout. **Fix:** add Accept header & reject parsed `errors` before recording publication. [Cargo registry API](https://doc.rust-lang.org/cargo/reference/registry-web-api.html).

Registry request otherwise matches Cargo documentation: correct PUT endpoint, raw-token Authorization, octet-stream body, two little-endian byte lengths, JSON then crate bytes. [Metadata builder:415](/Volumes/D/claude/rightkit/scripts/ci/publish.mjs:415) supplies `name`, `vers`, complete dependency fields including rename, features, authors, description, license/readme fields, links & `rust_version`.

Both hosts’ receipts must match manifest hash, commit, unit/version/file/hash & current run ID: [upload.mjs:175](/Volumes/D/claude/rightkit/scripts/ci/upload.mjs:175). Downloads default to current run; no cross-run substitution path found. Manifest run ID & receipt run attempts are not independently enforced.

No first-run flag defect found in documented [pnpm pack options](https://pnpm.io/cli/pack) or [Cargo package options](https://doc.rust-lang.org/cargo/commands/cargo-package.html). Pinned Rust action embeds **1.98.0**, requiring no toolchain input; sparse-checkout inputs are valid, & tar uses relative archive names to avoid Windows drive parsing.

Current dry route cannot reach either publication token or OIDC; standard read-only GitHub token remains used by checkout/setup actions. Dry dispatch is suitable for collecting execution evidence.

DRY RUN: SAFE TO DISPATCH
REAL PUBLISH npm: NOT YET; REAL PUBLISH crates: NOT YET
