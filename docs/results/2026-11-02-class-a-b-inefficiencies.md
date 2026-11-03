# Class A and Class B: every inefficiency measured, and the floor

rkn, 2026-11-02. Revised the same day with corrections from Fable (Dell receipts) and RustRight. Measured on the Mac: HeardRight through broker `e1de426` with the trusted gate 0.6.24, plus one RightKit unit run. Dell figures come from Fable and HRwin and are labelled as theirs.

**Evidence:**
- Logs: `/Volumes/D/scratch/rk-timing/` (A1-baseline, A-jobs1/4/8/16, B1-baseline) and `/Volumes/D/scratch/rk-assets/1-ffmpeg*.log`.
- Broker receipt breakdowns come from `/Volumes/D/scratch/rk-timing/receipts.py`.

## Measured

### Class A, HeardRight (296 checks, `--to check`)

| Lanes (`RK_GATE_JOBS`) | Wall | Sum of scan time | Result |
|---|---|---|---|
| 1 | 186 s | 124 s | pass |
| 4 (default) | 200 s, then 154 s | 442 s, then 416 s | pass |
| 8 | 152 s | 706 s | pass |
| 16 | — | — | **fail**: race, item A7 |

### Class B, HeardRight 9c1b15fa (`--to compile`, 70 class B scans, 92 broker jobs)

| | Seconds |
|---|---|
| Wall time, including about 150 s of Class A | **3,188** |
| Inside the broker: cargo | 1,320 |
| Inside the broker: START bind | 609 |
| Inside the broker: after-exit bind | 790 |
| Inside the broker: queue | 327 |
| Inside the broker: diagnostics | 41 |
| Inside the broker: total | 2,398 |
| Gate-side gaps outside the broker | about 640 |
| Median cargo time per job | 13.4 |
| Longest cargo time per job | 85 |

Class B scan time by kind: rustdoc 844 s, clippy 798 s, tests 711 s, feature checks 702 s, Swift builds 302 s.

**Dell (Fable, Oracle from receipts, ecceaf04-2):** **59.0 minutes** in total: Class A 293 s, pre-B compile 210 s, Class B 2,250 s, QA 750 s. The earlier figure of 78 minutes was wrong.
- 67 jobs ran one after another, about 33 s each, while cargo's median was 11 s and 63 of the 67 had 0 dirty units.
- Per-job medians (23.3 s in all):
  - admission hash 3.07 s;
  - START bind 5.70 s;
  - compiler 1.15 s;
  - after-exit bind 7.43 s;
  - cache finish 2.70 s;
  - final re-digest 2.85 s;
  - then a 10.6 s gate gap.
- The receipt's `queueMs` is admission hash plus START bind, not scheduler queue; the scheduler queue is 2 ms. Model: 67 × (23.3 + 10.6) s = 2,271 s against 2,250 s measured.
- Compile prep (metadata, audit, fetch, deny) runs again for each carried right-release call, 4 times in all.

### Class A, RightKit unit (`--to compile --unit cargo:rightkit-ffmpeg`, d6331c9a5)
- 1,636 scans were planned over 14,349 files.
- Class A alone ran 1,527 checks for **34 minutes**.
- `actionlint` ran 1,655 s against its 5-minute limit and failed Class A. Nothing compiled.
- On the rerun the same check passed in 1 s.

## Every inefficiency found

