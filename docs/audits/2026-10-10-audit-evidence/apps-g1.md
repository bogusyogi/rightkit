# Apps group 1 audit: CodeRight, MailRight, ViewRight (RightKit adoption inventory)

Date: 2026-10-10. Read-only. Method: manifests, grep, `wc -l`, `right-release ownership` scanner. No cargo/pnpm/builds run.
Reference: `rightkit/INDEX.md`, `rightkit/docs/CAPABILITY-MAP.md`, `docs/audits/2026-10-08-inventory-final.md:148-171`, registry versions from `rightkit/packages/release/rightkit-versions.json` (`registry` section; `stagedCargo` carries browser 0.2.3, sync 0.1.0 not yet in registry).

Caveats that apply to all three apps:
- The 2026-10-08 Wave 3 table is partly stale. Already done in CodeRight: `coderight-cache`, `coderight-ocr` (now `rightkit-ocr` native), Windows job objects (`rightkit_process::WindowsJob`, `engine/crates/tools/src/sandbox/implementation.rs:4`), the literal `D:/work` tree (no tracked path under `engine/bins/coderight`). Already done in MailRight: `rightkit-cache` (`src-tauri/src/attach_cache.rs:16`) and `rightkit-secrets` (`src-tauri/src/credential_store.rs:10`).
- All 0.x minor bumps are treated as potentially breaking.
- LOC "touched" figures are file-size upper bounds unless marked "est".
- A shell hook blocks command text containing certain credential words and the ownership-exceptions file, so those paths were inspected by file/directory names only, not by pattern grep.

---

## CodeRight

### 1. Shape
Last commit 2026-10-10 (`f96b73921`). Working tree: 2 modified receipt/doc files under `context/docs/`. Targets: macOS (aarch64), Windows, Linux (engine/TUI), iOS (Swift app, separate).

| Manifest | Contents | Rust/TS LOC |
|---|---|---|
| `engine/Cargo.toml` | workspace: 46 crates + `bins/coderight` (daemon/CLI/TUI host). Largest: bins/coderight 275,807; tools 83,839; tui 71,257; engine 56,954; orchestration 31,223; driver_host 27,978; providers 24,698; storage 18,887 | ~700k |
| `apps/coderight-tauri/src-tauri/Cargo.toml` | Tauri shell 14,255 + `crates/auth` 506 + `crates/daemon-client` 4,842 (boot_probation 1,741, app_discovery 1,025, ssh_tunnel 764, daemon_lifecycle 653, stream_transport 649) | 19.6k |
| `apps/coderight-tauri/package.json` | React front end (`src/` 126,077 LOC TS/TSX) | 126k |
| `context/engine/Cargo.toml` | Membrane/cortex workspace: 18 crates (membrane-runtime 109,430; blueprint 47,232; adapt 16,672; membrane 16,589; federation 11,870; cortex-store 11,460; membrane-mcp 3,355) | ~245k |
| `apps/coderight-web/package.json` | web app, 2,157 LOC, no RightKit dependency | 2.2k |
| `apps/coderight-ios` | Swift app, `project.yml:26-28` RightKitSwift `from: 0.1.0` (registry 0.1.1); products RightKitLicense, RightKitDiagnostics | n/a |
| `context/package.json` | npm tooling (`@rightkit/hooks`, `ax`, `git`, `release`) | n/a |

### 2. RightKit adoption and pins (registry latest in last column)
| Manifest:line | Package | Pinned | Registry latest | Verdict |
|---|---|---|---|---|
| engine/Cargo.toml:150 | rightkit-watch | =0.1.0 | 0.3.1 | STALE (breaking minor); lock also holds 0.3.0 via context |
| engine/Cargo.toml:151 | rightkit-fs | =0.1.0 | 0.3.3 | STALE (breaking) |
| engine/Cargo.toml:152 | rightkit-oauth | =0.2.0 | 0.3.0 | STALE (breaking) |
| engine/Cargo.toml:153 | rightkit-http | =0.1.0 | 0.2.4 | STALE (breaking) |
| engine/Cargo.toml:154 | rightkit-mcp | =0.5.1 | 0.5.2 | stale patch |
| engine/Cargo.toml:155 | rightkit-secrets | =0.1.0 | 0.1.2 | stale patch |
| engine/Cargo.toml:156 | rightkit-cache | =0.1.0 | 0.1.0 | ok |
| engine/Cargo.toml:157 | rightkit-ort | =0.2.0 | 0.2.2 | stale patch |
| engine/Cargo.toml:158 | rightkit-db | =0.2.1 | 0.3.2 | STALE (breaking); engine lock carries both 0.2.1 and 0.3.0 |
| engine/Cargo.toml:159 | rightkit-browser | =0.2.0 | 0.2.2 (staged 0.2.3) | stale patch |
| engine/crates/tools/Cargo.toml:82 | rightkit-sandbox | =0.2.4 | 0.3.3 | STALE (breaking) |
| engine/crates/tools/Cargo.toml:83 | rightkit-process | =0.3.2 | 0.3.3 | stale patch (lock also has 0.2.1) |
| engine/crates/media/Cargo.toml:8 | rightkit-ocr | 0.1.2 | 0.1.2 | ok |
| engine/crates/support_bundle/Cargo.toml:11 | rightkit-logs | =0.2.0 | 0.2.0 | ok |
| apps/coderight-tauri/src-tauri/Cargo.toml:48 | rightkit-license | =0.1.3 | 0.1.3 | ok |
| :49 | rightkit-logs | =0.2.0 | 0.2.0 | ok |
| :50 and crates/auth/Cargo.toml:10 | rightkit-tauri | =0.1.1 | 0.1.2 | stale patch (x2) |
| :51 | rightkit-shell | =0.1.2 | 0.1.4 | stale patch |
| :52 | rightkit-ort | =0.2.0 | 0.2.2 | stale patch |
| context/engine/crates/cortex-store:11, cortex:14, membrane-blueprint:52, membrane-runtime:22, membrane-transcript:16 | rightkit-db | =0.3.0 | 0.3.2 | stale patch (5 manifests) |
| context/engine/crates/membrane-blueprint/Cargo.toml:50 | rightkit-embed | =0.2.2 | 0.3.1 | STALE (breaking) |
| membrane-blueprint:51, membrane-runtime:48 | rightkit-ort | =0.2.0 | 0.2.2 | stale patch |
| membrane-blueprint:53 | rightkit-watch | =0.3.0 | 0.3.1 | stale patch |
| membrane-mcp:14 | rightkit-mcp | =0.5.1 | 0.5.2 | stale patch |
| membrane-runtime:23 | rightkit-process | =0.3.2 | 0.3.3 | stale patch |
| apps/coderight-tauri/package.json:127 | @rightkit/legal | 0.3.1 | 0.3.1 | ok |
| :128, :129 | @rightkit/license, logs | 0.1.8, 0.1.4 | same | ok |
| :130 | @rightkit/shell | 0.1.0 | 0.1.0 | ok (declared, no import found in src) |
| :131 | @rightkit/tauri | 0.1.1 | 0.2.0 | STALE (breaking) |
| :132 | @rightkit/updates | 0.2.5 | 0.2.5 | ok |
| :145 | @rightkit/release | 0.2.133 | 0.2.147 | stale patch |
| context/package.json:48, :52, :53 | @rightkit/hooks, git, release | 0.1.1, 0.2.24, 0.2.121 | 0.1.2, 0.2.33, 0.2.147 | stale (3) |

