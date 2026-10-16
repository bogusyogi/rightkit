import { cleanup, render, screen, waitFor } from "@testing-library/react";
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
