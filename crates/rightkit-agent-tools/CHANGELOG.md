# Changelog

## 0.4.0

- Breaking, hence 0.4.0 (0.3.5 was never published): the `coding_tools` module and its features left this crate for `rightkit-agent-coding-tools`.

- Added: `serve_commands(suite, &AppProfile, ServiceConfig, handler)` hosts the release-side half of `<app>.health` and `<app>.command` on `rightkit-daemon`: it answers `health`, runs `command{name,args}` only when the profile's command policy and the hard exposure rules allow the name (`AppProfile::command_permitted`, now also what the client tools and `batch` use), and refuses every other method with `tool_not_allowed`. The handler gets `(call context, name, args text)`, the args text as the client sent it. `AppProfile::service_config(role, rank, version)` builds the matching `ServiceConfig` (service `profile.service`, `right-ctl` allowed the profile's enabled methods). An app that hand-wrote this service can drop its method, command and args handling. Additive, Breaking: none.
- Added: `AppProfile::with_speech()` (field `speech`) selects which app gets the `speech.*` tools; the built-in HeardRight profile sets it, so `add_app`, `control_toolbox` and `right-ctl heardright transcribe` behave as before. `right-ctl <app> transcribe` follows the profile, not the app name. A hand-built HeardRight profile must now call `with_speech()` to get `speech.*`.
- Breaking (`speech` feature): `backends::speech::speech_tools(suite, role)` takes the caller's `rightkit_speech_client::HostRole` instead of always sending `CutRight`. `add_app` keeps presenting that role (`CTL_SPEECH_ROLE`); `add_app_as(tb, suite, profile, role)` lets an embedding app pass its own.
- `coding_tools`: the web-fetch envelope calls `rightkit_http::wrap_untrusted_envelope` with the same tag and `tool:` prefix as before; output unchanged (#1572).

- `coding_tools`: the `schedule_cron` tool's time zone code (`normalize_time_zone`, `time_zone_offset_minutes_at` and their helpers, 181 lines identical to `rightkit-scheduler::timezone`) is deleted and re-exported from `rightkit-scheduler` 0.1.2 (new optional dependency, on with `coding_tools`). Same results. Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-platform-redraw.md.

- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.3.3 (unpublished)

- Added (rightkit#421): feature `mcp-provider` and `ToolboxProvider`, a `rightkit_mcp::ToolProvider` over a `Toolbox` for one `Caller` (src/mcp_provider.rs). `tools/list` is `Toolbox::list_for`; every `tools/call` runs `Toolbox::call_cancellable`, so exposure rules, allowlist, schema and admission apply unchanged; refusals are tool-level errors, an unknown tool is `ToolNotFound`; MCP cancellation reaches the tool's `CancelToken`. M22 decision recorded in the spec adjudication. The `rightkit-mcp` dependency no longer enables `agent_mcp` itself (feature `coding_tools` does), and its floor is 0.7.4.
- Added (rightkit#1245): `coding_tools` tool `list_mcp_sources` reports each MCP source's state (`ready`, `no-tools`, `failed`, `pending`) and tool count from `McpManager::connection_states`, without starting a server or calling a tool; sources the channel permissions hide are omitted. Registered in the always-on set (hidden with the other MCP tools when mobile MCP is off).
- Removed test-only code: feature `coding_tools-test-mode`, `local_provider_mock_base` (the `RIGHTKIT_E2E_LOCAL_PROVIDER_MOCK` endpoint redirect; Cloudflare and Fly.io always use their real API base) and `observe_revision_for_tests` (Adrian, 2026-11-03: "make sure no debug assertions in their respective apps/ repos"; "My decision was no unit tests. Only e2e."; "dev mode, is mine. im the fucking owner, my data. no dummy data ... real data. snapshot and restore."). Release behaviour unchanged.

- Added (rightkit#795): a loopback HTTP transport for apps whose control surface is not a `control-<app>` pipe service (CodeRight). `AppProfile::with_http(HttpEndpoint)` / `AppProfile::http` moves the profile's tools onto `POST <path>` with `{"method","params"}` and an `{"ok","result"|"error"}` answer (wire contract `control-http v1`, `src/backends/http.rs`). `HttpEndpoint::new` and `parse` refuse any non-loopback address; an optional bearer token is read from a named file on every call and is never held in the profile, argv, environment or a log. The profile's methods, command policy, batch checks and the hard exposure rules (no paste or pasteboard tool, dictation layer) apply to both transports unchanged. `right-ctl` selects it with `RIGHT_CTL_HTTP_<APP>=ip:port[/path]` (and `RIGHT_CTL_HTTP_<APP>_TOKEN_FILE`). `AppProfile` gains the public field `http`, so a struct literal needs `http: None`; the constructors set it.
- Documented (rightkit#795): an agent host's own model-facing registry is not exposed through `right-ctl` (the README states the exemption); the host applies `exposure::forbidden` to its own tool and command names so the pasteboard rule holds there too.

## 0.3.4
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.3.2

- Fix: `coding_tools` edit tools (`apply_patch`, `edit_file`, `multi_edit`, `undo_edit`, `write_file`) pass the `.rightkit` state directory to `HistoryStore::new`; with the feature on, the crate did not compile against `rightkit-agent-engine-history`. `invoke` for the web-fetch envelope uses `rightkit_http::wrap_untrusted_web_text_coderight`.

## 0.3.1 (unreleased)

- `coding_tools` sync with the product engine: new `computer` tool (Linux desktop control) and `browser_consent` (`browser_open consent=<id>`, reuse of a user's signed-in profile under a revocable receipt); tools declare `write_targets` (the engine writes the restore point, so `edit_file`, `write_file`, `multi_edit`, `apply_patch` and `notebook` no longer snapshot) and `effect_class` / `approval_class` (also on `ToolCapability`); launch hardening (scrubbed credential env, program directories readable in the Linux sandbox, own-session process trees, device and git-config reads); `ask_user` has no timer and keys answers by question index; `tool_search` is lexical only. Removed: `browser_control_*`, `mcp_auth`, `transcribe`, `browser_act secret_ref`, `browser_fetch headless`, feature `coding_tools-embeddings`.
- Feature `coding_tools` (+ `coding_tools-host`, `-code-mode`, `-embeddings`, `-test-mode`, `-test-support`): the agent's coding tool set (file service, edit/undo, shell, search, browser, MCP bridge, web fetch, code mode) moved from CodeRight `coderight-tools`. `invoke_skill` reads skill manifests through the host-installed `rightkit_agent_host_traits::SkillCatalog` (no catalog installed: the file is returned as written).

## 0.3.0 — unpublished staging

- BREAKING: public `rightkit-daemon` types change identity. `control_toolbox` and `add_app` take `&rightkit_daemon::Suite` (`src/lib.rs:39`, `:48`, `:57`), and `CallContextExt` extends `rightkit_daemon::CallContext` (`src/peer.rs:9`, `:18`, `:35`). The daemon dependency is 0.4.0 (`Cargo.toml:23`); published 0.2.0 pins `rightkit-daemon ^0.3.0`. The automation floor moves to 0.6.0 (`Cargo.toml:24`). The speech-client floor stated in the 0.2.1 entry below is now actually in the manifest: `rightkit-speech-client` 0.4.0 (`Cargo.toml:26`, was 0.3.6, whose daemon line predates 0.4.0). The 0.2.1 entries below were never published and ship under this number.

## 0.2.1 — unpublished staging

- rightkit-speech-client floor 0.4.0 (feature `speech`, on by default), following its transcript 0.4 bump. The speech backend is private (only `Suite`/`Tool` are exported), so no public API change.
- **Exposure hardening: camelCase and PascalCase names.** The hard exposure rules split a name into words on every non-alphanumeric character only, so `pasteText`, `getClipboard`, `startDictation`, `recentTranscripts` and `pushToTalk` were one token each and passed (`paste_text`, `start_dictation` were refused). Names now also split on case boundaries (`pasteText` -> `paste`, `text`), acronym ends (`PTTStart` -> `ptt`, `start`) and letter/digit changes (`paste2text`), so every camelCase form is refused exactly like its snake_case form, for tool names, command names, `batch` command steps and `AppProfile::sensitive` command lists. The words `pasteboard` and `clipboard` are also refused anywhere inside a separator-free name (`getclipboardtext`, `READPASTEBOARD`). A name that hides the word inside a longer all-lowercase run (`pastetext`, `pasteurize`) is still judged by whole words. Behaviour change only; no API change. Tests in `src/exposure.rs` and `src/backends/control.rs`.
- **HeardRight lock is by app name.** A profile registered under the app name `heardright` is always treated as sensitive and dictation-protected, so `AppProfile::standard("heardright")` no longer yields `heardright.eval`, `dom`, `ax`, `screenshot`, `wait` or `batch`, and `add_app` applies the dictation exposure layer to it. `AppProfile::allow_patterns` no longer lists a method the profile does not enable. Built-in `AppProfile::for_app("heardright")` is unchanged.
- **Applicability record (M22, PK15-2).** New `applicability` module: `AgentRole` (`client | host | both | absent`), `ApplicabilityRecord { role, entrypoints }` with `from_value`, `from_manifest` (reads the `M22` key of a `rightkit-mandatory.json`), `unverified_entrypoints`, `missing_entrypoint_files` and `has_agent_surface`, `ApplicabilityError`, and `APPLICABILITY_KEY`. The rules match `right-release ownership-mandatory`; the record is report-only. Additive.
- **Tests (PK15-1, PK15-3).** `tests/desktop_tools.rs` drives `desktop.*` through the toolbox over the simulated desktop: status, apps, windows, snapshot (secure values never read), a semantic press, `max_tier=foreground` refused by the gate (`invalid_arguments`) and by the tool (`tool_not_allowed`), an unlisted app and an unconfirmed risky press refused, and a denied approval changing nothing. `tests/negative_gate.rs`: a paste or pasteboard tool cannot register (snake, kebab, camel and Pascal case) and a paste command is refused for a caller allowed everything; HeardRight `eval` and every generic read is refused under the built-in, a method-requesting and a standard profile.
- The package description no longer names `rightkit-qa-driver` (the `<app>.*` backend is `rightkit-qa`'s `control` server).
- **Docs: `<app>.*` is development/QA-only.** The README no longer claims an always-compiled in-app control service in shipped apps; installed apps are driven through `desktop.*` (M22 review, 2026-10-19).

## 0.2.0

- Tool schemas set `additionalProperties: false`; seven QA control methods are exposed.
- BREAKING: HeardRight's dictation, delivery and history bans no longer apply to every app. Pasteboard and paste names stay refused everywhere (`exposure::forbidden`); the dictation layer applies to a profile with `with_dictation_rules()` (the built-in HeardRight profile) or a toolbox app named with `Toolbox::protect_dictation`. `AppProfile` gains the public `dictation` field. `FORBIDDEN_PHRASES` is renamed `DICTATION_PHRASES`, and dictation tokens moved to `DICTATION_TOKENS`.

## 0.1.5 (2026-10-17)

- Added: `validate_args_strict(schema, args)`: `validate_args` plus unknown-argument rejection (a schema without `additionalProperties` is strict; `true` or a schema object opens it). `validate_args` is unchanged. Closes the Pulse `Args::parse` gap (rightkit#515).
- Agent demo contract (feature `demo`, non-default): `demo.request`, `demo.apps`, `demo.snapshot`, `demo.act`, `demo.batch` and `demo.end` as typed client tools over the host service `rightkit.agent.demo`. Per-session token, typed error codes, and a 20-step batch limit. The wire contract is in `docs/demo-contract.md`. The version bump waits for the owner's release.
- `right-ctl demo <tool> [args]` (feature `demo`). A default or release build contains no demo code.

## 0.1.5

- Added: `CallContextExt::peer_executable()` on `rightkit_daemon::CallContext`, returning the verified executable of the calling process (`Option<PathBuf>`), for a consent prompt that must show which program is asking. It runs the transport's own peer check (`CallContext::verify_peer_executable`: pid and start time recorded at accept, connection still open) and reads the executable the OS found; it replaces probing with an impossible path and reading `found_executable` from the error. `None` fails closed (a device caller, an unreadable or changed process, an unsupported platform). A fact for display and audit, not an authorisation. Needs `rightkit-daemon` 0.2.4 (`verify_peer_executable`), so the dependency floor is now 0.2.4.

## 0.1.3

- `batch` and `wait` control methods (rightkit-qa-driver 0.4): one call for many steps, and one call that waits in the page. A batch is checked step by step against the app profile, so a sensitive app (HeardRight) cannot reach `eval`, `dom`, `ax` or `screenshot` through it; `wait` and `batch` count as generic read channels. `screenshot` accepts `inline`.
- One connection per app profile is reused across tool calls (it was one connect and hello per call).
- README: driving an app is documented as one `right-ctl run` or `batch` call, not one command per step.

## 0.1.2

First published release. Earlier 0.1.x versions were internal to the repository and never reached crates.io.

- One typed `Toolbox` for agents: named tools with JSON argument schemas and an effect class, every call passing one gate (hard exposure rules, per-caller allowlist, argument validation, admission hook, journal).
- Tool families: `<app>.*` over each app's in-app control service through `rightkit-daemon`; `desktop.*` over `rightkit-automation` (feature `automation`); browser and speech tools (features `browser`, `speech`).
- The `right-ctl <app> <command>` CLI.
- MCP hosting removed; the agent surface is the CLI contract.
