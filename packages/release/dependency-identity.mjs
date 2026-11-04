// The identity of installed pnpm dependencies (GC-A24, rightkit#1506; Adrian, 2026-11-05: "if that's the best shape, do it").
//
// The gate installs `--offline --frozen-lockfile` from pnpm's content-addressed store, which checks every package against the
// lockfile's integrity hash, so the installed bytes are a function of the lockfile, the store and pnpm. Their identity is therefore
// pnpm's own record of the install, not a walk over every installed byte (the former walk read ~44k files per root and duplicated
// pnpm's integrity check). Per node_modules root this binds:
//   - .modules.yaml and .pnpm-workspace-state-v1.json, with the install timestamps pnpm writes on every install removed;
//   - .pnpm/lock.yaml, the lockfile pnpm recorded for the virtual store it built;
//   - every top-level entry (including one level of @scope): its name, kind and link target;
//   - any link that resolves outside the repository and outside every walked root (a `link:`/`file:` package pnpm did not take from
//     the store), hashed whole, because no lockfile integrity covers those bytes.
// A root that does not exist is bound as absent.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const sha = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const inside = (p, dir) => p === dir || p.startsWith(dir + path.sep);

/**
 * The bytes of a pnpm install-state file as identities bind them: pnpm stamps the time of every install into `.modules.yaml`
 * (`prunedAt`) and `.pnpm-workspace-state-v1.json` (`lastValidatedTimestamp`, `validation.recordedAtNs`, the lockfile's
 * `mtimeNs`/`ctimeNs`). Only those fields are dropped (measured 2026-11-05: they were the only bytes of 867 entries in 104 roots
 * that differed between two identical installs). Any other file is returned unchanged.
 */
export function installStateBytes(name, bytes) {
  if (name === ".modules.yaml") return Buffer.from(String(bytes).replace(/^\s*"?prunedAt"?\s*:.*$/m, ""));
  if (name === ".pnpm-workspace-state-v1.json") {
    try {
      const state = JSON.parse(String(bytes)); delete state.lastValidatedTimestamp;
      if (state.validation && typeof state.validation === "object") {
        delete state.validation.recordedAtNs;
        for (const entry of Object.values(state.validation.files ?? {})) if (entry && typeof entry === "object") { delete entry.mtimeNs; delete entry.ctimeNs; }
      }
      return Buffer.from(JSON.stringify(state));
    } catch { return bytes; }
  }
  return bytes;
}

const STATE_FILES = [".modules.yaml", ".pnpm-workspace-state-v1.json", path.join(".pnpm", "lock.yaml")];

/** Whole-content hash of a directory or file outside the store (a linked local package). Bounded; never follows links inside it. */
function hashOutside(target, budget) {
  const h = crypto.createHash("sha256");
  const walk = (p) => {
    if (++budget.count > budget.max) throw new Error(`linked package ${target} exceeds ${budget.max} entries`);
    const st = fs.lstatSync(p);
    if (st.isSymbolicLink()) { h.update(`link\0${path.relative(target, p)}\0${fs.readlinkSync(p)}\0`); return; }
    if (st.isDirectory()) { h.update(`dir\0${path.relative(target, p)}\0`); for (const n of fs.readdirSync(p).sort()) { if (n === "node_modules" || n === ".git") continue; walk(path.join(p, n)); } return; }
    if (st.isFile()) { h.update(`file\0${path.relative(target, p)}\0${st.mode & 0o111 ? "x" : "-"}\0${sha(fs.readFileSync(p))}\0`); return; }
  };
  walk(target);
  return h.digest("hex");
}

/**
 * The identity of the installed dependencies under `dirs` (node_modules directories, absolute or relative to `root`).
 * `repoRoot` (default `root`) bounds what counts as "inside the repository" for linked packages.
 */
export function installIdentity(root, dirs, { repoRoot = root, maxLinkedEntries = 200000 } = {}) {
  const h = crypto.createHash("sha256");
  const realRepo = fs.realpathSync(repoRoot);
  const walked = [];
  for (const d of [...dirs].sort()) { try { walked.push(fs.realpathSync(path.resolve(root, d))); } catch { /* absent: bound below */ } }
  const outside = new Set();
  const budget = { count: 0, max: maxLinkedEntries };
  const rel = (p) => (inside(p, realRepo) ? path.relative(realRepo, p).split(path.sep).join("/") : p);
  for (const d of [...dirs].sort()) {
    const dir = path.resolve(root, d);
    h.update(`root\0${rel(dir)}\0`);
    let st; try { st = fs.lstatSync(dir); } catch (error) { if (error?.code === "ENOENT" || error?.code === "ENOTDIR") { h.update("absent\0"); continue; } throw error; }
    if (!st.isDirectory()) { h.update(`not-a-directory\0${st.mode}\0`); continue; }
    for (const f of STATE_FILES) {
      let bytes; try { bytes = fs.readFileSync(path.join(dir, f)); } catch (error) { if (error?.code === "ENOENT") { h.update(`${f}\0absent\0`); continue; } throw error; }
      h.update(`${f}\0${sha(installStateBytes(path.basename(f), bytes))}\0`);
    }
    const entry = (p, name) => {
      const est = fs.lstatSync(p);
      if (est.isSymbolicLink()) {
        const target = fs.readlinkSync(p);
        h.update(`link\0${name}\0${target}\0`);
        let real = null; try { real = fs.realpathSync(p); } catch (error) { if (error?.code !== "ENOENT") throw error; h.update("dangling\0"); }
        if (real && !inside(real, realRepo) && !walked.some((w) => inside(real, w))) outside.add(real);
        return;
      }
      h.update(`${est.isDirectory() ? "dir" : est.isFile() ? "file" : "other"}\0${name}\0`);
    };
    for (const name of fs.readdirSync(dir).sort()) {
      if (name === ".pnpm" || name === ".bin" || name.startsWith(".")) continue;
      const p = path.join(dir, name);
      if (name.startsWith("@") && fs.lstatSync(p).isDirectory()) { for (const sub of fs.readdirSync(p).sort()) entry(path.join(p, sub), `${name}/${sub}`); }
      else entry(p, name);
    }
  }
  for (const real of [...outside].sort()) h.update(`outside\0${real}\0${hashOutside(real, budget)}\0`);
  return h.digest("hex");
}