| # | Inefficiency | Evidence | Cost measured | Fix | Owner |
|---|---|---|---|---|---|
| A1 | **Class A is never reused**, by design | `rightkit-gate.mjs:61` "class A scans are never cached" | the full 150-200 s on every run, even with nothing changed | key every scan on its input digest (GC-T14 already requires this) | gate |
| A2 | **Broker-carried cargo reads are excluded from the result cache** | `deterministic-scans.mjs:2513-2519` (`viaSnapshotBroker`, `carriedRead`) | 14 cargo calls re-run on every run | cache them by input digest | gate |
| A3 | **Each read-only cargo call pays snapshot hashing twice** | receipts: bind 1.5-4 s plus after-exit 2.5-6 s around 0.3-3 s of cargo | about 66 s per HR Class A | no per-call binds for read-only calls; the stage lease and the gate's verification at open and close cover them (RustRight agrees) | broker |
| A4 | **The cargo reads run as serial chains, with gate-side gaps** | receipts: 14 jobs, submits 7-40 s apart, queue 0 | about 60 s | run independent chains in parallel and remove the gaps | gate |
| A5 | **More lanes don't help: scans contend** | 1 lane 186 s vs 8 lanes 152 s; the scan sum rises from 124 s to 706 s | the lanes give nothing back | remove the work (A1 to A4) rather than adding lanes | gate |
| A6 | **About 39 s of prep before the first check** | A1-baseline: start to first broker submit | 39 s | profile it: ownership, snapshot verification of 3,869 files, planning | gate |
| A7 | **Race: the client returns before the job starts, and the gate judges a single status read** | A-jobs16.log:310-311; receipt `48862630` SUCCEEDED | one false-red run | wait for the terminal state (WATCH); record the client's exit code and signal | gate |
| A8 | **A single-crate unit's Class A costs 34 min** | ffmpeg run: the 1,212 node-check and 172 changelog scans cost about 0 s. The cost was cargo scans over the 19-crate dependency closure (metadata 1,890 s, deny 874 s, audit 303 s, package-list 265 s, summed) plus the actionlint hang. The closure is real. GC-A03 and GC-I03 keep file-only scans repository-wide. | 34 minutes | reuse (A1, A2) and no per-call binds (A3); scoping changes nothing | gate |
| A9 | **The scan clock let actionlint run 1,655 s against a 5-minute limit** | 1-ffmpeg.log [1525/1527] | 27 minutes, then a red Class A | enforce wall-clock limits and kill at the limit; retry a hung tool once | gate |
| A10 | **Class A runs again inside every stage** (`--to compile` repeats the check) | B1 log: about 150 s of Class A inside the compile | about 150 s per stage | reuse the Class A verdict for the same commit and inputs (GC-T14, A1) | gate |
| B1 | **Too many jobs: one broker job per crate per verb** (92 on the Mac, 67 on the Dell), all in one Cargo workspace | job count × (23.3 s broker + 10.6 s gate gap) reproduces the measured time | about 50 minutes | **batch**: one job per workspace per verb (check, clippy, doc, test compile), about 5 jobs. Broker parallelism gains nothing inside one workspace (cargo's target lock and one build per repository, `service.rs:1386`). Corrected by Fable and RustRight. | gate |
| B2 | **Snapshot binds on every compile job** | 609 + 790 s = 1,399 s, 106% of cargo's 1,320 s | about 23 minutes | bind once per stage under the lease | broker |
| B3 | ~~Queue wait 327 s~~ **withdrawn**: `queueMs` is admission hashing plus START bind, already counted in B2 (Fable) | — | — | — | — |
| B4 | **Gate gaps between jobs** | about 640 s; Dell 13.6 s per job | about 11 minutes | submit the next job immediately; pipeline the submissions | gate |
| B5 | **No reuse of broker compile results** | `deterministic-scans.mjs:2517`; Dell: 63 of 67 jobs had 0 dirty units | the whole run, on an unchanged tree | cache by input digest plus the receipt (GC-T14) | gate |
| B6 | **Tests execute inside the broker**, against GC-A08 | the installed gate and broker | test time is held in broker slots | GC-A08: compile-only in the broker; the gate runs the test binaries | gate + broker (written, not live) |
| B7 | **Compile prep repeats for each carried right-release call** | Dell: 4 times | minutes | prep once per stage | gate |
| B8 | **Cold-history refusals** (D1, `RR_UNEXPLAINED_COLD`): **blocks every HR package** (the Dell 13:20 run on 1abc7a02 failed here again) | 1abc7a02: 39 refusals | one full run per lockfile change | broker: RustRight local commits `658eef0` (full) and `46fd1b4` (interim); **gate:** `snapshot-request.mjs:641-649` (RA-03) also refuses cause-unexplained and must change | broker + gate, first |

## Floor (calculated from the receipts; not measured)

| Step | Class A (HR) | Class B (HR) | Total |
|---|---|---|---|
| Today | 150-200 s | about 50 min | **about 55 min** |
| A3 + B2: per-call binds removed | about 130 s | about 27 min | about 30 min |
| A4 + B3 + B4: gaps and queue removed | about 70 s | about 22 min (cargo alone, serial) | about 23 min |
| B1: batch to about 5 jobs per workspace | about 70 s | about 3-5 min | **about 5-6 min** on a fresh commit |
| A1 + A2 + A10 + B5: results reused by input digest | a few seconds | seconds on an unchanged tree; minutes for a one-crate change | **under 1 min** to rerun the same tree |
| A8: unit scope for RightKit | 34 min falls to the unit's closure | — | — |

## Order of fixes, largest win first

0. The cold judge (B8), broker plus gate RA-03, because it blocks every HR package.
1. Result reuse by input digest across Class A and Class B (A1, A2, A10, B5). This is GC-T14, already in the contract and not implemented.
2. Snapshot binds (A3, B2):
   - RustRight's local fast path, `ad027cc`, re-hashes only stat-changed files; measure it first.
   - Once per stage under the lease: RustRight says the broker has no stage boundary today, and the exit bind catches a build script that rewrites tracked files. Open.
3. Batching class B jobs per workspace per verb, in the gate (B1).
4. Gate-side gaps and serial chains (A4, B4, B7).
5. Unit scope for RightKit Class A (A8).
6. Robustness: the A7 race and A9 limit enforcement, so no run goes false-red.
7. The broker cold-history fix B8, Adrian's decision.

Status, 2026-11-03:
- Done:
  - Class A cargo reads run directly in the gate, not through the broker (GC-A11). This removes A2, A3 and A8: Class A for rightkit-asr, 1,212 checks, measured 58 s; ffmpeg took 34 min before.
  - A7, the race (0.6.25).
  - B6, tests outside the broker (GC-A08).
  - B8, the cold refusal, removed by RustRight in b00220f.
- In @rightkit/release 0.6.31: A9. Scan spawns are always asynchronous, so a limit always fires; a timed-out Class A tool is retried once. The 27-min actionlint "hang" was the gate's event loop held by a synchronous broker call: shellcheck and actionlint both report 1,655 s.
- Next, 0.6.32: A1, A2, A10 and B5, result reuse by input digest (GC-T14).
- With RustRight:
  - B2: bind once per lease; ad027cc is installed but not yet measured.
  - B1: batched carried requests are accepted by the broker; the gate side is open.
- Open: A4, A6, B4 and B7.
