# Consumer snapshot c9f222a1

Named 2026-10-12 04:15 IST by the RightKit lead. Supersedes snapshot 626d27e3.

Commit: Orthic-Labs/rightkit main `c9f222a1`.
Mac path: `/Volumes/D/rightkit-publish/snapshots/consumer-c9f222a1/claude/rightkit` (plain `git archive`, the whole repository, 57 crates).
Dell: `git archive c9f222a1` from the Dell clone.

Nothing here is published. Copy crates into the app's `rk-snapshot/` and keep the `# RIGHTKIT OVERRIDE: remove when <crate> <version> is on crates.io` lines.

## Version numbers do not tell snapshots apart

Twenty crates changed since 626d27e3 and NONE changed its version number. Patch by path and record the snapshot commit beside each override. Changed: fs, http, pdf, process, secrets, settings, service, jobs, daemon, qa, db, shell, search, embed, models, llm, license, sandbox, tts, ort, media, overlay, contracts, browser. Unchanged: crash, control, logs, cache, hotkeys, tauri, review, channel-memory.

## Evidence

Every library in this commit has compiled on both CI hosts. The last pull-request run over this code (PR 19, base 69bfffae; the only later changes are to tests) was 85 of 87 on macOS and 84 of 87 on Windows, every red a test, none a library. No full main run has completed at exactly this commit; one is queued. ScrapeRight compiled an earlier state of the reduced crates as a consumer on the Mac (trial e45bd699) and passed its test suites and queue journey.

Known reds at this commit, all tests: rightkit-fs `concurrent_processes_never_expose_a_mixed_file` and two rightkit-http `phases.rs` tests fail intermittently on Windows (test races; fixes written, not yet landed); rightkit-qa browser tests can stall on first navigation on Windows.

Not reviewed: the five reduced crates (fs, http, pdf, process, secrets) have not had their comparative review. The rule on rightkit-secrets is unchanged: no build carrying it is launched against a real login keychain until it passes.

## What changes for a consumer

**rightkit-process** (reduced)
- `OwnedCommand::kill_on_parent_exit` is Linux-only. On macOS and other Unix hosts `spawn()` returns Unsupported before anything starts. Call it only on Linux; on macOS the child must watch its own parent, or the app must end its children itself.
- `PidProof` and the Windows `spawn_detached_tolerating_denied_breakaway` / `DetachedBreakaway` are gone.
- Waits reap only with non-blocking looks; an unbounded wait wakes about once a second.

**rightkit-fs** (reduced)
- `ReplaceMode::ReplaceFile`, `REPLACEFILE_WRITE_THROUGH` and `FsError::ReplaceInterrupted` are gone.
- A parent directory is synced only when the call created it.
- macOS: an ACL with an allow entry is refused (`UnsafeAcl`); nothing is repaired; a deny-only ACL is accepted.
- Windows: a reader can be refused for an instant while another process replaces the file. Pre-existing; retry in the reader.

**rightkit-settings**: `ReplaceMode::ReplaceFile` and `InterruptedReplace` are gone. A failed quarantine or backup refresh fails the write (`Error::PreserveFailed`). Key order is preserved by default (`KeyOrder::Sorted` for the old bytes).

**rightkit-secrets** (reduced): `migrate_plaintext_file` takes three arguments again and always retains the source file. `FileMigration = NoFile | Retained | StoredValueDiffers | StoreUnavailable`.

**rightkit-http** (reduced): download state lives in an owner-only `.rk-download` directory beside the destination, keyed by a hash of the file name; a 416 always restarts from zero. Windows `ProxyMode::System` fails closed with `SystemProxyUnsupported { element }` on anything it cannot follow exactly, including "Automatically detect settings".

**rightkit-pdf** (reduced): saving goes through rightkit-fs and refuses what it cannot preserve (`CommitError::Refused`, `CommitError::Replaced`); the crate now depends on rightkit-fs 0.3.4. A page whose decoded content exceeds 8 MiB is `malformed_content`. Adding text or a new font to a page whose content is split between an operator's operands and the operator works again (the IRS-form regression). After any edit that adds a stream, locators taken before the edit are stale.

