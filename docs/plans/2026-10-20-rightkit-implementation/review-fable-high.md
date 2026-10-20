# Fable high review — RightKit implementation packets (frozen snapshot 2026-10-20)

Reviewer: independent adversarial pass (Fable, fresh context). Scope: the seven frozen packets against Adrian's objective (apps keep only product code; every shared mechanic has one RightKit owner) and the signed spec. Read-only; no build, test, publish or message was run. The one command I could not run is `right-release --version` on this host (sandbox refused it), so R6's "0.2.165" claim is unverified, not disputed.

## Inputs verified

Frozen bytes reviewed from `D:\scratch\rk-packet-review-20261020\frozen\`. SHA-256 computed and matched against `review-input-hashes.json`:

| File | SHA-256 | Manifest match |
|---|---|---|
| 01-packages.md | 9cdfbef924ead56067cf27707aff50784a42fa1da45c643622d3fff0466b0e08 | yes |
| 02-pulse.md | 0047753374d56c6b2639c7e0bcffa7c0ead4c4c7d77affe5569b990d19a800bc | yes |
| 03-heardright.md | b738b870eea8a70cb66258195059e0213d0cee755bea99d3f21656f5aadb6232 | yes |
| 04-viewright.md | d9376559739c4ca93a7bfc9b11fa290f884da7e02c9e3035937ad6ece2c6f669 | yes |
| 05-mailright.md | 94d4e064e1a651c4673dc93085ddfe1e23bcf5c44b0fdae8a96af3b7abb8db48 | yes |
| 06-scraperight.md | ccfba6a9f91e184ab415151fe8018cd311239e01c7d11480bc658ab028475cb1 | yes |
| 07-coderight.md | 147b9b9b0e7d0fb6605a73ce269aa74c2e8a281b356f14412ebd841ee79af5b4 | yes |
| README.md | d07015e4e2dcac61f610299fbf76622cabc7fa2ad408cba50d41f833287e0afd | not in manifest (recorded) |
| review.md | a74ae372532619158eb2db0a85b28fe7a5653e84025769feb5dc0a55e33c515a | not in manifest (recorded) |
| spec adjudication | f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49 | matches README/packets |

Source evidence read against: RightKit working tree at HEAD 5ce6e2c3 (dirty, as the packets state), app checkouts at the HEADs listed in README.md.

What held up: M1–M23 and D1–D22 each land in a PK or an app task; task-ID ranges are complete and unique; the Cargo edge list in §2 matches the manifests except one optional marker (FH28); the npm edges match; the published/candidate version table matches the registry section and the crate manifests; the collision, no-yank and next-number rules are sound; the cycle guard (db never depends on credentials or daemon) holds against the manifests.

## Findings

### BLOCKER

**FH01 — Readiness is PK-granular, so the "no global barrier" model does not work.**
Packet/line: 01-packages.md §6 rules 451–457; README.md 28; every app dependency map.
Claim: "App work starts as soon as its required subset is satisfied"; `tasks.<AP>.ready` is derived only from `contracts.PKnn.status == satisfied`, and `satisfied` requires "every task of that PK is checked" (01:453–454).
Evidence: every PK bundles an already-published or early-shippable piece with large "required" work. AP-VR-04 (one-file keyed lock, PK01-5) cannot become ready until PK01-6/7 `FileTransaction` for MailRight is coded and qualified (01:106–108). AP-PU-01 depends on PK08, whose contract includes PK08-6 macOS `PdfHost` for ViewRight and PK08-4 overlay for HeardRight (01:229–231; 02:106). AP-HR-18 root convergence depends on PK07, which closes only with PK07-4 approvals for Pulse/Mail (01:211; 03:128). AP-HR-03 waits for PK03-3..6 (03:113). AP-MR-10 cannot even move MailRight from `rightkit-qa =0.7.0` (mailright `src-tauri/Cargo.toml:69`) to the published 0.7.1 (spec D2 minimum) until PK14-3/4 land (05:68; 01:333). The intermediate releases the packet explicitly allows (lease patch, capture fix, credentials 0.3.1, PK05-1/2) only ever yield `partial`, which never makes any AP task ready.
Correction: key contracts at task granularity. `contracts` carries one entry per PK task id (`PK01-1`, `PK01-5`, …) with its own `status`; `requiresContracts` in every app task lists task ids, not PK ids; `ready` derives from those. Each PK's "Publication" line names which task ids a given publication event satisfies. Regenerate the six dependency maps accordingly (e.g. AP-VR-04 → PK01-1, PK01-5; AP-HR-18 → PK07-1, PK07-3; AP-MR-10 → PK14-1 for the 0.7.1 pin).

### HIGH

**FH02 — The confirmed release-selector defect (D22) is gated behind unrelated account-session work.**
Packet/line: 03-heardright.md AP-HR-16 (40), dependency map 126; 01-packages.md PK05 181, 169.
Claim: AP-HR-16 "depends PK05, PK14, PK17".
Evidence: the fix the spec names is "native authorizer replaces fallback" (spec §4E 129) and PK05 itself says `is_pro()` is replaced by `EngineEntitlements`, which is already published in license 0.2.3 (01:167, 181). But PK05 `satisfied` requires PK05-5/6 (`AccountSessionClient`, Pro bridge, link codes) which have nothing to do with `heardright-text-pipeline/src/settings.rs:318–328` (verified: the env fallback is at 322–325 with no cfg gate; production callers `finalize_transcript.rs:568`, `worker_main.rs:640`, `command_results.rs:72`, `l3_cleanup/providers.rs:43`).
Correction: split AP-HR-16 into (a) entitlement fallback removal, depending only on the published license 0.2.3 `EngineEntitlements` (no PK contract) and listed as the first HeardRight task; (b) the two test-mode selectors (`delivery.rs:31`, `engine.rs:46`) depending on PK14-5. With FH01 applied, (b) names PK14-5 only.

**FH03 — The apps' existing exception ledgers are invisible to every packet.**
Packet/line: all of 02–07 (zero occurrences of `rightkit-exceptions`); README.md 21.
Claim: each app packet's residue/exception column starts empty ("pending") and the executor "records justified remnants".
Evidence: every app already carries `docs/rightkit-exceptions/M*.md` with kept items and deletion decisions: CodeRight 23 files, ViewRight 20, Pulse 17, MailRight 5, HeardRight 1, ScrapeRight 1. They assert retained mechanics the packets must retire, e.g. ViewRight `M8.md` "B1 in-process save lock (kept)" for `destination_write_lock` (the very PK01-5 deletion target); CodeRight `M7.md` B1 keeps the loopback HTTP/bearer engine with anchors `main_runtime_server.rs:268`, `:1343`, `daemon/discovery.rs:1`, `startup.rs:25-37`; Pulse `M5.md` B2 keeps a raw `Command` pass-through (`core/src/context_cmd.rs:28-30`). They also reference cross-app files outside any repo (`D:/scratch/m-components/CROSS-APP-M7.md`, `CROSS-APP-M8.md`). A fresh executor given only the packet will either leave these "kept" entries standing (retained mechanics) or contradict them without knowing.
Correction: add one task per app packet (e.g. AP-XX-nn "exception ledger reconciliation"): for every entry in `docs/rightkit-exceptions/*.md`, record disposition `retired by <PK task>` / `carried with sunset` / `product data`, and update or delete the file in the same change as the deletion. Packet 01 lists, per PK, which app ledger entries its owner work retires (at minimum: VR M8 B1 → PK01-5; CR M7 B1 → PK06-5; Pulse M5 → PK02).

**FH04 — `@rightkit/tauri` 0.3.0 (BREAKING) has no PK owner.**
Packet/line: 01-packages.md baseline 23, npm edges 69, lanes 81; no PK task.
Claim: the baseline table lists `@rightkit/tauri` 0.2.3 → 0.3.0 "licence client removal (BREAKING), dialog-qa".
Evidence: no PK01–PK17 task qualifies or publishes it (grep of the packet: only lines 23, 69, 81). It is the K04 typed bridge (Astra K04 "owners"), the dependency of `@rightkit/logs` and peer of `@rightkit/app-update` (`packages/tauri/package.json:34,37`), all six apps pin 0.2.3, and its licence-client removal is the renderer half of the `@rightkit/license` 0.3.0 breaking change that PK05-2 publishes.
Correction: add PK04-7 (or PK05-7): run `@rightkit/tauri` tests, decide `dialog-qa` against its consumer (the unpublished `rightkit-qa` `dialog` module, FH12), publish 0.3.0 in the PK04/PK05 group with a BREAKING note; list it in the npm lane and in the deletion consumers of the licence tasks (AP-VR-03, AP-CR-01, AP-MR-06, AP-SR-06, AP-HR-04, AP-PU-06).

**FH05 — PK17-9 lets an app keep a shared release mechanic when only one app has it.**
Packet/line: 01-packages.md 377.
Claim: "A recipe enters @rightkit/release only where two apps share the mechanics or a packet names the deletion".
Evidence: the locked rule is the opposite: Astra §1.1 item 3 "One current consumer is sufficient when behavior is app-neutral"; adjudication §6 "Platform specificity, one current consumer, thin glue or historical format alone do not qualify" as an exception; §2 "Repeated workflow orchestration belongs in RK". Under 377 HeardRight's nested-bundle signing (`scripts/sign-macos-bundle.mjs::signHelperBundles`, verified at line 139) or ScrapeRight's `prepare-runtime.mjs` staging could stay app-local because no second app has them.
Correction: replace the sentence with "A recipe enters @rightkit/release where any app packet's M19 closure names an app-neutral mechanic; app-specific identifiers, payload lists and assertions stay as config."

**FH06 — CodeRight's loopback server side has no deletion task.**
Packet/line: 07-coderight.md AP-CR-03 (55), M4/M7 rows (30, 33), C2/C3 (18–19); 01-packages.md PK06-5 (194).
Claim: PK06-5 supplies "bounded parser, loopback bind, bearer/capability verification inside the owner"; AP-CR-03 migrates "the actual desktop-daemon-client implementation".
Evidence: the mechanics PK06-5 replaces live on the server side: `engine/bins/coderight/src/main_runtime_server.rs:268` (`TcpListener::bind(("127.0.0.1", port))`), `:1336` (token file), `engine/bins/coderight/src/auth.rs:78` (`require_token`), `daemon/transport.rs`, `daemon/discovery.rs`, `startup.rs` (all named by CodeRight's own `docs/rightkit-exceptions/M7.md` B1). AP-CR-03 anchors only `crates/daemon-client/*` (the client). No CR task names the server files, so the duplicate listener/auth stays after "adoption".
Correction: extend AP-CR-03 (or add AP-CR-15) with the server-side anchors above as PK06-5 deletion targets, acceptance: the engine binds through the daemon loopback feature; bearer verification is the owner's; wire unchanged for iOS/web clients.

**FH07 — The first publications cannot produce a §6 snapshot "with no value typed by hand".**
Packet/line: 01-packages.md PK17-7 (376), PK17 publication (381 "none blocks crate publishes"), §6 (407), executor brief 1.g (471).
Claim: snapshots are "emitted by the publisher"; "No value is typed by hand".
Evidence: the emitter is new work in PK17-7; `@rightkit/release` 0.4.0/0.4.1 has no snapshot writer (packet 366 lists 0.4.1 content; none is a handoff emitter). PK01-1, PK02-1, PK03-1 are allowed to publish first (01:85, 111, 126, 144) and 381 says nothing in PK17 blocks crate publishes. Their snapshots would be hand-written, which §6 forbids, or missing, which blocks every dependent AP task.
Correction: make PK17-7 (run from the source-qualified tree copy per R6) a prerequisite of the first publication event, or define in §6 a `manual-snapshot` form (publisher evidence paths + registry readback copied verbatim, `publisherTool.source = "manual"`) accepted only until PK17-7 lands and regenerated afterwards.

### MED

**FH08 — `@rightkit/license` 0.3.0 `./testing` safety depends on an unpublished release tool.**
Packet/line: 01-packages.md PK05-2 (173), PK17 (366); app licence tasks 02:30, 03:28, 04:71, 05:64, 06:60, 07:53.
Evidence: the `development`-condition preflight and the `license-testing` bundle markers are in `@rightkit/release` 0.4.1 "unpublished staging" (`packages/release/CHANGELOG.md:5-6`); all six apps pin `@rightkit/release` 0.4.0. No app licence task requires PK17.
Correction: PK05-2 publication note: "apps adopt 0.3.0 only after PK17-1 publishes 0.4.1 and the app pins it"; add PK17-1 to the `requiresContracts` of the six licence tasks.

**FH09 — The manifest has no entries for already-published, unchanged packages.**
Packet/line: 01-packages.md §6 404–406, 455 ("versions are read from published-release-manifest.json"); 05-mailright.md 16 (legal-ui 0.4.0, onboarding 0.6.1 pins, verified `package.json:51,54`).
Evidence: snapshots exist only per publication event. device 0.3.1 (PK05-4 "no republish"), db 0.5.0, app-settings 0.2.4, rightkit-qa 0.7.1, legal-ui 0.4.1, onboarding 0.6.2 (if PK10 ships no patch), sidecar, platform-ui, theme, legal, pdf 0.3.2, overlay 0.1.2 never get a snapshot, so an app cannot derive those pins from the manifest.
Correction: PK17-7 also emits `baseline` entries (registry readback + archive sha256 + `publisherTool`) for every published, unchanged package a PK contract relies on; each PK lists them under "Published".

**FH10 — `wv2-0_39` is consumed by three apps but has no gate row.**
Packet/line: 01-packages.md PK04 qualification 161, PK08-1 226, PK17-4 373.
Evidence: ViewRight (`src-tauri/Cargo.toml:77`), ScrapeRight and CodeRight (`:61`) pin `rightkit-tauri-shell =0.4.0` with `default-features = false, features = ["wv2-0_39"]`, and `rightkit-qa` with `wv2-0_39`. `ci/gate.config.json` tauri-shell rows are `[]`, `engine`, `logs` (default = `wv2-0_38`, `crates/rightkit-tauri-shell/Cargo.toml:13`); rightkit-qa check row is `control,wv2-0_38`. PK17-4 adds no 0_39 row, so the "W with wv2-0_38 and wv2-0_39" qualification cannot be executed from the gate.
Correction: PK17-4 adds tauri-shell `--no-default-features --features wv2-0_39` (and `wv2-0_39,engine,logs`) and rightkit-qa `control,wv2-0_39` check rows, Windows only.

**FH11 — Gate rows the packet relies on are uncommitted.**
Packet/line: 01-packages.md 161, 282, 373; executor brief 463 ("unrelated dirty files … never staged").
Evidence: `git diff HEAD -- ci/gate.config.json` shows the tauri-shell `logs` featureSet, the `engine,logs` check row and the `rightkit-pdfium` entry are working-tree only. PK17-4 names only `update` for tauri-shell, so nobody stages the rows PK04/PK11 cite.
Correction: PK17-4 lists the `logs` and `engine,logs` rows and names `ci/gate.config.json` as a staged file of that task (pdfium is outside scope: leave it dirty).

**FH12 — rightkit-qa is not "= source"; the tree carries unpublished API under the published number.**
Packet/line: 01-packages.md baseline 34 versus PK14 320–321, 325.
Evidence: `crates/rightkit-qa/src/lib.rs:48,56` export `dialog` and `fake_cli` (files present), Cargo version 0.7.1 = registry 0.7.1, CHANGELOG heading "0.7.1 — unpublished staging … Not built or run locally" while the spec records 0.7.1 as published. PK14-1 says "relabel 0.7.1 CHANGELOG" without bumping the tree version.
Correction: baseline row → "published 0.7.1; tree 0.7.1 carries unpublished dialog/fake_cli/ui_dialog additions"; PK14-1 first bumps the tree to the next number, then keeps or removes the additions by consumer (the only candidate consumer is `@rightkit/tauri` 0.3.0 `dialog-qa`, FH04).

**FH13 — Pulse engine anchors name symbols that do not exist.**
Packet/line: 02-pulse.md AP-PU-03 (27), M4 row (52), ledger (79).
Evidence: `stdio_supervised_child` and `resident_service` have zero matches in any `.rs` under `D:\Claude\pulse`. Actual anchors: `engine/src/resident.rs:48–54` (`rightkit_daemon::{host_with_options, …}`), `:55` (`rightkit_job_queue::{CommandSet, Dispatcher, DispatcherHandler}`), `:697` (`method_table`), `:780` (`Dispatcher::new`).
Correction: replace the two names with `host_with_options` / `DispatcherHandler` call sites at those lines.

**FH14 — PK08-7 invents a HeardRight app-lock consumer and Pulse's real lock code has no anchors.**
Packet/line: 01-packages.md PK08-7 (232), deletion consumers 237; 02-pulse.md AP-PU-08 (32); 03-heardright.md (no lock task).
Evidence: HeardRight has no app lock, PIN or OS-authentication code (no `LocalAuthentication`, `LAContext`, `app_lock`, `biometric`, Windows Hello under `src-tauri`, `macos-native`, `heardright-*`). The "HeardRight/MailRight" pairing comes from Fable r1 line 343 (open owner work), which the adjudication did not adopt; the spec preserves only Pulse's PIN vs remote-approval distinction. Pulse's lock mechanics exist but are unanchored: `core/src/remote/unlock.rs:518` (`verify_pin`), `windows/src/unlock_auth.rs`, `windows/src/remote_alerts.rs:326-334,404` (`Way::Pin`/`Way::Hello`), `mac/Notch/Sources/Agent/ChatUnlockPanel.swift:30` (LAContext at 66,142,386,454), `ios/Sources/Owners/AppLock.swift:8`.
Correction: PK08-7 consumers = Pulse only; remove HeardRight from 232 and 237. AP-PU-08 lists the files above as PK08-7/PK03 (OS-auth primitive) deletion targets, keeping the PIN authority and action policy as Pulse data.

**FH15 — PK16-2 and AP-PU-16 have no Swift source anchors for the Pulse mechanics to move.**
Packet/line: 01-packages.md PK16-2 (357), 362; 02-pulse.md AP-PU-16 (40), ledger P6 (84).
Evidence: the only Pulse Swift anchor is `mac/Hub/Package.swift:23` (the 0.8.1 pin). The hub mechanics live in `swift/RightKitHub` (`mac/Hub/Package.swift:21` is `.package(path: "../../swift/RightKitHub")`, a self-contained package), `mac/Notch/Sources`, `ios/Sources`. An executor of packet 01 cannot locate "Pulse Swift hub/supervised-service/helper-transport mechanics".
Correction: PK16-2 and AP-PU-16 name the files/types in `swift/RightKitHub/Sources` and `mac/Notch/Sources` that duplicate RightKitSystem/Store/EngineClient products, and the HeardRight helper files under `macos-native/HeardRightPillMac`.

**FH16 — ScrapeRight does own a database; M9 is wrongly "not established".**
Packet/line: 06-scraperight.md M9 row (37), AP-SR-08 (97), PK03 prerequisite (115).
Evidence: `D:\Claude\scraperight\crates\store\Cargo.toml:9` pins `rightkit-db =0.5.0` (`bundled, statement-cache`); `src-tauri` has none, which is why the packet's `src-tauri/src` search missed it.
Correction: M9 Applies (plain SQLite, like ViewRight); AP-SR-08 anchors `crates/store` and routes its root/open through PK07 (and PK03 only for lifecycle/relocation if the store is ever moved); no encryption.

**FH17 — `tools/setup-workspace.py` does not exist in ViewRight or MailRight.**
Packet/line: 04-viewright.md 10; 05-mailright.md 10.
Evidence: no `tools/` directory in ViewRight; no `setup-workspace*` anywhere in MailRight.
Correction: delete the instruction or name the real setup step (`pnpm install`; the Rust side runs through the RustRight broker already named in the verification tables).

**FH18 — CodeRight has no desktop `switches.json`, and its mandatory file contradicts the packet on M22.**
Packet/line: 07-coderight.md AP-CR-11 (98), M22 row (48), AP-CR-12 (99).
Evidence: the only `switches.json` is `apps/coderight-ios/switches.json`; `release.mjs:450` fails a release build when the app's switches.json is missing. `D:\Claude\coderight\rightkit-mandatory.json:5` declares M22 `notApplicable` (control server refused under ruling O-4) while the packet marks M22 "Applies".
Correction: AP-CR-11 creates `apps/coderight-tauri/switches.json` from the full selector inventory (M23) before any release-switch scan; AP-CR-12 reconciles the mandatory-file declaration with the PK15-2 applicability record (`client|host|both|absent`) rather than both standing.

**FH19 — AP-CR-03 migrates a file with no callers that the app has already decided to delete.**
Packet/line: 07-coderight.md AP-CR-03 (55); review.md FV06.
Evidence: `ssh_tunnel` appears only as `mod ssh_tunnel;` (`lib.rs:73`), the re-export (`src/ssh_tunnel.rs:3`) and the crate module (`crates/daemon-client/src/lib.rs:8`); no call site. CodeRight `docs/rightkit-exceptions/M7.md`: "The SSH-tunnel client … has no caller and is DELETED under the M7 order."
Correction: AP-CR-03 deletes both `ssh_tunnel.rs` files after a repository-wide caller search; it is not a migration target.

**FH20 — Raw `std::process::Command` spawn sites have no deletion task in four apps.**
Packet/line: 03-heardright.md M5 row (54, "Provisional"); 05-mailright.md M5 (37), AP-MR-09 (67); 07-coderight.md AP-CR-02 (54); 02-pulse.md M5 (53).
Evidence (production code, not via rightkit-child-process): HeardRight `src-tauri/src/commands/system.rs:175,187,217`, `commands/updates.rs:114` (`/bin/sh -c <script>` relaunch, a shell-text launch K02 forbids), `commands/files.rs:50`, `commands/diagnostics.rs:279,291,605,619`, `commands/microphone.rs:39`, `commands/catalog.rs:24`, `heardright-command-host/src/command_dispatch/power.rs` (ten sites), `app_launch.rs:481`, `keys.rs:1100`; MailRight `src-tauri/src/startup_error.rs:53` (osascript, the FV02 case); CodeRight `lib.rs:288,299` (git); Pulse `core/src/worker.rs:261`, `core/src/context_cmd.rs:30`, `core/src/bridge/deliver_codex.rs:1259`, `core/src/main.rs:1574`.
Correction: each packet's M5 row becomes Required with these anchors as PK02 consumers (`run`/`run_with_stdin`/`OwnedCommand`, all published in 0.5.2) or as exception-ledger entries with sunset (FH03); HeardRight `updates.rs:114` gets its own line: replace the shell script with a typed launch.

**FH21 — Pulse device-link hosting (P5) has a ledger row but no task.**
Packet/line: 02-pulse.md ledger 83; tasks 25–41.
Evidence: `engine/src/devicelink_host.rs:1762` (`pub struct DeviceLinkHost`) is named only in the ledger; AP-PU-07 (approvals) and AP-PU-15 (agent surfaces) do not mention it.
Correction: add it to AP-PU-15 (PK07-5 service registration, PK07-4 approvals) with the "extract generic pairing/session mechanics" scope.

**FH22 — Pulse root cutover is over-gated.**
Packet/line: 02-pulse.md AP-PU-01 (25, 106); AP-PU-07 (31, 112).
Evidence: AP-PU-01 requires PK06 and PK08 ("native root-coupled host surfaces") although the root context is PK07 and the only root participants are PK01 (files) and PK03 (budget DB, AP-PU-04). AP-PU-07 requires PK06 although approvals are PK07-4 and admission PK15.
Correction: AP-PU-01 → PK07 (+PK01; PK03 for the budget-DB participant); AP-PU-07 → PK07, PK15.

**FH23 — The tauri-shell ↔ rightkit-pdf dependency direction is left to the executor.**
Packet/line: 01-packages.md §2 73, PK08-6 (231).
Evidence: "decided in the PK08-6 review" is a design choice deferred past the lock; the spec already assigns authorization to tauri-shell and the view lifecycle to pdf (K08).
Correction: fix it now: tauri-shell gains an optional `pdf-host` feature depending on rightkit-pdf; rightkit-pdf never depends on tauri-shell.

**FH24 — The shell-package coordination list is incomplete.**
Packet/line: README.md 31.
Evidence: PK06-3 (`EngineHost` in tauri-shell `engine`, 01:192) and PK10-2 (legal-receipt service registered through PK04-2, 01:260) also modify rightkit-tauri-shell.
Correction: "PK04/06/08/10/11/13 may modify one shell package".

**FH25 — AP-VR-01 and AP-MR-01 are whole-app barriers in disguise.**
Packet/line: 04-viewright.md AP-VR-01 (68) and every "Depends: AP-VR-01 + …"; 05-mailright.md AP-MR-01 (59).
Evidence: one task sets "exact direct pins … for each applicable PK", and every other task depends on it, so no ViewRight or MailRight task can start until all seventeen contracts are published.
Correction: state that AP-XX-01 is re-entered per readiness event, pins only the packages the newly ready task consumes, and closes last; or fold pinning into each task.

**FH26 — RightKitSwift pin URLs and versions disagree across apps and the ledger; PK16 names neither.**
Packet/line: 01-packages.md PK16 352 ("tag 0.8.1 (app pins)"), PK16-3 358.
Evidence: Pulse pins `https://github.com/bogusyogi/rightkit` exact 0.8.1 (`mac/Hub/Package.swift:23`, `mac/Notch/project.yml:46-49`); HeardRight pins `https://github.com/Orthic-Labs/rightkit.git` exact 0.8.0 (`macos-native/HeardRightPillMac/Package.swift:26`, `ios-native/project.yml:17-19`); ScrapeRight and CodeRight pin Orthic-Labs 0.8.1; `packages/release/rightkit-versions.json` `stagedSwift` says Orthic-Labs 0.7.0 and `swift` 0.4.0; the RightKit checkout remote is `bogusyogi/rightkit.git`.
Correction: PK16 states the canonical URL that `right-release publish swift` tags, requires the tag to resolve from both URLs (or moves every app to the canonical one in its AP-16 task), and PK16-3 updates the `swift`/`stagedSwift` ledger entries at readback. 03-heardright.md records the current 0.8.0 pin.

**FH27 — rightkit-oauth 0.9.1 is an unlisted staged delta on a planned edge.**
Packet/line: 01-packages.md baseline 10–35, §2 56–58, PK17-4 373.
Evidence: `crates/rightkit-oauth/Cargo.toml:3` is 0.9.1; registry 0.9.0; daemon `remote` (opt) and credentials (opt) edges exist; the `remote`/loopback gate rows PK17-4 adds compile it; PK10-3 names "the app's rightkit-oauth command".
Correction: add an oauth row to the baseline table with the RV08 disposition (hold, or qualify with the daemon `remote` rows).

### LOW

**FH28** — 01-packages.md 55: `fs ─(opt)─> db` is wrong; `crates/rightkit-db/Cargo.toml:32` makes fs a required dependency. Drop "(opt)".

**FH29** — 01-packages.md 174: "(A2)" is an undefined reference. Replace with the rule text or delete.

**FH30** — 01-packages.md 377–378: PK17-9 precedes PK17-8. Renumber.

**FH31** — Anchor drift (packet says → actual): 03:85/56 `cloud_sync_cloudkit.rs:181` is a `log_message` call inside `migrate_legacy`; `environment` is :37, `helper_path` :58. 03:29/91 `ApiKeysPanel.tsx::saveKey` is 95–126 (`validate_l3_key` :101, `set_l3_api_keys` :112). 06:19 `commands.rs:14` is `settings_path`; `FileBackend::new` is :56. 06:22 `license.rs:58` is the `Entitlements.verified` field; `update_native_check` :625, `update_native_install` :658. 06:18 `cookies_path` is :104. 05:17 `SqlSettings` is `accounts_labels.rs:8–9`. 07:20 `rotate_daemon_log` is `lib.rs:1247`. 07:57 `native_oauth::{login,refresh}` are `native_oauth_login`/`native_oauth_refresh` (`native_oauth.rs:12,24`); `cc.coderight.native-oauth` and `KeyringCompat` are in `src-tauri/src/token_store.rs:35,42`, not `crates/auth`. 02:164 `scripts/release/candidate.mjs::run` is a local helper (line 8) in an argv CLI, not an export. 02:66 `engine::apps::updates` is `engine/src/apps_windows_updates.rs` via `#[path]`. 04:24 `getInstallHeaders` is `updateLicense.ts:521` (387 is `licenseHeaders`). 07:102 `PairAndChatE2ETests.swift` is under `apps/coderight-ios/UITests/`, not `Tests/`.

**FH32** — 04-viewright.md AP-VR-10 (78): ViewRight `src/app/useGlobalShortcuts.ts:55` is an app-local keydown dispatcher (imports only `isTypingTarget`), while MailRight and ScrapeRight use react-shell `createShortcutRegistry`. Name it as a PK09 deletion target.

**FH33** — 06-scraperight.md M16 row (44), AP-SR-11 (100): ScrapeRight has no `on_stall`/watchdog usage anywhere in `src-tauri/src` (only HeardRight `app_run.rs:54` does). Change "existing shell watchdog callback" to "adopt the shell watchdog".

**FH34** — 01-packages.md PK03 deletion consumers (145) list ViewRight `logs.rs:33` raw rusqlite, but 04-viewright.md AP-VR-13 treats PK03 as conditional and the DB is plain SQLite. Move that consumer to PK07 (root/DB-open ownership).

**FH35** — 07-coderight.md AP-CR-07 (94): it instructs restoring registration for canvas/native-window commands that `lib.rs:1–11` declares unreachable dead code pending triage (`docs/pending/rust-build-surgery.md §5`), and `generate_handler!` (1717–1759) registers none of `native_windows`, `approval_bubble` or `window_state`. Re-wiring is a product decision, not a migration; record whose decision it is, otherwise delete per the app's inventory.

**FH36** — 06-scraperight.md prerequisite map (120): the cookie artifact is assigned to PK08 ("browser artifact features"); the owner is PK12-4 with PK01 publication. Fix the map and AP-SR-02's dependency line.

**FH37** — 03-heardright.md AP-HR-06 (30, 116) depends on PK08 for the watchdog; the extension is PK11-4 (tauri-shell `watchdog.rs`). PK11 only.

**FH38** — 01-packages.md PK11 qualification (282) cites "crash gate default + `window-errors`", but crash has no gate entry and PK17-4 adds none; ViewRight, MailRight and ScrapeRight all enable `window-errors`. Add the row to PK17-4.

**FH39** — 02-pulse.md M5 row (53): Pulse already documents raw spawns and child-process adoption in `docs/rightkit-exceptions/M5.md`; "Provisional" ignores an existing inventory (see FH03).

## Claimed fixes in review.md, checked

FV01–FV07, AV08–AV13 and RV06/RV07/RV09 are present in the frozen files as described. RV01 ("Each AP task waits only for its PK subset") is present as prose but not as mechanism (FH01). RV12's manifest correction is present but incomplete for unchanged packages (FH09). RV13's M19 closures are present in every app packet, but the PK17-9 admission rule undercuts them (FH05).

## Verdict per packet

| Packet | Verdict | Blocking IDs |
|---|---|---|
| 01-packages.md | not ready | FH01, FH04, FH05, FH07; plus FH08, FH09, FH10, FH11, FH12, FH23, FH26, FH27 |
| 02-pulse.md | not ready | FH01 (dependency model), FH03, FH13, FH14, FH21, FH22, FH15 |
| 03-heardright.md | not ready | FH02, FH03, FH20, FH14 (remove invented consumer), FH26 |
| 04-viewright.md | not ready | FH01, FH03, FH17, FH25; FH10 must land in 01 first |
| 05-mailright.md | not ready | FH01, FH03, FH09, FH17, FH25, FH20 |
| 06-scraperight.md | not ready | FH01, FH03, FH16, FH36; FH10 in 01 |
| 07-coderight.md | not ready | FH01, FH03, FH06, FH18, FH19, FH20 |
