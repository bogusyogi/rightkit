# Changelog

## 0.1.0 (unpublished)

Issue #960 (one catalog source generated to TypeScript, Rust and Swift, with a completeness check).

- New crate. Source format decided: a folder of `<locale>.json` string catalogs with `en.json` as the reference (the files `@rightkit/i18n` and `rightkit-i18n` already read), not PO or Fluent.
- `check` reports missing or empty keys, extra keys and placeholder mismatches per locale; `generate --target ts|rust|swift` writes deterministic tables and refuses an incomplete catalog; `--check` detects a stale output.