Stale pin count (distinct manifest+crate pins, counting the five context rightkit-db manifests once): 11 engine, 4 tauri (incl. 2 rightkit-tauri), 6 context cargo, 5 npm = 26. Breaking-minor gaps: watch, fs, oauth, http, db, sandbox (engine); embed (context); @rightkit/tauri (npm).

Lock duplicates: `engine/Cargo.lock` carries rightkit-db 0.2.1 + 0.3.0, rightkit-process 0.2.1 + 0.3.2, rightkit-watch 0.1.0 + 0.3.0. Tauri lock carries rightkit-fs 0.1.0, service 0.1.0, framed-sidecar 0.1.0 transitively (older than registry fs 0.3.3, service 0.1.3).

Real usage today: rightkit-db (19 files), fs (11), ort (8), mcp (7), process (5), logs (4), watch (2), cache (2), tauri (2), shell (3), sandbox/oauth/http/browser/ocr/embed/license (1 each). JS: `@rightkit/license` 2 imports, `logs` 1, `tauri` 1, `updates` 3 in `apps/coderight-tauri/src`.

### 3. Still-to-move table
Legend: ON = already on RightKit; DUP = in-house duplicate; N/A; GAP = RightKit owner missing/insufficient.

| Capability | State | Evidence and RightKit owner |
|---|---|---|
| http-client | DUP | 78 non-test files use `reqwest::` directly (342 mention lines; 4 blocking): bins/coderight 37, tools 15, providers 10, tauri 13, remote/mobile_runtime/mcp 3. Only `bins/coderight/src/github_signin.rs:32` uses rightkit-http. Owner `rightkit-http` `async` feature (AsyncClient/SseStream). Note rightkit-http's async feature sits on reqwest 0.12; apps use 0.13 (`engine/bins/coderight/Cargo.toml:87`), so adoption needs the RightKit side aligned or both versions linked. |
| os-credential-store | DUP (tauri) / ON (engine providers) | `src-tauri/src/token_store.rs` 129 + `crates/auth/src/token_store.rs` 140 + `keyring-core 1.0.0` and 3 platform keyring crates (`src-tauri/Cargo.toml:45,56-65`); `src/license.rs` also uses keyring_core. Engine `providers/src/keystore.rs` 481 already wraps rightkit-secrets via `os-keyring`. Owner `rightkit-secrets` 0.1.2. |
| oauth-sign-in | DUP | providers/subscription_auth native_oauth.rs 848 + pkce.rs 113 + device_code.rs 378; mcp/src/oauth.rs 521; tauri native_oauth.rs 65 + auth/native_oauth.rs 321 (three PKCE copies). Owner `rightkit-oauth` 0.3.0 (loopback PKCE, device flow, presets). GAP: OpenAI/Codex and Command Code provider contracts and gateway-broker flow are not RightKit presets. |
| owned-processes | DUP | bins/coderight/src/daemon/kill_tree.rs 455; tools process_tree.rs 156; daemon-client daemon_lifecycle.rs 653 (adopt-or-spawn, presence); tools no_window.rs 71. Owner `rightkit-process` 0.3.3 (OwnedChild/AdoptedProcess/restart/health) and `rightkit-sandbox::process_tree`. Windows job object itself is ON (`implementation.rs:4`). |
| os-sandbox | DUP + GAP | tools/src/sandbox: windows_appcontainer.rs 3,667 (spawn core; only ancestor-lease sweep uses rightkit-sandbox, line 29), macos_seatbelt 273 + config_seatbelt 145, linux_sandbox 218 + seccomp_landlock 292, deny_read 158, denial_report 414 + denial_signature 338, managed_pty 630/200/688 (portable-pty direct), process_resource_authority 693+40. Total 7,756 LOC. Owner `rightkit-sandbox` 0.3.3 (its Windows AppContainer is 1,870 LOC, ported from CodeRight). GAP: RightKit API is blocking, no SandboxTier or deny_read, Windows confined PTY returns Unsupported (`rightkit-sandbox/README.md:83`), user-owned workspace blocker recorded in `docs/pending/rightkit-alignment-2026-10-08.md` (git ancestor stat on C:\Users). |
| sqlite-storage | DUP (partial) | storage crate ON (rightkit-db). Direct `rusqlite` + own PRAGMA/open in engine: tools/worktree_pool/db.rs 833, orchestration teams/store.rs 169 + checkpoint.rs 309 (+board, mailbox, missionlog), learning/proposal_store.rs 196, bins evo_db.rs 191, daemon/migrate.rs 474, doctor_impl.rs, workflow crate; context: membrane-runtime, blueprint, transcript, cortex use bundled rusqlite beside rightkit-db (10 files with open/PRAGMA, ~29k LOC of host files). Owner `rightkit-db` 0.3.2. Est touch ~1,100 LOC. |
| licensing (Rust) | DUP | bins/coderight/src/license.rs 375 re-derives token verification with ed25519-dalek (header says so); `api_session_config_license_dispatch.rs` 221 gates features. Owner `rightkit-license` 0.1.3. Tauri side already ON. |
| licensing (JS) | ON (wrappers kept) | `@rightkit/license` + `updates` used; `src/state/updateLicense.ts` 400 + `hooks/useLicenseState.ts` 141 + `components/updater` 333 are glue. |
| desktop-shell | DUP | tauri window_state 321, os_shell/autostart 145, tray 219, windows 197, presence_dnd 377 (RightKit has window_state, autostart, tray, windows, presence). Single-instance, deep_link parse, notify already ON. Keep mini_chat 183, approval_bubble 929, popup_policy, consent_dialog as app policy. Owner `rightkit-shell` 0.1.4. `@rightkit/shell` declared (`package.json:130`) but no import found. |
| desktop-overlay | GAP | approval_bubble 929 + mini_chat 183 could use `rightkit-overlay`; crate has no release record (INDEX flag MISMATCH). Blocker: publish. |
| update-policy / updater | GAP | `src-tauri/src/updater/mod.rs` 634 + `daemon-client/boot_probation.rs` 1,741 (post-update probation and rollback). No RightKit owner (`@rightkit/updates` is JS policy only). Candidate new `rightkit-updater`. |
| mcp | DUP (partial) | Client transport ON (rightkit-mcp). Remaining: bins/coderight/src/mcp_server.rs 804 (RightKit ServerHost cutover patch prepared, unapplied: `engine/crates/mcp/server-cutover.patch` per `docs/pending/rightkit-mcp-cutover-result.md`), mcp crate lsp_bridge 593, shared_pool 440, pool 353, trust_gate 263; context membrane-mcp 3,355 (http_security 177, jsonrpc 129, tools host). Six open RightKit API gaps listed in that doc (cold-connect cancel, connect timeout, envelope validation, blanket origin flag, elicitation cancel); status in rightkit-mcp 0.5.2 unverified. |
| llm-providers | GAP | providers crate 24,698 LOC (subscription_auth, credential_broker 1,616, constrained_sampling, replay, failover, model_metadata). `rightkit-llm` 0.1.0 has no release record and is blocking-only (README:51). Not movable. |
| model-installation | N/A | `local_models` 1,195 is hardware/runtime fit scoring and a stub inference seam ("no on-device weights"); no downloader in engine. Optional: `rightkit-models` hardware fit overlaps (hardware.rs). |
| ocr | ON | media crate uses `rightkit-ocr` native (`crates/media/Cargo.toml:8`). `coderight-ocr` deleted. |
| onnx-runtime | ON (adapter) / DUP (context) | `engine/crates/ort_common` 289 is a thin env/data-root adapter over rightkit-ort (keep). Context: `cortex-core` carries direct `ort =2.0.0-rc.13` and `fastembed` with `embed.rs` 951 LOC; blueprint already uses `rightkit_embed::Embedder` (`hybrid_task.rs:515`). Owner `rightkit-embed` 0.3.1 + `rightkit-ort`. |
| ffmpeg/media | N/A / GAP | media crate 1,820 shells installed binaries; `tts_pipeline.rs` 822 (chunking, RMS match, crossfade stitch) has no RightKit equivalent (audio/tts lack crossfade). Upstream candidate. |
| audio capture | N/A | none in repo |
| structured-logging / crash | ON (logs) / GAP (crash) | `src-tauri/src/logs.rs` 110 and `support_bundle` 405 on rightkit-logs. No rightkit-crash adoption: the only panic hook is TUI terminal restore (`bins/coderight/src/tui_host.rs:2664`). Engine `telemetry` crate 4,507 (OTel, resource reports) has no RightKit home. |
| safe-filesystem / file-watching | ON (engine) / DUP (context) | rightkit-fs used in 11 engine files, rightkit-watch in `api_fs_watch.rs`. Context: 6 hand-rolled `write_atomic`/`replace_file` sites (membrane/native_init.rs:377, activation.rs:4954, cortex-store installation_identity.rs:408/454, blueprint lib_receipt_store.rs:114, runtime background_review_input.rs:121), ~250 LOC → `rightkit-fs` 0.3.3. |
| disk-cache | ON | `rightkit-cache` in engine and orchestration; `coderight-cache` gone. |
| search | N/A | `tools/src/search.rs` 1,770 is ripgrep-style content search, not the query language. |
| settings-persistence | N/A | `coderight-config` 5,185 is layered TOML with different model (confirmed N/A in overlap doc). |
| browser-automation | ON | `tools/src/browser.rs` 3,568 uses rightkit_browser (:438); CodeRight keeps SSRF/watchdog policy. Pin is 0.2.0. |
| MCP server / hotkeys / QA harness / tauri adapters | n/a / n/a / GAP-adopt / ON | No hotkeys. `rightkit-qa` not adopted (qa/ dir, `right-qa.config.mjs`, wdio plugin). Tauri adapters ON via rightkit-tauri. |
| UI: onboarding, titlebar, palette, shortcuts, theme, i18n, legal, settings | DUP | `components/onboarding` 593 + OnboardingApp 251 + onboardingStore 97 (@rightkit/onboarding); TitleBar 159, CommandPalette 503 + logic 45, lib/shortcuts 597 + settings/keyboard 401 (@rightkit/app-shell, platform-ui); themeStore 91 + importedThemeStore 357 + components/themes 94 (@rightkit/theme); i18n 369 (@rightkit/i18n); LegalAcceptance 182 + LegalGate 29 + state/legal 129 (@rightkit/legal-ui); settings 11,374 is mostly product-specific, LicenseTab 158 fits settings-ui. @rightkit/legal-ui, onboarding, app-shell, theme, i18n, settings-ui, platform-ui are not declared in `package.json`. |

