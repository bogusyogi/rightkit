# Changelog

## 0.1.3

- Publish the dependency requirements main already declares: `rightkit-fs` 0.5.0, where the published 0.1.2 requires 0.4.9. The 0.1.2 manifest on crates.io stays as published; this version carries the new requirements.

## 0.1.2

- Host naming option, default unchanged: `set_deeplink_scheme` / `deeplink_scheme()` (default `rightkit-agent`, `DEFAULT_DEEPLINK_SCHEME`); the deeplink builder writes it and the parser requires it.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-recipes` (CodeRight).
