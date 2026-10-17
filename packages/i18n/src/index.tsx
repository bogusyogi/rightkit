import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

export type Catalog = Record<string, unknown>;
export type Locale = string;
export type LocaleLoader<C extends Catalog> = (locale: Locale) => Promise<Partial<C>> | Partial<C>;

/** One entry of the app's language picker. `aliases` are extra tags that resolve to `id` (e.g. `zh-TW` for `zh-hant`). */
export interface LocaleInfo {
  id: Locale;
  name: string;
  englishName: string;
  aliases?: readonly string[];
}

/**
 * Saves and loads the chosen language, typically through the app's config client (`display.language`).
 * `load` resolves to the stored tag or `undefined`; `save` rejects when the write fails, and the provider rolls back.
 */
export interface LocalePersistence {
  load(): Promise<Locale | undefined>;
  save(locale: Locale): Promise<void>;
}

export interface I18nOptions<C extends Catalog> {
  defaultLocale?: Locale;
  loaders?: Record<string, LocaleLoader<C>>;
  rtlLocales?: readonly Locale[];
  /** Language picker entries. Default: just the default locale. */
  locales?: readonly LocaleInfo[];
  /** Saves the language chosen through `setLocale` and restores it on mount. Without it nothing is saved. */
  persistence?: LocalePersistence;
}

export interface I18nValue<C extends Catalog> {
  locale: Locale;
  catalog: C;
  t: C;
  loading: boolean;
  error: Error | null;
  setLocale: (locale: Locale) => Promise<void>;
  /** The `locales` option as given. */
  locales: readonly LocaleInfo[];
  /** True while a chosen language is being saved through `persistence`. */
  saving: boolean;
  /** Set when saving the chosen language failed. The previous language is restored. Cleared by the next `setLocale`. */
  saveError: Error | null;
}

const toError = (cause: unknown): Error => cause instanceof Error ? cause : new Error(String(cause));

const isRecord = (value: unknown): value is Catalog => typeof value === "object" && value !== null && !Array.isArray(value);

export function mergeCatalog<C extends Catalog>(base: C, override: unknown): C {
  if (!isRecord(override)) return base;
  const merge = (left: unknown, right: unknown): unknown => {
    if (!isRecord(left) || !isRecord(right)) return right;
    const result: Catalog = { ...left };
    for (const [key, value] of Object.entries(right)) result[key] = merge(result[key], value);
    return result;
  };
  return merge(base, override) as C;
}

export interface TranslateOptions {
  /** Returned when no loaded catalog has a string for the key. Default: the key itself. */
  fallback?: string;
  /** Passed to a function message (e.g. `theme.installed` taking a name). Ignored for plain strings. */
  args?: readonly unknown[];
}

/** A catalog message: a string, or a function that builds one from arguments. */
export type Message = string | ((...args: never[]) => string);

const hasOwn = (value: object, key: string) => Object.prototype.hasOwnProperty.call(value, key);

/** A string or function message at `key` (exact top-level key first, then a dotted path). Other values are missing. */
function lookupMessage(catalog: unknown, key: string): Message | undefined {
  if (!isRecord(catalog)) return undefined;
  const isMessage = (value: unknown): value is Message => typeof value === "string" || typeof value === "function";
  if (hasOwn(catalog, key)) {
    const direct = catalog[key];
    return isMessage(direct) ? direct : undefined;
  }
  let node: unknown = catalog;
  for (const part of key.split(".")) {
    if (!isRecord(node) || !hasOwn(node, part)) return undefined;
    node = node[part];
  }
  return isMessage(node) ? node : undefined;
}

function render(message: Message | undefined, args: readonly unknown[]): string | undefined {
  if (message === undefined) return undefined;
  if (typeof message === "string") return message;
  return (message as (...a: unknown[]) => string)(...args);
}

const normalizeTag = (value: string): string => value.trim().replace(/_/g, "-").toLowerCase();

