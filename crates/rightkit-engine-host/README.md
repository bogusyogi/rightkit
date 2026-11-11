# rightkit-engine-host

The engine binary framework: one registry of named commands served two ways, as a headless JSON
CLI and as the resident stdio sidecar a shell supervises (over `rightkit-job-queue`'s
`Dispatcher`), with the shared wire types (`EngineError`, `EngineEvent`, `EngineRequest`,
`engine.init`/`engine.health`), contract and TypeScript command-union export, the
QA marker, and the caller side of the same CLI (`call`) for one engine calling another.

Built from the framework pieces of GenRight's `genright-engine` (`main.rs`, `main_stdio.rs`,
`commands.rs`) and `genright-protocol` (`lib.rs`) on 2026-11-01. Product commands, settings and
init payloads stay in the app: it registers its commands and hooks here.

CLI: `<engine> <words...> [--input <json>|-] [--dry-run] [--wait] [--request-id <id>]
[--wait-timeout <s>] [--data-dir <dir>]`; built-ins `--version`, `--help`, `--contract`,
`--contract-ts [<TypeName>]`. Results are JSON on stdout; failures are
`{"error":{code,message,details?}}` with exit 1 (failed) or 2 (usage).

Caller side (`call`): `EngineProcess` runs an installed engine by absolute path, passes `--input`
on the command line up to 8 KiB and through `--input -` and stdin past it (a null stdin otherwise),
caps each captured stream at 64 MiB and ends the engine's process tree at its deadline;
`parse_reply` turns stdout into the result or `EngineError`; `handshake` checks the callee's
`doctor` (exact contract version, minimum capability versions). `rightkit-media-gen-client` is
built on it.

Not here: the video project's engine layer (`rightkit_video_project::engine` and `cmd`, feature
`engine-commands`) and the doctor report (`rightkit-doctor`). HeardRight's engine speaks typed frames
over `rightkit-job-queue` directly and does not use this crate; it is not on the mandatory set.
