import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it } from "vitest";
import { createI18n, mergeCatalog } from "./index";

const english = { language: { label: "Language" }, nested: { keep: "Keep", replace: "Replace" } };
const { I18nProvider, useI18n } = createI18n(english, {
  loaders: { ar: () => ({ language: { label: "اللغة" } }) }, rtlLocales: ["ar"],
});

function Probe() {
  const { locale, t, setLocale } = useI18n();
  return <><output data-testid="locale">{locale}</output><output data-testid="label">{t.language.label}</output><button onClick={() => void setLocale("ar")}>switch</button></>;
}

describe("@rightkit/i18n", () => {
  afterEach(cleanup);
  it("falls back recursively and applies loaded locale", async () => {
    render(<I18nProvider><Probe /></I18nProvider>);
    expect(screen.getByTestId("label")).toHaveTextContent("Language");
    screen.getByRole("button", { name: "switch" }).click();
    await waitFor(() => expect(screen.getByTestId("label")).toHaveTextContent("اللغة"));
    expect(screen.getByTestId("locale")).toHaveTextContent("ar");
    // The direction is applied in an effect after the label renders; wait for it rather than
    // reading it in the same tick (it read "ltr" once on a slow runner).
    await waitFor(() => expect(document.documentElement.dir).toBe("rtl"));
    expect(screen.getByTestId("locale")).toBeTruthy();
  });
  it("translates synchronously from loaded catalogs with fallbacks", async () => {
    const sync = createI18n(
      { greeting: "Hello", "Open Log Folder.": "Open Log Folder.", menu: { open: "Open", quit: "Quit" }, count: 3 },
      { loaders: { fr: () => ({ greeting: "Bonjour", "Open Log Folder.": "Ouvrir le dossier.", menu: { open: "Ouvrir" } }) } },
    );
    // Default locale and unloaded locales resolve from the default catalog; nothing loads.
    expect(sync.t("en", "greeting")).toBe("Hello");
    expect(sync.isLoaded("fr")).toBe(false);
    expect(sync.t("fr", "greeting")).toBe("Hello");
    await sync.loadLocale("fr");
    expect(sync.isLoaded("fr")).toBe(true);
    expect(sync.t("fr", "greeting")).toBe("Bonjour");
    // Dotted paths, and an exact key that itself contains a dot.
    expect(sync.t("fr", "menu.open")).toBe("Ouvrir");
    expect(sync.t("fr", "Open Log Folder.")).toBe("Ouvrir le dossier.");
    // Missing in the locale falls back to the default catalog, key by key.
    expect(sync.t("fr", "menu.quit")).toBe("Quit");
    // Missing everywhere: the fallback option, else the key (the source string) itself.
    expect(sync.t("fr", "Save as...", { fallback: "Save" })).toBe("Save");
    expect(sync.t("fr", "Save as...")).toBe("Save as...");
    // A non-string value is treated as missing, as are inherited object properties.
    expect(sync.t("fr", "count")).toBe("count");
    expect(sync.t("fr", "menu")).toBe("menu");
    expect(sync.t("fr", "toString")).toBe("toString");
    // An empty translation is a translation.
    const blank = createI18n({ x: "X" }, { loaders: { de: () => ({ x: "" }) } });
    await blank.loadLocale("de");
    expect(blank.t("de", "x")).toBe("");
  });
  it("preserves sibling catalog keys", () => {
    expect(mergeCatalog(english, { nested: { replace: "neu" } })).toEqual({ language: { label: "Language" }, nested: { keep: "Keep", replace: "neu" } });
  });
});

