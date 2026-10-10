# Changelog

## 0.1.2 - 2026-10-10

- Crate description now starts with "DEPRECATED: use rightkit-qts-ggml-sys directly", so the deprecation shows on the registry page. Set `repository` to `https://github.com/bogusyogi/rightkit` and add `rust-version = "1.98.0"`. `license` stays `Apache-2.0` (vendored upstream). No code or API change.

## 0.1.1

- Deprecate the crate. `rightkit-qts` depends on `rightkit-qts-ggml-sys` directly; use that crate instead. The API is unchanged and no further releases are planned beyond compatibility fixes.
- Correct `PINNED.md`: `ort` is pinned `=2.0.0-rc.13`, matching `rightkit-qts`.
