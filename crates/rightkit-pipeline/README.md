# rightkit-pipeline

Deprecated. The crate is now `rightkit-media-gen-client`, the client of the generation engine
contract (submit, follow, import, crash-safe journal, model catalog checks). 0.2.0 re-exports it
for one release; depend on `rightkit-media-gen-client` instead.

| 0.1.x item | Now |
|---|---|
| `GenRightClient` | `rightkit_media_gen_client::GenerationClient` (deprecated alias here) |
| everything else at the root and in `engine`, `error`, `journal`, `models`, `orchestrator`, `generated_source` | the same names in `rightkit_media_gen_client` |
| `ProcessRunner`, `Runner`, `RawOutput`, `EngineInfo` | `rightkit_engine_host::call` (re-exported by the client) |
| `channel_recipe` (feature `channel-recipe`) | `rightkit_video_project::channel_recipe` (feature `channel-recipe`) |

Moved into RightKit from `cutright-genright` (CutRight) on 2026-11-01.
