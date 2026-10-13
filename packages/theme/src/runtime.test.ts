import { describe, expect, it } from "vitest";
import { applyThemeAttribute, themeAttributeValue, themeBootScript } from "./runtime.js";

function fakeEl() {
  const attrs = new Map<string, string>();
  return {
    attrs,
    setAttribute: (n: string, v: string) => void attrs.set(n, v),
    removeAttribute: (n: string) => void attrs.delete(n),
  };
}

function runBoot(script: string, stored: string | null, el: ReturnType<typeof fakeEl>) {
  const localStorage = { getItem: () => stored };
  new Function("localStorage", "document", script)(localStorage, { documentElement: el });
}

describe("themeAttributeValue / applyThemeAttribute", () => {
  it("default: dark writes data-theme=dark", () => {
    expect(themeAttributeValue("dark")).toBe("dark");
    const el = fakeEl();
    applyThemeAttribute("dark", el);
    expect(el.attrs.get("data-theme")).toBe("dark");
  });

  it("darkRemovesAttribute: dark removes it, light writes it", () => {
    const el = fakeEl();
    el.attrs.set("data-theme", "light");
    applyThemeAttribute("dark", el, { darkRemovesAttribute: true });
    expect(el.attrs.has("data-theme")).toBe(false);
    applyThemeAttribute("light", el, { darkRemovesAttribute: true });
    expect(el.attrs.get("data-theme")).toBe("light");
  });

  it("is a no-op without a root", () => {
    expect(() => applyThemeAttribute("dark", null)).not.toThrow();
  });
});

describe("themeBootScript", () => {
  it("is unchanged without options", () => {
    expect(themeBootScript("k")).toBe(
      '(function(){try{var v=localStorage.getItem("k");if(v==="light"||v==="dark")document.documentElement.setAttribute("data-theme",v);}catch(e){}})();',
    );
    const el = fakeEl();
    runBoot(themeBootScript("k"), "dark", el);
    expect(el.attrs.get("data-theme")).toBe("dark");
  });

  it("darkRemovesAttribute: stored dark leaves the attribute off, light sets it, system/none leaves it", () => {
    const s = themeBootScript("k", { darkRemovesAttribute: true });
    for (const [stored, want] of [["dark", undefined], ["light", "light"], ["system", undefined], [null, undefined]] as const) {
      const el = fakeEl();
      el.attrs.set("data-theme", "stale");
      runBoot(s, stored, el);
      if (stored === "dark") expect(el.attrs.has("data-theme")).toBe(false);
      else if (want) expect(el.attrs.get("data-theme")).toBe(want);
      else expect(el.attrs.get("data-theme")).toBe("stale");
    }
  });
});
