# Changelog

## Unreleased

Added (#616, CodeRight port; additive):
- `locales` option (`{ id, name, englishName, aliases? }`) and `normalizeLocale(tag)`: exact id or alias first, then the longest prefix (`zh-CN` -> `zh`, `zh-hant-TW` -> `zh-hant`). `useI18n()` and the returned object expose `locales`.
- `persistence` option (`load(): Promise<Locale | undefined>`, `save(locale)`). The stored tag is restored after mount without saving. `setLocale` saves the chosen language; a failed save restores the previous language, sets `saveError` and rejects. `useI18n()` adds `saving` and `saveError`.
- Function messages: a catalog value may be a function. `t(locale, key, { args })` calls it with `args`; `useI18n().t` types them through the catalog.

Tests: `locale registry, persistence and function messages` in `src/index.test.tsx`. Not built or run; verification is the root integration batch. `dist/` is not regenerated.

## 0.1.1 - unreleased

- Added a synchronous `t(locale, key, { fallback }?)` and `isLoaded(locale)` to the object `createI18n` returns (rightkit#241, for Ember), for code outside React such as command handlers and plain modules. It reads only catalogs already loaded (the default locale, plus any passed to `loadLocale` or set through the provider) and never loads. `key` is an exact top-level key (a source string may contain dots) or a dotted path. Resolution is the locale's catalog, then the default catalog, then `options.fallback`, then the key itself, so a source string used as the key reads as itself when untranslated. Non-string values count as missing; an empty string is a translation. Additive: `useI18n().t` (the catalog object) is unchanged. New exported type `TranslateOptions`. Test: `translates synchronously from loaded catalogs with fallbacks` in `src/index.test.tsx`. Not built or run; verification is the CI run. `dist/` is not regenerated.

## 0.1.0

- Initial release: `createI18n`, `mergeCatalog`, provider and `useI18n`.