In-house duplicates with a published owner: 18 (12 Rust: http, secrets, oauth, process, sandbox, sqlite, daemon license, shell, embed/ort-context, fs-context, mcp-server, membrane-mcp; 6 JS: onboarding, app-shell, theme, i18n, legal-ui, license/update glue). Estimated LOC to move or delete: ~24,000, of which ~7,800 is the sandbox/PTY block gated on RightKit gaps, leaving ~16,000 movable now.

### 4. Ownership scanner
Command: `right-release ownership --app /Volumes/D/claude/coderight --json`. Exit 1, saved to `apps-g1-coderight-ownership.json`. Result: `protected-evidence: committed ownership: protected release checkout has tracked or untracked changes`. No rows, so counts verified-integration / declared-dependency / not-detected / blocked / excepted are all unavailable. Cause: dirty tree (2 modified files: `context/docs/reference/benchmarks/vector-backend/round1/windows/receipt.json` and `context/docs/research/legacy-source-corpus/papers/2511.07328_Q_RAG_Value_Based_Multistep_Retrieval.md`). Re-run after the tree is clean or commit. Grep analysis above substitutes. CAPABILITY-MAP shows only browser-automation is block-tier, and CodeRight is ON for it.

### 5. Handover list (CodeRight chat)
1. Bump pins to registry latest in one pass and unify lock duplicates (db 0.3.2, fs 0.3.3, watch 0.3.1, process 0.3.3, ort 0.2.2, oauth 0.3.0, http 0.2.4, secrets 0.1.2, mcp 0.5.2, sandbox 0.3.3, shell 0.1.4, tauri 0.1.2, embed 0.3.1, @rightkit/tauri 0.2.0, release 0.2.147) → est 300 LOC manifest plus API fixes; blocker: breaking 0.x minors (fs, db, watch, oauth, http, sandbox).
2. Replace `kill_tree.rs`, `process_tree.rs`, `daemon_lifecycle.rs`, `no_window.rs` → `rightkit-process@0.3.3` / `rightkit-sandbox` process_tree; est 1,335 LOC; blocker: none; Windows test needed.
3. Replace tauri `window_state`, `os_shell/{autostart,tray,windows}`, `presence_dnd` → `rightkit-shell@0.1.4`; est 1,260 LOC; blocker: none (keep mini_chat, deep-link consent).
4. Replace tauri keyring-core token stores and the three PKCE/device-flow copies → `rightkit-secrets@0.1.2` + `rightkit-oauth@0.3.0`; est 2,600 LOC; blocker: ChatGPT/Codex and Command Code flows need RightKit presets or stay as app code.
5. Replace daemon `license.rs` (+dispatch) → `rightkit-license@0.1.3`; est 375 LOC; blocker: none. Then context engine: cortex-core `embed.rs` + direct ort rc.13 → `rightkit-embed@0.3.1`; 6 atomic-write sites → `rightkit-fs@0.3.3`; direct rusqlite sites → `rightkit-db@0.3.2` (est 1,200 LOC total; Membrane chat owns `context/**`).
6. Apply the unapplied `server-cutover.patch` (`mcp_server.rs` 804) and re-check the six RightKit mcp gaps against 0.5.2; est 800 LOC.
7. Reqwest migration to `rightkit-http` async client, staged by crate (providers, tools, mcp, tauri); est 1,700 LOC; blocker: reqwest 0.12 vs 0.13 skew in rightkit-http and SSE/cookie parity.
8. Frontend: declare and adopt `@rightkit/onboarding`, `app-shell` (TitleBar, CommandPalette, shortcuts), `theme`, `i18n`, `legal-ui`; est 5,100 LOC; blocker: none, but visual QA required.
9. Sandbox/PTY block (7,756 LOC) → `rightkit-sandbox@0.3.3`; blocker: RightKit needs async API, SandboxTier, deny_read, confined Windows PTY, and the C:\Users ancestor fix. Send as RightKit follow-up, do not re-implement.
10. RightKit gaps to file: boot_probation/updater (2,375 LOC), overlay publication (929+183), tts crossfade (822), rightkit-llm publish (providers 24,698), rightkit-crash adoption in daemon and Tauri host.

