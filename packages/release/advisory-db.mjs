// The pinned RustSec advisory database for the class A cargo-deny and cargo-audit scans.
//
// Class A is deterministic and offline for advisories: the same commit gives the same verdict until the pin in
// advisory-db.json moves. Neither tool fetches at run time; both read the checkout this module materialises at
// <cacheRoot>/advisory-db/<commit>. Moving the pin is a reviewed commit (and re-runs the scans: the pin is part of the
// scan's command environment, hence of its cache key). Fetching the pinned commit is the only network step, once per
// commit per machine; a machine that cannot fetch and has no cache refuses, it never falls back to the tools' own fetch.
import { spawnSync } from "./background-process.mjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const PIN_FILE = fileURLToPath(new URL("./advisory-db.json", import.meta.url));
const COMMIT = /^[0-9a-f]{40}$/;

/** The pin ({ url, commit }) from advisory-db.json; throws when it is missing or malformed. */
export function readAdvisoryPin({ file = PIN_FILE, readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
  let pin;
  try { pin = JSON.parse(readFile(file)); } catch (e) { throw new Error(`advisory database pin ${file} is unreadable (${e.message}); class A advisory scans refuse to run without it`); }
  return checkPin(pin);
}

function checkPin(pin) {
  if (!pin || typeof pin.url !== "string" || !/^https:\/\/\S+$/.test(pin.url) || typeof pin.commit !== "string" || !COMMIT.test(pin.commit)) {
    throw new Error('advisory database pin must be { "url": "https://...", "commit": "<40 lowercase hex>" }');
  }
  return { url: pin.url, commit: pin.commit };
}

/** Where advisory databases are cached: RK_ADVISORY_CACHE_ROOT, else %LOCALAPPDATA%\RightKit (Windows) or ~/.cache/rightkit (never XDG_CACHE_HOME, see cache-policy.mjs). */
export function advisoryCacheRoot({ env = process.env, platform = process.platform, home = os.homedir() } = {}) {
  const override = env.RK_ADVISORY_CACHE_ROOT;
  if (override) {
    if (!path.isAbsolute(override)) throw new Error("RK_ADVISORY_CACHE_ROOT must be an absolute path");
    return path.resolve(override);
  }
  if (platform === "win32" && env.LOCALAPPDATA && path.win32.isAbsolute(env.LOCALAPPDATA)) return path.win32.join(env.LOCALAPPDATA, "RightKit");
  return path.join(home, ".cache", "rightkit");
}

const defaultGit = (args, { cwd } = {}) => {
  const r = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true, shell: false, env: { ...process.env, GIT_TERMINAL_PROMPT: "0" } });
  return { status: r.status, stdout: r.stdout ?? "", stderr: r.stderr ?? "", error: r.error };
};

/**
 * Materialise pin.commit at <cacheRoot>/advisory-db/<commit> and return that path. A present checkout is reused only when its
 * `git rev-parse HEAD` equals the pin; otherwise it is built in a sibling temp directory (git init, fetch --depth 1 origin
 * <commit>, checkout FETCH_HEAD, verify HEAD) and renamed into place. Throws with the reason when it cannot.
 * `git(args, { cwd })` => { status, stdout, stderr, error? } is injectable.
 */
