import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

export type Catalog = Record<string, unknown>;
export type Locale = string;
export type LocaleLoader<C extends Catalog> = (locale: Locale) => Promise<Partial<C>> | Partial<C>;

export interface I18nOptions<C extends Catalog> {
  defaultLocale?: Locale;
  loaders?: Record<string, LocaleLoader<C>>;
  rtlLocales?: readonly Locale[];
}

export interface I18nValue<C extends Catalog> {
  locale: Locale;
  catalog: C;
  t: C;
  loading: boolean;
  error: Error | null;
  setLocale: (locale: Locale) => Promise<void>;
}

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

export function createI18n<C extends Catalog>(english: C, options: I18nOptions<C> = {}) {
  const defaultLocale = options.defaultLocale ?? "en";
  const loaders = options.loaders ?? {};
  const rtl = new Set(options.rtlLocales ?? []);
  const catalogs = new Map<Locale, Partial<C>>([[defaultLocale, english]]);
  const Context = createContext<I18nValue<C>>({
    locale: defaultLocale, catalog: english, t: english, loading: false, error: null,
    setLocale: async () => undefined,
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
    const [error, setError] = useState<Error | null>(null);
    const localeRef = useRef(locale);
    useEffect(() => {
      localeRef.current = locale;
      if (typeof document !== "undefined") {
        document.documentElement.lang = locale;
        document.documentElement.dir = rtl.has(locale) ? "rtl" : "ltr";
      }
    }, [locale]);
    const setLocale = useCallback(async (next: Locale) => {
      setLoading(true); setError(null);
      try {
        const nextCatalog = await load(next);
        setCatalog(nextCatalog); setLocaleState(next);
      } catch (cause) {
        const nextError = cause instanceof Error ? cause : new Error(String(cause));
        setError(nextError); setLocaleState(localeRef.current); throw nextError;
      } finally { setLoading(false); }
    }, []);
    useEffect(() => {
      if (initialLocale !== defaultLocale && !catalogs.has(initialLocale)) void setLocale(initialLocale);
    }, [initialLocale, setLocale]);
    const value = useMemo<I18nValue<C>>(() => ({ locale, catalog, t: catalog, loading, error, setLocale }), [locale, catalog, loading, error, setLocale]);
    return <Context.Provider value={value}>{children}</Context.Provider>;
  }

  function useI18n(): I18nValue<C> { return useContext(Context); }
  return { Provider, I18nProvider: Provider, useI18n, loadLocale: load, mergeCatalog: (override: unknown) => mergeCatalog(english, override), defaultLocale };
}
