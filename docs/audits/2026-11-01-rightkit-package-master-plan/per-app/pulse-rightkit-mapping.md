# Pulse: RightKit mapping

Repo `pulse` at `5c8f93bb` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`pulse-rightkit-files.json`](pulse-rightkit-files.json).

Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).

## Summary

Corrected 2026-11-11 by the shape review (`docs/audits/2026-11-11-shape-matrix/reports/library-and-swift-mapping-redraw.md`): nothing is planned into `RightKitUI` (settings and app entry are app code), and the Notch view snapshots stay with the app; `RightKitHubQA` keeps only the Hub fixtures, renderer and AX tour, which are Hub QA tooling like the three fakes it already holds. Totals below are recomputed from the corrected rows; the per-file list in the app repo was not changed here.

| Action | Meaning | Files | Lines |
|---|---|---:|---:|
| NEW | new RightKit package (or new piece of one) | 988 | 447,784 |
| EXTEND | existing RightKit package gains this | 174 | 78,719 |
| ADOPT | RightKit already ships it; delete app copy & use owner | 96 | 34,679 |
| APPTEST | app journey/e2e test: stays with the app (P7, §6), runs on rightkit-qa | 26 | 12,734 |
| VENDOR-KEEP | vendored 3rd-party: keep as plain dependency, delete vendored copy | 18 | 71,409 |
| DROP | delete | 4 | 23,187 |
| SHELL | thin app composition that stays in app | 37 | 4,403 |
| CONFIG | app manifest/config that stays | 5 | 181 |
| NOTCODE | docs/data, not code | 25 | 4,528 |
| **Total** | | **1,373** | **677,624** |

Code leaving the app (NEW + EXTEND + ADOPT + LEGION + vendor + DROP): **655,778 lines**. Staying in the app as product code: **0 lines**; as shell/config: **4,584 lines** (1% of code), plus **12,734 lines** of app journey/e2e tests.

## Mapping table

| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |
|---|---|---|---|---:|---:|---|
| `@rightkit/agent-ui` | npm | NEW | remote approvals & owner settings | 5 | 1,308 | `hub/src/views/` |
| `@rightkit/config` | npm | NEW | shared presets | 3 | 58 | `./`, `.cargo/` |
| `@rightkit/system-ui` | npm | NEW | React system hub: overview, storage, cleanup, monitor, apps, chat, activity, membrane, drive health, charts | 29 | 7,889 | `hub/src/`, `hub/src/components/`, `hub/src/lib/` +1 dirs |
| `RightKitAgent` | Swift | NEW | phone remote-control screens | 15 | 4,258 | `ios/Sources/Presentation/`, `ios/Sources/Screens/` |
| `RightKitNotch` | Swift | NEW | macOS notch surface: notch window, design system, rings & cards, model | 65 | 17,668 | `mac/Notch/Face/`, `mac/Notch/Sources/DesignSystem/`, `mac/Notch/Sources/Features/` +3 dirs |
| `RightKitNotch` | Swift | NEW | macOS notch app host: delegate, hub bridge & launcher, engine service, terminal command, l10n | 18 | 4,717 | `mac/`, `mac/Notch/Shared/`, `mac/Notch/Sources/` +2 dirs |
| `RightKitSystemUI` | Swift | NEW | SwiftUI system hub sections: overview, storage, cleanup, monitor, apps, chat, activity, membrane | 71 | 21,612 | `mac/Hub/`, `mac/Hub/Sources/PulseHub/`, `mac/Hub/Sources/PulseHub/Sections/` +10 dirs |
| `rightkit-adapt` | crate | NEW | governed behavioural learning (taste + insights) | 38 | 15,823 | `membrane/engine/crates/membrane-adapt/`, `membrane/engine/crates/membrane-adapt/src/`, `membrane/engine/crates/membrane-adapt/src/insights/` +1 dirs |
| `rightkit-agent-engine` | crate | NEW | Swift agent UI/bridge | 6 | 2,228 | `mac/Notch/Sources/Agent/` |
| `rightkit-agent-engine` | crate | NEW | always-on agent: task envelope & transitions, principals, intents, effects, device/approval binding, voice handoff; agent loop, providers, budget (Swift: RightKitAgent) | 18 | 2,090 | `core/src/agent/`, `ios/Sources/Agent/` |
| `rightkit-agent-engine` | crate | NEW | windows agent glue | 1 | 779 | `windows/src/` |
| `rightkit-agent-engine` | crate | NEW | task CLI | 1 | 629 | `core/src/` |
| `rightkit-agent-gateway` | crate | NEW | remote tools for agents (files, app launch, clipboard, chat read/start/term, browser address), local approvals, audit trail | 35 | 18,794 | `core/src/`, `core/src/remote/`, `core/src/remote/protected_guard/` +1 dirs |
| `rightkit-agent-gateway` | crate | NEW | agent gateway: LLM provider & tool routing | 4 | 1,964 | `gateway/`, `gateway/src/` |
| `rightkit-ai-accounts` | crate | NEW | Claude Desktop/CLI account switching & Code-session sync across accounts | 14 | 9,028 | `core/src/`, `engine/src/`, `mac/Notch/Sources/System/` +1 dirs |
| `rightkit-ai-accounts` | crate | NEW | Claude/Codex plan usage, credentials, resets & credits (Swift providers) | 22 | 5,908 | `mac/Notch/Sources/Providers/` |
| `rightkit-ai-accounts` | crate | NEW | Claude sessions (Swift) | 16 | 2,656 | `mac/Notch/Sources/Sessions/` |
| `rightkit-ai-accounts` | crate | NEW | Claude Desktop state on Windows | 1 | 521 | `windows/src/` |
| `rightkit-app-lock` | crate | NEW | owner unlock auth / app lock | 3 | 502 | `ios/Sources/Owners/`, `windows/src/` |
| `rightkit-app-lock` | crate | NEW | Windows Hello owner check | 1 | 267 | `hub/src-tauri/src/` |
| `rightkit-app-manager` | crate | NEW | installed-app inventory, leftovers, uninstall plans, updates, usage, startup items, quit/auto-quit, disk-image install | 22 | 13,498 | `core/src/`, `core/src/apps_windows/`, `engine/src/` +2 dirs |
| `rightkit-app-manager` | crate | NEW | process control CLI | 1 | 687 | `core/src/` |
| `rightkit-app-manager` | crate | NEW | tests | 1 | 285 | `core/tests/` |
| `rightkit-approvals` | crate | NEW | signed phone/owner approvals for privileged & reserved actions; Windows admin service | 16 | 12,450 | `core/src/`, `core/src/devicelink/`, `core/src/remote/` +2 dirs |
| `rightkit-approvals` | crate | NEW | Swift approvals | 4 | 654 | `ios/Sources/Approvals/` |
| `rightkit-chat-bridge` | crate | NEW | deliver messages into running Claude/Codex chats (hooks, UIA/AX typing), envelopes, hub heartbeat, chat skill install, cross-user boundary | 32 | 27,031 | `core/src/`, `core/src/chat/`, `engine/src/` +2 dirs |
| `rightkit-chat-bridge` | crate | NEW | tests | 2 | 1,583 | `core/tests/` |
| `rightkit-clipboard-history` | crate | NEW | clipboard history: capture, detect, images, paste, seal, search, strip, shelf, snippet expansion | 18 | 12,760 | `mac/Notch/Sources/Conveniences/`, `windows/src/`, `windows/src/clip_paste/` +1 dirs |
| `rightkit-context` | crate | NEW | context engine: protocol, client, provider SDK, federation, budget/lanes, runtime, schemas, hooks | 187 | 95,959 | `membrane/engine/`, `membrane/engine/crates/membrane/`, `membrane/engine/crates/membrane-client/` +26 dirs |
| `rightkit-context` | crate | NEW | host side of context engine | 5 | 2,552 | `engine/src/`, `mac/Notch/Sources/System/`, `windows/src/` |
| `rightkit-desktop-tweaks` | crate | NEW | Finder cut/paste, path field, window closer, dock-click minimize, window maximizer, event tap hub (Swift + Win) | 15 | 2,032 | `mac/Notch/Sources/Conveniences/`, `mac/Notch/Sources/Conveniences/Windows/` |
| `rightkit-desktop-tweaks` | crate | NEW | Finder sync extension | 1 | 136 | `mac/Notch/FinderExtension/` |
| `rightkit-disk-cleanup` | crate | NEW | bounded metadata scans, cleanup plans/rules w/ undo journal, duplicates, folder growth, snapshots, storage browser, media compression | 27 | 22,352 | `core/src/`, `engine/src/`, `engine/src/journal/` |
| `rightkit-disk-cleanup` | crate | NEW | tests | 6 | 2,694 | `core/tests/` |
| `rightkit-engine-host` | crate | NEW | engine binary framework: sidecar + headless CLI modes, command registration, contract export & TS codegen, human/JSON presentation, engine client | 12 | 8,909 | `core/src/`, `core/src/ipc/`, `engine/src/` +1 dirs |
| `rightkit-launcher` | crate | NEW | command launcher: calculator, dictionary, files, notes, rates, snippets, commands (Win32 + Swift) | 49 | 22,237 | `mac/Notch/Sources/Launcher/`, `windows/src/` |
| `rightkit-memory` | crate | NEW | durable memory store (Cortex): scopes, identity, telemetry | 34 | 14,843 | `membrane/engine/crates/cortex-core/`, `membrane/engine/crates/cortex-core/src/`, `membrane/engine/crates/cortex-store/` +1 dirs |
| `rightkit-mesh` | crate | NEW | device mesh (iroh): link, device circle, join tokens, relay & mailbox/admission, FFI for iOS/Android (already named rightkit-mesh but lives in app repo: move & publish) | 53 | 28,747 | `android/meshlink/`, `android/meshlink/src/main/kotlin/dev/orthic/meshlink/`, `core/src/devicelink/` +16 dirs |
| `rightkit-mesh` | crate | NEW | device link module glue | 6 | 1,754 | `core/src/devicelink/`, `core/src/devicelink/transport/` |
| `rightkit-mesh` | crate | NEW | Swift mesh client | 7 | 1,693 | `ios/Sources/Mesh/` |
| `rightkit-mesh` | crate | NEW | Swift devices | 2 | 906 | `mac/Notch/Sources/Devices/` |
| `rightkit-mesh` | crate | NEW | device CLI | 1 | 507 | `core/src/` |
| `rightkit-mesh` | crate | NEW | paired computers (Swift) | 2 | 424 | `ios/Sources/App/` |
| `rightkit-mesh` | crate | NEW | devices UI | 1 | 374 | `windows/src/` |
| `rightkit-privileged-helper` | crate | NEW | privileged helper & elevation broker (macOS SMAppService helper, Windows elevation), trash policy | 6 | 673 | `engine/src/`, `mac/Notch/Elevate/`, `mac/Notch/Helper/` +1 dirs |
| `rightkit-repo-graph` | crate | NEW | repository graph: structure, evidence, drift | 75 | 37,279 | `membrane/engine/crates/membrane-blueprint/`, `membrane/engine/crates/membrane-blueprint/src/`, `membrane/engine/crates/membrane-blueprint/src/providers/` +1 dirs |
| `rightkit-system-monitor` | crate | NEW | CPU/memory/GPU/disk gauges, process inspection, SMART drive health, metrics history, volumes & eject | 28 | 12,747 | `core/src/`, `engine/src/`, `mac/Notch/Sources/System/` +2 dirs |
| `rightkit-system-monitor` | crate | NEW | tests | 2 | 465 | `core/tests/` |
| `rightkit-system-monitor` | crate | NEW | process probe | 1 | 120 | `mac/Sources/PulseProbe/` |
| `rightkit-web-session` | crate | NEW | agent browser logins on rightkit-browser | 3 | 2,204 | `core/src/`, `core/src/browser/` |
| `rightkit-web-session` | crate | NEW | Swift browser logins | 3 | 530 | `ios/Sources/Browser/` |
| `@rightkit/ax` | npm | EXTEND | agent plugin launcher | 2 | 243 | `plugin-launcher/`, `plugin-launcher/src/` |
| `@rightkit/outdated` | npm | EXTEND | upstream donor check | 4 | 324 | `scripts/` |
| `@rightkit/qa` | npm | EXTEND | probes, fixtures, win-installed crawler, protected keys | 23 | 3,170 | `scripts/`, `scripts/candidate/`, `scripts/fixtures/apfs/` +4 dirs |
| `@rightkit/react-shell` | npm | EXTEND | app frame composition, splash, engine API client (app keeps a short composition) | 3 | 1,352 | `hub/src/` |
| `@rightkit/release` | npm | EXTEND | server deploy scripts | 1 | 328 | `deploy/hetzner/` |
| `@rightkit/release` | npm | EXTEND | release staging, settings codegen, fonts | 4 | 281 | `scripts/fonts/`, `scripts/release/` |
| `@rightkit/release` | npm | EXTEND | release config logic moves into @rightkit/release; app keeps a JSON data config (P8) | 1 | 257 | `./` |
| `@rightkit/settings-ui` | npm | EXTEND | settings sections | 6 | 2,950 | `hub/src/views/` |
| `@rightkit/settings-ui` | npm | EXTEND | devices, pairing, guests, nearby | 4 | 1,716 | `hub/src/views/` |
| `@rightkit/theme` | npm | EXTEND | icon-pack builder (all platforms) from brand source | 2 | 445 | `brand/icon-pack/tools/`, `brand/icon-pack/web/` |
| `@rightkit/theme` | npm | EXTEND | app stylesheet: brand tokens & shell css from @rightkit/theme / react-shell | 3 | 335 | `hub/src/` |
| `RightKitHub` | Swift | EXTEND | API gap first: see docs/rightkit-exceptions/M6.md#b8 | 1 | 437 | `swift/RightKitHub/Sources/RightKitEngineClient/` |
| `RightKitHubQA` | Swift | EXTEND | hub fixtures, renderer, AX tour | 16 | 2,277 | `mac/Hub/Sources/PulseHubFixtures/`, `mac/Hub/Sources/pulse-hub-render/`, `mac/Hub/Sources/pulse-hub-tour/` |
| `rightkit-agents` | crate | EXTEND | Claude Code/Codex transcript discovery & normalisation | 15 | 2,835 | `membrane/engine/crates/membrane-transcript/`, `membrane/engine/crates/membrane-transcript/src/`, `membrane/engine/crates/membrane-transcript/tests/` |
| `rightkit-android` | crate | EXTEND | Android app scaffold: activity, permissions, screens, QR scanner, mesh client | 13 | 1,334 | `android/app/src/main/kotlin/dev/orthic/pulse/android/`, `android/app/src/main/kotlin/dev/orthic/pulse/android/mesh/`, `android/app/src/main/kotlin/dev/orthic/pulse/android/ui/` |
| `rightkit-app-settings` | crate | EXTEND | cross-platform settings manifest + codegen (Rust/Swift/Win32/React) | 5 | 7,270 | `hub/src/`, `scripts/`, `settings/` +1 dirs |
| `rightkit-app-settings` | crate | EXTEND | settings CLI | 1 | 651 | `core/src/` |
| `rightkit-automation` | crate | EXTEND | OS permission status & prompts | 2 | 1,069 | `engine/src/`, `mac/Notch/Sources/System/` |
| `rightkit-credentials` | crate | EXTEND | secure prompt | 1 | 114 | `hub/src-tauri/src/` |
| `rightkit-daemon` | crate | EXTEND | resident service host, supervisor, single instance, watchdog, event log | 7 | 3,567 | `engine/src/` |
| `rightkit-fs` | crate | EXTEND | native metadata adapters, bulk dir reads, volume identity, account home | 6 | 1,653 | `core/src/`, `core/src/platform/` |
| `rightkit-identity` | crate | EXTEND | device identity & short-code pairing | 2 | 1,360 | `core/src/devicelink/` |
| `rightkit-job-queue` | crate | EXTEND | activity leases | 1 | 63 | `engine/src/` |
| `rightkit-overlay-win` | crate | EXTEND | Win32 notch surface toolkit: render, text layout, motion, cards, alerts, tool wheel | 24 | 20,798 | `mac/Notch/Sources/Conveniences/`, `windows/src/` |
| `rightkit-overlay-win` | crate | EXTEND | native Win32 notch app host: message loop, hub bridge & launch, lifecycle, placement, RAII, engine client | 8 | 10,429 | `windows/src/` |
| `rightkit-record` | crate | EXTEND | screenshots via WGC | 3 | 4,656 | `windows/src/` |
| `rightkit-spend` | crate | EXTEND | per-project spend budgets | 3 | 1,655 | `core/src/`, `membrane/engine/crates/membrane-core/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call) | 9 | 4,819 | `hub/src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | build identity, icon embedding, tauri config restore | 3 | 347 | `engine/`, `hub/src-tauri/`, `windows/` |
| `rightkit-vault` | crate | EXTEND | agent logins & payment secrets | 1 | 1,984 | `core/src/remote/` |
| `@rightkit/i18n` | npm | ADOPT | i18n | 1 | 7,215 | `windows/src/` |
| `@rightkit/onboarding` | npm | ADOPT | onboarding/legal/licence | 10 | 920 | `hub/src/lib/`, `hub/src/onboarding/` |
| `RightKitHotkeys` | Swift | ADOPT | keyboard | 1 | 151 | `mac/Notch/Sources/Keyboard/` |
| `RightKitHub` | Swift | ADOPT | duplicate copy of RightKit's RightKitHub: delete | 52 | 7,680 | `swift/RightKitHub/`, `swift/RightKitHub/Sources/CRightKitEngine/`, `swift/RightKitHub/Sources/CRightKitEngine/include/` +12 dirs |
| `RightKitUpdate` | Swift | ADOPT | updater | 6 | 703 | `mac/Notch/Sources/Updater/` |
| `RightKitUpdate` | Swift | ADOPT | own updater duplicates RightKitUpdate | 1 | 325 | `mac/Notch/Sources/App/` |
| `rightkit-cache` | crate | ADOPT | cache | 1 | 89 | `engine/src/` |
| `rightkit-child-process` | crate | ADOPT | bounded subprocess | 1 | 124 | `core/src/` |
| `rightkit-crash` | crate | ADOPT | crash | 2 | 227 | `core/src/`, `windows/src/` |
| `rightkit-file-watch` | crate | ADOPT | watch | 1 | 328 | `engine/src/` |
| `rightkit-fsindex` | crate | ADOPT | disk index | 1 | 2,980 | `engine/src/` |
| `rightkit-hotkeys` | crate | ADOPT | hotkeys | 2 | 1,798 | `windows/src/` |
| `rightkit-logs` | crate | ADOPT | diag | 2 | 845 | `engine/src/`, `windows/src/` |
| `rightkit-send` | crate | ADOPT | nearby sharing glue & UI | 10 | 9,261 | `core/src/`, `engine/src/`, `mac/Notch/Sources/Sharing/` +1 dirs |
| `rightkit-tauri-shell` | crate | ADOPT | autostart & update | 2 | 1,640 | `windows/src/` |
| `rightkit-tauri-shell` | crate | ADOPT | shell hosts | 3 | 393 | `hub/src-tauri/src/` |
| `tree-sitter grammars` | other | VENDOR-KEEP | vendored tree-sitter grammars; use crates.io grammar crates | 18 | 71,409 | `membrane/engine/crates/membrane-grammars-vendored/`, `membrane/engine/crates/membrane-grammars-vendored/grammars/ql/`, `membrane/engine/crates/membrane-grammars-vendored/grammars/ql/src/` +8 dirs |
| `none` | other | DROP | hand-rolled zstd/json & vendored zstd: use crates/system lib | 4 | 23,187 | `mac/Notch/Sources/Vendor/zstd/`, `windows/src/` |
| `app-shell` | other | APPTEST | app journey/e2e tests | 5 | 6,539 | `hub/qa-e2e/`, `hub/qa-e2e/tests/`, `qa/release/installed/` +1 dirs |
| `app-shell` | other | APPTEST | app journey/e2e/unit tests of the app binary | 20 | 4,941 | `core/examples/`, `core/tests/`, `ios/UITests/` |
| `app-shell` | other | APPTEST | Notch view snapshots (was `RightKitHubQA`, which holds Hub fakes; app view snapshots stay with the app) | 1 | 1,254 | `mac/Notch/Sources/App/` |
| `app-shell` | other | SHELL | Notch settings screens and the `Preferences` model (was `RightKitUI`; the settings model moves to generated settings on `RightKitStore` once those exist) | 14 | 2,745 | `mac/Notch/Sources/Settings/` |
| `app-shell` | other | SHELL | iOS app entry & views (was `RightKitUI`) | 6 | 845 | `ios/Sources/App/`, `ios/Tests/` |
| `app-shell` | other | SHELL | hub app entry | 9 | 272 | `hub/`, `hub/src/`, `hub/src-tauri/` +1 dirs |
| `app-shell` | other | SHELL | engine manifest | 1 | 130 | `engine/` |
| `app-shell` | other | SHELL | core tests/examples | 1 | 127 | `core/` |
| `app-shell` | other | SHELL | android app | 4 | 114 | `android/`, `android/app/`, `android/gradle/` |
| `app-shell` | other | SHELL | windows manifest | 1 | 113 | `windows/` |
| `app-shell` | other | SHELL | pulse CLI commands, IPC model, presentation | 1 | 57 | `core/src/` |
| `app-shell` | other | CONFIG | app manifests | 5 | 181 | `./`, `plugins/codex/pulse/hooks/` |
| `none` | other | NOTCODE | documentation pages | 25 | 4,528 | `docs/card-design-pass/`, `docs/design-refresh/`, `docs/liquid-motion-pass/` +2 dirs |

## What stays in the app (SHELL)

Entry points, command tables & app-specific wiring over RightKit packages. Largest files:

| File | Lines |
|---|---:|
| `engine/Cargo.toml` | 130 |
| `core/Cargo.toml` | 127 |
| `windows/Cargo.toml` | 113 |
| `hub/src-tauri/Cargo.toml` | 90 |
| `hub/vite.config.ts` | 73 |
| `core/src/context_cmd.rs` | 57 |
| `android/app/build.gradle.kts` | 53 |
| `hub/src-tauri/deny.toml` | 53 |
| `android/gradle/libs.versions.toml` | 39 |
| `android/settings.gradle.kts` | 16 |
| `hub/index.html` | 16 |
| `hub/onboarding.html` | 12 |
| `hub/src-tauri/.cargo/audit.toml` | 10 |
| `hub/src/vite-env.d.ts` | 9 |
| `android/build.gradle.kts` | 6 |
| `hub/src/i18n.ts` | 6 |
| `hub/rust-toolchain.toml` | 3 |
