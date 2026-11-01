# rightkit-doctor

A doctor framework for engines and command-line tools. The app supplies its identity, its
capabilities and its checks through one trait; this crate supplies the rest.

- `verdict`: pass / warn / block checks, the rolled-up verdict, text rendering, JSON with a
  schema tag, and the exit-code rule (0 ready, 1 warnings, 2 blocked); `--verbose` evidence.
- `row`: the engine check row (`id`, `required`, `status`, `detail`, optional `location`) and the
  `readiness` roll-up (`ready` when every required row is `ok`).
- `report`: the engine doctor report envelope (`app`, `engine`, `app_version`,
  `contract_version`, `capabilities`, `readiness`, `paths`, `platform`, `dry_run`, app extras).
- `command`: the `doctor` command for `rightkit-engine-host` registries (`DoctorHost`, `spec`,
  `register`, `wrap`).
- `probe`: tools on `PATH` and their versions, writable folders, bounded child processes, free
  ports, the system clipboard. Probes never read a secret or send a request.
- `clock`: UTC timestamps and stderr lifecycle lines.
- `host_dirs`: agent-host configuration folders and small file helpers.

Built on 2026-11-01 from the product-neutral parts of the doctors of Legion, CodeRight,
PostRight, GenRight, CutRight and MailRight. What each app checks (its models, stores, sandbox
tiers, hook trust and naming contracts) stays in the app and plugs in through `DoctorHost`.

## Startup

```rust,ignore
let host = std::sync::Arc::new(MyDoctor::new());          // implements DoctorHost
let registry = rightkit_doctor::command::register(registry);
let handler = rightkit_doctor::command::wrap(host, inner_handler);
```