export function createI18n<C extends Catalog>(english: C, options: I18nOptions<C> = {}) {
  const defaultLocale = options.defaultLocale ?? "en";
  const loaders = options.loaders ?? {};
  const rtl = new Set(options.rtlLocales ?? []);
  const locales: readonly LocaleInfo[] = options.locales ?? [{ id: defaultLocale, name: defaultLocale, englishName: defaultLocale }];
  const persistence = options.persistence;
  const catalogs = new Map<Locale, Partial<C>>([[defaultLocale, english]]);
  // Lower-cased ids and aliases -> canonical id. The default locale always resolves.
  const known = new Map<string, Locale>([[normalizeTag(defaultLocale), defaultLocale]]);
  for (const info of locales) {
    known.set(normalizeTag(info.id), info.id);
    for (const alias of info.aliases ?? []) known.set(normalizeTag(alias), info.id);
  }
  // Without an explicit `locales`, the loader keys are the registry, so a stored tag such as `fr` restores.
  if (options.locales === undefined) for (const key of Object.keys(loaders)) known.set(normalizeTag(key), key);
  /**
   * Resolves a tag to a locale id: exact id or alias first, then the longest matching prefix
   * (`zh-CN` -> `zh`, `zh-hant-TW` -> `zh-hant`). `undefined` when nothing matches.
   */
  const normalizeLocale = (value: string): Locale | undefined => {
    const parts = normalizeTag(value).split("-").filter(Boolean);
    for (let n = parts.length; n >= 1; n -= 1) {
      const hit = known.get(parts.slice(0, n).join("-"));
      if (hit !== undefined) return hit;
    }
    return undefined;
  };
  const Context = createContext<I18nValue<C>>({
    locale: defaultLocale, catalog: english, t: english, loading: false, error: null,
    setLocale: async () => undefined, locales, saving: false, saveError: null,
  });

  const load = async (locale: Locale): Promise<C> => {
    const cached = catalogs.get(locale);
    if (cached) return mergeCatalog(english, cached);
    const loader = loaders[locale];
    const translated = loader ? await loader(locale) : {};
    catalogs.set(locale, translated);
    return mergeCatalog(english, translated);
  };

  function Provider({ children, initialLocale = defaultLocale }: { children: ReactNode; initialLocale?: Locale }) {
    const [locale, setLocaleState] = useState(initialLocale);
    const [catalog, setCatalog] = useState<C>(() => mergeCatalog(english, catalogs.get(initialLocale)));
    const [loading, setLoading] = useState(false);
    const [saving, setSaving] = useState(false);
    const [error, setError] = useState<Error | null>(null);
    const [saveError, setSaveError] = useState<Error | null>(null);
    const localeRef = useRef(locale);
    /** Bumped by every selection (setLocale); a commit or restore from an older generation is discarded. */
    const selection = useRef(0);
    /** Saves run one at a time, in call order, so a slower older save never lands after a newer one. */
    const saveChain = useRef<Promise<void>>(Promise.resolve());
    const pendingSaves = useRef(0);
    useEffect(() => {
      localeRef.current = locale;
      if (typeof document !== "undefined") {
        document.documentElement.lang = locale;
        document.documentElement.dir = rtl.has(locale) ? "rtl" : "ltr";
      }
    }, [locale]);
    /** Loads and shows `next` without saving it. Throws, leaving the current language, when loading fails. */
    const commit = useCallback(async (next: Locale, gen: number): Promise<void> => {
      const nextCatalog = await load(next);
      // A newer selection superseded this one while it loaded: discard it rather than overwrite the newer choice.
      if (gen !== selection.current) return;
      setCatalog(nextCatalog); setLocaleState(next); localeRef.current = next;
    }, []);
    const setLocale = useCallback(async (requested: Locale) => {
      const next = normalizeLocale(requested) ?? requested;
      const gen = selection.current + 1;
      selection.current = gen;
      const current = () => gen === selection.current;
      const previous = localeRef.current;
      setLoading(true); setError(null); setSaveError(null);
      try {
        await commit(next, gen);
      } catch (cause) {
        const nextError = toError(cause);
        if (current()) { setError(nextError); setLoading(false); }
        throw nextError;
      }
      if (!current()) return;
      if (!persistence) { setLoading(false); return; }
      const job = saveChain.current.then(() => persistence.save(next));
      saveChain.current = job.catch(() => undefined);
      pendingSaves.current += 1; setSaving(true);
      let failure: Error | null = null;
      try { await job; } catch (cause) { failure = toError(cause); }
      pendingSaves.current -= 1; setSaving(pendingSaves.current > 0);
      // Rollback only while this selection is still the newest: the stored choice did not change, so the shown
      // language must not either. A superseded failure leaves the newer selection in place.
      if (failure && current()) await commit(previous, gen).catch(() => undefined);
      if (current()) { if (failure) setSaveError(failure); setLoading(false); }
      if (failure) throw failure;
    }, [commit]);
    useEffect(() => {
      // The initial locale is shown without saving; it is already the stored choice (or the default).
      if (initialLocale !== defaultLocale && !catalogs.has(initialLocale)) void commit(initialLocale, selection.current).catch((cause) => setError(toError(cause)));
    }, [initialLocale, commit]);
    useEffect(() => {
      // Restores the stored choice after mount. A stored tag that no longer resolves is ignored.
      // An explicit selection made while the read is pending wins: the restore is then discarded.
      if (!persistence) return;
      let live = true;
      const gen = selection.current;
      void (async () => {
        try {
          const stored = await persistence.load();
          const target = stored === undefined ? undefined : normalizeLocale(stored);
          if (live && target !== undefined && target !== localeRef.current) await commit(target, gen);
        } catch (cause) { if (live) setError(toError(cause)); }
      })();
      return () => { live = false; };
    }, [commit]);
    const value = useMemo<I18nValue<C>>(() => ({ locale, catalog, t: catalog, loading, error, setLocale, locales, saving, saveError }), [locale, catalog, loading, error, setLocale, saving, saveError]);
    return <Context.Provider value={value}>{children}</Context.Provider>;
  }

  function useI18n(): I18nValue<C> { return useContext(Context); }

  /**
   * Synchronous lookup for code outside React (command handlers, plain modules). Reads only the
   * catalogs already loaded (the default locale, plus every locale passed to `loadLocale` or set
   * through the provider); it never loads. `key` is an exact top-level key (a source string may
   * contain dots) or else a dotted path into the catalog. Resolution: the locale's catalog, then
   * the default-locale catalog, then `options.fallback`, then `key` itself, so a source string
   * used as the key reads as itself when nothing translates it. Values that are not strings
   * count as missing. An unloaded locale resolves like the default locale.
   */
  const t = (locale: Locale, key: string, options: TranslateOptions = {}): string => {
    const args = options.args ?? [];
    return render(lookupMessage(catalogs.get(locale), key), args) ?? render(lookupMessage(english, key), args) ?? options.fallback ?? key;
  };
  const isLoaded = (locale: Locale): boolean => catalogs.has(locale);

  return { Provider, I18nProvider: Provider, useI18n, loadLocale: load, mergeCatalog: (override: unknown) => mergeCatalog(english, override), defaultLocale, t, isLoaded, locales, normalizeLocale };
}
