# @rightkit/i18n

Small, brand-neutral i18n runtime for React apps. The app supplies an English catalog and optional lazy locale loaders. Missing keys in a translated catalog fall back to English, merged recursively.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/i18n` | `createI18n(english, options)`, `mergeCatalog(base, override)`, types `Catalog`, `Locale`, `LocaleLoader`, `I18nOptions`, `I18nValue` |

`createI18n` returns `{ Provider, I18nProvider, useI18n, loadLocale, mergeCatalog, defaultLocale, t, isLoaded }`. `Provider` takes `initialLocale`. `useI18n()` returns `{ locale, catalog, t, loading, error, setLocale }`. Options: `defaultLocale` (default `"en"`), `loaders`, `rtlLocales` (sets `dir="rtl"` on `<html>`).

`locales` (`{ id, name, englishName, aliases? }[]`) is the language picker list; `normalizeLocale(tag)` maps `zh-CN` to `zh` and aliases such as `zh-TW` to `zh-hant`. `persistence` (`load()`, `save(locale)`) restores the chosen language on mount and saves it on `setLocale`, rolling back when the save fails (`saveError`). Catalog values may be functions; `t(locale, key, { args })` calls them.

`t(locale, key, { fallback }?)` is a synchronous lookup for code outside React. It reads only catalogs already loaded (the default locale and any passed to `loadLocale`) and never loads. `key` is an exact top-level key (so a source string with dots works) or a dotted path. It resolves the locale's catalog, then the default catalog, then `fallback`, then `key` itself. `isLoaded(locale)` says whether a locale's catalog is loaded.

Peer dependency: `react` `>=18 <21`.

## Example

```tsx
import { createI18n } from "@rightkit/i18n";
import en from "./locales/en.json";

const i18n = createI18n(en, {
  loaders: { fr: async () => (await import("./locales/fr.json")).default },
});

export const { Provider, useI18n } = i18n;

// <Provider><App /></Provider>
// const { t, setLocale } = useI18n(); await setLocale("fr");
```

## Known issues

- No other package in this workspace declares `@rightkit/i18n`; consumers in app repositories outside it are unverified. Confirm a consumer before relying on this package.
