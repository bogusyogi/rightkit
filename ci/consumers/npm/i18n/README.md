# @rightkit/i18n npm consumer (draft, npm.@rightkit/i18n)

Packed-archive consumer for `@rightkit/i18n` 0.2.0 (workspace version). Recipe ID: `i18n-catalog-provider-ssr-consumer`. The unit is installed only from the sealed `file:` archive. Its one peer, `react` (`>=18 <21`), is installed as an exact reviewed pin together with `react-dom`, `jsdom` and the Testing Library packages the effect scenarios need; none of them is a dependency of the unit.

Closure (exact pins, all installed): `react` 19.3.1, `react-dom` 19.3.1, `jsdom` 30.1.3, `@testing-library/react` 16.3.3, `@testing-library/dom` 10.4.2 (the Testing Library DOM package is a peer of the React one). These are the pins `@rightkit/settings-ui` and `@rightkit/onboarding` already use. No `@rightkit/*` package is in the closure.

Surface (exact `exports` key): `.`, whose runtime module exports exactly `createI18n` and `mergeCatalog`. The shipped type declarations name `Catalog`, `I18nOptions`, `I18nValue`, `Locale`, `LocaleInfo`, `LocaleLoader`, `LocalePersistence`, `Message`, `TranslateOptions`, `createI18n` and `mergeCatalog`, and the consumer asserts that list.

Scenarios (7):

1. Installed manifest bound to the archive (name and version, `type: module`, the `types` and `import` conditions of `.`, the single `react` peer, `sideEffects: false`, no dependencies), the exact module export surface and type-declaration names, and the ten members `createI18n` returns.
2. `mergeCatalog`: recursive merge that keeps sibling keys, arrays and leaves replace instead of merging, a non-object or array override returns the base, the base is never mutated, and the instance form merges over its own catalog.
3. The synchronous `t` and `isLoaded`: default and unloaded locales read the default catalog and never load, dotted paths and exact keys that contain dots, key-by-key fallback to the default catalog, the `fallback` option, then the key itself, non-string and inherited values counted as missing, an empty translation kept, function messages called with `args`, asynchronous loaders called with the locale and cached, a locale without a loader, and a loader that throws.
4. The locale registry: `normalizeLocale` for exact ids, aliases (`zh-TW`, `zh_HK`), language prefixes (`zh-CN`, `zh-hant-TW`), case and whitespace, and unknown or empty tags; `locales` as given and as defaulted; a registry taken from the loader keys; a non-English default locale.
5. Server-rendered markup (`react-dom/server` `renderToStaticMarkup`): the exact markup for the default catalog, an unloaded `initialLocale` showing the default catalog, a loaded French catalog with untranslated keys falling back to English, deterministic output, a right-to-left locale's catalog, the hook without a Provider, function messages, the `loading`, `error`, `saving`, `saveError` and `locales` members, and two instances that share nothing.
6. Provider effects in a DOM (jsdom, mounted with Testing Library): `setLocale` loads and applies a catalog, sets `lang` and `dir` on the document element for a right-to-left locale and back, keeps untranslated English keys, a failed load rejects with the loader's error and leaves the language as it was, the error clears on the next selection, and `initialLocale` loads after mount.
7. Persistence: a stored tag is restored through the registry without being saved again, a stored tag that no longer resolves is ignored, a chosen language is saved and a failed save rolls the language back, sets `saveError` and rejects while the next attempt succeeds, saves run in call order and a superseded failure does not roll back a newer selection, and a delayed startup restore cannot overwrite an explicit selection.

jsdom is a DOM emulation in Node, not a browser; no browser, no network, no credentials, no user data, no timer longer than 30 ms. Writes nothing.

Not covered: real browser layout and the document language and direction as a browser applies them, and apps that consume the package (the package's own README notes that no other package in the workspace declares it).