---

## MailRight

### 1. Shape
Last commit 2026-10-10 (`4deba63`), tree clean. Targets: Windows and macOS Apple Silicon only (`src-tauri/Cargo.toml:6-13`; Linux explicitly unsupported).

| Manifest | Contents | LOC |
|---|---|---|
| `src-tauri/Cargo.toml` | single crate `mailright` (lib + bins mailright, load_driver): 34,070 Rust LOC. Biggest areas: runtime 3,382, db/search 2,364, db/semantic 1,658, webimg 881, sync 768, trust 505, app_lock 421, google 481 | 34,070 |
| `package.json` | React/Tauri front end `src/` 26,893 TS/TSX/CSS | 26,893 |
| `workers/waitlist/package.json` | Cloudflare worker (not scanned, see section 4) | small |

### 2. RightKit adoption and pins
| Manifest:line | Package | Pinned | Registry | Verdict |
|---|---|---|---|---|
| src-tauri/Cargo.toml:58 | rightkit-db (sqlcipher) | =0.2.1 | 0.3.2 | STALE (breaking) |
| :88 | rightkit-license | =0.1.3 | 0.1.3 | ok |
| :89 | rightkit-logs | =0.2.0 | 0.2.0 | ok |
| :90 | rightkit-tauri | =0.1.1 | 0.1.2 | stale patch |
| :104 | rightkit-embed | =0.2.1 | 0.3.1 | STALE (breaking) |
| :106 | rightkit-cache | =0.1.0 | 0.1.0 | ok |
| :108 | rightkit-secrets | =0.1.0 | 0.1.2 | stale patch |
| :110 | rightkit-oauth | =0.2.0 | 0.3.0 | STALE (breaking) |
| :111 | rightkit-http | =0.1.0 | 0.2.4 | STALE (breaking) |
| :113 | rightkit-shell | =0.1.2 | 0.1.4 | stale patch |
| package.json:67,68,70,93 | @rightkit/license, logs, updates, legal | 0.1.8, 0.1.4, 0.2.5, 0.3.1 | same | ok |
| package.json:69 | @rightkit/tauri | 0.1.1 | 0.2.0 | STALE (breaking) |
| package.json:94 | @rightkit/release | 0.2.135 | 0.2.147 | stale patch |

