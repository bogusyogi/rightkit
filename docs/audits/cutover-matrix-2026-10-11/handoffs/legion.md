# Legion — RightKit migration handoff

Source root: /Volumes/D/claude/legion

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/legion.json)

## rightkit-browser — N

N: `legion script` actively launches a direct headless-Chrome driver for visual checks; `rightkit-browser` exposes the same CDP page navigation/interaction runtime. No RightKit browser route is called.

Route visual script browser sessions through rightkit-browser; retain Legion detector policy.

- Local: [run_r07](/Volumes/D/claude/legion/engine/bins/legion/src/commands/script.rs:1136) — live script handler resolves browser then constructs `r07::RealChromeDriver` at line 1154.
- Owner: [page.rs](/Volumes/D/claude/rightkit/crates/rightkit-browser/src/page.rs:352) — `BrowserPage::goto` is callable CDP page navigation within shared BrowserSession runtime.

## rightkit-db — N

N: Legion’s durable decision store opens & initializes SQLite directly; rightkit-db provides configured SQLite bootstrap/recovery. No shared DB bootstrap is used.

Adopt rightkit-db bootstrap beneath Legion’s decision schema & migrations.

- Local: [DecisionStore::open](/Volumes/D/claude/legion/engine/crates/legion-decisions/src/store.rs:46) — live decision-store constructor opens SQLite then initializes schema.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-db/src/lib.rs:175) — `DbOptions::new` starts shared SQLite bootstrap with journal, durability, owner-only & recovery controls.

## rightkit-http — N

N: active Google OAuth workflow creates direct reqwest client requests; rightkit-http exposes `Request` + `Client::send` for same blocking HTTPS request boundary. No shared client route is called.

Replace direct OAuth/SEO HTTP transport with rightkit-http while preserving Google-specific request policy.

- Local: [ReqwestTokenClient](/Volumes/D/claude/legion/engine/crates/legion-runtime/src/wf_port/w2_030/google_auth.rs:668) — live OAuth CLI wires blocking reqwest token POST transport.
- Owner: [client.rs](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:772) — `Client::send` executes shared bounded request; `Request::post` starts at line 411.

## rightkit-mcp — N

N: Legion ships its own stdio JSON-RPC MCP server implementing initialize/tools/list/tools/call; rightkit-mcp provides ServerHost + stdio server transport for same server boundary. No RightKit MCP host is used.

Host Legion tools through rightkit-mcp stdio server adapter; preserve release-binding gate.

- Local: [Server::handle](/Volumes/D/claude/legion/engine/bins/legion-mcp/src/server.rs:125) — production server dispatches MCP initialize, tools/list & tools/call.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-mcp/src/lib.rs:38) — shared exports `ServerHost`, `ServerTransport`, `ToolProvider` & admission interfaces.

## rightkit-oauth — N

N: live `seo/google-auth` owns loopback browser OAuth, code exchange & refresh; rightkit-oauth’s OAuthClient provides those same Google loopback/refresh APIs. No shared OAuth client is called.

Move Google OAuth transport/state mechanics to rightkit-oauth; retain Legion SEO scopes & commands.

- Local: [run_oauth_flow](/Volumes/D/claude/legion/engine/crates/legion-runtime/src/wf_port/w2_030/google_auth.rs:804) — active CLI opens browser, binds loopback listener & exchanges Google auth code.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-oauth/src/lib.rs:353) — `OAuthClient::begin_loopback`, `finish_loopback`, `exchange_code` & `refresh` are callable shared APIs.

## rightkit-process — N

N: Legion’s effect executor owns child spawn, process group, cancellation & descendant cleanup; rightkit-process owns safe child lifecycle/restart primitives. No shared process lifecycle route is used.

Adopt rightkit-process child containment beneath Legion effect policy & receipts.

- Local: [run_process](/Volumes/D/claude/legion/engine/crates/legion-effects/src/unix.rs:47) — live governed-effects executor spawns child, isolates process group & handles cancellation.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-process/src/lib.rs:19) — shared exports `OwnedCommand`, `OwnedChild`, `TerminationOptions` & restart/health lifecycle APIs.

