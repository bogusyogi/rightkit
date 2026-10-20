# Changelog

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
