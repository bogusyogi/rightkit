import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import assert from "node:assert/strict";

// src/index.css is an @import stub: the entry file only lists
// `@import './styles/index-NN.css'` directives, and the actual rules live in the
// 17 partials under src/styles/. Resolve the @import graph and concatenate the
// local partials in source order so the cascade matches what ships. Vendor
// imports (e.g. `tailwindcss/theme.css`) are skipped — this test only asserts
// on our own rules.
function loadResolvedCss(): string {
  const indexPath = join(import.meta.dirname, "../src/index.css");
  const index = readFileSync(indexPath, "utf8");
  const srcDir = join(import.meta.dirname, "../src");
  return index.replace(
    /@import\s+["']([^"']+)["'](?:\s+[^;]+)?;?/g,
    (_match, spec: string) => {
      if (!spec.startsWith(".")) return "";
      return readFileSync(join(srcDir, spec), "utf8");
    },
  );
}

const css = loadResolvedCss();

function cssBlock(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = css.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`));
  assert.ok(match, `missing CSS block for ${selector}`);
  return match![1];
}

test("reader action bar stays sticky while the open email scrolls", () => {
  const block = cssBlock(".reader-bar");
  assert.match(block, /position:\s*sticky;/);
  assert.match(block, /top:\s*0;/);
  assert.match(block, /z-index:\s*\d+;/);
  assert.match(block, /background:\s*var\(--reader-chrome\);/);
});

test("titlebar utility controls sit left on Windows and right on macOS", () => {
  const controls = cssBlock(".tb-right");
  assert.match(controls, /position:\s*absolute;/);
  assert.match(controls, /left:\s*8px;/);

  const macControls = cssBlock('.rk-shell[data-platform="macos"] .tb-right');
  assert.match(macControls, /left:\s*auto;/);
  assert.match(macControls, /right:\s*10px;/);
});

test("reader surface is themed separately from the app sidebar", () => {
  const reader = cssBlock(".reader");
  assert.match(reader, /--reader-surface:\s*#f8f3eb;/);
  assert.match(reader, /--reader-chrome:\s*#fbf7ef;/);
  assert.match(reader, /--reader-line:\s*#e4d9cc;/);

  const content = cssBlock(".content:has(.reader)");
  assert.match(content, /background:\s*#f8f3eb;/);

  assert.match(
    css,
    /\.reader-split-pane\s*\{[^}]*background:\s*#f8f3eb;/s
  );
});
