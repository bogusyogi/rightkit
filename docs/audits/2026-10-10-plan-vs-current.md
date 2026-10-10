# RightKit: plan vs current state (2026-10-10)

Basis: bogusyogi/rightkit main at `08252a68`, bogusyogi/claude main at `f18d3d1aa`, the plan corpus below, `git log --since=2026-10-08` (412 commits), `right-release ownership trust-compare`, and `generate-index.mjs --check`. Read-only audit; nothing in RightKit was changed to produce it.

## 1. The plan corpus (what RightKit was meant to be)

| Document | Role | Dated |
|---|---|---|
| `docs/plans/2026-07-13-rightkit-shared-sdk-architecture.md` (workspace) | Original architecture: layers, principles, the module list | 2026-07-13, Swift section superseded 07-15 |
| `docs/plans/2026-08-15-rightkit-architecture.md` (workspace) | "Locked Thin Path" build and release architecture | 2026-08-15, LOCKED |
| `tasks/reviews/2026-10-08-rightkit-converged-plan.md` | Review plan P0–P4: ownership catalog, broker admission, publication binding, enforcement, cleanup | 2026-10-08, rev 4 |
| `rightkit/docs/plans/2026-10-08-content-pipeline-execution-plan.md` | RK0–RK10 content-pipeline crates | 2026-10-08 |
| `rightkit/docs/audits/2026-10-08-inventory-final.md` | Inventory actions, waves 0–4 | 2026-10-09 |
| `docs/plans/2026-10-05-rightkit-build-capacity-and-dev-parity.md` | Build capacity steps 1–5 | 2026-10-05 |
| `docs/RIGHTKIT-SHARED-PACKAGES.md`, `rightkit/docs/CAPABILITY-MAP.md`, `rightkit/docs/OWNERSHIP-ENFORCEMENT.md` | Contracts and enforcement reference | mixed |
| `rightkit/STATUS.md` | The one "where is RightKit" file | 2026-10-08 (stale) |

### The 07-13 shape

- Principles: no hosted CI; dependencies from native registries, never a sibling checkout or Git dependency; registries hold no secrets or models; one release pipeline with patch/update entitlement lanes; one updater; Rust is the security boundary for paid features; QA of the installed app is shared.
- Layers: canonical source → R2 artifact store (public signed, private gated) → each app = product UI + thin Tauri shell + engine (linked or sidecar) + `right-release.config.mjs` / `right-qa.config.mjs`.
- Planned modules (17): `@rightkit/{release,license,logs,updates,tauri,platform-ui,qa}`, `rightkit-{license,logs,tauri,process,framed-sidecar,ort,ocr,media,asr}`, `RightKitSwift`. Plus `ios-app-template`, `rightkit model promote`, `right-release publish cargo`, `right-release suite-doctor`.

### The 08-15 build architecture

Native tools only (Cargo, sccache, OS process groups, rustup, Tauri CLI); one heavy-build permit; one target dir per workspace+checkout+toolchain; never move `CARGO_HOME`; RightRelease is a sequencer with resumable checkpoints (`compiled → bundled → signed → attested → final`). Its own status section left three decisions open for Adrian: enable sccache, 2 builds vs 1 permit, `CARGO_BUILD_BUILD_DIR`.

## 2. What RightKit is now

- **56 crates, 24 npm packages, RightKitSwift.** 55 crates listed in `INDEX.md` plus `rightkit-fsindex` (uncommitted work). Against the 17-module plan, every original module exists and is published except `RightKitSwift` registry distribution (deliberately a git tag) and `ios-app-template` (not found).
- **Build broker is RustRight** (`bogusyogi/rustright`, Rust), live on Mac and Windows since 2026-10-09. The Node control plane (`build-control-{core,broker,clients,staging,phase0}`) was deleted (`8d886e91`). `packages/build-control-policy` (agent hooks) stays Node.
- **Ownership enforcement is advise-and-audit.** Catalog `a7121f96…`, protected policy bundle `4fa03baa…`, Mac trust enrolled (key checkpoint seq 1, root fingerprint `932f0049…`). Only `browser-automation` has block-tier fingerprints; 32 capabilities advise, 36 are declaration-only. Protected trust matches the running policy on the Mac; Dell not compared from here.
- **Release tooling**: `@rightkit/release` 0.2.147 live (recorded in the feed today, `08252a68`). QA receipts warn and record (`aea09d39`); `right-release qa audit` exists; weekly audit scheduled by Adrian via Codex. SwiftPM project type, MSIX target and App Installer updater were added 2026-10-09 (0.2.139–0.2.143).
- **Repo and path**: RightKit is its own repository (`bogusyogi/rightkit`, 2026-10-09) at `/Volumes/D/claude/rightkit` and `D:\Claude\rightkit` (moved from `tools/rightkit` on 2026-10-10).
- **Publishing**: every publish goes from the Mac clean clone `/Volumes/D/rightkit-publish/claude/rightkit` with darwin-only host policy (Adrian 2026-10-07).

