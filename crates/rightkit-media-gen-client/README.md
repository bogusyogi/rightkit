# rightkit-media-gen-client

Client of the generation engine contract: drive an installed media-generation engine (submit, follow, import) with a crash-safe journal and recovery, model catalog checks and generated-source import, over rightkit-engine-host's engine call.

An engine or app that wants generated media (an editor, a campaign tool) calls the installed
media-generation engine, the CLI `rightkit-media-gen` serves (GenRight's engine; contract v2,
`docs/ENGINE-CLI.md` in GenRight):

| Module | Contents |
|---|---|
| `engine` | `GenerationClient`: `doctor` handshake (contract 2 and the required capabilities), `models`, `generate`, `media modalities`, `media generate`, `job status/result/list/ack/cancel`, `asset resolve`, with typed replies (`JobRecord`, `ResultDescriptor`, `AssetDescriptor`, `AssetResolution`). |
| `orchestrator` | `Orchestrator`: submit with a caller request id, recover after a restart (look the request up first, resubmit only when the engine never recorded it), wait, import into a project's `media/`, acknowledge. |
| `journal` | The crash-safe import journal (`<project>/imports/journal.json`). |
| `models` | The model catalog and spec checks (durations, kinds, cheapest model). |
| `generated_source` | Reading and importing a generated-source shot manifest. |

The process call is `rightkit_engine_host::call` (absolute engine path, `--input` or stdin, bounded
capture, the error envelope, the contract and capability handshake): every engine-to-engine caller
uses that one implementation. `ProcessRunner` is `rightkit_engine_host::call::EngineProcess`.

Host events carry `event` names `media_gen.request`, `media_gen.job`, `media_gen.job_progress`,
`media_gen.progress`, `media_gen.import`, `media_gen.resolved`, `media_gen.ack` and
`media_gen.ack_failed`. Imported files keep the names earlier builds wrote
(`media/genright-<job>-<asset>.<ext>`), so a project imported before keeps resolving.

Moved into RightKit from CutRight's `cutright-genright` on 2026-11-01 as `rightkit-pipeline`; named for
what it holds on 2026-11-11. `rightkit-pipeline` 0.2.0 re-exports this crate for one release.
