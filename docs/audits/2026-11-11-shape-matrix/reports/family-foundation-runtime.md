# Family review: Foundation, runtime

Read-only review of 33 packages. Nothing was built or run.

## How this was done, and its limits

- **Import scan (verified).** For every package I listed the symbols each app imports from it (`rightkit_<crate>::...` in every `.rs` file of ViewRight live and main, Pulse, CodeRight, MailRight, PostRight, CutRight, GenRight, ScrapeRight, HeardRight main). This is the main evidence for "does one API serve all of them".
- **Line location (verified).** For the larger app files the plan maps to a package, I looked up which RightKit crate actually holds their lines.
- **Reading.** I read module documentation and manifests of the packages and the heads of the app files named below. I did not read function bodies side by side. Where a finding rests on names rather than bodies it is marked **suspicion**.
- **Depth.** Deep (scan, line location, package layout, app file heads): daemon, job-queue, db, models, spend, qa, release, logs. Light (scan and manifest only): child-process, sidecar, fs, file-watch, credentials, crypto, @rightkit/logs, crash, http, device, identity, oauth, passkeys, sync, sync-keys, qa-driver, @rightkit/qa, qa-chrome, ci, ax, onnx, time, cache, decimal, contracts. `@rightkit/qa` was asked for as deep; its API was not read function by function.
- **HeardRight** verdicts are taken from `hr-review-storage-network.md`, `hr-review-crash-logs-license.md` and `hr-review-process-runtime.md`. Those do not cover `rightkit-qa`, `@rightkit/qa`, `@rightkit/release`, `@rightkit/ci`, `@rightkit/ax`, spend, time, cache, oauth or contracts; HeardRight is "not verified" there.
- **ViewRight live (0.1.123)** links only `rightkit-logs`, `rightkit-license`, `rightkit-tauri` and five npm packages. Everything else ViewRight "fits" refers to ViewRight main; upgrade compatibility from the live build was not checked.
- Legion source is not available. The matrix rows count `feeds` from the plan's inventory.

## The cross-app answer

1. **The low line match is the expected result, not a gap.** The surveyed apps already call these crates (Pulse imports `rightkit-fs` in 180 files, CodeRight `rightkit-http` in 85). What the plan maps to a foundation package is mostly (a) a thin app wrapper over the crate, or (b) domain code that already sits in another RightKit crate: CodeRight's daemon in `rightkit-agent-server`, GenRight's ledger in `rightkit-media-gen`, GenRight's model management in `rightkit-remote-compute`, PostRight's publish HTTP in `rightkit-social-publish`, Pulse's platform code in `rightkit-disk-cleanup`, Pulse's pairing in `rightkit-mesh`, the renderer mocks in `media-gen-ui`, `mail-ui` and `library-ui`.
2. **The high matches are the problem cases.** `rightkit-logs` 95% of CodeRight, `rightkit-models` 47% and `rightkit-daemon` 15% are CodeRight crates copied into foundation packages behind features (`agent_telemetry`, `agent_local_models`, `agent_daemon_core`, `agent_daemon_runtime`). That is agent product code inside the mandatory set.
3. **One API does serve all apps** for child-process, sidecar, credentials, device, time, oauth, file-watch, both logs packages, and the cores of fs, db, http, crash, onnx, spend, job-queue and qa.
4. **Three packages need redrawing:** `rightkit-daemon` (eight concerns), `rightkit-models` (two stores), `rightkit-qa-driver` (deprecated shim).

## rightkit-job-queue