## 3. What changed from the plans, and why

| Planned | Now | Why (source) |
|---|---|---|
| 17 modules (07-13) | 56 crates + 24 packages | Each app audit extracted a shared capability (ocr, pdf, mcp, browser, control, qa, db, http, …); the content pipeline added RK0–RK10. Consistent with the principle "RightKit owns shared capabilities", not with the original scope. |
| No hosted CI (07-13 §principles) | Public releases run on RightKit CI (GitHub Actions via `@rightkit/git` templates, hosted runners); 0.2.147 itself is a hosted-runner fix | Workspace rule: "public releases use RightKit CI; private builds use `win` or `ssh mac`" (`AGENTS.md` releases section). The rationale line was not located in the plan corpus; the 07-13 ban was never formally superseded. |
| Node build control plane v2.8 (08-15, "LOCKED") | RustRight Rust broker; Node packages deleted | `bogusyogi/rustright` `docs/decisions/0001-rust-broker.md`; parity suite passed on the Mac matrix. Decided 2026-10-08, cut over 2026-10-09. The 08-15 doc is still marked LOCKED and its three open decisions (sccache on, permits, `CARGO_BUILD_BUILD_DIR`) were never closed; they now belong to the RustRight chat. |
| Converged plan P2: enforce block tier on both hosts | Dropped. Advise tier plus "RightKit first" workspace rule, Legion `/architect` from `rightkit owns`, `/audit` advisory findings | Adrian 2026-10-10: "yes drop it … rightkit first"; DEC-proportional-trust ("we cannot enforce so much that there is absolutely zero trust"). Not yet written into the converged plan or STATUS.md. |
| QA receipt gate blocks build/upload (`c6203348`, 10-09) | Warn and record; `qa audit` for review (`aea09d39`, 10-10) | Adrian: "we don't need to block every single release … this has bitten me before". |
| QA key anti-tamper deny rules (`5075e066`) | Reverted (`c831cb03`, `4e3650a6`) | DEC-proportional-trust; the `**/` rule syntax was also wrong. |
| Publish only from the authenticated Windows primary checkout (SHARED-PACKAGES L117-135) | Publish from the Mac clean clone | Mac is the lead host; darwin-only host policy, Adrian 2026-10-07. The doc was not updated. |
| Swift via registry (07-13 ADR) | Private git repo + exact tag | Superseded 2026-07-15, now a locked invariant in `rightkit/AGENTS.md`. |
| Tauri-only product shape (07-13 layers) | Also SwiftPM native apps with MSIX/App Installer | Pulse and SwiftRight app chats needed it (19 commits 10-09/10-10). Unplanned scope; no plan document covers it. |
| Inventory 53 crates (10-09) | 56 | `rightkit-computer` deleted (wave 1.4); `rightkit-daemon`, `rightkit-fsindex`, `rightkit-automation` and others added. |

## 4. What is pending

### 4a. Converged plan P0–P4

No P-row has its exit evidence recorded; STATUS.md still shows the 10-08 picture.