describe("@rightkit/i18n locale registry, persistence and function messages", () => {
  afterEach(cleanup);
  const locales = [
    { id: "en", name: "English", englishName: "English" },
    { id: "zh", name: "中文", englishName: "Chinese" },
    { id: "zh-hant", name: "繁體中文", englishName: "Chinese (Traditional)", aliases: ["zh-TW", "zh_HK"] },
    { id: "ar", name: "العربية", englishName: "Arabic" },
  ];

  it("normalizes tags to registry ids, aliases and language prefixes", () => {
    const i18n = createI18n({ x: "X" }, { locales });
    expect(i18n.normalizeLocale("zh-CN")).toBe("zh");
    expect(i18n.normalizeLocale("zh-TW")).toBe("zh-hant");
    expect(i18n.normalizeLocale("zh_HK")).toBe("zh-hant");
    expect(i18n.normalizeLocale("zh-hant-TW")).toBe("zh-hant");
    expect(i18n.normalizeLocale("AR")).toBe("ar");
    expect(i18n.normalizeLocale("fr-CA")).toBeUndefined();
    expect(i18n.normalizeLocale("")).toBeUndefined();
    expect(i18n.locales).toEqual(locales);
  });

  it("calls function messages with args in sync lookups and keeps string fallbacks", () => {
    const i18n = createI18n({
      common: { tryHint: (name: string) => `Try ${name}` },
      theme: { installed: (name: string) => `${name} installed` },
      plain: "Plain",
    });
    expect(i18n.t("en", "common.tryHint", { args: ["settings"] })).toBe("Try settings");
    expect(i18n.t("en", "theme.installed", { args: ["Dark"] })).toBe("Dark installed");
    expect(i18n.t("en", "plain", { args: ["ignored"] })).toBe("Plain");
    expect(i18n.t("en", "missing.key", { fallback: "Nope" })).toBe("Nope");
  });

  it("restores the stored language on mount without saving it again", async () => {
    const saved: string[] = [];
    const persistence = { load: async () => "zh-TW", save: async (l: string) => { saved.push(l); } };
    const i18n = createI18n({ language: { label: "Language" } }, {
      locales, persistence, loaders: { "zh-hant": () => ({ language: { label: "語言" } }) },
    });
    const Show = () => { const { locale, t } = i18n.useI18n(); return <output data-testid="out">{locale}:{t.language.label}</output>; };
    render(<i18n.Provider><Show /></i18n.Provider>);
    await waitFor(() => expect(screen.getByTestId("out")).toHaveTextContent("zh-hant:語言"));
    expect(saved).toEqual([]);
  });

  it("saves a chosen language and rolls back when the save fails", async () => {
    let failSave = true;
    const saved: string[] = [];
    const persistence = {
      load: async () => undefined,
      save: async (l: string) => { if (failSave) throw new Error("disk full"); saved.push(l); },
    };
    const i18n = createI18n({ language: { label: "Language" } }, {
      locales, persistence, loaders: { ar: () => ({ language: { label: "اللغة" } }) }, rtlLocales: ["ar"],
    });
    let api: ReturnType<typeof i18n.useI18n> | undefined;
    const Probe = () => { api = i18n.useI18n(); return <output data-testid="locale">{api.locale}</output>; };
    render(<i18n.Provider><Probe /></i18n.Provider>);
    await expect(api!.setLocale("ar")).rejects.toThrow("disk full");
    await waitFor(() => expect(api!.saveError?.message).toBe("disk full"));
    expect(api!.locale).toBe("en");
    expect(api!.saving).toBe(false);
    failSave = false;
    await api!.setLocale("ar");
    await waitFor(() => expect(screen.getByTestId("locale")).toHaveTextContent("ar"));
    expect(saved).toEqual(["ar"]);
    expect(api!.saveError).toBeNull();
  });

  it("restores a stored tag that only a loader provides when no locales registry is given", async () => {
    const i18n = createI18n({ language: { label: "Language" } }, {
      loaders: { fr: () => ({ language: { label: "Langue" } }) },
      persistence: { load: async () => "fr", save: async () => undefined },
    });
    expect(i18n.normalizeLocale("fr")).toBe("fr");
    expect(i18n.normalizeLocale("fr-CA")).toBe("fr");
    const { I18nProvider: Provider, useI18n: useRestored } = i18n;
    const Show = () => { const { locale, t } = useRestored(); return <output data-testid="restored">{locale}:{t.language.label}</output>; };
    render(<Provider><Show /></Provider>);
    await waitFor(() => expect(screen.getByTestId("restored")).toHaveTextContent("fr:Langue"));
  });

  it("discards an older failed save so it cannot roll back a newer selection", async () => {
    const saves: string[] = [];
    const pending = [gate<void>(), gate<void>()];
    let calls = 0;
    const persistence = {
      load: async () => undefined,
      save: (l: string) => { saves.push(l); return pending[calls++]!.promise; },
    };
    const i18n = createI18n({ language: { label: "Language" } }, {
      loaders: { ar: () => ({ language: { label: "اللغة" } }), fr: () => ({ language: { label: "Langue" } }) }, persistence,
    });
    let select!: (locale: string) => Promise<void>;
    const Probe = () => { const api = i18n.useI18n(); select = api.setLocale; return <output data-testid="gen">{api.locale}</output>; };
    render(<i18n.Provider><Probe /></i18n.Provider>);
    const first = select("ar").then(() => null, (cause: unknown) => cause);
    await waitFor(() => expect(screen.getByTestId("gen")).toHaveTextContent("ar"));
    // The ar save is in flight when a newer selection commits.
    const second = select("fr");
    await waitFor(() => expect(screen.getByTestId("gen")).toHaveTextContent("fr"));
    pending[0]!.reject(new Error("disk full"));
    expect(await first).toBeInstanceOf(Error);
    // The superseded failure must not roll the shown language back to en or to ar.
    expect(screen.getByTestId("gen")).toHaveTextContent("fr");
    pending[1]!.resolve();
    await second;
    expect(screen.getByTestId("gen")).toHaveTextContent("fr");
    expect(saves).toEqual(["ar", "fr"]);
  });

  it("does not let a delayed startup restore overwrite an explicit selection", async () => {
    const restore = gate<string | undefined>();
    const i18n = createI18n({ language: { label: "Language" } }, {
      loaders: { ar: () => ({ language: { label: "اللغة" } }), fr: () => ({ language: { label: "Langue" } }) },
      persistence: { load: () => restore.promise, save: async () => undefined },
    });
    let select!: (locale: string) => Promise<void>;
    const Probe = () => { const api = i18n.useI18n(); select = api.setLocale; return <output data-testid="gen">{api.locale}</output>; };
    render(<i18n.Provider><Probe /></i18n.Provider>);
    await act(async () => { await select("ar"); });
    expect(screen.getByTestId("gen")).toHaveTextContent("ar");
    await act(async () => { restore.resolve("fr"); await new Promise((done) => setTimeout(done, 10)); });
    expect(screen.getByTestId("gen")).toHaveTextContent("ar");
  });
});

function gate<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}
