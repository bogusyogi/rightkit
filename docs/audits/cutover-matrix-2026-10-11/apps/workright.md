# WorkRight

Fresh source-only audit at WorkRight `f4a27f8099456893aa74e7ec87a13f68c676506f` against RightKit `40ddcd6a10e066c84d28fb8c53828b67fad302e5`. Matrix covers all 97 catalog rows: **3 N, 0 M, 0 Y, 94 NA**. No Swift/Xcode/watchOS target exists, so all 15 Swift rows are NA. No active RightKit dependency/import was found.

## Active local routes requiring cutover

| Component | WorkRight production evidence | RightKit owner API | Migration task |
|---|---|---|---|
| `cargo:rightkit-db` | `crates/local-store/src/lib.rs:168` opens SQLite; `backend/server/src/db.rs:397` opens backend pool | `crates/rightkit-db/src/lib.rs:237` `DbOptions::open_writer` | Replace local DB open/pool paths; prove SQLCipher/key handling, migrations, WAL & backup behavior. |
| `cargo:rightkit-logs` | `crates/core-service/src/main.rs:196` installs tracing; `backend/server/src/main.rs:85` initializes tracing | `crates/rightkit-logs/src/tracing_file.rs:154` `install_tracing` | Adopt shared sinks, rotation & redaction; retain WorkRight event fields and service diagnostics. |
| `npm:@rightkit/app-shell` | `apps/agent-ui/src/App.tsx:43`; `apps/dashboard/src/App.tsx:32` render local navigation/frame | `packages/app-shell/src/react/layout.tsx:518` `AppShell` | Adopt shared frame/navigation shell while retaining WorkRight routes and monitoring UX. |

`cargo:rightkit-daemon` is NA: its local daemon-api route/state contract differs from WorkRight's public workforce-ingest service. `cargo:rightkit-jobs` is NA: worker-job helper is dormant & jobs route is only heartbeat. `cargo:rightkit-record` is NA: capture emits aggregate activity counters, not frames or verified session bundles. `cargo:rightkit-secrets` & `cargo:rightkit-settings` are NA: caller-supplied DB keys and workforce policy cache do not implement OS credential storage or versioned settings documents. `npm:@rightkit/platform-ui`, `settings-ui`, `theme`, `shell`, & `logs` are NA: current React targets provide routes/status polling with no matching platform, settings, theme, bridge, or diagnostics client. `npm:@rightkit/legal` is NA: consent receipt logic is Rust-only with no JavaScript bridge. `npm:@rightkit/updates` is NA: updater config has no runtime check/install client. `cargo:rightkit-service` is NA: `core-service` only registers OS manifests (`crates/core-service/src/lib.rs:166`, `scripts/install-core.sh:62`); no active local endpoint discovery/authenticated transport exists.

Full row-level evidence, searches & rationale: [workright.json](./workright.json).