| Row | Current | Pending |
|---|---|---|
| P0 Mac broker restore | Superseded by RustRight cutover | Reboot + `LISTENING` evidence, watchdog cooldown: RustRight chat |
| P0 catalog | Catalog 3.0.0 committed; digests match on Mac | Complete-scope report and digest equality on both hosts not recorded |
| P1 broker admission (Windows) | Code landed | Refusal tests per D1 list not recorded |
| P1 approval contracts / schema v2 | Code landed; Mac trust enrolled (seq 1) | Native stage/sign/verify on both hosts not recorded; Dell enrollment state unverified from here |
| P1 publication binding (Mac) | Sealing + `.crate` binding landed | Exit test "every entry rejects bad seals" not recorded |
| P1 D5 crate fixes | Most landed (secrets, jobs, sandbox, mcp, hooks, updates, db) | `rightkit-pdf` `save.rs:414` gate; `sqlcipher-system` commit |
| P1 HeardRight exception drafts | Waiting on Adrian | Regenerate from fresh scan, then Adrian signs |
| P2 advisory gates | Pre-commit gate, manifest hook, drift check landed; hook mis-tokenises shell operators (`2>&1`, `\|`) as npm names (Legion, 10-10) | Gate in every app repo, `workspace-doctor` drift check; fix the tokeniser (policy-bundle change, batch for trust-sync) |
| P2 full reachability | Not started | Only needed if enforce is ever revived |
| P2 enforce | Dropped by Adrian | Record the disposition in the plan and STATUS.md |
| P3 per-app burn-down | App chats run on their own | Wave 3 items below; app pins stale (desktop-app-template has 6 outstanding bumps, 3 breaking) |
| P4 cleanup | Node broker retired | `runtime-*` GC; `@rightkit/release` ownership split (= inventory 4.2) |

### 4b. Content pipeline RK0–RK10

Code exists for every row; none is published; STATUS.md still says RK3/5/6/8/10 are "blocked".

| Id | Crate | Code now | Pending |
|---|---|---|---|
| RK0 | `rightkit-contracts` 0.1.2 | 2.3k LOC, 18 tests, `publish = false` | Vendor schemas, publish; Hetzner run (waits on RK7) |
| RK1 | oauth server mode, Linux secrets | oauth 0.3.0 server mode shipped | Linux secrets backend, Meta token exchange: blocked on RK7/RK10 |
| RK2 A | Mac off-screen capture spike | Root-caused to App Nap (`d2b9dd2f`); evidence captures untracked under `spikes/rk2-macos/evidence/` | Commit or archive the evidence; close the row |
| RK3 | `rightkit-record` 0.1.1 | 9.4k LOC, 151 tests; Windows helper folded in (`7ca4653f`) | `qa record: true` integration; publish decision |
| RK4 | `rightkit-align` | Local route, 28 tests, synthetic fixtures | Real QTS audio on Mac/XPS; paid route needs Adrian's spend approval |
| RK5 | `rightkit-review` | 1.1k LOC, 39 tests | Consumer wiring (PostRight) |
| RK6 | `rightkit-channel-memory` | 16 tests | PostRight P3 / GenRight G7 fixtures |
| RK7 | Linux native build route (Hetzner) | No commits found | Not started; gates RK0 publish, RK1, RK10 |
| RK8 | `rightkit-scenario` | 3.4k LOC, 49 tests | GUI worker, leases, reconnect proof |
| RK9 | `rightkit-compose` | Spring easing + click effects landed (`ca41ba38`) | Done to plan scope; needs a consumer |
| RK10 | `rightkit-daemon` | 1.8k LOC, 44 tests | HTTPS listener on Hetzner (RK7) |

### 4c. Inventory waves

- Wave 0 (security): done, `8b6c6fb5` (mcp 0.5.1 bounded stdio, search fail-closed, models atomic rename, right-ctl signs built artifact, license-v2 parity).
- Wave 1 (trims): done except 1.10 (asr/qts execution-provider trims wait on a CI and consumer check).
- Wave 2 (consolidations): batch landed `ca41ba38` (fs primitives, feature boundaries, qts-ggml deprecated, hygiene, product names). 2.3 (one shared archive/lock/download layer for models) not individually verified.
- Wave 3 (app donors): all 18 items pending, owned by app chats. Includes HeardRight 3.2 (`heardright-model-store`) and MailRight 3.13 (keyring/ureq/reqwest).
- Wave 4: done 4.4 (`89404995`), 4.6 (`21099448`), 4.7 (`a3c74d90`), 4.8 (`8cff435a`). Pending 4.1 (RustRight client API, then release imports; `managed-cargo.mjs` / `managed-roots.mjs` still in `packages/release`), 4.2 (ownership split out of release: 62 ownership files still inside), 4.3 (retire models root stack after GenRight ports), 4.5 (pregenerate ggml/protobuf).
- The 12 decisions for Adrian (inventory §3) have no recorded dispositions.

