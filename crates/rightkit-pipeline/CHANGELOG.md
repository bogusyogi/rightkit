# Changelog

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- Deprecated: the code moved with `git mv` to `rightkit-media-gen-client` 0.1.0, named for what it holds; this crate re-exports it. `GenRightClient` is a deprecated alias of `GenerationClient`.
- Breaking: the `channel-recipe` feature is gone; `channel_recipe` is `rightkit_video_project::channel_recipe` (`rightkit-video-project` 0.1.3, feature `channel-recipe`).
- Breaking: the process call is `rightkit_engine_host::call` (one implementation for every engine-to-engine caller); host events are `media_gen.*` (were `genright.*`). See the `rightkit-media-gen-client` changelog.
- The `live_step3` example moved to `rightkit-media-gen-client`.

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.
- Removed the `fake-engine` feature, the `fake-genright-engine` binary, `tests/orchestration.rs`, the public `Failpoint` enum, `Orchestrator::set_failpoint`, `Error::Injected` and `ProcessRunner::with_env` (test-only code).

## 0.1.1 (unreleased)

- New, additive (feature `channel-recipe`, off by default): `channel_recipe::validate(body, styles)`, the typed read-only check of a channel recipe body (edit presets, formats, caption styles from the caller's `StyleCatalog`, colour and audio ids, LUT and level ranges), moved from CutRight's `cutright-engine`. Unknown keys are kept, never refused.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cutright-genright` (CutRight).
