import { describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import {
  DEFAULT_MEDIA_PLAYER_LABELS,
  DEFAULT_MEDIA_SPEEDS,
  MediaPlayer,
  mediaErrorMessage,
  nextMediaSpeed,
  resolveMediaLabels,
} from "./MediaPlayer.js";
import { acquireResource, normalizeResourceHandle } from "./adapters.js";
import { resolveIcon } from "./icons.js";

const html = (props: Partial<Parameters<typeof MediaPlayer>[0]> = {}) =>
  renderToStaticMarkup(<MediaPlayer src="/a.mp3" kind="audio" {...props} />);

describe("resource handles", () => {
  it("accepts plain release functions, handles and nothing", () => {
    const release = vi.fn();
    const plain = normalizeResourceHandle(release);
    plain.update({ dirty: true });
    plain.release();
    expect(release).toHaveBeenCalledOnce();

    const update = vi.fn();
    const rel2 = vi.fn();
    const handle = normalizeResourceHandle({ release: rel2, update });
    handle.update({ dirty: true });
    handle.release();
    expect(update).toHaveBeenCalledWith({ dirty: true });
    expect(rel2).toHaveBeenCalledOnce();

    const none = normalizeResourceHandle(undefined);
    expect(() => {
      none.update({ dirty: false });
      none.release();
    }).not.toThrow();
  });

  it("acquireResource forwards the request and normalizes the result", () => {
    const update = vi.fn();
    const acquire = vi.fn(() => ({ release: () => {}, update }));
    const handle = acquireResource({ acquireResource: acquire }, { kind: "k", dirty: false });
    handle.update({ dirty: true, stateExternalized: true });
    expect(acquire).toHaveBeenCalledWith({ kind: "k", dirty: false });
    expect(update).toHaveBeenCalledWith({ dirty: true, stateExternalized: true });
  });
});

describe("speeds and labels", () => {
  it("cycles through custom speeds and wraps", () => {
    expect(nextMediaSpeed([1, 2, 3], 1)).toBe(2);
    expect(nextMediaSpeed([1, 2, 3], 3)).toBe(1);
    expect(nextMediaSpeed([1, 2, 3], 9)).toBe(1);
    expect(nextMediaSpeed([], 1.5)).toBe(1.5);
    expect(DEFAULT_MEDIA_SPEEDS).toContain(1);
  });

  it("merges partial labels over defaults and ignores undefined", () => {
    const labels = resolveMediaLabels({ errorTitle: "Nope", retry: undefined });
    expect(labels.errorTitle).toBe("Nope");
    expect(labels.retry).toBe(DEFAULT_MEDIA_PLAYER_LABELS.retry);
    expect(DEFAULT_MEDIA_PLAYER_LABELS.errorTitle).toBe("Preview failed");
  });

  it("uses overridden strings for element errors", () => {
    const labels = resolveMediaLabels({ errorUnsupported: "Kodek?" });
    expect(mediaErrorMessage({ error: { code: 4 } } as unknown as HTMLMediaElement, labels)).toBe(
      "Kodek?",
    );
  });
});

describe("icons", () => {
  it("prefers overrides, including null, else the built-in", () => {
    expect(resolveIcon("play", { play: "X" })).toBe("X");
    expect(resolveIcon("play", { play: null })).toBeNull();
    expect(renderToStaticMarkup(<>{resolveIcon("play", {})}</>)).toContain("<svg");
  });
});

describe("MediaPlayer props", () => {
  it("shows artwork and open-original by default", () => {
    const out = html();
    expect(out).toContain("rk-media-audio-face");
    expect(out).toContain('aria-label="Open original"');
    expect(out).toContain("rk-media-speed");
  });
  it("hides artwork and open original on request but keeps the audio element", () => {
    const out = html({ showArtwork: false, showOpenOriginal: false });
    expect(out).not.toContain("rk-media-audio-face");
    expect(out).not.toContain("Open original");
    expect(out).toContain("<audio");
  });
  it("renders a speed menu with custom speeds", () => {
    const out = html({ speedControl: "menu", speeds: [1, 2, 3] });
    expect(out).toContain("<select");
    expect(out).toContain('class="rk-media-speed-menu"');
    expect(out).not.toMatch(/<select[^>]*rk-media-btn/);
    expect(out).toContain("3×");
    expect(out).not.toContain("0.5×");
  });
  it("applies label and icon overrides", () => {
    const out = html({
      labels: { openOriginal: "Show file", play: "Start" },
      icons: { external: <b id="ext">E</b> },
    });
    expect(out).toContain('aria-label="Show file"');
    expect(out).toContain('aria-label="Start"');
    expect(out).toContain('id="ext"');
  });
});

describe("entry split", () => {
  it("root entry never reaches fabric; image entry does", async () => {
    const { readFileSync } = await import("node:fs");
    const { resolve, dirname } = await import("node:path");
    const reach = (entry: string) => {
      const seen = new Set<string>();
      let fabric = false;
      const visit = (file: string) => {
        if (seen.has(file)) return;
        seen.add(file);
        const src = readFileSync(file, "utf8");
        if (/from\s+"fabric"/.test(src)) fabric = true;
        for (const m of src.matchAll(/(?:from\s+|import\()"(\.[^"]+)"/g)) {
          const rel = m[1]!.replace(/\.js$/, "");
          for (const ext of [".ts", ".tsx"]) {
            try {
              readFileSync(resolve(dirname(file), rel + ext));
              visit(resolve(dirname(file), rel + ext));
              break;
            } catch {}
          }
        }
      };
      visit(resolve(__dirname, entry));
      return fabric;
    };
    expect(reach("index.ts")).toBe(false);
    expect(reach("image.ts")).toBe(true);
  });
});

describe("styles.css specificity contract", () => {
  const css = readFileSync(new URL("../styles.css", import.meta.url), "utf8")
    .replace(/\/\*[\s\S]*?\*\//g, "");
  const selectors: string[] = [];
  for (const m of css.matchAll(/(?:^|[};{])\s*([^{};@]+?)\s*\{/g)) selectors.push(m[1]!.trim());

  it("wraps every default rule in :where() so app CSS always wins", () => {
    expect(selectors.length).toBeGreaterThan(50);
    const bare = selectors.filter((s) => !/^:where\(/.test(s));
    expect(bare).toEqual([]);
  });
  it("keeps the fallback box and speed menu on their own classes", () => {
    expect(selectors).toContain(":where(.rk-media-fallback)");
    expect(selectors).toContain(":where(.rk-media-speed-menu)");
    const menu = /:where\(\.rk-media-speed-menu\)\s*\{([^}]*)\}/.exec(css)![1]!;
    expect(menu).toContain("font: inherit");
    expect(menu).not.toMatch(/min-width|min-height|font-size/);
  });
});