## rightkit-qa — N

N: `legion script qa` runs visual browser QA using local Chrome lifecycle; rightkit-qa exposes BrowserTarget launch/page/goto for same browser-QA execution boundary. No shared QA runner is called.

Port Legion browser-QA runner onto rightkit-qa; keep Legion’s product-specific assertions.

- Local: [run_r54](/Volumes/D/claude/legion/engine/bins/legion/src/commands/script.rs:2352) — active CLI dispatches r54 browser QA runner.
- Owner: [browser.rs](/Volumes/D/claude/rightkit/crates/rightkit-qa/src/browser.rs:115) — `BrowserTarget::launch` creates shared QA browser target; `goto` starts at line 188.

## rightkit-sandbox — N

N: Legion implements macOS sandbox-exec profiles for governed audit commands; rightkit-sandbox exposes reusable SandboxPolicy/Sandbox spawn boundary. No shared sandbox is called.

Use rightkit-sandbox for process isolation; preserve Legion audit network/write policy.

- Local: [authenticate](/Volumes/D/claude/legion/engine/crates/legion-effects/src/sandbox.rs:68) — live macOS path writes sandbox profile & wraps command in `/usr/bin/sandbox-exec`.
- Owner: [lib.rs](/Volumes/D/claude/rightkit/crates/rightkit-sandbox/src/lib.rs:39) — shared exports `SandboxCommand`, `SandboxPolicy`, `Sandbox` & `SandboxOutput`.

## rightkit-secrets — N

N: Legion’s active Google OAuth command persists tokens as local JSON; rightkit-secrets exposes OS credential read/write APIs for same credential persistence need. No shared secret store is used.

Store OAuth refresh tokens via rightkit-secrets; preserve SEO account selection.

- Local: [run_with_io](/Volumes/D/claude/legion/engine/crates/legion-runtime/src/wf_port/w2_030/google_auth.rs:1050) — successful CLI OAuth exchange saves token through `save_oauth_token_file`.
- Owner: [apple_helper.rs](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/apple_helper.rs:141) — `AppleHelper::write` persists a `Secret`; paired `read` starts at line 127.

## @rightkit/ax — N

N: native release assembly calls Legion’s Rust port of RightKit AX portable-core assembly/validation, duplicating owner’s portable-core API instead of invoking package.

Call @rightkit/ax portable-core from release assembly or isolate a versioned bridge; remove Rust port after parity proof.

- Local: [assemble_portable_core](/Volumes/D/claude/legion/engine/xtask/src/assemble_native_release.rs:734) — production native release assembly invokes Legion local portable-core implementation, then validates it.
- Owner: [portable-core.mjs](/Volumes/D/claude/rightkit/packages/ax/plugin/portable-core.mjs:83) — owner exports `validateResourceClosure`; same module defines portable-core schema & assembly functions.

Delivery: @rightkit/ax 0.2.4 is declared only as devDependency; native assembly calls local Rust port.

## RightKitArtifacts — N

N: Cocoa installer locally stages bundled directory, replaces a version directory, switches `current`, then validates installed CLI. RightKitArtifacts covers verified tar.gz stage/activate/recover but has no production route; its archive & fixed-top-level contract requires Legion payload packaging/layout change.

Package assembled payload as digest-pinned `.tar.gz` with one stable top-level directory; map Legion version/current semantics & post-activation repair/status/doctor validation before routing installer through ArtifactInstaller.

- Local: [install](/Volumes/D/claude/legion/scripts/release/macos/LegionInstaller.swift:37) — Stages bundled `payload`, replaces `versions/<version>`, atomically switches `current` symlink & runs repair/status/doctor validation.
- Owner: [ArtifactInstaller.install](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitArtifacts/ArtifactInstaller.swift:146) — Owner verifies a SHA-256-pinned .tar.gz with one top-level directory, stages, activates, validates & recovers interrupted installs.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