Stale: 7 cargo + 2 npm = 9 of 14 pins. Lock also carries transitive rightkit-fs 0.1.0 (registry 0.3.3), rightkit-ort 0.1.4 (0.2.2), rightkit-service 0.1.0 (0.1.3), framed-sidecar 0.1.0.

Adoption in use: db (28 call sites across db/, runtime/, commands/), shell (desktop.rs, tauri_run.rs: autostart, deep_link, instance, notify, open_url), tauri (license adapters), license, logs (logs.rs 175), embed (`db/semantic/embedder.rs`), cache (`attach_cache.rs`), secrets (`credential_store.rs`), oauth + http (`oauth.rs`, `google.rs`).

### 3. Still-to-move table
| Capability | State | Evidence and owner |
|---|---|---|
| http-client | DUP | `reqwest 0.13` direct (`Cargo.toml:73`) in google.rs 386, google/backoff.rs 95 (own Retry-After and jittered backoff), webimg.rs 337 (redirect policy, size-limited body), translate.rs 273, ai.rs 213, oauth.rs, drafts_commands.rs. Owner `rightkit-http` async client 0.2.4 (retry/backoff, Retry-After, size limits). Est touch ~1,300 LOC file size, ~600 changed. Note rightkit-http async is on reqwest 0.12. Scanner flags this (blocked/advise). |
| search | DUP | `db/search/` lexer 202, ast 338, operator_parse 105, predicates 209, snippet 87, search_terms 133, query 335 = 1,409 non-test LOC (+ ast_tests 220) vs `rightkit-search` 0.2.0 (lexer, AST, caller-defined operators, FTS5/SQL predicate compiler, snippets). Use `default-features=false` plus `sqlite` feature so rusqlite stays unified with rightkit-db sqlcipher. |
| desktop-shell | DUP | `webview_idle.rs` 336 duplicates `rightkit-shell/src/idle.rs` (same `DEFAULT_SUSPEND_AFTER`, hide/show/suspend state machine). Rest of shell usage is ON. Window chrome/tray otherwise via tauri plugins and rightkit-shell. |
| mcp | DUP | `mcp.rs` 232: axum loopback bearer/Origin-gated read-only server. Owner `rightkit-mcp` ServerHost (loopback HTTP, per-tool admission, `ReadOnlyPolicy`). Removes `axum` dependency. |
| llm-providers | GAP | `ai.rs` 213 is an OpenAI-compatible BYOK chat client. Owner `rightkit-llm` 0.1.0 has no release record, so no pin possible; it is also blocking (ureq) while MailRight is async (use spawn_blocking). |
| structured-logging / diagnostics | partial DUP | logs.rs is ON. `diag.rs` 230 (PII redaction of diagnostics lines) and `lib.rs:233-247` `append_rotating_log` + startup.log duplicate rightkit-logs redaction/rotation; est 265 LOC. |
| crash-reporting | GAP (adoption) | No panic hook or crash capture in `src-tauri/src`. Adopt `rightkit-crash` 0.2.0 (est ~100 LOC). |
| os-credential-store | ON | `credential_store.rs` 165 on rightkit-secrets (frozen item names preserved). |
| disk-cache | ON | `attach_cache.rs` 229 on rightkit-cache. |
| oauth-sign-in | ON | `oauth.rs` 131 on rightkit-oauth (stale 0.2.0). |
| sqlite-storage | ON | rightkit-db sqlcipher; sqlite-vec (`db/semantic/vec_ext.rs` 69) stays app-specific. |
| embeddings | ON | rightkit-embed (stale 0.2.1). |
| tauri-host-adapters | ON | `commands/license.rs:233-249` uses `rightkit_tauri::DeviceService` and `AuthorizationService`; the scanner advise at line 240 is a false positive. |
| settings-persistence | N/A | settings in encrypted DB `settings` table (`db_schema.rs:328`), not file settings. |
| file-watching / model-install / OCR / ONNX / ffmpeg / audio / process / sandbox / hotkeys / overlay / MCP client | N/A | none needed (embed model fetched at build by `scripts/fetch-embed-model.mjs`). |
| QA harness | GAP (adoption) | `qa/native/smoke.e2e.mjs`, `right-qa.config.mjs`, wdio webdriver plugin; neither `rightkit-qa` nor `@rightkit/qa` declared. Optional. |
| UI: app-shell, theme, legal-ui, pdf-ui, doc-render, shortcuts, settings frame, platform | DUP | TitleBar.tsx 134 (scanner: drag-region x2); `lib/theme.ts` 297 + `scripts/check-theme-contrast.ts` 94 + `check-token-colors.ts` 75 (scanner: theme x3; header says ported from ReadRight); LegalGate 38 + LegalAcceptance 197 (`@rightkit/legal` only declared as dev dep; legal-ui absent); AttachmentPdfPreview 165 (scanner: pdfjs-dist -> @rightkit/pdf-ui); EmailFrame 282 + html.ts 82 (`@rightkit/doc-render` safe email frame); ShortcutHelpOverlay 58 + shortcuts.ts 129 + useKeyboardShortcuts 209; platform.ts 54 (@rightkit/platform-ui); Settings views (Appearance 124, License 289, Diagnostics 260, SettingsView 121, Panel pieces) ~1,100 -> `@rightkit/settings-ui`. onboarding/i18n not present. |

