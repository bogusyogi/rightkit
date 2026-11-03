# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- New, additive (feature `channel-recipe`, off by default): `channel_recipe::validate(body, styles)`, the typed read-only check of a channel recipe body (edit presets, formats, caption styles from the caller's `StyleCatalog`, colour and audio ids, LUT and level ranges), moved from CutRight's `cutright-engine`. Unknown keys are kept, never refused.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cutright-genright` (CutRight).
