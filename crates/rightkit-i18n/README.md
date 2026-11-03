# rightkit-i18n

Rust runtime for the shared `@rightkit/i18n` catalogs. It loads the same JSON catalog files the TypeScript package loads, so a native shell (for example the Windows notch) can read the copy the webview UI reads.

Catalog capability: `localization`. Not yet published (0.1.0, unreleased).

## Entry points

- `I18n::new(english, default_locale, locales)`: the runtime. `english` is a `Catalog` (a `serde_json::Map`). `locales` is the language picker list (`LocaleInfo { id, name, english_name, aliases }`), or `None` to register locales one at a time with `register_locale`.
- `load_locale(locale, json)`: loads one locale's catalog from its JSON text. Only a JSON object is accepted.
- `normalize_locale(tag)`: exact id or alias first, then the longest matching prefix (`zh-CN` to `zh`, `zh-hant-TW` to `zh-hant`). `None` when nothing matches.
- `t(locale, key, fallback)`: the locale's string for `key`, else the English string, else `fallback`, else `key`. `key` is an exact top-level key or a dotted path. Non-string values count as missing. Never loads anything.
- `catalog(locale)`: the English catalog merged with the locale's catalog (`merge_catalog`, recursive, objects merge key by key).
- `is_loaded(locale)`, `default_locale()`.

## Scope

Catalog messages are strings. The TypeScript package also accepts function messages with arguments; those exist only in JavaScript and are not represented here, so a catalog that uses them is not readable by this crate.

No unsafe code. The crate does not read files itself; the app reads the catalog and passes the text to `load_locale`.