In-house duplicates: 14 (6 Rust, 8 JS). Estimated LOC to move: ~6,700 (Rust ~3,750 incl. ~1,300 HTTP file size; JS ~2,900).

### 4. Ownership scanner
Command exit 1, saved to `apps-g1-mailright-ownership.json`. `ok: false`, error `ownership-coverage: 1 ownership coverage gap(s)`: `pnpm-lock.yaml` unscanned (`lockfile-root-unresolved`, detail `workers/waitlist/package.json`). Commit `4deba63`.

States over 71 capability rows: declared-dependency 11 (disk-cache, sqlite-storage, embeddings, licensing, structured-logging, oauth-sign-in, os-credential-store, desktop-shell, legal-assent, release-tooling, update-policy); not-detected (status `unscanned`) 55; blocked (advise tier) 5; verified-integration 0; excepted 0. No block-tier findings. Advise findings (8 occurrences, 5 capabilities):
- http-client: `reqwest 0.13` dependency, `src-tauri/Cargo.toml`.
- pdf-ui: `pdfjs-dist ^6.1.200`, `package.json`.
- theme-ui x3: `scripts/check-theme-contrast.ts:16`, `src/lib/theme.ts:176`, `src/lib/theme.ts:202`.
- tauri-host-adapters: `src-tauri/src/commands/license.rs:240` (false positive: file already uses rightkit-tauri).
- app-shell-ui x2: `src/components/TitleBar.tsx:60`, `:77`.

### 5. Handover list (MailRight chat)
1. Bump pins: db 0.3.2, embed 0.3.1, oauth 0.3.0, http 0.2.4, secrets 0.1.2, shell 0.1.4, tauri 0.1.2, `@rightkit/tauri` 0.2.0, `@rightkit/release` 0.2.147 → est 150 LOC; blocker: 0.x breaking (db, embed, oauth, http, @rightkit/tauri); resolve lock duplicates (fs, ort, service).
2. Replace `db/search/*` lexer/AST/compiler/snippet → `rightkit-search@0.2.0`; est 1,409 LOC; blocker: none (feature flags), keep golden query tests (`ast_tests`, `search_parser_tests`).
3. Replace `webview_idle.rs` → `rightkit-shell@0.1.4` idle; est 336 LOC; blocker: none.
4. Replace direct reqwest (google, backoff, webimg, translate, oauth, drafts) → `rightkit-http@0.2.4` AsyncClient; est 600 changed LOC; blocker: async client built on reqwest 0.12.
5. Replace `mcp.rs` → `rightkit-mcp@0.5.2` ServerHost with ReadOnlyPolicy; est 232 LOC; blocker: none.
6. Frontend: adopt `@rightkit/app-shell` (TitleBar, shortcut registry), `@rightkit/theme`, `@rightkit/legal-ui`, `@rightkit/pdf-ui`, `@rightkit/doc-render`, `@rightkit/settings-ui`, `@rightkit/platform-ui`; est 2,900 LOC; blocker: none, needs visual QA.
7. `diag.rs` + `append_rotating_log` → `rightkit-logs@0.2.0`; est 265 LOC. Add `rightkit-crash@0.2.0`.
8. `ai.rs` → `rightkit-llm`; blocker: crate unpublished and blocking. Hold.
9. Fix scanner coverage gap by declaring `workers/waitlist` in pnpm workspace or excluding it; then re-run the scanner.

