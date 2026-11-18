// A tool's version across gate runs, without starting the tool again while its program file is unchanged (GATE-CONTRACT NN01, NN25).
//
// Every door run recorded the versions of cargo-deny, cargo-audit, actionlint, shellcheck, rustc and pnpm in the stage binding (gate-stages.mjs
// bindingOf), by starting each one. So a warm run whose actionlint and shellcheck rows were cache hits still started actionlint and shellcheck
// (witness fixture, Dell, 2026-11-17: NN01 failed on exactly that). The binding decides whether a whole stage may be reused, so a remembered
// answer must belong to exactly the program that would run:
//   - the name is resolved on PATH here, as the operating system resolves it (PATHEXT on Windows);
//   - only a real executable is remembered, keyed by its resolved path, size and modification time and the arguments asked;
//   - a .cmd or .bat shim (pnpm, the broker client) is never remembered: what it runs is not the file itself.
// A failed answer is never stored.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

/** The file `cmd` names when started with `env` on `platform`, or null. Absolute paths are taken as they are. */
export function resolveOnPath(cmd, { env = process.env, platform = process.platform, exists = (f) => { try { return fs.statSync(f).isFile(); } catch { return false; } } } = {}) {
  if (typeof cmd !== "string" || !cmd) return null;
  const exts = platform === "win32" ? (path.extname(cmd) ? [""] : String(env.PATHEXT ?? ".COM;.EXE;.BAT;.CMD").split(";").filter(Boolean).map((e) => e.toLowerCase())) : [""];
  const p = platform === "win32" ? path.win32 : path.posix;
  if (p.isAbsolute(cmd)) { for (const e of exts) if (exists(cmd + e)) return cmd + e; return null; }
  if (cmd.includes("/") || cmd.includes("\\")) return null; // a relative path depends on the working directory: not resolved here
  const dirs = String(env.PATH ?? env.Path ?? "").split(platform === "win32" ? ";" : ":").filter(Boolean);
  for (const dir of dirs) for (const e of exts) { const f = p.join(dir, cmd + e); if (exists(f)) return f; }
  return null;
}

/** The remembering key of `cmd args` resolved to `file`, or null when it must not be remembered. */
export function versionKey(cmd, args, file, { stat = (f) => fs.statSync(f) } = {}) {
  if (!file || /\.(cmd|bat)$/i.test(file)) return null;
  let st; try { st = stat(file); } catch { return null; }
  if (!st?.isFile?.()) return null;
  return crypto.createHash("sha256").update(JSON.stringify(["rightkit.tool-version/1", cmd, args, file, st.size, Math.trunc(st.mtimeMs)])).digest("hex");
}

/**
 * `run(cmd, args)` -> the first line of its output, or null; remembered in `storeFile` (JSON, key -> version). Returns the version or null.
 * `storeFile` null: nothing is remembered (the answer is still returned).
 */
export function rememberedVersion(cmd, args, { run, storeFile = null, env = process.env, platform = process.platform, resolve = resolveOnPath, stat, readFile = (f) => fs.readFileSync(f, "utf8"), writeFile = (f, t) => { fs.mkdirSync(path.dirname(f), { recursive: true }); const tmp = `${f}.${process.pid}.tmp`; fs.writeFileSync(tmp, t); fs.renameSync(tmp, f); } } = {}) {
  const key = storeFile ? versionKey(cmd, args, resolve(cmd, { env, platform }), stat ? { stat } : {}) : null;
  let store = {};
  if (key) { try { store = JSON.parse(readFile(storeFile)) ?? {}; } catch { store = {}; } if (typeof store[key] === "string" && store[key]) return store[key]; }
  const version = run(cmd, args);
  if (key && typeof version === "string" && version) { try { writeFile(storeFile, JSON.stringify({ ...store, [key]: version })); } catch { /* not remembered: asked again next time */ } }
  return version;
}
