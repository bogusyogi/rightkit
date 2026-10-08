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

`createI18n` returns `{ Provider, I18nProvider, useI18n, loadLocale, mergeCatalog, defaultLocale }`. `Provider` takes `initialLocale`. `useI18n()` returns `{ locale, catalog, t, loading, error, setLocale }`. Options: `defaultLocale` (default `"en"`), `loaders`, `rtlLocales` (sets `dir="rtl"` on `<html>`).

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

- No consumers were found in the scanned app repositories (npm audit 2026-10-08). Confirm a consumer before relying on this package.