---

## ViewRight

### 1. Shape
Last commit 2026-10-10 (`27249b78`). Working tree: 3 untracked docs (`docs/LENS_AUDIT_2026-08-31.md`, `docs/design/viewright-polish-pass.html`, `tasks/handoffs/2026-10-07/`). Targets: macOS (universal DMG) and Windows (NSIS). `.cache/` ignored per request. `vendor/docx-editor` (4 npm packages) is vendored third-party.

| Manifest | Contents | LOC |
|---|---|---|
| `src-tauri/Cargo.toml` | app crate `viewright` 0.1.165 | 9,270 Rust (commands/ included) |
| `src-tauri/crates/pdf/Cargo.toml` | `viewright-pdf` 0.1.0 (inspect 1,473, compose 1,094, model 846, save 527, font 502) | 4,586 |
| `package.json` | React front end: documents/ 58,140, app/ 17,851, state/ 3,318, settings/ 2,488 | ~82k |
| `workers/waitlist`, `functions/`, `site/` | marketing/worker, out of scope | n/a |

Largest app Rust files: mac_pdf_view.rs 1,847; commands/license.rs 917; mac_associations/macos.rs 903; lib.rs 737; markdown_resources.rs 737; documents.rs 685; persistence.rs 499.

### 2. RightKit adoption and pins
| Manifest:line | Package | Pinned | Registry | Verdict |
|---|---|---|---|---|
| src-tauri/Cargo.toml:58 | rightkit-db | =0.2.1 | 0.3.2 | STALE (breaking) |
| :60 | rightkit-watch | =0.3.1 | 0.3.1 | ok |
| :62 | rightkit-shell | =0.1.3 | 0.1.4 | stale patch |
| :63 | rightkit-logs | =0.1.0 | 0.2.0 | STALE (breaking) |
| :64 | rightkit-license | =0.1.3 | 0.1.3 | ok |
| :65 | rightkit-tauri | =0.1.1 | 0.1.2 | stale patch |
| :68 | rightkit-secrets | =0.1.1 | 0.1.2 | stale patch |
| :70 | rightkit-http | =0.2.1 | 0.2.4 | stale patch |
| :73 | rightkit-ocr (native) | =0.1.2 | 0.1.2 | ok |
| package.json:97 | @rightkit/doc-render | 0.1.3 | 0.1.4 | stale patch |
| :98 | @rightkit/legal-ui | 0.1.3 | 0.2.0 | STALE (breaking) |
| :99, :100, :102, :104, :148 | license, logs, pdf-ui, updates, legal | 0.1.8, 0.1.4, 0.1.2, 0.2.5, 0.3.1 | same | ok |
| :101 | @rightkit/media-ui | 0.2.6 | 0.3.0 | STALE (breaking) |
| :103 | @rightkit/tauri | 0.1.1 | 0.2.0 | STALE (breaking) |
| :149 | @rightkit/qa-chrome | 0.1.2 | 0.1.3 | stale patch |
| :150 | @rightkit/release | 0.2.135 | 0.2.147 | stale patch |

Stale: 6 cargo + 6 npm = 12 of 20 pins. Lock carries rightkit-fs 0.1.0 and 0.3.0 and service/framed-sidecar 0.1.0.

In use: ocr (OcrLine, decode_encoded, canary, win_native/vision), license (verify_token), logs, db (rusqlite re-export), watch (notify re-export, DirWatchSet), shell (`lib.rs:228` Shell::builder), tauri (SecureService, DeviceService, `native_license_adapter.rs`), secrets, http. JS: doc-render 15 imports, media-ui 12, pdf-ui 7, legal-ui 1, license, logs, tauri, updates.

