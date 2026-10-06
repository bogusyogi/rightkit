import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AppShell, createThemeStore, filterNavGroups, firstNavItem, type NavGroup } from "./react.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const groups: NavGroup[] = [
  { title: "Create", items: [{ id: "image", label: "Image" }, { id: "video", label: "Video", keywords: ["movie"] }] },
  { title: "Library", items: [{ id: "jobs", label: "Jobs", badge: "2" }] },
];
const adapter = { minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {} };
const render = (platform: "macos" | "windows", activeId = "video") =>
  renderToStaticMarkup(
    <AppShell
      groups={groups}
      activeId={activeId}
      onNavigate={() => {}}
      title="Video"
      wordmark={<>Acme<span>Right</span></>}
      theme="dark"
      onThemeChange={() => {}}
      onOpenSettings={() => {}}
      statusFooter={<div>Engine ok</div>}
      windowControls={adapter}
      platform={platform}
    >
      <main>content</main>
    </AppShell>,
  );

describe("app shell", () => {
  it("marks the active row, keeps groups, and fills every slot", () => {
    const html = render("windows");
    expect(html).toMatch(/class="rk-item is-active"[^>]*aria-current="page"/);
    expect(html).toContain("<h6>Create</h6>");
    expect(html.match(/data-rk-fuse=/g)).toHaveLength(2);
    expect(html).toContain("Engine ok");
    expect(html).toContain("<span>Right</span>");
    expect(html).toContain('aria-label="Settings"');
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain("<main>content</main>");
  });

  it("uses native traffic lights on macOS and caption buttons elsewhere", () => {
    const mac = render("macos");
    expect(mac).toContain("rk-top__lights");
    expect(mac).not.toContain("rk-caption");
    const win = render("windows");
    expect(win).not.toContain("rk-top__lights");
    expect(win).toContain("rk-caption__close");
  });

  it("filters Jump-to by label or keyword and drops empty groups", () => {
    expect(filterNavGroups(groups, "movie").map((g) => g.items.map((i) => i.id))).toEqual([["video"]]);
    expect(firstNavItem(filterNavGroups(groups, "zzz"))).toBeUndefined();
    expect(filterNavGroups(groups, "").length).toBe(2);
  });

  it("persists the theme choice and applies data-theme", () => {
    const data = new Map<string, string>();
    const attrs = new Map<string, string>();
    const store = createThemeStore({
      storageKey: "t.theme",
      storage: { getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v), removeItem: (k) => void data.delete(k) },
      root: { setAttribute: (n, v) => void attrs.set(n, v), removeAttribute: (n) => void attrs.delete(n) },
    });
    store.save("dark");
    expect([store.read(), attrs.get("data-theme")]).toEqual(["dark", "dark"]);
    store.save("system");
    expect([store.read(), attrs.has("data-theme"), data.size]).toEqual(["system", false, 0]);
  });

  it("ships the 12px concave join and brand-token contract in shell.css", () => {
    const css = readFileSync(join(root, "shell.css"), "utf8");
    expect(css).toContain(".rk-item .rk-fuse--top { top: -12px; }");
    expect(css).toContain("fill: var(--plane)");
    expect(css).toContain("--accent: var(--rk-brand)");
    expect(css).toContain("--accent: var(--rk-brand-dark)");
  });

  it("is a dual-licensed package with the canonical directory", () => {
    const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
    expect(pkg.license).toBe("MIT OR Apache-2.0");
    expect(pkg.repository.directory).toBe("tools/rightkit/packages/app-shell");
    expect(pkg.exports["./shell.css"]).toBe("./shell.css");
  });
});
