# Browser standalone consumer

Static candidate for PK12-2/4 R2(a) packed artifact & R2(b) fetched artifact. Named consumer: ScrapeRight `inapp_sign_in.rs` cookie publication. Source uses public crate APIs only. Current browser API exposes `BrowserSession` + `LaunchOptions`; no public `BrowserBuilder` exists.

Committed `Cargo.toml` pins exact selected versions (`rightkit-browser = "=0.3.5"`, `rightkit-fs = "=0.4.6"` with feature `fs` for `private` verify APIs); no template substitution. Keep `cookie-export` enabled & `install` disabled. Both RightKit versions must exist in sealed directory registry for R2(a), then real registry for R2(b). No path, Git, workspace, patch or test-seam dependency, & no `Cargo.lock` committed in this directory (validator rejects it); harness pins & retains lock output outside recipe. R2(b) must bind same exact browser/fs versions & run unchanged source. Existing harness owns registry configuration, packed/fetched hashes, host gates & execution output.

Required inputs, never defaulted:

| Input | Requirement |
|---|---|
| `RK_R2_CFT_ROOT` | Absolute managed CFT root containing `current` platform layout. Caller-provisioned pinned bundle; consumer downloads nothing. |
| `RK_R2_CFT_SHA256` | SHA-256 of exact `ManagedRoot::executable()` bytes, from trusted sealed CFT asset evidence. |
| `RK_R2_HEADLESS_SHELL_SHA256` | Windows: exact hash of `current/chrome-headless-shell-win64/chrome-headless-shell.exe`; same pinned CFT version as Chrome. |
| `RK_R2_SCRATCH` | Existing absolute scratch root (`D:/scratch/...` on Dell or `/Volumes/D/scratch/...` on Mac). |

Preflight canonicalizes executables beneath managed `current`, verifies exact executable hashes, binds explicit resolver environment before runtime threads start & checks public resolver selected those bytes. Provisioner must hold managed installation immutable throughout run, verify full archive/tree hash including DLLs/resources & record same-version Chrome/shell evidence. Executable hash alone does not establish supporting-file identity. No fallback to user Chrome/cache or asset skip. Run only on Windows/macOS for native pass; other hosts exercise typed `ConfinementUnsupported` & exit nonzero.

| Proof row | Executed behavior |
|---|---|
| `managed_cft_resolver` | Public ManagedRoot + CFT resolver select pinned managed executable; missing assets/hash mismatch fails. |
| `real_tabs_import_close` | Real Chrome owns two about:blank tabs; distinct identities, selection, close, unknown-page refusal & empty tab registry. Imports three session cookies through public BrowserPage API & reads actual browser store through public `raw().execute()` with typed `Network.getCookies` URLs (`https://example.test/`, `https://sub.example.test/app`, `https://unrelated.example/`) while tab remains `about:blank`; no navigation or network request. Explicit HTTPS covers secure cookie & `/app` covers preference cookie path. |
| `cookie_export_scope_netscape_private_preservation_clear` | Exports observed browser cookies for `example.test`; excludes unrelated domain; preserves secure/HttpOnly/domain/path through Netscape round-trip. Separate persistent-cookie vector verifies expiry. Public fs verify-only APIs inspect owner-only artifact/parent & strict artifact DACL/mode. Empty/invalid replacement preserves previous bytes. Public clear callback invalidates simulated engine auth snapshot, including absent-artifact case. |
| `browser_owned_shutdown_profile_removed` | All tab/cookie success/error/operation-timeout paths await owner-scoped graceful shutdown, then bounded cancel fallback; temporary profile must disappear. |
| `fixture_trusted_action` | Immutable local bundle, source/actions digests, real native confinement, trusted click changes DOM, assertion succeeds, identity-bound terminal cleanup. |
| `fixture_cancel` | Cancellation interrupts missing-element action; typed Cancelled, zero completed actions & complete identity-bound cleanup required. |
| `browser_r2` | Emitted only after every preceding row passes. |

Fixture terminal checks compare launch/terminal host, process-instance, browser-session, profile, source & actions identity; require browser/transport/proxy/server/profile cleanup flags; inspect profile absence, endpoint refusal & successful bounded rebind. Consumer issues no global process kill or PID kill. Native process-tree receipts come from package gate/native fixture probes; consumer relies on public authenticated cleanup attestation. Hard launch/run bounds fail qualification; fixture ownership/Drop remains responsible for cleanup if bound expires. Consumer never reports timeout as success.

This consumer adds no FixtureSession feature or capture API: PK14-6 remains separate dispatch. It does not replace package gate rows, W/M held-resource budget, headed smoke, upstream sandbox egress proof, packed-manifest inspection or archive/readback binding. No commands, builds or tests were run while preparing these files.