### 3. Still-to-move table
| Capability | State | Evidence and owner |
|---|---|---|
| pdf-engine | DUP (largest) | `src-tauri/crates/pdf` 4,586 LOC + `commands/pdf_adapter.rs` 363 vs `rightkit-pdf` 0.2.0 (inspect 1,842, compose 1,926, model 1,338, save 789, font 768, merge, extract). `diff` shows 369/856/498/262/276 differing lines for inspect/compose/model/save/font, so RightKit has diverged and parity must be proven before cutover. |
| desktop-shell | DUP | donors already extracted into RightKit: path_broker.rs 394 (+131 tests) -> `rightkit-shell/src/broker.rs`; binary_stream.rs 218 + stream_http.rs 106 -> `stream.rs`; commands/clipboard.rs 358 -> `clipboard.rs`; setup_recovery.rs 136 + startup_cache.rs 46 -> `recovery.rs`; traffic_lights.rs 145 -> `frameless.rs`; site_root.rs 395 (per-tab site roots, partially in broker). 1,798 LOC non-test. Owner `rightkit-shell` 0.1.4. Currently only `Shell::builder` is used (`lib.rs:228`). |
| ocr | DUP (worker) | `ocr_worker.rs` 331 (one-shot child process, PNG on stdin) vs `rightkit-ocr` `worker.rs` (`WorkerCommand::run`, `run_if_requested`). `ocr_backend.rs` 51 is a shim; `commands/ocr.rs` 289 keeps policy. |
| sqlite-storage | DUP (partial) | `persistence.rs` 499: own `_migrations`, WAL verify, integrity check, quarantine (lines 81-245) vs rightkit-db `migrate.rs`/`recover.rs`; est 250 LOC. `settings_kv` stays app data. |
| file-watching | DUP (partial) | `watcher.rs` 293 uses `rightkit_watch::notify` directly with its own debounce loop beside `DirWatchSet`; est 200 LOC. |
| licensing | DUP (partial) | `commands/license.rs` 917 + `license_clock.rs` 92 + secure-store plumbing; RightKit `AuthorizationService`/clock available in rightkit-tauri; est 400 LOC, keep clock-rollback policy. JS `state/updateLicense.ts` 713 wraps @rightkit/license + updates (trim ~300). |
| structured-logging | ON | `logs.rs` 195 on rightkit-logs (pin 0.1.0 stale). No crash adoption: panic hook in `lib.rs` only logs location (Cargo comment line ~150). Adopt `rightkit-crash`. |
| os-credential-store | ON | `rightkit_secrets::Store` for license, device id, clock. |
| http-client | ON | rightkit-http for license refresh; `tauri-plugin-http` used in JS. No direct reqwest/ureq. |
| model-install / ONNX / ffmpeg / audio / process / sandbox / LLM / MCP / oauth / hotkeys / overlay | N/A | no such code (OCR is native only by design). |
| search | N/A (optional) | `commands/markdown_index.rs` 200, `explorer.rs` 451 filename listing; `rightkit-search` filename-index or `rightkit-fsindex` (staged, unpublished) could replace later. |
| settings-persistence | N/A | SQLite `settings_kv`. |
| updater | GAP | Tauri updater plugin only; no RightKit Rust updater with probation (same gap as CodeRight). |
| native PDF view / file associations | GAP | `mac_pdf_view.rs` 1,847 (PDFKit), `commands/mac_associations/*` 1,351, `context_menu.rs` 289, `print_preview.rs` 128: no RightKit owner. Candidates for rightkit-shell additions if another app needs them. |
| UI: theme, command palette, shortcuts, toasts, settings frame, chrome | DUP | `settings/themes.ts` 183 + ThemeSection (@rightkit/theme); CommandPalette 208 + useCommandPalette 306 + ShortcutHelp 161 + useGlobalShortcuts 310 (partly doc-specific); ToastViewport 98 + toastBus 25; SettingsPanel 244 + controls 101 + AppearanceMenu 125 + sections (Updates, About, Diagnostics, Privacy) ~900; `app/chrome.tsx` 455 (title bar) and `platform.ts` 51 (@rightkit/app-shell, platform-ui, settings-ui). |
| document UI: legal, pdf-ui, media-ui, doc-render | ON | adopted (stale pins for legal-ui, media-ui, doc-render). |

In-house duplicates: 13 (6 Rust, 7 JS rows). Estimated LOC to move: ~10,700 (viewright-pdf 4,949 incl. adapter; shell donors 1,798; ocr_worker 331; persistence 250; watcher 200; license 400; JS ~2,800).

### 4. Ownership scanner
Command exit 1, saved to `apps-g1-viewright-ownership.json`: `protected-evidence: committed ownership: protected release checkout has tracked or untracked changes` (3 untracked paths above). No rows, no counts. Re-run with a clean tree. Grep analysis substitutes.

### 5. Handover list (ViewRight chat)
1. Bump pins: db 0.3.2, logs 0.2.0, shell 0.1.4, tauri 0.1.2, secrets 0.1.2, http 0.2.4; npm legal-ui 0.2.0, media-ui 0.3.0, doc-render 0.1.4, `@rightkit/tauri` 0.2.0, qa-chrome 0.1.3, release 0.2.147 → est 200 LOC; blocker: breaking minors (db, logs, legal-ui, media-ui, @rightkit/tauri).
2. Replace `viewright-pdf` crate (+ `pdf_adapter.rs`) → `rightkit-pdf@0.2.0`; est 4,950 LOC; blocker: parity proof (RightKit sources differ by 260-860 lines per file; run ViewRight PDF golden/e2e before deleting). Biggest single win.
3. Replace path_broker, binary_stream, stream_http, site_root, clipboard, setup_recovery, startup_cache, traffic_lights → `rightkit-shell@0.1.4` (broker, stream, clipboard, recovery, frameless); est 1,800 LOC; blocker: none, Mac and Windows runs required.
4. Replace `ocr_worker.rs` → `rightkit-ocr@0.1.2` worker; est 331 LOC; blocker: none (keep one-shot-per-request policy on Mac).
5. `persistence.rs` migrations/quarantine → `rightkit-db@0.3.2` migrate/recover; `watcher.rs` debounce → `rightkit-watch@0.3.1`; est 450 LOC; blocker: persisted DB format must stay byte-compatible.
6. `commands/license.rs` + `license_clock.rs` → `rightkit-tauri` AuthorizationService; est 400 LOC.
7. Frontend: `@rightkit/theme`, `app-shell` (palette, shortcuts, toasts, chrome), `settings-ui`, `platform-ui`; est 2,800 LOC; blocker: none, visual QA.
8. Adopt `rightkit-crash@0.2.0`; est ~100 LOC.
9. RightKit gaps to file: Rust updater with boot probation, PDFKit native view, file-association helpers.
