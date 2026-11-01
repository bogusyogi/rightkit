# rightkit-engine-host

The engine binary framework: one registry of named commands served two ways, as a headless JSON
CLI and as the resident stdio sidecar a shell supervises (over `rightkit-job-queue`'s
`Dispatcher`), with the shared wire types (`EngineError`, `EngineEvent`, `EngineRequest`,
`engine.init`/`engine.health`), contract and TypeScript command-union export, and the
isolated-data-folder rule.

Built from the framework pieces of GenRight's `genright-engine` (`main.rs`, `main_stdio.rs`,
`commands.rs`) and `genright-protocol` (`lib.rs`) on 2026-11-01. Product commands, settings and
init payloads stay in the app: it registers its commands and hooks here.

CLI: `<engine> <words...> [--input <json>|-] [--dry-run] [--wait] [--request-id <id>]
[--wait-timeout <s>] [--data-dir <dir>]`; built-ins `--version`, `--help`, `--contract`,
`--contract-ts [<TypeName>]`. Results are JSON on stdout; failures are
`{"error":{code,message,details?}}` with exit 1 (failed) or 2 (usage).
