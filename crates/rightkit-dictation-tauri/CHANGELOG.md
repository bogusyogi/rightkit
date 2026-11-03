# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight. Platform delivery, focus capture, history stores,
  settings, vocabulary, sync and media control are reached through `host::DictationHost`.
- Environment switches renamed to `RIGHTKIT_DICTATION_ENGINE`, `RIGHTKIT_SIDECAR_AUTORESTART`, `RIGHTKIT_HISTORY`.
- `watchdog_tick` takes the state-emission closure instead of an app handle.
- The microphone-denied pill label no longer names the product.