1. **Exists as:** `crates/rightkit-job-queue`, `swift/RightKitSwift/Sources/RightKitJobQueue`.
2. **Planned feeds:** scraperight 5,177, genright 501, postright 141, pulse 63.
3. **Today's implementation:** fresh RightKit implementation; three layers behind features (store, contract, supervisor).
4. **Across apps:** One API already serves every app that has a queue; the layers are feature-split correctly. Open: a third engine supervisor beside rightkit-daemon::supervisor and rightkit-child-process restart policy, and no store hook that lets an app keep an in-memory registry as authority without rebuilding it (ScrapeRight).
5. **Shape: gaps** (reference app: heardright; depth deep; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | hr-review-process-runtime: right shape with gaps; only EngineSupervisor and launch frame used, no persistent queue |
   | pulse | fits | already on Dispatcher/JobStore/CommandSet/serve_report in 16 files; activity_lease.rs is mis-mapped (a polling lease file, not a job) |
   | coderight | fits | uses only InlineCancelRegistry; its turn scheduling lives in rightkit-agent-server |
   | genright | fits | on JobStore/JobLocks/EngineSupervisor; lease.rs and worker.rs are already in rightkit-media-gen |
   | mailright | fits | JobTable/StatusModel/Transition for the outbox |
   | scraperight | gaps | on JobStore + pool::StagedPool, but keeps a 2.5k-line in-memory registry as the authority with the store as write-behind (queue.rs:5-9) |
   | cutright | fits | Dispatcher/serve/run_cli/doctor |
   | postright | fits | Dispatcher/JobTable; protocol/schema.rs is mis-mapped |

   n/a (no code mapped, no import found): viewright.
7. **Boundary problems:** EngineSupervisor here, supervisor::supervise in rightkit-daemon and RestartPolicy/HealthPolicy in rightkit-child-process are three supervision surfaces (existence verified, overlap is a suspicion); Pulse activity_lease.rs and PostRight protocol/schema.rs mapped here by path, not jobs.
8. **Before publication:** Decide one engine supervisor (job-queue vs daemon) and re-export from the other; Add a registry/read-model hook so ScrapeRight can drop its parallel in-memory job authority, or record it as an app need; Carry the HeardRight gaps from hr-review-process-runtime.

## rightkit-child-process

1. **Exists as:** `crates/rightkit-child-process`.
2. **Planned feeds:** scraperight 278, pulse 124.
3. **Today's implementation:** RightKit implementation; HeardRight main fully on it.
4. **Across apps:** Every surveyed app already spawns through OwnedCommand/run/RunOptions; no app-side spawn layer of any size remains.
5. **Shape: best** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | hr-review-process-runtime: right shape; macOS parent-death watchdog needs a signed-build run |
   | viewright | fits | OwnedCommand, reveal, process_usage in 4 files (main only; live 0.1.123 does not link it) |
   | pulse | fits | 72 files; core/src/subprocess.rs is already in rightkit-app-manager |
   | coderight | fits | 62 files incl. tokio_support |
   | genright | fits | 23 files |
   | mailright | fits | 5 files |
   | scraperight | fits | 13 files; helper_process.rs (278 lines) not read |
   | cutright | fits | 7 files |
   | postright | fits | sweep/AdoptedProcess |
7. **Boundary problems:** Pulse subprocess.rs mapped here but its lines are in rightkit-app-manager.
8. **Before publication:** none.

## rightkit-sidecar

1. **Exists as:** `crates/rightkit-sidecar`.
2. **Planned feeds:** mailright 3,109, coderight 3.
3. **Today's implementation:** RightKit implementation of bounded JSON-lines framing.
4. **Across apps:** Framing only, and every engine-carrying app uses it. The 3.1k MailRight lines mapped here are not framing: they are a private per-launch rightkit-daemon service.
5. **Shape: best** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | hr-review-process-runtime: right shape; wire is now a hello/request envelope |
   | pulse | fits | json-line and length_prefixed readers |
   | coderight | fits | 7 files |
   | genright | fits | 1 file |
   | mailright | fits | read_json_line only; core_ipc/* belongs to rightkit-daemon (PrivateSecret, supervise_adopted) |
   | scraperight | fits | wire::Frame |

   n/a (no code mapped, no import found): viewright, cutright, postright.
7. **Boundary problems:** MailRight core_ipc/* (3.1k lines) is mapped here; it is a rightkit-daemon private service (core_ipc/mod.rs:3-11).
8. **Before publication:** none.

## rightkit-daemon

1. **Exists as:** `crates/rightkit-daemon`, `swift/RightKitSwift/Sources/RightKitDaemonClient`.
2. **Planned feeds:** coderight 13,142, mailright 5,589, postright 2,628, cutright 567, genright 384.
3. **Today's implementation:** RightKit local-service core plus merged-in parts: retired remote daemon crate (PostRight wire), CodeRight loopback HTTP and CodeRight daemon_core/daemon_runtime copied under features.
4. **Across apps:** The local same-user service API is right and Pulse, MailRight, CutRight and GenRight host on it unchanged. But the crate is eight things: local service, remote HTTPS host, loopback bearer HTTP, relay, run mode and app roots, root migration/cutover, installed-app discovery, approval, and CodeRight's lifecycle authority. ViewRight and ScrapeRight link it only for mode/AppRoot/Suite, and rightkit-models links it only for Suite::resolve.
5. **Shape: wrong** (reference app: pulse; depth deep; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | hr-review-process-runtime: right shape with gaps (speech service only; peer_trust.rs superseded by verify_peer; one SID query not exported) |
   | viewright | fits | uses only mode::AppId, AppRoot, Suite; no service |
   | pulse | fits | 25 files: host_with_options, Client::connect_expecting, supervisor::supervise, try_adopt_or_spawn, contract::MethodTable |
   | coderight | gaps | its daemon is loopback HTTP with a bearer token and lives verbatim in rightkit-agent-server; from this crate it uses loopback::*, Suite and the agent_daemon_* modules that are its own code |
   | genright | gaps | hosts on host/Handler/Allowlist; discovery.rs (384 lines) re-implements discover_apps_in_with |
   | mailright | fits | private service with PrivateSecret, ClientPool, supervise_adopted, mode |
   | scraperight | fits | mode::init_or_exit and AppId only |
   | cutright | gaps | hosts on it and calls discover_macos_with/discover_windows_with, yet keeps a 567-line discovery.rs wrapper |
   | postright | fits | remote::Daemon, remote::client, ticker heartbeat, stop_on_stdin_close, mode |
7. **Boundary problems:** agent_daemon_core and agent_daemon_runtime are CodeRight's daemon_core/daemon_runtime crates (lifecycle.rs 166/166 lines) and pull axum and rightkit-agent-engine-protocol into a foundation crate; mode, Suite, AppRoot, root_cutover (about 5k lines) are path and run-mode policy, used by apps with no daemon; rightkit-fs has its own app_dirs::AppDirs; remote + link + relay (HTTPS, device enrolment, OAuth callback) is a second product in the crate; pairing also exists in rightkit-identity, rightkit-mesh and rightkit-device-link; approval module here and approval tokens in rightkit-crypto; CodeRight daemon files and MailRight runtime/* are mapped here by path but are app or agent-server code.
8. **Before publication:** Move mode/Suite/AppRoot/root_cutover to one roots crate (or rightkit-fs) and make daemon, models and qa depend on that; Move agent_daemon_core/agent_daemon_runtime to rightkit-agent-server; Split remote/link/relay into their own crate, leaving local service + loopback; Make GenRight and CutRight delete their discovery.rs in favour of discover_apps_in_with; Carry the HeardRight gaps (verify_peer pin, SID query export).

## rightkit-fs

1. **Exists as:** `crates/rightkit-fs`, `swift/RightKitSwift/Sources/RightKitFS`.
2. **Planned feeds:** pulse 1,653, scraperight 1,015, postright 436, cutright 137, genright 128, mailright 94.
3. **Today's implementation:** RightKit implementation.
4. **Across apps:** Atomic write, private dirs, digests, locks and verified copy are used by every app with the same calls. Gaps are HeardRight's (root edge cases, Windows replace) and the split of root resolution between this crate and rightkit-daemon.
5. **Shape: gaps** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | hr-review-storage-network: right shape with gaps (HR_APP_DATA_DIR and redirected HOME, ReplaceFileW vs rename) |
   | viewright | fits | AppDirs, atomic_write_with_options, append_record, contain; main only |
   | pulse | fits | 180 files; core/src/platform/* is already rightkit-disk-cleanup (win_native.rs 210/210) |
   | coderight | fits | 134 files |
   | genright | fits | 48 files, AppDirs + StateDir |
   | mailright | fits | 26 files |
   | scraperight | fits | 21 files; runtime.rs and runtime_paths.rs path policy not read |
   | cutright | fits | 67 files; pins =0.4.7 |
   | postright | fits | 15 files |
7. **Boundary problems:** App root resolution exists three times: fs::app_dirs::AppDirs, fs::StateDir, daemon::Suite/AppRoot/mode::roots; SHA-256 and BLAKE3 are in both fs::digest and rightkit-crypto; apps import both; Pulse platform/* mapped here but lives in rightkit-disk-cleanup.
8. **Before publication:** One root resolver (see rightkit-daemon action); One hashing owner: fs::digest re-exports rightkit-crypto or the reverse; HeardRight F2/F3/F6 from hr-review-storage-network.

## rightkit-file-watch

1. **Exists as:** `crates/rightkit-file-watch`, `swift/RightKitSwift/Sources/RightKitWatch`.
2. **Planned feeds:** coderight 460, pulse 328, viewright 205.
3. **Today's implementation:** RightKit implementation with three front ends.
4. **Across apps:** ViewRight, Pulse and CodeRight are each on the crate through a different front end (FileSetWatcher/DirWatchSet, TypedWatcher/WatchCursor, FileWatcher/WatchRegistration). The three match three real needs: open documents, a replayable tree, subscriber registrations.
5. **Shape: best** (reference app: viewright; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | viewright | fits | FileSetWatcher, DirWatchSet, EventFilter; watcher.rs is a 205-line adapter |
   | pulse | fits | TypedWatcher, WatchCursor, Granularity; watch.rs maps events to the storage index |
   | coderight | fits | FileWatcher, DebouncedWatchReceiver; api_fs_watch.rs is already in rightkit-agent-server |

   n/a (no code mapped, no import found): heardright, genright, mailright, scraperight, cutright, postright.
7. **Boundary problems:** Whether the three front ends share one backend was not verified.
8. **Before publication:** Confirm one backend under the three front ends and document which to pick.

## rightkit-db

1. **Exists as:** `crates/rightkit-db`, `swift/RightKitSwift/Sources/RightKitDB`.
2. **Planned feeds:** mailright 2,552, postright 1,211.
3. **Today's implementation:** RightKit implementation; encrypted lifecycle built for HeardRight.
4. **Across apps:** Open, migrate, quarantine, backup, restore, checkpoint job and read pool are used by every app with a database, with no second SQLite wrapper found. Missing: encrypted in-place compaction (MailRight) and adoption of the rotation/recovery code HeardRight still carries.
5. **Shape: gaps** (reference app: heardright; depth deep; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | hr-review-storage-network: right shape with gaps; about 1,200 lines of rotation and recovery still app-side |
   | viewright | fits | open_read_only, OpenOrDegrade, QuarantineStamp, migrate; main only |
   | pulse | fits | 55 files; migrate, quarantine_database_opts, DurabilityProfile |
   | coderight | fits | 60 files; ReadPool, Step::Code, rollback_to, pending_marker |
   | genright | fits | 3 crates |
   | mailright | gaps | keeps db_gate (lock order and app-lock gate) and db_compaction (in-place VACUUM); wal_checkpoint is already WalCheckpointJob; migration_gate is in rightkit-mail-store |
   | scraperight | fits | one file, migrate + add_column_if_missing |
   | postright | fits | backup.rs is a manifest layer over online_backup_to, restore_database_locked, StoreLock |

   n/a (no code mapped, no import found): cutright.
7. **Boundary problems:** MailRight db/migration_gate.rs is mapped here; 56 of 64 lines are in rightkit-mail-store; job-queue re-exports rusqlite and db; Pulse imports rusqlite through rightkit-job-queue as well as rightkit-db.
8. **Before publication:** Add verified in-place compaction for keyed databases, from MailRight db_compaction; Decide whether a connection gate (writer, read pool, app lock) is a package type or stays MailRight's; HeardRight gaps from hr-review-storage-network.

## rightkit-credentials

1. **Exists as:** `crates/rightkit-credentials`, `swift/RightKitSwift/Sources/RightKitCredentials`.
2. **Planned feeds:** heardright 752, genright 379, mailright 274, pulse 114.
3. **Today's implementation:** RightKit implementation.
4. **Across apps:** Store/Namespace/Config/GuardMode is the only credential API in use across eight apps; app files mapped here are thin naming wrappers.
5. **Shape: best** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | hr-review-storage-network: right shape; default-keychain-only read is an open edge |
   | viewright | fits | Store with GuardMode::Memory |
   | pulse | fits | Store, Secret, handoff; secure_prompt.rs 76% in package |
   | coderight | fits | Store, apple_helper, conditional writes |
   | genright | fits | Store, EnvReader, Source |
   | mailright | fits | Store, WindowsTarget |
   | scraperight | fits | Store, apple_helper |
   | cutright | fits | Store::generate_key |
   | postright | fits | Store, system_backend |
7. **Boundary problems:** none found.
8. **Before publication:** none.

## rightkit-crypto

1. **Exists as:** `crates/rightkit-crypto`.
2. **Planned feeds:** none in the plan.
3. **Today's implementation:** RightKit implementation.
4. **Across apps:** Used as-is by six apps for hashing, HMAC, base64 and constant-time compare. Hashing is duplicated by rightkit-fs::digest, and the crate also carries approval-token minting.
5. **Shape: gaps** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | hr-review-storage-network: right shape, base64 only |
   | viewright | fits | hex_encode |
   | pulse | fits | 27 files |
   | coderight | fits | 57 files |
   | genright | fits | incl. approval::mint, canonical_json |
   | mailright | fits | base64 |
   | postright | fits | hmac, base64 |

   n/a (no code mapped, no import found): scraperight, cutright.
7. **Boundary problems:** sha256/blake3 also in rightkit-fs::digest; approval tokens here, approval workflow in rightkit-daemon::approval.
8. **Before publication:** Single hashing owner; Drop unused base64/getrandom deps noted in the HeardRight review.

## rightkit-logs

1. **Exists as:** `crates/rightkit-logs`.
2. **Planned feeds:** coderight 4,075, pulse 845, scraperight 686, mailright 314, postright 166, viewright 166, cutright 105, genright 62.
3. **Today's implementation:** RightKit implementation (used by live HeardRight and live ViewRight) plus CodeRight's telemetry crate copied under the agent_telemetry feature.
4. **Across apps:** The logging core (JsonlStore, EventRecord, scrub, RotatingTextWriter, tracing file, collect_diagnostics) serves all nine apps. The 95% CodeRight match is its agent telemetry (prompt overhead, context ceiling, agent observability, repair), which is agent product code and not logging.
5. **Shape: gaps** (reference app: heardright; depth deep; confidence high).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | hr-review-crash-logs-license: right shape |
   | viewright | fits | live 0.1.123 already ships JsonlStore + collect_diagnostics; logs.rs is location and gate only |
   | pulse | fits | 28 files; diag_reports.rs (macOS report watcher) and windows/diag.rs are Pulse features, not log code |
   | coderight | gaps | core use fits; engine/crates/telemetry is in the package verbatim and should move to an agent crate |
   | genright | fits | 30 files |
   | mailright | fits | diag.rs wrapper |
   | scraperight | fits | tracing_layer_checked + collect_diagnostics |
   | cutright | fits | 1 wrapper |
   | postright | fits | diagnostics.rs wrapper |
7. **Boundary problems:** agent_telemetry (about 3k lines) is CodeRight agent product code in a mandatory foundation crate; identity::random_uuid and system/metrics live in a logging crate; Pulse diag_reports.rs is mapped here but is an OS crash-report watcher (closer to rightkit-crash).
8. **Before publication:** Move agent_telemetry to a rightkit-agent-* crate; Move random_uuid out (apps import it from logs).

## @rightkit/logs

1. **Exists as:** `packages/logs`.
2. **Planned feeds:** none in the plan.
3. **Today's implementation:** RightKit implementation, 808 lines.
4. **Across apps:** Every app's package.json depends on it, including both live builds; no app-side renderer logger was mapped.
5. **Shape: best** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | hr-review-crash-logs-license: right shape |
   | viewright | fits | dependency present |
   | pulse | fits | dependency present |
   | coderight | fits | dependency present |
   | genright | fits | dependency present |
   | mailright | fits | dependency present |
   | scraperight | fits | dependency present |
   | cutright | fits | dependency present |
   | postright | fits | dependency present |
7. **Boundary problems:** none found.
8. **Before publication:** none.

## rightkit-crash

1. **Exists as:** `crates/rightkit-crash`.
2. **Planned feeds:** heardright 3,616, pulse 227, mailright 68, cutright 35, scraperight 21.
3. **Today's implementation:** built from HeardRight's crash capture.
4. **Across apps:** One capture path serves all apps, but two install entry points are in use: install_panic_hook (five apps) and install_process_hook with ProcessKind (three apps).
5. **Shape: gaps** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | hr-review-crash-logs-license: right shape with gaps (1 s sample, hang cooldown); usage-telemetry upload must not move here |
   | viewright | fits | install_panic_hook; main only |
   | pulse | fits | install_panic_hook, HangWatchdog, dump_helper, NativeCapture |
   | coderight | fits | install_process_hook, build_support_bundle |
   | genright | fits | install_panic_hook, list_pending |
   | mailright | fits | install_process_hook |
   | scraperight | fits | install_panic_hook + ProcessKind |
   | cutright | fits | install_process_hook, set_recording_enabled |
   | postright | fits | install_panic_hook |
7. **Boundary problems:** HeardRight usage-telemetry upload (5.3k lines) is mapped here and does not belong (HeardRight review).
8. **Before publication:** Keep one install entry point and deprecate the other; HeardRight capture gaps.

## rightkit-http

1. **Exists as:** `crates/rightkit-http`, `swift/RightKitSwift/Sources/RightKitHTTP`.
2. **Planned feeds:** postright 597, scraperight 409, coderight 186, genright 176.
3. **Today's implementation:** RightKit implementation.
4. **Across apps:** Client/Request/RetryPolicy/AddressPolicy is the one HTTP client in every app; app files mapped here are thin or already in domain crates. rightkit-models still declares ureq directly beside rightkit-http.
5. **Shape: gaps** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | hr-review-storage-network: right shape with gaps; three ureq-parity clients remain app-side |
   | viewright | fits | percent_decode_component only |
   | pulse | fits | Client, ApprovedOrigins |
   | coderight | fits | 85 files, AsyncClient; http_client.rs 86% in package |
   | genright | fits | protocol/http.rs is in rightkit-local-gen |
   | mailright | fits | Client, AsyncClient, MultipartRelated |
   | scraperight | fits | net.rs 62% in package; netpolicy |
   | postright | fits | QuotaStore, parse_retry_after_at; publish/http.rs is in rightkit-social-publish |

   n/a (no code mapped, no import found): cutright.
7. **Boundary problems:** rightkit-models depends on ureq as well as rightkit-http (Cargo.toml); whether it bypasses the address policy is not verified.
8. **Before publication:** Remove direct ureq use from rightkit-models; HeardRight gaps.

## rightkit-device

1. **Exists as:** `crates/rightkit-device`, `crates/rightkit-device-link`, `crates/rightkit-device-link-types`.
2. **Planned feeds:** none in the plan.
3. **Today's implementation:** RightKit implementation, split out of rightkit-tauri.
4. **Across apps:** DeviceService and device_id_hash are used identically by all eight Tauri apps.
5. **Shape: best** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | hr-review-process-runtime: right shape, device id stable |
   | viewright | fits | DeviceService + device_id_hash |
   | pulse | fits | DeviceService + device_id_hash |
   | coderight | fits | DeviceService + device_id_hash |
   | genright | fits | DeviceService + device_id_hash |
   | mailright | fits | DeviceService + device_id_hash |
   | scraperight | fits | DeviceService + device_id_hash |
   | cutright | fits | DeviceService + device_id_hash |
   | postright | fits | DeviceService + device_id_hash |
7. **Boundary problems:** The crate also exports LogHost/LogService and DiagnosticHost/DiagnosticService (used by Pulse and ScrapeRight): host adapters for logs in a crate named device.
8. **Before publication:** Rename or move the log and diagnostic host adapters.

## rightkit-identity

1. **Exists as:** `crates/rightkit-identity`.
2. **Planned feeds:** pulse 1,360.
3. **Today's implementation:** RightKit implementation; no surveyed app imports it.
4. **Across apps:** No app calls it. The Pulse pairing code planned for it is already in rightkit-mesh (pairing.rs 315/317 lines), and pairing also exists in rightkit-daemon::remote and rightkit-device-link.
5. **Shape: gaps** (reference app: pulse; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | gaps | devicelink/pairing.rs and identity.rs are in rightkit-mesh, not here |

   n/a (no code mapped, no import found): heardright, viewright, coderight, genright, mailright, scraperight, cutright, postright.
7. **Boundary problems:** Device pairing appears in rightkit-identity, rightkit-mesh, rightkit-daemon::remote and rightkit-device-link (existence verified, sameness is a suspicion).
8. **Before publication:** Read the four pairing implementations side by side and keep one before publishing.

## rightkit-oauth

1. **Exists as:** `crates/rightkit-oauth`, `swift/RightKitSwift/Sources/RightKitOAuth`.
2. **Planned feeds:** coderight 809.
3. **Today's implementation:** RightKit implementation.
4. **Across apps:** CodeRight and MailRight both run on OAuthClient/TokenStore/callback/pkce; CodeRight's token_store.rs is a TokenStore implementation, which the trait intends.
5. **Shape: best** (reference app: coderight; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | coderight | fits | 9 files |
   | mailright | fits | OAuthClient, GoogleIdVerifier |

   n/a (no code mapped, no import found): heardright, viewright, pulse, genright, scraperight, cutright, postright.
7. **Boundary problems:** Carries a server feature used only by rightkit-daemon::remote.
8. **Before publication:** none.

## rightkit-passkeys

1. **Exists as:** `crates/rightkit-passkeys`.
2. **Planned feeds:** none in the plan.
3. **Today's implementation:** RightKit adapter over passkey-rs, 289 lines.
4. **Across apps:** No surveyed app imports it; shape cannot be judged against a consumer.
5. **Shape: gaps** (reference app: none; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|

   n/a (no code mapped, no import found): heardright, viewright, pulse, coderight, genright, mailright, scraperight, cutright, postright.
7. **Boundary problems:** none found.
8. **Before publication:** Find a first consumer before it is called mandatory; not verified.

## rightkit-sync

1. **Exists as:** `crates/rightkit-sync`, `swift/RightKitSwift/Sources/RightKitCloudSync`.
2. **Planned feeds:** scraperight 6,854, heardright 2,081.
3. **Today's implementation:** RightKit engine; HeardRight main runs on it.
4. **Across apps:** ScrapeRight and HeardRight both run on the same Backend/AuthProvider pass. ScrapeRight's 6.8k lines are its record mapper, schema, hooks and the engine-to-shell CloudKit transport, which are app-side by design; the scheduler is still per app.
5. **Shape: gaps** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | hr-review-storage-network: right shape with gaps (own scheduler, one-way ledger migration) |
   | pulse | fits | BackoffPolicy only |
   | scraperight | gaps | 11 files on the engine; keeps its own runtime/scheduler and an engine-to-shell transport seam (cloud_sync_transport.rs:1-6) |

   n/a (no code mapped, no import found): viewright, coderight, genright, mailright, cutright, postright.
7. **Boundary problems:** none found.
8. **Before publication:** Add the shared scheduler both apps hand-roll; Consider owning the engine-to-shell CloudKit transport seam.

## rightkit-sync-keys

1. **Exists as:** `crates/rightkit-sync-keys`.
2. **Planned feeds:** none in the plan.
3. **Today's implementation:** RightKit implementation.
4. **Across apps:** No surveyed app imports it.
5. **Shape: gaps** (reference app: none; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|

   n/a (no code mapped, no import found): heardright, viewright, pulse, coderight, genright, mailright, scraperight, cutright, postright.
7. **Boundary problems:** none found.
8. **Before publication:** First consumer before mandatory; not verified.

## rightkit-qa

1. **Exists as:** `crates/rightkit-qa`, `swift/RightKitSwift/Sources/RightKitHubQA`.
2. **Planned feeds:** coderight 6,854, heardright 4,649, postright 907, mailright 488.
3. **Today's implementation:** RightKit implementation, 52k lines.
4. **Across apps:** control::Control + harness::Harness/Scenario + workspace::QaWorkspace is the one launch-and-drive API in ViewRight, Pulse, MailRight, ScrapeRight and CutRight; CodeRight and PostRight use suite::run_suite. It cannot drive a terminal app: CodeRight's 6.8k-line Python PTY and ConPTY harness has no counterpart.
5. **Shape: gaps** (reference app: heardright; depth deep; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | not covered by the three HeardRight reviews named for this family; planned 4.6k lines, 5% line match; not verified |
   | viewright | fits | Control, Harness, QaWorkspace, embedded::Control |
   | pulse | fits | same, plus process::Tracker |
   | coderight | gaps | desktop on embedded::Control and run_suite; TUI dogfood (pty.py, conpty_*.py, inside_out.py) needs a PTY driver the package lacks |
   | genright | fits | 57 files; media, mock, loudness, engine::Sidecar |
   | mailright | fits | binary::resolve, guard, storage::managed_root; test_root is mode policy |
   | scraperight | fits | Control, guard |
   | cutright | fits | 14 files incl. media probes |
   | postright | fits | mock::MockServer, userdata, installed::refuse_installed; mock-platform is a PostRight fake |
7. **Boundary problems:** Media probes (ffprobe, wav, loudness), mock HTTP server, Android and journeys receipts share one crate with the control client; rightkit-qa-driver is a deprecated shim over the control feature.
8. **Before publication:** Add a PTY/terminal driver or rule that TUI acceptance stays CodeRight's; Consider splitting media assertions from the control harness.

## rightkit-qa-driver

1. **Exists as:** `crates/rightkit-qa-driver`.
2. **Planned feeds:** none in the plan.
3. **Today's implementation:** deprecated shim (its own Cargo description) over rightkit-qa control and rightkit-automation.
4. **Across apps:** Superseded by rightkit-qa; no surveyed app imports it. It should not be in the set apps adopt.
5. **Shape: wrong** (reference app: none; depth light; confidence high).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|

   n/a (no code mapped, no import found): heardright, viewright, pulse, coderight, genright, mailright, scraperight, cutright, postright.
7. **Boundary problems:** Same capability as rightkit-qa::embedded.
8. **Before publication:** Remove from the mandatory set; publish no new versions.

## @rightkit/qa

1. **Exists as:** `packages/qa`.
2. **Planned feeds:** heardright 14,575, genright 4,923, pulse 3,170, coderight 2,987, scraperight 2,361, mailright 1,769, viewright 678.
3. **Today's implementation:** RightKit implementation, 7.2k lines (CDP page, control client, a11y, visual, evidence, journeys).
4. **Across apps:** All apps depend on it. The about 1% line match is expected: what the plan maps here is app test scripts and renderer IPC mocks, and the large mocks are already in domain UI packages (GenRight browser-mock-api.ts 1990/2008 in media-gen-ui, MailRight qaMock in mail-ui, ScrapeRight qaMock in library-ui).
5. **Shape: best** (reference app: heardright; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | not covered by the named HeardRight reviews; 14.6k planned lines not verified |
   | viewright | fits | dependency; scripts/test-tooling-e2e.test.mjs is app tooling |
   | pulse | fits | linux-guest, apfs fixtures and footprint probes are Pulse-only scripts |
   | coderight | fits | check-*.mjs scripts are app gates |
   | genright | fits | mocks in media-gen-ui |
   | mailright | fits | mocks in mail-ui |
   | scraperight | fits | mock in library-ui |
   | cutright | fits | dependency |
   | postright | fits | dependency |
7. **Boundary problems:** Renderer IPC mocks are mapped here but belong with each domain UI package; Package API not read function by function.
8. **Before publication:** Re-map the mock files to their UI packages in the plan.

## @rightkit/qa-chrome

1. **Exists as:** `packages/qa-chrome`.
2. **Planned feeds:** none in the plan.
3. **Today's implementation:** RightKit implementation, 1.9k lines.
4. **Across apps:** Chrome for Testing resolver and launcher; every app depends on it and none carries its own.
5. **Shape: best** (reference app: none; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | n/a | not covered by the HeardRight reviews |
   | viewright | fits | dependency present |
   | pulse | fits | dependency present |
   | coderight | fits | dependency present |
   | genright | fits | dependency present |
   | mailright | fits | dependency present |
   | scraperight | fits | dependency present |
   | cutright | fits | dependency present |
   | postright | fits | dependency present |
7. **Boundary problems:** none found.
8. **Before publication:** none.

## @rightkit/release

1. **Exists as:** `packages/release`.
2. **Planned feeds:** heardright 20,331, coderight 1,497, scraperight 676, pulse 609, postright 594, cutright 320.
3. **Today's implementation:** RightKit implementation grown from HeardRight's release scripts; 104k lines in about 380 flat modules.
4. **Across apps:** One config contract (right-release.config.mjs) is present in every app including both live builds, so adoption is real. The package is a monolith: build, signing, notarisation, updater, cargo/npm/swift publishing, ownership governance, QA admission, disk and snapshot management, iOS and Android, plus single-app rule files.
5. **Shape: gaps** (reference app: heardright; depth deep; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | not covered by the named HeardRight reviews; live build has a right-release.config.mjs; 20k planned lines not verified |
   | viewright | fits | config in live and main |
   | pulse | fits | config; membrane-assets and stage-receipt scripts remain |
   | coderight | fits | desktop and iOS configs |
   | genright | fits | config |
   | mailright | fits | config |
   | scraperight | fits | config; bundled-asset scripts remain |
   | cutright | fits | config |
   | postright | fits | config; crates/tools/check.rs is its own gate |
7. **Boundary problems:** legion-semantic-contracts.mjs is one app's rules inside the shared package; asr-artifact-adoption.mjs, media-sidecar-catalog.mjs, model-promote.mjs are domain policy; Ownership governance (about 35 ownership-*.mjs files) and registry publishing are separate products from app release; App asset-fetch scripts (fetch-ocr-assets, fetch-embed-model, fetch-tanker) are each app's copy of one job.
8. **Before publication:** Move Legion and ASR rule files behind a per-app rule plug-in; Split ownership governance and RightKit's own publishing from the app release CLI; Add one verified asset-fetch step to replace the per-app fetch scripts.

## @rightkit/ci

1. **Exists as:** `packages/ci`.
2. **Planned feeds:** none in the plan.
3. **Today's implementation:** RightKit implementation, 4.9k lines.
4. **Across apps:** Workflow templates and drift check for public repos; no app-side code planned for it.
5. **Shape: best** (reference app: none; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|

   n/a (no code mapped, no import found): heardright, viewright, pulse, coderight, genright, mailright, scraperight, cutright, postright.
7. **Boundary problems:** none found.
8. **Before publication:** none.

## @rightkit/ax

1. **Exists as:** `packages/ax`.
2. **Planned feeds:** genright 246, pulse 243.
3. **Today's implementation:** RightKit implementation, 4.4k lines.
4. **Across apps:** Plugin scaffolding and conformance gates; the two mapped files are an eval grader and a plugin launcher binary, neither a second implementation.
5. **Shape: best** (reference app: pulse; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | fits | plugin-launcher/src/main.rs is a Rust launcher, not AX code |
   | genright | fits | evals/grade.mjs is an eval grader |

   n/a (no code mapped, no import found): heardright, viewright, coderight, mailright, scraperight, cutright, postright.
7. **Boundary problems:** Both feeding files look mis-mapped (not verified by reading).
8. **Before publication:** none.

## rightkit-models

1. **Exists as:** `crates/rightkit-models`, `crates/rightkit-dictation-models`.
2. **Planned feeds:** heardright 6,182, genright 3,008, coderight 1,853, scraperight 1,838, cutright 908, mailright 340.
3. **Today's implementation:** two stores in one crate: a RightKit revisioned store, and the flat suite store ported from ScrapeRight's scraperight-model-store; plus CodeRight's local_models crate under a feature.
4. **Across apps:** The crate has two independent on-disk layouts and two APIs (suite/mod.rs:20-24). MailRight and GenRight install through Store/ModelSpec/InstallOptions; HeardRight, ScrapeRight and CutRight through suite::*; CodeRight uses only hardware fit. No duplicate code needs one store, and the live app's layout is the suite one.
5. **Shape: wrong** (reference app: heardright; depth deep; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | hr-review-process-runtime: right shape with gaps (Ultra spec and grant duplicated, Whisper off the route, full hash every launch); rightkit-dictation-models is a near copy of its model store |
   | coderight | fits | hardware/probe only; agent_local_models is its own crate copied in (158/160 lines) |
   | genright | gaps | revisioned Store in 8 files; model_management/* is already in rightkit-remote-compute |
   | mailright | gaps | revisioned Store plus suite::catalog and suite::paths in the same file set |
   | scraperight | fits | suite store is its own code; model_ops and commands_models are thin |
   | cutright | fits | suite::ensure_ready and catalog; build-*.py pack scripts are release tooling |

   n/a (no code mapped, no import found): viewright, pulse, postright.
7. **Boundary problems:** Two model stores with separate layouts, locks and GC entry points; suite/catalog.rs (1.6k lines) compiles named app models (parakeet, embedding gemma) into the foundation crate; agent_local_models is CodeRight code and pulls rightkit-sandbox and tokio; rightkit-dictation-models duplicates HeardRight's model store (HeardRight review); Depends on rightkit-daemon only for Suite::resolve; declares ureq beside rightkit-http; CutRight scripts/runtime/build-*.py are mapped here but are packaging scripts.
8. **Before publication:** Pick the suite layout as the one store and migrate Store users (MailRight, GenRight) or give Store a suite backend; Move the trusted catalog entries to data each app or a catalog crate supplies; Move agent_local_models to an agent crate; Resolve rightkit-dictation-models vs heardright-model-store; HeardRight M7 full-hash-on-launch.

## rightkit-onnx

1. **Exists as:** `crates/rightkit-onnx`.
2. **Planned feeds:** scraperight 304, coderight 205.
3. **Today's implementation:** RightKit implementation.
4. **Across apps:** Runtime resolution and configuration are used the same way by six apps; session building is still per app.
5. **Shape: gaps** (reference app: heardright; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | hr-review-process-runtime: right shape with gaps (own session builder, thread-budget copy) |
   | pulse | fits | SessionOptions, configured_runtime |
   | coderight | fits | resolve_runtime; ort_common is a thin crate |
   | genright | fits | 8 files |
   | mailright | fits | provision_runtime |
   | scraperight | fits | 9 files; ort_common thin |
   | cutright | fits | configure_runtime_verified |

   n/a (no code mapped, no import found): viewright, postright.
7. **Boundary problems:** CodeRight and ScrapeRight each keep an ort_common crate over it.
8. **Before publication:** Fold the two ort_common crates into the package; HeardRight O5.

## rightkit-spend

1. **Exists as:** `crates/rightkit-spend`.
2. **Planned feeds:** cutright 4,345, genright 2,755, postright 2,272, pulse 1,655.
3. **Today's implementation:** fresh RightKit ledger; PostRight's rules read closest to it.
4. **Across apps:** Pulse, PostRight and GenRight (through rightkit-media-gen) all reserve and commit on the same Ledger, each adding price and approval layers the crate deliberately leaves out. Two things are worked around: an allowance has no currency, and budget approval seals exist both in the crate and in Pulse. CutRight feeds nothing real: its ledger is an editorial decision log.
5. **Shape: gaps** (reference app: pulse; depth deep; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | gaps | budget.rs on Ledger/ReserveOptions (line 42) but keeps approval, HMAC seals, reports and audit; the crate has approved.rs and SealKey::from_keychain |
   | genright | fits | ledger.rs is already rightkit-media-gen on Ledger::reserve_on |
   | cutright | n/a | cutright-ledger is a hash-chained decision ledger (ledger.rs:1-9), not spend; mis-mapped, 4.3k lines |
   | postright | gaps | on Ledger; adds an allowance_currencies table because the crate's allowance holds no currency; price catalogue is app-side |

   n/a (no code mapped, no import found): heardright, viewright, coderight, mailright, scraperight.
7. **Boundary problems:** CutRight cutright-ledger is mapped here and is not a spend ledger; Approved-budget sealing in both rightkit-spend::approved and Pulse budget.rs (suspicion of duplicate); Price qualification rules are repeated per app (PostRight 60-day rule).
8. **Before publication:** Add an optional currency to Allowance; Make Pulse use approved.rs seals or remove them from the crate; Re-map CutRight's ledger to a review/evidence package.

## rightkit-time

1. **Exists as:** `crates/rightkit-time`.
2. **Planned feeds:** postright 233, mailright 69.
3. **Today's implementation:** RightKit implementation; PostRight calendar_time.rs is 84% in it.
4. **Across apps:** DateTime, civil date math and HTTP dates are used by six apps with no competing implementation left.
5. **Shape: best** (reference app: pulse; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | fits | 17 files |
   | coderight | fits | 20 files |
   | genright | fits | 3 files |
   | mailright | fits | utc_stamp.rs half in package |
   | scraperight | fits | 1 file |
   | postright | fits | calendar_time.rs can be deleted |

   n/a (no code mapped, no import found): heardright, viewright, cutright.
7. **Boundary problems:** none found.
8. **Before publication:** none.

## rightkit-cache

1. **Exists as:** `crates/rightkit-cache`.
2. **Planned feeds:** pulse 89.
3. **Today's implementation:** RightKit implementation.
4. **Across apps:** Two unrelated things share the crate: a size-capped disk cache (MailRight, CutRight) and LLM prompt-cache key tracking (CodeRight, PostRight). Pulse's cache.rs is a versioned snapshot file and uses neither.
5. **Shape: gaps** (reference app: pulse; depth light; confidence medium).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | n/a | cache.rs is a snapshot store on pulse_core::snapshot, not a cache |
   | coderight | fits | prefix::* only |
   | mailright | fits | DiskCache |
   | cutright | fits | DiskCache |
   | postright | fits | prefix::fnv1a_64 only |

   n/a (no code mapped, no import found): heardright, viewright, genright, scraperight.
7. **Boundary problems:** prefix (prompt-cache keys) belongs with rightkit-llm; Pulse cache.rs mis-mapped.
8. **Before publication:** Move prefix to rightkit-llm.

## rightkit-decimal

1. **Exists as:** `crates/rightkit-decimal`.
2. **Planned feeds:** none in the plan.
3. **Today's implementation:** RightKit implementation, 242 lines.
4. **Across apps:** No surveyed app imports it.
5. **Shape: gaps** (reference app: none; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|

   n/a (no code mapped, no import found): heardright, viewright, pulse, coderight, genright, mailright, scraperight, cutright, postright.
7. **Boundary problems:** none found.
8. **Before publication:** First consumer before mandatory; not verified.

## rightkit-contracts

1. **Exists as:** `crates/rightkit-contracts`, `contracts`.
2. **Planned feeds:** postright 1,218.
3. **Today's implementation:** RightKit implementation.
4. **Across apps:** PostRight and CutRight validate through it and reimplement no rule. The crate mixes content-pipeline documents, record sessions and the daemon API contract, so it is a domain crate placed in the mandatory runtime set.
5. **Shape: gaps** (reference app: postright; depth light; confidence low).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | cutright | fits | Contract, validate, typed record and brief views |
   | postright | fits | contracts_io.rs runs the crate's stages in order (lines 3-5) |

   n/a (no code mapped, no import found): heardright, viewright, pulse, coderight, genright, mailright, scraperight.
7. **Boundary problems:** Content brief and delivery envelope are media/social domain; daemon-api is runtime.
8. **Before publication:** Take it out of the mandatory set or split daemon-api from the content contracts.

## Family summary

Counts: best 12, gaps 18, wrong 3, missing 0.

**Real needs the packages do not meet**

- A terminal (PTY, ConPTY) driver for QA: CodeRight's 6.8k-line Python harness has no counterpart in `rightkit-qa`.
- Compaction of a keyed database in place (MailRight `db_compaction`).
- A currency on a spend allowance (PostRight adds its own table).
- A shared sync scheduler (HeardRight and ScrapeRight each have one).
- A job store that can back an in-memory read model (ScrapeRight keeps a parallel registry).

**Accidental divergence**

- Two crash install calls (`install_panic_hook`, `install_process_hook`) for the same job.
- Two installed-app discovery copies (GenRight, CutRight) beside `discover_apps_in_with`.
- Two `ort_common` crates (CodeRight, ScrapeRight) over `rightkit-onnx`.
- Per-app asset fetch scripts beside `@rightkit/release`.
- Two model store APIs chosen by app history, not by need.

**Duplicates inside RightKit (existence verified; sameness of bodies is a suspicion unless stated)**

- Root and run-mode resolution: `rightkit-fs::app_dirs`, `rightkit-fs::StateDir`, `rightkit-daemon::{Suite, AppRoot, mode}`.
- Hashing: `rightkit-fs::digest` and `rightkit-crypto`.
- Supervision: `rightkit-job-queue::EngineSupervisor`, `rightkit-daemon::supervisor`, `rightkit-child-process` restart policy.
- Pairing: `rightkit-identity`, `rightkit-mesh`, `rightkit-daemon::remote`, `rightkit-device-link`.
- Model stores: `rightkit-models::store`, `rightkit-models::suite`, `rightkit-dictation-models` (the last verified as a near copy in the HeardRight review).
- Budget seals: `rightkit-spend::approved` and Pulse `core/src/budget.rs`.

**Plan mapping errors worth fixing in the matrix**

- CutRight `cutright-ledger` (4.3k lines) is an editorial decision ledger, not spend.
- MailRight `core_ipc/*` (3.1k lines) is a daemon service, not sidecar framing.
- Pulse `activity_lease.rs`, `cache.rs`, `diag_reports.rs`, `platform/*`, `subprocess.rs`, `devicelink/*` each belong elsewhere.
- Renderer IPC mocks (GenRight, MailRight, ScrapeRight) belong to their UI packages, not `@rightkit/qa`.
- No app consumes `rightkit-passkeys`, `rightkit-sync-keys`, `rightkit-decimal`, `rightkit-identity` or `rightkit-qa-driver`; they should not be in a mandatory set on today's evidence.

## Not verified

- Function-level behaviour of any package against any app other than through HeardRight's earlier reviews.
- Version pins: apps pin different releases (CutRight pins `rightkit-fs =0.4.7`); RightKit was read at head.
- Whether `rightkit-file-watch`'s three front ends share one backend.
- `@rightkit/qa`, `@rightkit/ci`, `@rightkit/ax`, `@rightkit/qa-chrome` APIs.
- HeardRight against qa, release, ci and ax.
- ViewRight's upgrade path from live 0.1.123 onto the crates main uses.
