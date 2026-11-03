# Changelog

## 0.1.0 (unpublished)

Issue #416 (Pulse Windows port: the native notch needed a Rust runtime for the `@rightkit/i18n` catalogs; `windows/src/i18n.rs` held its own copy).

- New crate. `I18n` loads JSON catalogs, resolves locale tags as `normalizeLocale` does, merges a locale over English as `mergeCatalog` does, and looks up keys as `t` does.
- Function messages from the TypeScript package are not represented; catalogs are JSON string messages.
