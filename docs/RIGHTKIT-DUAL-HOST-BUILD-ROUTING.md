# RightKit Dual-Host Rust Routing

**Status:** implemented & Pi validated
**Hosts:** macOS workstation + Windows i9/64 GiB workstation
**Transport:** existing passwordless `ssh win` bridge
**Authority:** extends `RIGHTKIT-THIN-BUILD-CONTROL.md`; each host broker remains sole local Cargo executor.

## 1. Outcome

Use both workstations as one bounded Rust execution pool without sharing target directories,
mutating interactive checkouts, or depending on Hetzner.

- macOS stays source-authoritative during normal work.
- Windows runs Windows-compatible `cargo check`, `cargo clippy`, `cargo test`, & explicit
  Windows builds from exact Mac worktree snapshots.
- macOS runs macOS-native Tauri builds, signing, notarization, & local Tauri development.
- Windows runs Windows-native Tauri builds, installers, & Windows-only tests.
- Every Cargo process enters its host-local RightKit broker. SSH never launches Cargo directly.

Hetzner adds no useful native-platform coverage here. It remains unrelated server capacity,
not part of desktop Rust routing.

## 2. Workload cost

Cargo commands are not equally strenuous.

| Work | Typical pressure | Routing |
|---|---|---|
| `check` | compiler CPU + RAM + target writes; no final link | Windows-preferred when Windows-compatible |
| `clippy` | `check` plus lint analysis; often slightly slower | Windows-preferred when Windows-compatible |
| `test` | compile + link + test execution; workload-dependent RAM/CPU/I/O | Windows-preferred for portable/Windows tests |
| `build` | compile + final link; release/LTO may be heaviest | target-native host |
| `tauri dev` | frontend dev server + Rust compile + resident GUI | local native host |
| Tauri release | frontend build + Rust release link + bundle/sign | target-native host only |

GPU/VRAM does not materially accelerate ordinary Cargo work. Windows value is CPU, RAM,
native Windows coverage, separate thermal envelope, & parallel host availability.

## 3. Source authority & dirty worktrees

Remote validation never pulls, rebases, stashes, resets, or edits Windows interactive checkout.
Generated `AGENTS.md` dirt therefore needs no special case.

Mac coordinator builds content-addressed snapshot without changing current index or worktree:

1. Create temporary Git index from `HEAD`.
2. Add tracked changes, deletions, & non-ignored untracked files into temporary index.
3. Write synthetic tree + commit objects.
4. Archive synthetic commit.
5. Hash archive & transfer it to managed Windows input storage.

Windows verifies hash, rejects unsafe archive paths, & installs snapshot into managed replica.
No source result is pulled back because validation commands are source-read-only. stdout,
stderr, exit status, request ID, snapshot digest, & broker receipt are returned.

Source-changing commands (`clippy --fix`, `cargo fix`, format writes, dependency updates) remain
local. Any future remote mutation protocol must return explicit patch, never sync checkout state.

Before extraction, worker enumerates every member and rejects absolute, drive-qualified,
parent-traversal, alternate-stream, reserved-Windows, symlink, hardlink, junction/reparse, device,
FIFO, or other non-regular/non-directory entries. It also rejects names equal under Windows
case-folding or trailing-dot/space normalization, plus Git submodule entries until recursive
snapshot support exists. Extraction targets new empty directory on same volume; atomic rename
installs complete tree, so failed validation/extraction cannot change active slot source.

## 4. Topology

```text
Mac agent
  |
  | rightkit remote windows cargo <check|clippy|test|build>
  v
Mac coordinator
  |-- synthetic worktree snapshot
  |-- SHA-256 + typed request
  |-- scp archive
  `-- ssh win <RightKit remote worker submit payload>
                         |
                         v
Windows managed replica pool (default: 3 stable slots/repository)
  |-- durable slot lease
  |-- hash verification + safe extraction
  |-- atomic source replacement
  `-- rightkit cargo ...
              |
              v
Windows local RightKit broker
  |-- global queue
  |-- workspace exclusion
  |-- aggregate compiler budget
  |-- Job Object containment
  `-- logs + terminal receipt
