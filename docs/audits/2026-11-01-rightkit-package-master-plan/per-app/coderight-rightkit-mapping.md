# CodeRight: RightKit mapping

Repo `coderight` at `a642f3ff` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`coderight-rightkit-files.json`](coderight-rightkit-files.json).

Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).

## Summary

| Action | Meaning | Files | Lines |
|---|---|---:|---:|
| NEW | new RightKit package (or new piece of one) | 2,027 | 571,755 |
| EXTEND | existing RightKit package gains this | 617 | 257,545 |
| ADOPT | RightKit already ships it; delete app copy & use owner | 106 | 27,012 |
| APPTEST | app journey/e2e test: stays with the app (P7, §6), runs on rightkit-qa | 282 | 66,200 |
| VENDOR-KEEP | vendored 3rd-party: keep as plain dependency, delete vendored copy | 66 | 26,679 |
| DROP | delete | 46 | 13,415 |
| SHELL | thin app composition that stays in app | 12 | 568 |
| CONFIG | app manifest/config that stays | 12 | 553 |
| NOTCODE | docs/data, not code | 25 | 28,911 |
| **Total** | | **3,193** | **992,638** |

Code leaving the app (NEW + EXTEND + ADOPT + LEGION + vendor + DROP): **896,406 lines**. Staying in the app as product code: **0 lines**; as shell/config: **1,121 lines** (0% of code), plus **66,200 lines** of app journey/e2e tests.

## Mapping table

| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |
|---|---|---|---|---:|---:|---|
| `@rightkit/agent-ui` | npm | NEW | agent workbench UI: chat timeline, composer, sessions, runtime/goals/memory/review panels, approvals, plugins/skills, orchestration, bots, environments, state stores | 666 | 113,422 | `apps/coderight-tauri/src/components/`, `apps/coderight-tauri/src/components/app/`, `apps/coderight-tauri/src/components/app/composerIsland/` +62 dirs |
| `@rightkit/agent-ui` | npm | NEW | remote web client | 27 | 3,421 | `apps/coderight-web/`, `apps/coderight-web/public/`, `apps/coderight-web/scripts/` +3 dirs |
| `@rightkit/agent-ui` | npm | NEW | computer-use & browser panels | 5 | 1,687 | `apps/coderight-tauri/src/components/browser/`, `apps/coderight-tauri/src/hooks/browser/` |
| `@rightkit/config` | npm | NEW | shared presets | 1 | 4 | `./` |
| `@rightkit/dictation-ui` | npm | NEW | voice conversation | 6 | 1,090 | `apps/coderight-tauri/src/components/`, `apps/coderight-tauri/src/hooks/` |
| `@rightkit/terminal-ui` | npm | NEW | xterm terminal pane | 6 | 1,370 | `apps/coderight-tauri/src/components/`, `apps/coderight-tauri/src/hooks/` |
| `RightKitAgent` | Swift | NEW | Swift agent client: local runtime bridge, chat library, code editor, diff walkthrough, plan review, markdown rendering, approvals | 154 | 30,723 | `apps/coderight-ios/CodeRight/Approvals/`, `apps/coderight-ios/CodeRight/CommandPalette/`, `apps/coderight-ios/CodeRight/Demo/` +17 dirs |
| `rightkit-adapt` | crate | NEW | post-session learning pipeline | 31 | 8,073 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/learning/`, `engine/crates/learning/` +2 dirs |
| `rightkit-agent-engine` | crate | NEW | agent loop, edit history & diffs, task graph, runtime middleware & bounded resources, attention, session storage, context folds/rehydrate, trajectory journal, wire protocol | 262 | 99,073 | `engine/bins/coderight/src/`, `engine/crates/api_integrations/`, `engine/crates/api_integrations/src/` +36 dirs |
| `rightkit-agent-engine` | crate | NEW | interactive & headless session host | 6 | 13,951 | `engine/bins/coderight/src/` |
| `rightkit-agent-engine` | crate | NEW | embedded agent runtime for fork-less hosts (iOS) | 14 | 11,065 | `engine/crates/mobile_runtime/`, `engine/crates/mobile_runtime/src/` |
| `rightkit-agent-orchestration` | crate | NEW | multi-agent coordination: subagents, teams, delegation, roster | 120 | 37,582 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/orchestrator/`, `engine/bins/coderight/src/orchestrator/helpers/` +15 dirs |
| `rightkit-agent-orchestration` | crate | NEW | agent room | 1 | 288 | `apps/coderight-tauri/src-tauri/src/` |
| `rightkit-agent-policy` | crate | NEW | pure permission decision engine, destructive-action guard, policy packs, audit | 47 | 16,018 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_permissions/`, `engine/crates/coderight-legion-policy/` +6 dirs |
| `rightkit-agent-server` | crate | NEW | agent daemon HTTP API: sessions, runtime, contract/OpenAPI, tool invocations, run changes, health, workspace, teams, coordination | 189 | 64,253 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_contract/`, `engine/bins/coderight/src/api_contract/endpoints/` +8 dirs |
| `rightkit-agent-server` | crate | NEW | agent CLI & daemon runtime: CLI definitions, run/exec input/output/exit codes, config, presence, recovery, remote setup, project binding, skill/plugin/recipe routes over the installed Legion | 75 | 19,187 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_plugins/`, `engine/bins/coderight/src/api_recipes/` +1 dirs |
| `rightkit-agent-server` | crate | NEW | server & daemon client runtime | 11 | 5,853 | `engine/bins/coderight/src/` |
| `rightkit-agent-server` | crate | NEW | OpenAPI client codegen & contract checks | 16 | 2,246 | `apps/coderight-tauri/scripts/` |
| `rightkit-bench` | crate | NEW | agent benchmarks | 14 | 5,329 | `engine/bins/coderight/src/api_benchmarks/` |
| `rightkit-bot-messaging` | crate | NEW | remote access: Telegram bot, web auth, remote gateway | 33 | 6,489 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_remote/`, `engine/bins/coderight/src/telegram/` +4 dirs |
| `rightkit-doctor` | crate | NEW | doctor dimensions & probes | 7 | 1,652 | `engine/bins/coderight/src/` |
| `rightkit-evidence` | crate | NEW | spec-governed artifacts & coverage | 29 | 5,738 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_specs/`, `engine/bins/coderight/src/api_specs/evidence_protocol/` +4 dirs |
| `rightkit-github` | crate | NEW | GitHub App: auth, webhooks, PR/issue ops | 12 | 5,127 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/github_app/` |
| `rightkit-github` | crate | NEW | Swift forge connectors: GitHub, Cloudflare, Fly.io, git, PRs | 16 | 3,459 | `apps/coderight-ios/CodeRight/LocalRuntime/` |
| `rightkit-memory` | crate | NEW | adaptive memory retrieval & backend (over context engine) | 32 | 9,759 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/membrane_binding/`, `engine/crates/coderight-membrane-cli/` +7 dirs |
| `rightkit-mesh` | crate | NEW | pairing | 5 | 605 | `apps/coderight-ios/CodeRight/Pairing/` |
| `rightkit-repo-graph` | crate | NEW | tree-sitter skeletonization | 2 | 600 | `engine/crates/skeletonization/`, `engine/crates/skeletonization/src/` |
| `rightkit-scheduler` | crate | NEW | cron schedules & triggers for agent runs | 33 | 9,315 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_schedules/`, `engine/bins/coderight/src/api_triggers/` +4 dirs |
| `rightkit-terminal-ui` | crate | NEW | terminal UI toolkit: composer, transcript, wrapping, panes, markdown render, image input, render metrics | 207 | 94,376 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/tui_host/`, `engine/crates/tui/` +18 dirs |
| `@rightkit/legal` | npm | EXTEND | API gap first: M14.md B4 native legal manifest digest: rightkit-tauri-shell 0.6.0 LegalManifest::new takes precomputed values | 1 | 111 | `apps/coderight-tauri/src-tauri/src/` |
| `@rightkit/qa` | npm | EXTEND | QA & surface checks | 32 | 2,987 | `apps/coderight-tauri/scripts/`, `apps/coderight-tauri/scripts/lib/` |
| `@rightkit/react-shell` | npm | EXTEND | shell chrome, palette, pane boundaries, i18n | 26 | 2,739 | `apps/coderight-tauri/src/components/`, `apps/coderight-tauri/src/components/nativeWindows/`, `apps/coderight-tauri/src/hooks/` +4 dirs |
| `@rightkit/react-shell` | npm | EXTEND | app frame composition, splash, engine API client (app keeps a short composition) | 2 | 1,315 | `apps/coderight-tauri/src/` |
| `@rightkit/release` | npm | EXTEND | repo scripts | 4 | 1,240 | `scripts/` |
| `@rightkit/release` | npm | EXTEND | release config logic moves into @rightkit/release; app keeps a JSON data config (P8) | 2 | 257 | `apps/coderight-ios/`, `apps/coderight-tauri/` |
| `@rightkit/theme` | npm | EXTEND | design token extraction & context tools | 8 | 1,884 | `tools/`, `tools/context/` |
| `@rightkit/theme` | npm | EXTEND | contrast/a11y/theme gates | 8 | 1,413 | `apps/coderight-tauri/scripts/` |
| `@rightkit/theme` | npm | EXTEND | app stylesheet: brand tokens & shell css from @rightkit/theme / react-shell | 1 | 494 | `apps/coderight-tauri/src/` |
| `@rightkit/theme` | npm | EXTEND | Swift tokens | 5 | 342 | `apps/coderight-ios/CodeRight/Theme/` |
| `RightKitFS` | Swift | EXTEND | API gap first: M11.md I-3 synced-credential policy: in the RightKit working tree but untagged; RightKitSwift 0.8.1 is the lat | 1 | 203 | `apps/coderight-ios/CodeRight/Storage/` |
| `RightKitHTTP` | Swift | EXTEND | API gap first: M7.md B3 iOS SSE frame parser and replay: RightKitSwift 0.8.1 has no SSE client. | 1 | 67 | `apps/coderight-ios/CodeRight/Networking/` |
| `RightKitLicense` | Swift | EXTEND | API gap first: M12.md B5 iOS ContinuityBridgeClient: in the RightKit working tree but untagged; RightKitSwift 0.8.1 is the la | 1 | 183 | `apps/coderight-ios/CodeRight/Purchases/` |
| `RightKitLiveActivity` | Swift | EXTEND | widgets | 2 | 173 | `apps/coderight-ios/CodeRight/Widgets/` |
| `RightKitUI` | Swift | EXTEND | iOS app scaffold: app entry, content/tab views, sheets, menus, settings & account, update, haptics, shortcuts, widgets, file provider | 8 | 835 | `apps/coderight-ios/CodeRight/`, `apps/coderight-ios/CodeRightWidgets/`, `apps/coderight-ios/FileProviderExtension/` |
| `rightkit-agent-tools` | crate | EXTEND | coding tool suite: fs read/edit, shell, search, git, LSP, web, browser, MCP bridge, media | 194 | 86,121 | `engine/crates/tools/`, `engine/crates/tools/examples/`, `engine/crates/tools/src/` +18 dirs |
| `rightkit-agent-tools` | crate | EXTEND | LSP API | 1 | 162 | `engine/bins/coderight/src/` |
| `rightkit-agents` | crate | EXTEND | vendor coding drivers (Claude Code, Codex, Cursor, Cline, Grok, ACP), resumable NDJSON & JSON-RPC transports, login routes | 80 | 42,659 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/acp/`, `engine/bins/coderight/src/api_drivers/` +11 dirs |
| `rightkit-agents` | crate | EXTEND | vendor turn | 1 | 1,799 | `engine/bins/coderight/src/` |
| `rightkit-app-settings` | crate | EXTEND | layered TOML config & hot reload | 29 | 10,622 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_settings/`, `engine/bins/coderight/src/api_settings/tests/` +2 dirs |
| `rightkit-browser` | crate | EXTEND | browser workspace for agents | 7 | 3,460 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_browser_workspace/` |
| `rightkit-daemon` | crate | EXTEND | daemon lifecycle, local services, presence | 25 | 10,044 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/daemon/`, `engine/crates/daemon_core/` +3 dirs |
| `rightkit-daemon` | crate | EXTEND | daemon client: app discovery, connect | 5 | 3,092 | `apps/coderight-tauri/src-tauri/crates/daemon-client/`, `apps/coderight-tauri/src-tauri/crates/daemon-client/src/` |
| `rightkit-ffmpeg` | crate | EXTEND | API gap first: M5.md A163 bounded runner pid: rightkit-child-process 0.5.6 RunOutput has no pid and no spawn callback (run.rs | 1 | 981 | `engine/crates/media/src/` |
| `rightkit-hooks` | crate | EXTEND | lifecycle hook bus & runner | 8 | 5,423 | `engine/bins/coderight/src/`, `engine/crates/hooks/`, `engine/crates/hooks/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: M12.md K5 Apple Pro bridge flow (state, launch order, helper seam, commands): not in rightkit-license 0.2.5 or | 1 | 914 | `apps/coderight-tauri/src-tauri/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: M13.md A472 device binding of a stored record: rightkit-license 0.2.5 EngineEntitlementsConfig takes one devic | 1 | 622 | `engine/bins/coderight/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: M12.md K4 Windows link-code redemption: rightkit-tauri-shell 0.6.0 licence host has no link commands (host/lic | 1 | 332 | `apps/coderight-tauri/src-tauri/src/` |
| `rightkit-llm` | crate | EXTEND | provider clients, replay, heuristic model routing, provider setup presets, local inference runtime | 57 | 33,990 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/inference_runtime/`, `engine/crates/providers/` +8 dirs |
| `rightkit-mcp` | crate | EXTEND | API gap first: outside-M-rows.md H-1 strict percent-decode of untrusted URL text: rightkit-http 0.3.8 percent_decode and perc | 1 | 332 | `engine/crates/mcp/src/lsp_bridge/` |
| `rightkit-qa` | crate | EXTEND | terminal/PTY/ConPTY capture & reconcile, dogfood harness, soak runner | 24 | 6,715 | `dogfood/`, `dogfood/harness/`, `handoff/` |
| `rightkit-qa` | crate | EXTEND | dogfood inventory | 1 | 139 | `engine/bins/coderight/src/` |
| `rightkit-sandbox` | crate | EXTEND | execution backends, environments, worktree pool | 24 | 7,177 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_execution/`, `engine/crates/execution/` +1 dirs |
| `rightkit-sidecar` | crate | EXTEND | stream transport | 1 | 3 | `apps/coderight-tauri/src-tauri/src/stream_transport/` |
| `rightkit-tauri-shell` | crate | EXTEND | Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call) | 2 | 2,426 | `apps/coderight-tauri/src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: M12.md U2-U4 update host: requirement-ineligible entitlement fallback, split check/download timeouts and relau | 1 | 210 | `apps/coderight-tauri/src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | build identity, icon embedding, tauri config restore | 1 | 3 | `apps/coderight-tauri/src-tauri/` |
| `rightkit-workflow` | crate | EXTEND | autonomous workflow control plane: runs, leases, fences, node executors | 49 | 26,076 | `engine/bins/coderight/src/api_workflows/`, `engine/bins/coderight/src/workflow/`, `engine/bins/coderight/src/workflow/triggers/` +4 dirs |
| `@rightkit/onboarding` | npm | ADOPT | onboarding | 6 | 789 | `apps/coderight-tauri/src/`, `apps/coderight-tauri/src/components/onboarding/` |
| `RightKitAppleCredentials` | Swift | ADOPT | Apple credentials helper | 2 | 321 | `apps/coderight-tauri/macos-native/CodeRightMac/`, `apps/coderight-tauri/macos-native/CodeRightMac/Sources/CodeRightAppleCredentials/` |
| `RightKitDiagnostics` | Swift | ADOPT | diagnostics | 9 | 886 | `apps/coderight-ios/CodeRight/Analytics/`, `apps/coderight-ios/CodeRight/Debug/`, `apps/coderight-ios/CodeRight/Diagnostics/` +1 dirs |
| `RightKitFS` | Swift | ADOPT | storage | 6 | 643 | `apps/coderight-ios/CodeRight/Storage/` |
| `RightKitHTTP` | Swift | ADOPT | networking | 4 | 577 | `apps/coderight-ios/CodeRight/CallbackURL/`, `apps/coderight-ios/CodeRight/Networking/` |
| `RightKitLicense` | Swift | ADOPT | account, licence, purchases | 10 | 1,576 | `apps/coderight-ios/CodeRight/Account/`, `apps/coderight-ios/CodeRight/License/`, `apps/coderight-ios/CodeRight/Purchases/` |
| `RightKitSpeech` | Swift | ADOPT | dictation | 1 | 121 | `apps/coderight-ios/CodeRight/Dictation/` |
| `RightKitUpdate` | Swift | ADOPT | updates | 2 | 127 | `apps/coderight-ios/CodeRight/Updates/` |
| `rightkit-daemon` | crate | ADOPT | daemon lifecycle | 2 | 6 | `apps/coderight-tauri/src-tauri/src/`, `apps/coderight-tauri/src-tauri/src/app_discovery/` |
| `rightkit-ffmpeg` | crate | ADOPT | media tool adapters | 1 | 16 | `engine/crates/media/` |
| `rightkit-file-watch` | crate | ADOPT | fs watch | 1 | 460 | `engine/bins/coderight/src/` |
| `rightkit-http` | crate | ADOPT | http | 1 | 186 | `engine/bins/coderight/src/` |
| `rightkit-license` | crate | ADOPT | licence & auth | 3 | 1,663 | `engine/bins/coderight/src/` |
| `rightkit-license` | crate | ADOPT | licence | 1 | 92 | `apps/coderight-tauri/src-tauri/src/` |
| `rightkit-logs` | crate | ADOPT | telemetry | 11 | 4,041 | `engine/crates/telemetry/`, `engine/crates/telemetry/src/` |
| `rightkit-logs` | crate | ADOPT | logs | 1 | 34 | `apps/coderight-tauri/src-tauri/src/` |
| `rightkit-mcp` | crate | ADOPT | MCP client/server | 28 | 10,638 | `engine/bins/coderight/src/`, `engine/bins/coderight/src/api_mcp/`, `engine/crates/mcp/` +2 dirs |
| `rightkit-models` | crate | ADOPT | local model cookbook | 3 | 1,853 | `engine/bins/coderight/src/`, `engine/crates/local_models/`, `engine/crates/local_models/src/` |
| `rightkit-oauth` | crate | ADOPT | token store & native OAuth | 3 | 579 | `apps/coderight-tauri/src-tauri/crates/auth/src/` |
| `rightkit-oauth` | crate | ADOPT | oauth & tokens | 2 | 230 | `apps/coderight-tauri/src-tauri/src/` |
| `rightkit-onnx` | crate | ADOPT | ORT adapters | 2 | 205 | `engine/crates/ort_common/`, `engine/crates/ort_common/src/` |
| `rightkit-tauri-shell` | crate | ADOPT | shell | 6 | 1,593 | `apps/coderight-tauri/src-tauri/src/`, `apps/coderight-tauri/src-tauri/src/native_windows/`, `apps/coderight-tauri/src-tauri/src/window_state/` |
| `rightkit-tauri-shell` | crate | ADOPT | notifications | 1 | 376 | `engine/bins/coderight/src/` |
| `WasmKit` | other | VENDOR-KEEP | vendored WasmKit: use SwiftPM dependency | 66 | 26,679 | `apps/coderight-ios/Vendor/WasmKit/`, `apps/coderight-ios/Vendor/WasmKit/Sources/CWASIPlatform/`, `apps/coderight-ios/Vendor/WasmKit/Sources/CWASIPlatform/include/` +11 dirs |
| `legion` | Legion | DROP | delete: CodeRight uses the installed Legion (skill manifests & trigger DSL, plugin marketplace & loader, recipes, custom commands) | 42 | 11,095 | `engine/crates/plugins/`, `engine/crates/plugins/src/`, `engine/crates/recipes/` +6 dirs |
| `none` | other | DROP | unused ssh tunnel (deletion held) | 2 | 2,103 | `apps/coderight-tauri/src-tauri/crates/daemon-client/src/`, `apps/coderight-tauri/src-tauri/src/` |
| `none` | other | DROP | unreachable mini chat | 2 | 217 | `apps/coderight-tauri/src/components/` |
| `app-shell` | other | APPTEST | app journey/e2e tests | 242 | 56,262 | `apps/coderight-ios/Tests/`, `apps/coderight-ios/UITests/`, `apps/coderight-ios/UITests/Fixtures/` +16 dirs |
| `app-shell` | other | APPTEST | app journey/e2e/unit tests of the app binary | 40 | 9,938 | `apps/coderight-tauri/src/`, `apps/coderight-tauri/src-tauri/capabilities/`, `apps/coderight-tauri/tests/` +7 dirs |
| `app-shell` | other | SHELL | tauri app | 8 | 421 | `apps/coderight-tauri/`, `apps/coderight-tauri/src-tauri/`, `apps/coderight-tauri/src-tauri/.cargo/` +1 dirs |
| `app-shell` | other | SHELL | CLI definitions, main runtime, interactive session host | 1 | 126 | `engine/bins/coderight/` |
| `app-shell` | other | SHELL | app entry | 3 | 21 | `apps/coderight-tauri/src/` |
| `app-shell` | other | CONFIG | engine workspace | 7 | 370 | `engine/`, `engine/.cargo/`, `engine/fixtures/role-packs/` |
| `app-shell` | other | CONFIG | app manifests | 5 | 183 | `assets/`, `assets/artifacts/`, `assets/harness-radar/` +2 dirs |
| `none` | other | NOTCODE | generated third-party notices (each app ships its own; produced by @rightkit/legal) | 4 | 26,163 | `apps/coderight-tauri/legal/`, `apps/coderight-tauri/legal/third-party/` |
| `none` | other | NOTCODE | documentation pages | 9 | 2,200 | `docs/mockups/`, `docs/mockups/ios-claude-chats/`, `docs/pending/workflow-control-plane-support/` +1 dirs |
| `none` | other | NOTCODE | doc/prototype page | 12 | 548 | `apps/coderight-ios/design/prototype/`, `apps/coderight-ios/design/prototype/artifacts/qa/scripts/` |

## What stays in the app (SHELL)

Entry points, command tables & app-specific wiring over RightKit packages. Largest files:

| File | Lines |
|---|---:|
| `apps/coderight-tauri/src-tauri/Cargo.toml` | 141 |
| `engine/bins/coderight/Cargo.toml` | 126 |
| `apps/coderight-tauri/vite.config.ts` | 87 |
| `apps/coderight-tauri/right-release.headless.config.mjs` | 83 |
| `apps/coderight-tauri/src-tauri/deny.toml` | 47 |
| `apps/coderight-tauri/about.toml` | 30 |
| `apps/coderight-tauri/src/qaBuild.ts` | 14 |
| `apps/coderight-tauri/src-tauri/crates/auth/Cargo.toml` | 13 |
| `apps/coderight-tauri/index.html` | 12 |
| `apps/coderight-tauri/src-tauri/.cargo/audit.toml` | 8 |
| `apps/coderight-tauri/src/vite-env.d.ts` | 4 |
| `apps/coderight-tauri/src/test-node-shims.d.ts` | 3 |