**rightkit-jobs**
- `RunOutcome::Unsettled(Job)` and `RunOutcome::Restored(JobRecord)` are new variants; `Staged::Handled` is new.
- A refused terminal write is held and replayed (`Runner::with_settle_later(false)` restores the old error). An explicit owner settle replaces a held runner outcome while the row is non-terminal.
- `HandlerOutcome::{Done, Settled, Pending}`, `ClaimScope`, `OpenFailure`.
- `keep_total` prunes terminal rows only.

**rightkit-qa**
- An engine or sidecar launch that names no isolated data directory is refused. `EngineTarget` gained fields: build it with `EngineTarget::new(binary).with_data_dir_env(..)` / `.with_data_dir_arg(..)` / `.forbid_args(..)`; struct literals no longer compile.
- The storage cap is per product (4 GB) with a 32 GB root ceiling; dead marker-less homes are swept; Windows dispose waits for the run's own processes.

**rightkit-shell**
- DevTools policy: `DevToolsPolicy {Denied, ContentOnly}`, process-start guard, per-webview gate. Windows: the policy keys are read through the registry API and an unreadable key refuses; a redirected runtime folder (policy or environment) refuses; one key handler blocks only DevTools, view-source and the reload keys, so find, print and zoom work.
- `IdleOptions` builder setters (`suspend_after`, `start_on_minimize`, `controller_hide`, `events`).
- `take_item_from_with(board, ..)` reads a named pasteboard and never the general clipboard; names that alias the system pasteboards are refused.

**rightkit-embed**: an accelerator is used only if it reproduces the CPU vectors at load. `ExecutionProvider::Auto` falls back to CPU with a recorded reason; an explicit `DirectMl` or `CoreMl` that fails returns `EmbedError::AcceleratorParity`. On the hardware measured, DirectML fails this for the EmbeddingGemma 2 Q4 bundle, so `Auto` on Windows resolves to CPU.

**rightkit-search**: per-character folding (`FOLD_VERSION` 2). An index built on the 626d27e3 copy must be rebuilt.

**rightkit-service**: `Client::connect_expecting(&ClientConfig, &HostExpectation)` verifies the host executable before any byte is sent, on macOS, Linux and Windows.

**rightkit-browser** (added to this note 2026-10-12 06:45 IST; it was missing, and GenRight met it on the Dell): on Windows a launcher refuses, with `BrowserError::ChromeSandboxAccess` and the `icacls` command that fixes it, a Chrome whose folder does not grant read and execute to `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) and `ALL RESTRICTED APPLICATION PACKAGES` (`S-1-15-2-2`). A Chrome unpacked into a user folder (Playwright's `ms-playwright\chromium-*`) has neither grant; before this check its network service crashed at start and the first navigation hung or aborted intermittently. The Chrome for Testing that `@rightkit/qa-chrome` installs is granted at install. Same version number, 0.2.4. Consumers whose Windows browser tests used a Playwright Chromium now fail deterministically where they used to pass or stall: use the managed Chrome for Testing; do not change the folder's access list by hand on a shared machine.

**rightkit-daemon**: `DeviceSession::send_unsigned` and `send_with_nonce` under the `testing` feature.

**rightkit-llm**: opt-in raw response capture. **rightkit-models**: trust roots, validated user agent, header and total timeouts, retries, a development-only loopback host.

**rightkit-tts**: model verification writes its cache file only when the content changes.

**rightkit-media**: the Windows `ffmpeg-7.1.5-export-ass-zimg-windows-x64` and `ffprobe-7.1.5-windows-x64` rows are approved.

**rightkit-ort, rightkit-overlay, rightkit-pdf**: `serde` and `serde_json` are pinned in the manifest, so a crate copied out of the workspace resolves.

## Not in this snapshot

- The guard that refuses a real per-user folder to a process under test (rightkit-service, rightkit-models): branch `mac/under-test-guard`.
- Identity-checked adopt, the machine lock and the caller lease (rightkit-process): branch `mac/process-coordination`.
- Windows sandbox profile clean-up and the cross-session lease fix: local on the Dell.
- A bounded-retry reader for files another process replaces.
