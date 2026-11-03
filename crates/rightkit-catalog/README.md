# rightkit-catalog

One localisation catalog source, generated to TypeScript, Rust and Swift, with a completeness check. It answers the "no Rust i18n owner" gap of rightkit#960 without a second catalog format: the source stays the JSON string catalogs `@rightkit/i18n` and `rightkit-i18n` already read.

Catalog capability: `localization-catalog`. Not yet published (0.1.0, unreleased).

## Decision: the source format is JSON

A catalog folder holds `<locale>.json` files (`en.json`, `de.json`, `zh-hant.json`). Each is an object of strings; nesting joins keys with `.`. `en.json` is the reference and is required. PO and Fluent were considered and not taken: the web package and the Rust runtime already load these JSON files, so JSON needs no converter and adds no second format. Plurals and contexts are separate keys (`files.one`, `files.other`), compared like any other key. A placeholder is `{name}`; `{{` and `}}` are literal braces.

## Commands

```sh
rightkit-catalog check i18n/
rightkit-catalog generate i18n/ --target ts    --out src/generated/messages.ts
rightkit-catalog generate i18n/ --target rust  --out src/generated/messages.rs
rightkit-catalog generate i18n/ --target swift --out Sources/App/Catalog.swift
rightkit-catalog generate i18n/ --target rust  --out src/generated/messages.rs --check   # exit 1 when stale
```

`check` exits 1 and prints one line per problem: a key English has that a locale lacks (or leaves empty), a key a locale has that English lacks, a message whose placeholders differ from English's. `generate` runs the same check first, so a generated table is always complete; output is sorted by key and deterministic. Put `check` in the build or CI step of any app that ships more than one locale.

## Generated shapes

- TypeScript: `export const messages = { "en": { "menu.open": "Open" }, ... } as const;` plus `Locale` and `MessageKey` types.
- Rust: `pub static MESSAGES_EN: Table` per locale (`Table = &'static [(&'static str, &'static str)]`), `LOCALES`, and `messages(locale) -> Option<Table>`. Feed it to `rightkit-i18n`'s `load_locale` after building the JSON, or look keys up directly.
- Swift: `public enum Catalog { static let locales; static let messages: [String: [String: String]] }`. Very large catalogs make a slow Swift dictionary literal; split by feature folder when that bites.

## Library

`load_dir`, `parse_catalog`, `check`, `generate`, `placeholders`, `Finding`, `Target`.

## Scope

Locale resolution and runtime lookup stay in `@rightkit/i18n` and `rightkit-i18n`. Function messages (TypeScript only) are rejected by the parser with an explicit error. No unsafe code.
