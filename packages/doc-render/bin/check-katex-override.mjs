#!/usr/bin/env node
// Flags a consumer lockfile that resolves katex below 0.18.2 (GHSA-238p-pmpm-9mq7: prototype pollution can
// bypass trust restrictions; vulnerable >=0.11.0 <0.18.2) while it also resolves @rightkit/doc-render.
// doc-render's remark-math -> micromark-extension-math asks for katex ^0.16, and a library cannot carry a pnpm
// override, so every consumer adds (pnpm-workspace.yaml `overrides:` or package.json "pnpm": { "overrides": ... }):
//   "micromark-extension-math>katex": "^0.19.0"
// Usage: rightkit-doc-render-check-katex [lockfile ...]   (default: ./pnpm-lock.yaml and ./package-lock.json when present)
// Exit 0: not consumed, or every resolved katex is 0.18.2 or later. Exit 1: a vulnerable katex is resolved.
// Exit 2: no lockfile could be read.
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export const SAFE_KATEX = [0, 18, 2];
const OVERRIDE = '"micromark-extension-math>katex": "^0.19.0"';

const parse = (version) => {
  const m = /^(\d+)\.(\d+)\.(\d+)/.exec(version);
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null;
};
const below = (a, b) => {
  for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] < b[i];
  return false;
};

/** The katex versions resolved, and whether @rightkit/doc-render is resolved, read from lockfile text. */
export function scanLockfile(text, name) {
  const katex = new Set();
  let docRender = false;
  if (name.endsWith(".json")) {
    const lock = JSON.parse(text);
    for (const [path, entry] of Object.entries(lock.packages ?? {})) {
      if (path.endsWith("node_modules/katex") && typeof entry?.version === "string") katex.add(entry.version);
      if (path.endsWith("node_modules/@rightkit/doc-render")) docRender = true;
    }
  } else {
    for (const line of text.split(/\r?\n/)) {
      // pnpm lockfile keys: `katex@0.16.47:` (v9+) or `/katex@0.16.47:` (older), quoted when they carry a suffix.
      const key = /^\s{2,6}'?\/?katex@(\d+\.\d+\.\d+[^:'\s]*)'?:/.exec(line);
      if (key) katex.add(key[1]);
      if (/^\s{2,6}'?\/?@rightkit\/doc-render@\d/.test(line) || /^\s+'?@rightkit\/doc-render'?:\s*$/.test(line)) docRender = true;
    }
  }
  return { katex: [...katex], docRender };
}

export function vulnerableKatex(versions) {
  return versions.filter((v) => {
    const parsed = parse(v);
    return parsed !== null && below(parsed, SAFE_KATEX);
  });
}

function main(argv) {
  const files = argv.length ? argv : ["pnpm-lock.yaml", "package-lock.json"].filter((f) => existsSync(f));
  if (!files.length) {
    console.error("rightkit-doc-render-check-katex: no lockfile found (pass a path)");
    return 2;
  }
  let status = 0;
  for (const file of files) {
    let text;
    try {
      text = readFileSync(resolve(file), "utf8");
    } catch (error) {
      console.error(`rightkit-doc-render-check-katex: cannot read ${file}: ${error.message}`);
      return 2;
    }
    const { katex, docRender } = scanLockfile(text, file);
    if (!docRender) {
      console.log(`${file}: @rightkit/doc-render is not resolved; nothing to check`);
      continue;
    }
    const bad = vulnerableKatex(katex);
    if (bad.length) {
      status = 1;
      console.error(`${file}: katex ${bad.join(", ")} is resolved (GHSA-238p-pmpm-9mq7, fixed in 0.18.2). Add ${OVERRIDE} to the pnpm overrides and reinstall.`);
    } else {
      console.log(`${file}: katex ${katex.length ? katex.join(", ") : "not resolved"} is not vulnerable`);
    }
  }
  return status;
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  process.exitCode = main(process.argv.slice(2));
}