export function ensureAdvisoryDb({ cacheRoot, pin, git = defaultGit, fsImpl = fs } = {}) {
  if (typeof cacheRoot !== "string" || !path.isAbsolute(cacheRoot)) throw new Error("advisory database: cacheRoot must be an absolute path");
  const { url, commit } = checkPin(pin);
  const base = path.join(cacheRoot, "advisory-db");
  const dest = path.join(base, commit);
  const head = (dir) => { const r = git(["rev-parse", "HEAD"], { cwd: dir }); return !r.error && r.status === 0 ? String(r.stdout).trim() : null; };
  if (fsImpl.existsSync(dest)) {
    if (head(dest) === commit) return dest;
    // A stale or damaged directory under the pinned name is never trusted; replace it.
  }
  const fail = (step, r) => new Error(`advisory database ${commit} is not available: git ${step} failed (${r.error?.message ?? (String(r.stderr).trim() || `exit ${r.status}`)}); class A refuses to scan advisories without the pinned database. Connect once to fetch ${url} or set RK_ADVISORY_CACHE_ROOT to a populated cache.`);
  fsImpl.mkdirSync(base, { recursive: true });
  const tmp = fsImpl.mkdtempSync(path.join(base, `.tmp-${commit.slice(0, 12)}-`));
  try {
    for (const [step, args] of [["init", ["init", "-q"]], ["remote add", ["remote", "add", "origin", url]], ["fetch", ["fetch", "-q", "--depth", "1", "origin", commit]], ["checkout", ["checkout", "-q", "FETCH_HEAD"]]]) {
      const r = git(args, { cwd: tmp });
      if (r.error || r.status !== 0) throw fail(step, r);
    }
    const got = head(tmp);
    if (got !== commit) throw new Error(`advisory database fetch returned ${got ?? "no commit"}, not the pinned ${commit}`);
    if (fsImpl.existsSync(dest)) fsImpl.rmSync(dest, { recursive: true, force: true });
    try { fsImpl.renameSync(tmp, dest); } catch (e) {
      // A concurrent run may have installed the same commit first: accept it when it verifies.
      if (!(fsImpl.existsSync(dest) && head(dest) === commit)) throw e;
    }
    return dest;
  } finally {
    fsImpl.rmSync(tmp, { recursive: true, force: true });
  }
}

// cargo-deny keeps each database in <db-path>/<name derived from the url>; this is the name it derives for the RustSec url
// (cargo-deny 0.20). A different url, or a tool that names it differently, fails closed: the tool runs --offline and finds no database.
const DENY_DB_DIRS = Object.freeze({ "https://github.com/rustsec/advisory-db": "advisory-db-3157b0e258782691" });

/**
 * cargo-deny has no command-line switch for the database location, only `[advisories] db-path` in its config. This writes an
 * overlay config (the repository's deny.toml with any db-path replaced) and a db-path directory whose entry for the pin url is the
 * pinned checkout, in a fresh temp directory. `--offline --config <overlay>` then reads exactly the pin and cannot fetch.
 * Returns { configPath, cleanup }.
 */
export function prepareDenyOverlay({ denyToml, dbDir, pin, tmpRoot = os.tmpdir(), fsImpl = fs } = {}) {
  const { url } = checkPin(pin);
  const name = DENY_DB_DIRS[url];
  if (!name) throw new Error(`cargo-deny database directory name for ${url} is not known; add it to DENY_DB_DIRS in advisory-db.mjs`);
  const dir = fsImpl.mkdtempSync(path.join(tmpRoot, "rk-deny-"));
  try {
    const dbPath = path.join(dir, "dbs");
    fsImpl.mkdirSync(dbPath);
    fsImpl.symlinkSync(dbDir, path.join(dbPath, name), process.platform === "win32" ? "junction" : "dir");
    const lines = String(denyToml ?? "").split(/\r?\n/);
    const out = []; let inAdvisories = false; let placed = false;
    for (const line of lines) {
      const header = /^\s*\[([^\]]*)\]\s*(#.*)?$/.exec(line);
      if (header) {
        inAdvisories = header[1].trim() === "advisories";
        out.push(line);
        if (inAdvisories) { out.push(`db-path = ${JSON.stringify(dbPath)}`); placed = true; }
        continue;
      }
      if (inAdvisories && /^\s*db-path\s*=/.test(line)) continue;
      out.push(line);
    }
    if (!placed) out.push("", "[advisories]", `db-path = ${JSON.stringify(dbPath)}`);
    const configPath = path.join(dir, "deny.toml");
    fsImpl.writeFileSync(configPath, `${out.join("\n")}\n`);
    return { configPath, cleanup: () => fsImpl.rmSync(dir, { recursive: true, force: true }) };
  } catch (e) {
    fsImpl.rmSync(dir, { recursive: true, force: true });
    throw e;
  }
}
