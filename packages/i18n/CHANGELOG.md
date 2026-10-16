# Changelog

## 0.1.1 - unreleased

- Added a synchronous `t(locale, key, { fallback }?)` and `isLoaded(locale)` to the object `createI18n` returns (rightkit#241, for Ember), for code outside React such as command handlers and plain modules. It reads only catalogs already loaded (the default locale, plus any passed to `loadLocale` or set through the provider) and never loads. `key` is an exact top-level key (a source string may contain dots) or a dotted path. Resolution is the locale's catalog, then the default catalog, then `options.fallback`, then the key itself, so a source string used as the key reads as itself when untranslated. Non-string values count as missing; an empty string is a translation. Additive: `useI18n().t` (the catalog object) is unchanged. New exported type `TranslateOptions`. Test: `translates synchronously from loaded catalogs with fallbacks` in `src/index.test.tsx`. Not built or run; verification is the CI run. `dist/` is not regenerated.

## 0.1.0

- Initial release: `createI18n`, `mergeCatalog`, provider and `useI18n`.
