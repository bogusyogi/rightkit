# Changelog

## 0.1.0 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- First release under this name: the generation engine client that `rightkit-pipeline` 0.1.x held (moved with `git mv` from `cutright-genright`): `GenerationClient` (was `GenRightClient`), `Orchestrator`, `Journal`, `ModelCatalog`, `generated_source`.
- The process call is `rightkit_engine_host::call` (`EngineProcess`, re-exported as `ProcessRunner`; `Runner`, `RawOutput`, `EngineInfo`, `parse_reply`, `handshake`); this crate keeps its typed `Error` and converts `CallError` into it. One behaviour change from that call: a relative engine path is refused (`Error::Spawn`), and the error envelope is recognised as an object whose only key is `error`.
- Host event names are `media_gen.*` (were `genright.*`); error messages say "generation engine" instead of naming the product.
- `channel_recipe` (feature `channel-recipe`) is not here: it is a render-preset check and moved to `rightkit-video-project` 0.1.3.
