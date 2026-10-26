import fs from "node:fs";
import { fileURLToPath } from "node:url";

const SUFFIXES = [".ts", ".tsx", "/index.ts", "/index.tsx"];

function resolveTs(specifier, context) {
  if (/^\.\.?\//.test(specifier) && context.parentURL && !/\.[a-z]+$/i.test(specifier)) {
    const base = new URL(specifier, context.parentURL);
    for (const suffix of SUFFIXES) {
      const candidate = new URL(base.href + suffix);
      if (fs.existsSync(fileURLToPath(candidate))) return { url: candidate.href, shortCircuit: true };
    }
  }
  return null;
}

function loadJson(url) {
  if (url.endsWith(".json") && url.startsWith("file:")) return { format: "json", source: fs.readFileSync(fileURLToPath(url), "utf8"), shortCircuit: true };
  return null;
}

// Synchronous hooks for `module.registerHooks()` (Node 22.15+/23.5+; `module.register()` is DEP0205 on Node 26).
export const hooks = {
  resolve(specifier, context, next) { return resolveTs(specifier, context) ?? next(specifier, context); },
  load(url, context, next) { return loadJson(url) ?? next(url, context); },
};

// The same hooks in the asynchronous shape `module.register()` loads from a separate thread.
export async function resolve(specifier, context, next) { return resolveTs(specifier, context) ?? next(specifier, context); }
export async function load(url, context, next) { return loadJson(url) ?? next(url, context); }
