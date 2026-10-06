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
    expect(document.documentElement.dir).toBe("rtl");
    expect(screen.getByTestId("locale")).toBeTruthy();
  });
  it("preserves sibling catalog keys", () => {
    expect(mergeCatalog(english, { nested: { replace: "neu" } })).toEqual({ language: { label: "Language" }, nested: { keep: "Keep", replace: "neu" } });
  });
});
