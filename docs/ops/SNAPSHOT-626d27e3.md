# RightKit consumer snapshot 626d27e3 (11 Oct 2026, 23:45 IST)

Path on the Mac: /Volumes/D/rightkit-publish/snapshots/consumer-626d27e3/claude/rightkit
Commit: bogusyogi/rightkit main 626d27e3 (Dell chats: `git archive 626d27e3` from the Dell clone).

Evidence: full main CI run 38158083629. Mac: every library compiles, 88 of 93 units pass; the 5 reds are test-target lints (daemon, fs, qa), one shell test race and a ledger copy check, none in library code. Windows: 75 of 90; reds are rightkit-browser (Chrome sandbox permission on the runner, fix in a pull request), test lints, and npm test suites being swept.

Versions: jobs 0.2.9, daemon 0.1.3, settings 0.2.0, control 0.3.0, shell 0.3.0, service 0.1.5, license 0.2.0, fs 0.3.4, db 0.4.0, qa 0.2.16, secrets 0.1.4, process 0.3.4, http 0.3.0, pdf 0.2.1, crash 0.2.1, llm 0.2.0, models 0.3.0, search 0.2.1, hotkeys 0.1.0, overlay 0.1.1.

Mechanism unchanged: copy the crates you need into your in-repo rk-snapshot/ (or point at the path above), `[patch.crates-io]` lines each preceded by `# RIGHTKIT OVERRIDE: remove when <crate> <version> is on crates.io`, exact pins in [dependencies]. A build with an override line is a dev build and is never released.

Breaking or behaviour changes in this snapshot:
- rightkit-control 0.3.0: the embedded QA control server is only compiled under the new feature `qa-control` (was `tauri-plugin`). It must sit behind an app feature `qa-native` that is not in default features; the dependency is optional. right-release refuses a release or update build that contains it.
- rightkit-qa 0.2.16: launch guard (the QA home and every path handed to the app must be inside a runner-created temp root on the internal disk; Documents, Desktop, Downloads, iCloud, external volumes and the app's real data folder are refused), `seed_from` refuses real user data unless declared scrubbed and verified, homes are removed at run end, dead homes older than 2 hours are swept at run start, and a run refuses to start when the QA root exceeds 4 GB. The first run will delete old dead homes under /private/tmp/rightkit-qa.
- rightkit-shell 0.3.0: an app id is normalised into its service name (dots become hyphens) and `Arbitration::InvalidAppId` is a new variant you must handle; `IdleOptions` is no longer `Copy` and has a private field (build with setters: `IdleOptions::default().suspend_after(d).suspend_after_with(|| ..)`; a struct literal no longer compiles outside the crate; on this snapshot commit use `let mut o = IdleOptions::default(); o.suspend_after = d;`); TrafficLights works without ShellState; `StreamPolicy::observer`; `ensure_http_cache_dir_with`; `ClipboardOptions` (unknown pasteboard types are not copied unless you opt in).
- rightkit-service 0.1.5: bounded fallback socket name and `HostError::Endpoint`; an oversized reply is the typed error `response_too_large`; `CallContext::peer()` gives the verified caller.
- rightkit-jobs 0.2.9: `shutdown_detailed`, staged pool, hold and resolve, `open_recovering`, sweep of unclaimed jobs.
- rightkit-daemon 0.1.3: `serve_blocking` + `StopHandle`; features `client` and `testing`.
- rightkit-settings 0.2.0: a `Value`-typed store opens again; an interrupted Windows replace carries `InterruptedReplace`.
- rightkit-fs 0.3.4: `FsError::ReplaceInterrupted`, `AtomicWriter::commit_checked`, `From<FsError> for io::Error`.
- rightkit-db 0.4.0: unique quarantine names, `without_optimize_on_open()`, `DbOptions` gained a public field.
- rightkit-license 0.2.0: `ClosureClockStore`, `judge_refresh_response`, `LifecycleMessages`.
- rightkit-crash 0.2.1: record survives a failed sink thread, `capture_backtrace(false)`, per-thread guard, file written before the sink.
- rightkit-llm 0.2.0: `LlmError::Refused`, empty messages kept on request, missing `choices` is an error.
- rightkit-http 0.3.0: `Request::secret_form`, `redact_value`, `scrub`; reserved destination names refused.

NOT in this snapshot (on main, not yet compiled in CI; next snapshot): the fourth-review fixes for secrets, fs, pdf, http and process; the DevTools policy; jobs `delete` clean-up, keyset paging and terminal import; search case folding; db unreadable-database policy; fs file sync before a checked commit; license lock-failure policy and message formatter.

Standing limits: no Mac build carrying this snapshot's rightkit-secrets may run against the real keychain (stay on the published secrets crate); rightkit-pdf 0.2.1 may be adopted behind an override for testing, but it still has open review findings (saving can weaken a file's permissions; a changed-font replacement can move neighbouring text) fixed only on main.