### 4d. Build capacity plan

Step 1 (measure) approved and in implementation; steps 2–5 unapproved proposals. Ownership moved to RustRight with the broker; the plan was not updated.

### 4e. Registry and version drift

- `generate-index.mjs --check`: `INDEX.md` and `CAPABILITY-MAP.md` drift from generated output.
- Staged, never published: asr 0.0.2, ocr 0.1.2, sync 0.1.0, transcript, vad, tts, contracts, automation, agent-room-core (last two are `publish = false` and should leave `stagedCargo`, inventory 1.6).
- Public crates with no release record: hotkeys, llm, overlay.
- Staged, not yet published: browser 0.2.3.
- `desktop-app-template` pins behind: platform-ui, tauri, updates, release, rightkit-process, rightkit-tauri.

### 4f. Documentation drift

- `STATUS.md`: dated 10-08; predates the RustRight cutover, the inventory waves, P2 drop, QA warn-and-record, path move, SwiftPM scope.
- `docs/RIGHTKIT-SHARED-PACKAGES.md`: Windows-only paths, pin table ~20 versions behind, says publish only from Windows, no broker, in-progress table lists published packages.
- `docs/OWNERSHIP-ENFORCEMENT.md`: still says installed enforcement is not active and no host has keys enrolled; Mac is enrolled.
- `docs/plans/2026-08-15-rightkit-architecture.md`: LOCKED, but its control plane is gone; three open decisions unowned.
- `2026-07-13` plan: hosted-CI ban never formally superseded.
- Converged plan: P2 enforce not marked dropped.

### 4g. Working tree and blockers

- Dirty, owner unknown: `crates/rightkit-fsindex` (8 modified + untracked CHANGELOG, LICENSE); `crates/rightkit-ocr` 0.2.0 bump (6 modified + untracked test), causes 6 local `right-suite-contract` failures; `Cargo.lock`; large untracked `spikes/rk2-macos/evidence/*.mp4`.
- Waiting on Adrian: Pulse Windows signing (AADSTS700213, portal work refused); diskpart on the Dell (RustRight V2 disk-full test); RK4 paid-route spend; HeardRight exception signing; 12 inventory decisions; class-1 installers and Removable Volumes grant (STATUS 10-08, unverified today).

### 4h. QA receipt gaps reported by Pulse (2026-10-10, via the Dell "Pulse win" chat)

Pulse adopted 0.2.146 with a qa block but records `qa.status: missing` because: (1) `right-release` only spawns `rightkit-qa run`; cargo-test `#[test]` Harness scenarios print `[qa] PASS` but no evidence line, so a cargo-test harness cannot feed receipts; (2) on Windows `rightkit-qa` canonicalises workspace paths to `\\?\C:\…`, which WebView2 rejects; (3) the installed stage can only drive `qa-native` debug builds through `rightkit-control`, so no installed app can pass; (4) `install-app` is Mac-only and the CI gate job cannot hand a source receipt to the build job. All four are RightKit-side work, unowned until scheduled.

## 5. Recommended next actions

1. Refresh `STATUS.md`, `INDEX.md`, `CAPABILITY-MAP.md`; mark P2 enforce dropped and the 08-15 plan superseded by RustRight (RightKit lead, this chat).
2. Adrian: keep or drop the fsindex and ocr dirty files.
3. Audit 4.2 (ownership split out of `@rightkit/release`), batched with the hook tokeniser fix for one trust-sync.
4. Start or park RK7 (Hetzner Linux route); it gates RK0 publish, RK1 and RK10.
5. App chats: wave 3 donors, starting with HeardRight 3.2 and MailRight 3.13; bump `desktop-app-template` pins.
