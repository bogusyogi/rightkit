# Runtime budgets for long-running shared crates

Added 2026-10-11 after rightkit-fsindex 0.1.0 reached 5.4 GB resident and 65% CPU inside the Pulse hub without any audit noticing (gap ledger #76). Every audit until then was static: declarations, ownership, duplication, adoption. None measured a crate under its real load.

## Rule: three classes

Every crate that reaches `right-release publish cargo` declares exactly one class in `Cargo.toml`. A crate with none is refused.

| Class | Declaration | Meaning | Publish gate | Where the budget is proven |
|---|---|---|---|---|
| **always-on** | `crates/<crate>/runtime-budget.json` (or `continuous = true` / `"always-on"`) | Once linked and started as intended, the crate keeps a thread, poller, listener, watcher or child alive, or holds an index or cache that grows with data scale, for the life of the process, **without the caller holding an explicit resource**. The 11 named owners stay here. | measured, in-ceiling budget at the exact version on every required host (below) | crate publish |
| **held-resource** | `continuous = "held-resource"` plus a `resources` table | Everything long-lived exists only while the caller holds a value the crate hands out (a model engine or session, a connection, a capture stream, a hook registration, a pool, a PTY session, a server handle) and ends when that value is dropped. | complete `resources` table; scanner finds no long-lived code outside it (below). **No measurement.** | **app adoption**: an installed-scope measurement of the app that links the crate |
| **false** | `continuous = false` + `continuous_reason` | Bounded per call. | scanner finds no pattern the reason does not name | n/a |

**Class 2 trades publish-time proof for adoption-time proof.** A held-resource crate's cost is chosen by the app that holds the value: how many engines, how many connections, how large a model. A synthetic two-host crate bench measures a workload nobody ships, and requiring it would freeze every dependent release behind measurements that cannot run on every host today. The honest number is the app's own installed run, so the gate moves the measurement there and keeps a structural publish check (nothing long-lived hides outside the listed resources). An unproven held-resource crate can therefore publish; an app that links it cannot qualify without the measurement.

### always-on crates

A continuous crate must declare and prove a runtime budget before it is published or adopted:

1. **Declare** absolute ceilings for resident memory, steady-state idle CPU, wake-ups per second and disk operations per second, and the `requiredScale` (unit and minimum count) the measurement must reach. A value that is not stated is `null`, and the gate refuses `null`: a budget is never inferred or invented.
2. **Measure** with a repeatable in-repo bench and record the run with `node packages/release/runtime-budgets.mjs record <crate-dir> --result <bench-output> --scope <synthetic|real|installed> --command "<exact command>" --description "<data set>"`. The command detects the host and HEAD, copies the raw output to `runtime-budget.evidence/<os>-<arch>-<version>-<scope>.txt` (commit it), stores its sha256 in the record, and refuses uncommitted code under test. Figures are never typed by hand.
3. **Expose** the figures at runtime through `status()` or an equivalent. The file points at that API (`status.api`, `status.file`: a `pub fn` or `pub struct` item, not a comment) and lists which figures it exposes; it must expose resident memory.
4. **Gate**: `right-release publish cargo` refuses a crate that fails the checks below. `runtimeBudgetAdoptionCheck` additionally needs a fresh `installed`-scope measurement taken from the installed app.
5. **First consumer at scale** (whole disk, whole mailbox, whole repo) runs the crate under that load with the figures visible before the app ships it on; a crate is not "adopted" until that run is recorded as an `installed` measurement naming the app and its version.

### held-resource crates

```toml
[package.metadata.rightkit]
continuous = "held-resource"

[[package.metadata.rightkit.resources]]
api = "Silero::load"          # a pub item path, resolved in `file` with the same resolver as status.api
file = "src/silero.rs"        # under src/
holds = ["memory", "thread"]  # thread | task | listener | child | memory (string or array)
scale = "one ONNX session: model weights, ORT arenas and intra-op threads; fixed per Silero value, independent of audio length"
release = "Drop"              # Drop, or the function that ends it
# static_note = "..."         # required when the file matches resident-static
```

The publish gate refuses a held-resource crate when:
- the `resources` table is missing or empty, an entry lacks `api`, `file`, `holds`, `scale` or `release`, `holds` is not one of the five kinds, `release` is `none`/`never`, or `file` is outside `src/` or absent;
- an `api` does not resolve to a `pub fn`/`pub struct|enum|trait|type` item (not a comment) in the named file;
- the scanner finds a continuous pattern in a file **no entry lists** (new long-lived code cannot hide), or a listed file matches `thread-spawn`, `task-spawn`/`runtime-builder` or `listener` with no entry for that file holding `thread`, `task` or `listener`;
- a listed file matches `resident-static` and no entry for it carries a `static_note` (a process-wide cache, catalog or collection is reviewed, not assumed bounded);
- a file matches `detached-child`: a child that outlives the holder is not behind a held value, so the crate is always-on (or the launch moves behind a held handle);
- the crate is one of the 11 named owners.

A crate cannot move from always-on to held-resource without the table naming every file the scanner flags: that is the "uncovered file" rule. An always-on part inside an otherwise held-resource crate (a global thread pool created at first use, a process-wide cache, a detached child) makes the crate always-on unless that part is itself behind a held value; the scanner flags it and the gate does not accept it silently.

**Adoption (where the proof happens).** `appRuntimeBudgetAdoptionCheck` runs inside `collectPreflight` (`packages/release/preflight.mjs`), which `release.mjs` calls from `resolveReleaseBuildInputs` for `right-release doctor`, `--dry-run` and the pre-build preflight that blocks a build (`preflightFailures`). It reads the app's `Cargo.lock` files (`cargoLockPaths`), finds locked crates listed in the shipped `runtime-budget-classes.json` (generated from the crates' manifests with `node packages/release/runtime-budgets.mjs classes crates`; a test fails when the file drifts) and requires, for each, an entry in `runtime-budget-adoption.json` at the app root: scope `installed`, `app.name` equal to the app, `app.version` equal to the app's current version, `crateVersion` equal to the locked version, raw bench evidence under `runtime-budget.evidence/` (hash and `RESULT budget` line re-checked), every figure within the crate's declared ceilings, and the declared `requiredScale` reached. A held-resource crate with undeclared ceilings or scale blocks every app that links it until they are declared. `runtimeBudgetAdoptionCheck` is the crate-side equivalent (it needs `app`, reads the crate's `runtime-budget.json`, and skips the per-host publish measurement). Today the classes file lists no crate, because no crate has yet been declared held-resource, so the app check is wired but inert. There is no `record` command for the app-side file yet; its records have the same shape and evidence rules as crate records.

**Filled examples** (checked against the real source by tests):
- `rightkit-vad`: the table above. It passes.
- `rightkit-speech-client`: `api = "connect"`, `file = "src/lib.rs"`, `holds = ["thread", "memory"]`, `scale = "one reader thread and one channel per connection; buffered frames grow with unread responses"`, `release = "Drop"`. The table is complete and the reader thread is covered, but the gate **refuses** it: `start_installed_engine` calls `spawn_detached`, a child that outlives any `SpeechClient` by design (the shared per-machine engine). That is a finding, not a gate defect; it needs a ruling (always-on, or the launch moved behind a held handle).

## The artifact: `crates/<crate>/runtime-budget.json`

Schema 1, checked by `packages/release/runtime-budgets.mjs`:

```json
{
  "schema": 1,
  "crate": "rightkit-fsindex",
  "hostPolicy": "darwin-only",
  "hostPolicyReason": "no Windows backend",
  "ceilings": { "residentMemoryBytes": null, "idleCpuPercent": null, "wakeupsPerSecond": null, "diskOpsPerSecond": null },
  "ceilingSources": { "residentMemoryBytes": "path:line where the ceiling is stated" },
  "requiredScale": { "unit": "entries", "minCount": 1000000, "source": "path:line" },
  "status": { "api": "Index::status()", "file": "src/index.rs", "exposes": ["residentMemoryBytes", "idleCpuPercent"] },
  "measurements": [{
    "host": { "os": "darwin", "arch": "arm64", "model": "Mac mini M2" },
    "crateVersion": "0.2.0", "commit": "<40-hex>", "date": "2026-10-11",
    "scope": "synthetic",
    "dataScale": { "description": "generated tree", "unit": "entries", "count": 1000000 },
    "command": "rightkit cargo bench -p rightkit-fsindex --bench budget -- mem 1000000",
    "observed": { "residentMemoryBytes": 1, "idleCpuPercent": 0, "wakeupsPerSecond": 0, "diskOpsPerSecond": 0 },
    "evidence": { "file": "runtime-budget.evidence/darwin-arm64-0.2.0-synthetic.txt", "sha256": "<64-hex>" }
  }]
}
```

`scope` is `synthetic`, `real` or `installed`; `installed` also needs `app: { "name", "version" }`.

**Bench output the measuring lane must print** (exactly one such line; `record` and the gate both parse it; other `RESULT` lines are ignored):

```
RESULT budget crate=<name> version=<semver> resident_bytes=<int> idle_cpu_percent=<n> wakeups_per_s=<n> disk_ops_per_s=<n> scale_unit=<word> scale_count=<int>
```

All ten fields are required and `scale_unit` must equal `requiredScale.unit`. The existing benches print other `RESULT` tags (`rss_footprint_mb`, `cpu_percent`) and none prints wake-ups or disk operations per second yet; each bench needs this line added.

The publish gate refuses when any of these holds:

| Refusal | Why |
|---|---|
| `Cargo.toml` `[package].name` differs from `--crate` | a renamed crate must not borrow another crate's declaration |
| no `runtime-budget.json` and no `continuous = false` + reason | an unknown crate is not assumed quiet |
| `continuous` is not `false`, `true`, `"always-on"` or `"held-resource"` | the class must be one of the three |
| held-resource: any refusal listed above | structural proof replaces measurement at publish |
| `continuous = false` on a listed owner, or beside a `runtime-budget.json`, or without a reason | a reviewed decision, one way or the other |
| `continuous = false` and non-test `src` matches a pattern in `CONTINUOUS_PATTERNS` (`thread-spawn`, `task-spawn`, `runtime-builder`, `sleep`, `timer`, `listener`, `watcher`, `event-loop`, `audio-stream`, `onnx-session`, `ggml`, `conn-pool`, `resident-static`, `detached-child`) and `continuous_reason` does not contain that id as a whole token | a false positive is resolved by a reason that names the pattern |
| `runtime-budget.json` missing, invalid, wrong schema, or for another crate | no artifact, no budget |
| any ceiling `null`, zero or non-numeric; `requiredScale` `null` or malformed | a budget without a ceiling or a scale is not a budget |
| status pointer missing, file absent or outside the crate, no `pub fn`/`pub struct` of that name outside comments, or `residentMemoryBytes` not exposed | the runtime must show the figure the budget names |
| no measurements, or none whose `crateVersion` equals the version being published | a ceiling is not evidence; 0.1.x says nothing about 0.2.0 |
| a measurement without evidence, with the evidence file missing, outside `runtime-budget.evidence/`, with a different sha256, or whose `RESULT budget` line differs from the record (crate, version, figures, scale) | figures must come from stored raw output |
| `residentMemoryBytes` observed as exactly 0 | implausible for a running process |
| an observed value above its ceiling in any measurement of the published version | the crate is over budget; a failing run is not hidden by a passing one |
| a required host has no measurement that is fresh, within all ceilings and at `requiredScale` | see hosts and freshness below |
| `hostPolicy` in the file disagrees with the release evidence `hostPolicy` (the authority) | one source of truth for darwin-only |
| README `## Runtime budget` missing or different from what the JSON generates | the human view cannot disagree with the artifact |

**Required hosts** are macOS (`darwin`) and Windows (`win32`): apps ship on both and the publication gate requires host runs on both. A crate is macOS-only only when the release evidence for that publish records `hostPolicy: "darwin-only"` with a non-empty `hostPolicyReason` (`isDarwinOnlyPolicy` in `publication-gate.mjs` is the one definition); the budget file may repeat it and is refused if it differs.

**Freshness.** A measurement at commit C is fresh only when all of these hold at publish time: C is a commit in this checkout and an ancestor of HEAD (a shallow clone must be deep enough to contain C; a detached HEAD is fine); `git diff C` is empty (committed and uncommitted) for the crate's `src`, `benches`, `Cargo.toml` (features) and `build.rs`, and for `src`, `Cargo.toml` and `build.rs` of every workspace crate it reaches through `path` dependencies, transitively (parsed from the manifests, including `[workspace.dependencies]` and target tables; dev-dependencies count for the crate itself only); no untracked file exists under those paths; and the workspace `Cargo.lock` closure of the crate is identical at C and now. The closure starts at the crate's `[[package]]` entry and follows every `dependencies` edge, so a change to a package the crate transitively uses (version, source, checksum or edges) is stale and a change to an unrelated package is not. Any git or parse failure refuses.

**Not covered.** Workspace `[profile.*]`, `[patch]`, the toolchain, and the host OS version are not part of freshness; re-measure after changing them. Evidence is checked for hash and content consistency and for being committed raw output; the gate does not run the bench and cannot prove it ran. The continuous-work scan is textual: it finds the listed patterns in non-test `src` (comments and `#[cfg(test)]` modules removed) and says nothing about crates that do continuous work through other means (for example callbacks into a framework).

**README view.** Generate the section instead of writing it: `node packages/release/runtime-budgets.mjs sync crates/<crate>`. The gate compares the section line by line with what the JSON renders.

## Required scale per crate

`requiredScale` is the workload the crate's own bench documents as its default. The gate requires a measurement of that unit at or above that count on each required host; adoption additionally needs the first-consumer run below.

| Crate | Publish scale (bench default, cited in the file) | First consumer for adoption (`installed` scope) |
|---|---|---|
| rightkit-fsindex | 1,000,000 entries (`benches/budget.rs` mem run) | whole disk in the installed app |
| rightkit-watch | 50,000 files (`benches/budget.rs`) | the largest watched repository in the installed app |
| rightkit-sync | 100,000 records (`benches/budget.rs`) | the largest real zone in the installed app |
| rightkit-daemon | 10,000 signed submissions (`benches/budget.rs`) | the installed host's real job volume |
| rightkit-jobs, -service, -cache, -logs, -shell, -qa, -send | not declared: no bench or doc states a scale; `requiredScale` is `null` and refused until declared with a source | installed app under its own production load |

## Declared state (2026-10-11)

The 11 files exist with ceilings and (where no bench states it) `requiredScale` all `null`, and no measurements, because no absolute ceiling is stated in any README, source, bench or doc. The gate refuses all 11 until each is declared and measured on every required host. `rightkit-fsindex` declares `darwin-only` because it has no Windows backend (its README, Known gaps); the release evidence for it must say the same.

Nine other crates declare `continuous = false` with a reason and pass the scan: `rightkit-align`, `-channel-memory`, `-compose`, `-contracts`, `-framed-sidecar`, `-license`, `-pdf`, `-review`, `-transcript`. The remaining workspace crates are undeclared and are refused at publish until someone reads them and declares them.

## Crates in this class today

rightkit-fsindex, rightkit-watch, rightkit-jobs (supervisor), rightkit-service, rightkit-daemon, rightkit-cache, rightkit-sync, rightkit-logs (tracing worker), rightkit-shell (watchdog, idle), rightkit-qa (harness), rightkit-send.

## Why the audits missed it

The 2026-10-10 audits checked what each crate declares and who owns what. They did not run anything. A crate can pass every static check and still be unusable at scale. This rule adds the measurement the audits lacked; the release gate makes it impossible to publish without it, and the artifact is machine-checked rather than read from prose.
