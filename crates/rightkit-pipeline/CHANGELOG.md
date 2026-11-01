# Changelog

## 0.1.2 (unreleased)

- Add `dispatch` (brief dispatch to an engine process with attempt rows, sweep of dead owners, envelope acceptance), `dispatch_protocol` and the `dispatch_host` seam, moved from PostRight. Engine executables are named by `RIGHTKIT_EDITOR_ENGINE` and `RIGHTKIT_GENERATOR_ENGINE`; the `test-mode` feature enables QA isolation.

## 0.1.1 (unreleased)

- New, additive (feature `channel-recipe`, off by default): `channel_recipe::validate(body, styles)`, the typed read-only check of a channel recipe body (edit presets, formats, caption styles from the caller's `StyleCatalog`, colour and audio ids, LUT and level ranges), moved from CutRight's `cutright-engine`. Unknown keys are kept, never refused.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cutright-genright` (CutRight).