```

Remote layer coordinates hosts; it is not distributed Cargo. Broker protocol stays local-only.

SSH helper performs no long-running work after submission. It verifies + installs snapshot,
atomically writes remote request intent, submits to already-persistent Windows RightKit broker
service, then atomically records broker request ID before acknowledging SUBMIT. Cargo process is
broker child inside Windows Job Object, never child of SSH console; closing SSH cannot cancel it.

Replica ownership is durable record, not process-held lock. Later submission reclaims slot only
after one broker STATUS proves prior job terminal, or accepted job is classified LOST. SSH helper
may exit while source stays immutable for broker-owned job.

### Agent lifecycle: submit, schedule, stop

Remote submission is detached. Coordinator returns durable remote request ID after Windows broker
accepts job; it does not attach to output or wait for completion.

Agent must then:

1. persist request ID in task state;
2. create one scheduled wake-up for 15 minutes later;
3. stop current run completely;
4. on wake, issue one STATUS request;
5. if terminal, read receipt/output once and continue; otherwise schedule next 15-minute wake and stop.

Agents must never poll Cargo status, run sleep loops, hold SSH connections, repeatedly attach, or
leave background status watchers. A human may request immediate status explicitly; this causes one
read, not polling. Scheduling belongs to agent host/runtime; RightKit exposes durable submit/status/
cancel operations and never fakes wake-up with a resident process.

Every accepted request carries absolute remote deadline, default four hours. STATUS returns one
of `QUEUED`, `RUNNING`, `TERMINAL`, or `LOST`. Broker `RK_JOB_NOT_FOUND` after acknowledged SUBMIT
becomes terminal `REMOTE_JOB_LOST`; deadline expiry requests cancellation then becomes terminal
`REMOTE_JOB_DEADLINE`. Agent never reschedules terminal, LOST, or deadline state.

## 5. Capacity model

Queue depth is unlimited within existing broker durability bounds. Active compilation is always
bounded. Unlimited simultaneous Cargo roots would overcommit CPU/RAM & make all jobs slower.

Each host exposes aggregate compiler-job budget derived from logical CPU + memory. Each admitted
root consumes command-class budget:

- analysis (`check`, `clippy`): about one third of host budget;
- test: about one half;
- build/release/run: up to sixteen jobs or full host budget, whichever is lower.

Windows i9/64 GiB therefore supports approximately three analysis roots, two test roots, or one
heavy build at once, including useful mixtures when total budget fits. Existing BUILD root cap
remains secondary safety bound. macOS 8 GiB normally admits one compile-heavy root, while Windows
continues working independently.

Stable replica slots let same repository use several Windows roots safely. Each slot has distinct
source path + RightKit target generation; shared Windows sccache retains compiler reuse. Repeated
jobs reuse slots instead of creating unbounded worktrees.

## 6. Routing policy

Default route:

1. Explicit `remote windows` always means Windows.
2. Windows-only target, test, Tauri build, or installer means Windows.
3. macOS-only target, Tauri build, signing, or notarization means macOS.
4. Portable `check`, `clippy`, & `test` prefer Windows when reachable; local fallback is explicit,
   not silent, so platform coverage remains truthful.
5. Same snapshot + same normalized command may be coalesced later; unrelated jobs queue normally.

`tauri dev` remains local because it is interactive, owns local windows/ports, watches source, &
becomes resident after compilation. RightKit already releases compile/workspace permits after
Cargo reports executable start; resident app must not consume compile capacity indefinitely.

## 7. Failure contract

Typed failures distinguish:

- Windows unreachable;
- archive transfer failure;
- digest mismatch;
- unsafe archive member;
- no replica slot before deadline;
- remote RightKit runtime unavailable;
- remote broker rejection;
- acknowledged job missing from broker (`REMOTE_JOB_LOST`);
- four-hour remote deadline (`REMOTE_JOB_DEADLINE`);
- Cargo/test failure.

SUBMIT success is not build success. It means request ID is durable and scheduled wake may safely
replace attached waiting. STATUS is idempotent and terminal receipt remains readable after agent
stops.

Write-before-ack order:

1. atomically persist `PREPARING` remote request + selected slot;
2. install verified snapshot;
3. submit to persistent broker;
4. atomically persist broker ID + `QUEUED` + deadline;
5. acknowledge remote request ID.

Crash before step 5 produces no acknowledged ID. Recovery marks stale `PREPARING` request failed;
broker receipts remain source of truth for rare submit/mapping crash interval.

Coordinator never silently retries locally because local success cannot replace requested Windows
coverage. Interrupted transfer leaves content-addressed input safe to reuse or garbage-collect.
Interrupted replica update never replaces active source. Slot lease has owner metadata + bounded
recovery from broker STATUS and request deadline.

## 8. Windows linker watchdog

Reported CodeRight failure was healthy Windows release linking killed by outer release watchdog:
CodeRight's Windows NSIS wrapper redirected Tauri/Cargo output to `.cache/build-windows-nsis.log`,
Windows descendant CPU was unavailable, & ten silent minutes looked stalled. No compile/test error
occurred.

CodeRight now declares wrapper's 45-minute ownership timeout in release config & passes same value
to wrapper. RightKit watches `.cache` plus output artifacts & floors outer inactivity limit at inner
owner timeout plus five minutes: 50 minutes by default. An operator may lengthen outer limit through
`RIGHT_RELEASE_STALL_MS`, but cannot shorten it below declared ownership contract. Unavailable CPU
remains unknown rather than evidence of idleness.

## 9. Security & invariants

- SSH payload is base64url JSON with strict schema; no user string becomes shell syntax.
- Remote worker allows only registered Cargo validation/build verbs & rejects source-mutating flags.
- Archive digest is verified before extraction; links, special entries, submodules, traversal,
  reserved Windows paths, & NTFS-normalized collisions are rejected.
- Replica roots live under `D:\.rightkit-managed\remote-workspaces`.
- Interactive Windows checkout under `D:\Claude` is read only for worker/runtime discovery.
- Host broker remains sole process owner, scheduler, cache owner, & receipt producer.
- Native target build/sign rules remain unchanged.

## 10. Initial deliverable

- `rightkit remote windows cargo <check|clippy|test|build> [args...]` coordinator;
- zero-config workspace transport defaults to SSH alias `win`, managed nvm4w Node, canonical
  `D:\Claude` worker, & managed remote-input root; `RIGHTKIT_REMOTE_WINDOWS_*` overrides remain for
  host migration;
- detached SUBMIT plus STATUS/CANCEL by remote request ID; no watch/poll path for agents;
- Windows snapshot worker with three stable replica slots per repository;
- strict payload/archive/command validation;
- broker-child survival after SSH helper exit, durable write-before-ack mapping, typed LOST/deadline,
  & single-shot STATUS tests;
- symlink/link/device/submodule/NTFS case-collision archive rejection tests;
- aggregate compiler budget in host scheduler + derived Windows sizing;
- focused tests for snapshot fidelity, dirty-tree isolation, payload rejection, replica exclusion,
  command policy, & compiler-budget admission;
- README + canonical build-control cross-reference.
