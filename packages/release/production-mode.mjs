// The release frontend build always resolves in production mode (M12: `@rightkit/license/testing` and
// other development-only conditional exports must not resolve into a release bundle). Release build
// children get NODE_ENV=production, which Vite's build honours ahead of a `.env` NODE_ENV and of any
// custom `--mode` (vite.dev/guide/env-and-mode, "NODE_ENV and Modes"). What the pin cannot override is
// a config that lists the `development` resolve condition explicitly, so the preflight refuses that.
// Package installs keep the caller's NODE_ENV (production would drop devDependencies).
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";

const SKIP_DIRS = new Set(["node_modules", "target", "dist", ".git", "src-tauri", "build", "out"]);
const VITE_CONFIG = /^vite\.config\.(?:[cm]?[jt]s)$/;

export function productionBuildEnv(env) {
  return { ...env, NODE_ENV: "production" };
}

function walk(dir, depth, visit) {
  let names;
  try { names = readdirSync(dir); } catch { return; }
  for (const name of names) {
    const full = path.join(dir, name);
    let stat;
    try { stat = statSync(full); } catch { continue; }
    if (stat.isDirectory()) {
      if (depth > 0 && !SKIP_DIRS.has(name) && !name.startsWith(".")) walk(full, depth - 1, visit);
    } else visit(name, full);
  }
}

/** Vite configs that would resolve development-only exports even in a production build. A text scan:
 * a computed conditions array is not seen (the post-build web-bundle marker scan is the backstop). */
export function productionModeProblems({ appRoot, depth = 3 }) {
  const problems = [];
  if (!appRoot || !existsSync(appRoot)) return problems;
  walk(appRoot, depth, (name, full) => {
    if (!VITE_CONFIG.test(name)) return;
    if (/conditions\s*:\s*\[[^\]]*["'`]development["'`]/.test(readFileSync(full, "utf8"))) {
      problems.push(`${path.relative(appRoot, full)} lists a "development" resolve condition; a release build must not resolve development-only exports`);
    }
  });
  return problems;
}
