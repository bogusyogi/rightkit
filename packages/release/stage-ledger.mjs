// The stage-output ledger (docs/GATE-CONTRACT.md "Commit-only build"; item 7 decision 2, question 1, rows 7.05/7.05b/7.07/7.08).
//
// A real build writes into the directory it builds from: Tauri's `gen/schemas`, staged sidecars, a frontend `dist`. The snapshot is
// exactly the commit's tree (plus the gate's installed dependencies) at the start of every stage; what a stage's own children write
// during it may remain at the stage's end only as LEDGER ENTRIES, each of which must be:
//   1. not tracked (not in `ls-tree <commit>`) and ignored by the commit's own ignore rules, judged with an empty bare GIT_DIR and a null
//      core.excludesFile, so neither the checkout's `info/exclude` nor a global excludes file ever widens what is ignored;
//   2. a regular file (a directory only as the parent of one), never a link, and with exactly one hard link;
//   3. never under a reserved path: a `target` segment, `.right-release`, `.git`, or a `node_modules` outside the gate's own dependency roots;
//   4. recorded by THIS walk, never by a child: the ledger lives in the door's memory and in the door receipt under the checkout's git
//      common directory, never inside the snapshot (GC:162).
// This module only walks, classifies and compares. The exact-snapshot verify is snapshot.mjs verifySnapshot, which takes the ledger.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

export const LEDGER_RESERVED_SEGMENTS = Object.freeze(["target", ".right-release", ".git", "node_modules"]);
export const NULL_DEVICE = process.platform === "win32" ? "NUL" : "/dev/null";

const fold = (platform = process.platform) => (platform === "darwin" || platform === "win32" ? (s) => s.toLowerCase() : (s) => s);

/** The comparable form of an entry: what the next stage's start verify must find unchanged. */
export const entryKey = (e) => `${e.path}\0${e.type}\0${e.size}\0${e.sha256}\0${e.execBit ? 1 : 0}`;

/** A clean env for the git calls below: nothing of the caller's GIT_* and no global or system configuration. */
function gitEnv(env = process.env) {
  const clean = Object.fromEntries(Object.entries(env).filter(([k]) => !/^GIT_/i.test(k)));
  return { ...clean, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: NULL_DEVICE, GIT_OPTIONAL_LOCKS: "0", GIT_TERMINAL_PROMPT: "0" };
}

/**
 * Which of `paths` (posix, relative to `dir`) the commit's own ignore rules ignore: `git check-ignore --no-index` run with an empty
 * bare GIT_DIR, the snapshot as the work tree and a null core.excludesFile, so only the snapshot's own `.gitignore` files apply.
 * Throws when git cannot answer (the caller fails closed).
 */
export function neutralIgnored({ dir, paths, run = spawnSync, env = process.env }) {
  if (!paths.length) return new Set();
  const bare = fs.mkdtempSync(path.join(os.tmpdir(), "rk-ignore-oracle-"));
  try {
    const made = run("git", ["init", "--bare", "-q", "--template=", bare], { encoding: "utf8", env: gitEnv(env), windowsHide: true });
    if (made.error || made.status !== 0) throw new Error(`cannot create the empty git directory for the ignore check: ${made.error?.message ?? String(made.stderr).trim()}`);
    const r = run("git", [`--git-dir=${bare}`, `--work-tree=${dir}`, "-c", `core.excludesFile=${NULL_DEVICE}`, "check-ignore", "--no-index", "-z", "--stdin"], {
      cwd: dir, input: `${paths.join("\0")}\0`, encoding: "utf8", env: gitEnv(env), windowsHide: true, maxBuffer: 1 << 28,
    });
    // 0: some path is ignored; 1: none is; anything else is git failing.
    if (r.error || (r.status !== 0 && r.status !== 1)) throw new Error(`git check-ignore failed (${r.error?.message ?? `exit ${r.status}: ${String(r.stderr).trim()}`})`);
    return new Set(String(r.stdout ?? "").split("\0").filter(Boolean));
  } finally { fs.rmSync(bare, { recursive: true, force: true }); }
}

/**
 * Walks `dir` and returns what it holds beyond the commit: `{ entries, problems }`. `tracked` is the set of every path in
 * `ls-tree <commit>` (posix); `dependencyRoots` the `node_modules` directories the gate installed under 5.09a (posix, relative; their
 * bytes are bound by the class A environment digest, not here). `entries` are admissible ledger entries; `problems` name every write
 * that cannot be one (not ignored, a link, a hard link, reserved, an empty directory, a `.git` holding more than the gate's config).
 */
export function walkSnapshot({ dir, tracked, dependencyRoots = new Set(), run = spawnSync, env = process.env, platform = process.platform }) {
  const f = fold(platform);
  const trackedFolded = new Set([...tracked].map(f));
  const trackedDirs = new Set();
  for (const p of tracked) { const parts = f(p).split("/"); for (let i = 1; i < parts.length; i++) trackedDirs.add(parts.slice(0, i).join("/")); }
  const deps = new Set([...dependencyRoots].map(f));
  const candidates = []; const problems = [];
  const reserved = (rel) => rel.split("/").find((seg) => LEDGER_RESERVED_SEGMENTS.includes(f(seg)));
  const walk = (abs, rel) => {
    let names;
    try { names = fs.readdirSync(abs, { withFileTypes: true }); } catch (e) { problems.push(`${rel || "."} cannot be read: ${e.message}`); return; }
    names.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    let any = false;
    for (const d of names) {
      const r = rel ? `${rel}/${d.name}` : d.name;
      const key = f(r);
      const p = path.join(abs, d.name);
      if (deps.has(key)) continue;
      any = true;
      if (key === ".git" && rel === "") {
        // 5.09b: only the gate's config-only origin file; its exact bytes are item 5's to verify.
        let inner = [];
        try { inner = fs.readdirSync(p); } catch { /* reported below */ }
        if (inner.length !== 1 || inner[0] !== "config") problems.push(`${r} holds more than the gate's config-only file (.git is reserved)`);
        continue;
      }
      let st;
      try { st = fs.lstatSync(p); } catch (e) { problems.push(`${r} cannot be read: ${e.message}`); continue; }
      if (st.isSymbolicLink()) { if (!trackedFolded.has(key)) problems.push(`${r} is a link the commit does not have (a stage output is never a link)`); continue; }
      if (st.isDirectory()) {
        if (trackedDirs.has(key)) { walk(p, r); continue; }
        const bad = reserved(r);
        if (bad) { problems.push(`${r} is under the reserved path segment ${JSON.stringify(bad)} (a stage output is never written there)`); continue; }
        const before = candidates.length + problems.length;
        walk(p, r);
        if (candidates.length + problems.length === before) problems.push(`${r} is an empty directory the commit does not have`);
        continue;
      }
      if (!st.isFile()) { problems.push(`${r} is neither a regular file nor a directory`); continue; }
      if (trackedFolded.has(key)) continue; // a tracked file: its bytes are verifySnapshot's
      const bad = reserved(r);
      if (bad) { problems.push(`${r} is under the reserved path segment ${JSON.stringify(bad)} (a stage output is never written there)`); continue; }
      if (st.nlink > 1) { problems.push(`${r} has ${st.nlink} hard links (a stage output has exactly one)`); continue; }
      candidates.push({ rel: r, abs: p, size: st.size, execBit: platform !== "win32" && (st.mode & 0o111) !== 0 });
    }
    return any;
  };
  walk(dir, "");
  let ignored;
  try { ignored = neutralIgnored({ dir, paths: candidates.map((c) => c.rel), run, env }); }
  catch (e) { return { entries: [], problems: [...problems, `the ignore rules of the commit could not be evaluated: ${e.message}`] }; }
  const ignoredFolded = new Set([...ignored].map(f));
  const entries = [];
  for (const c of candidates) {
    // A split package stage writes a declared root output (a DMG, an updater tarball) into the snapshot as a generated entry (row 7.04c R-T2 rule 5): it must be
    // ignored by the commit itself, so the message names the declared fields such an output comes from and the fix.
    if (!ignoredFolded.has(f(c.rel))) { problems.push(`${c.rel} is not ignored by the commit's own ignore rules (the checkout's info/exclude and global excludes never count); if the release config declares it (updater.artifacts[].file, installer.artifacts[].file, notarize.file, artifacts[] or a postSign output), add ${c.rel} to the commit's .gitignore`); continue; }
    let bytes;
    try { bytes = fs.readFileSync(c.abs); } catch (e) { problems.push(`${c.rel} cannot be read: ${e.message}`); continue; }
    entries.push({ path: c.rel, type: "file", size: bytes.length, sha256: crypto.createHash("sha256").update(bytes).digest("hex"), execBit: c.execBit });
  }
  entries.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  return { entries, problems };
}

/** What changed between two entry lists: `{ added, changed, removed }` as paths. */
export function diffEntries(before, after) {
  const b = new Map(before.map((e) => [e.path, e])); const a = new Map(after.map((e) => [e.path, e]));
  return {
    added: [...a.keys()].filter((p) => !b.has(p)),
    changed: [...a.keys()].filter((p) => b.has(p) && entryKey(b.get(p)) !== entryKey(a.get(p))),
    removed: [...b.keys()].filter((p) => !a.has(p)),
  };
}

export const sameEntries = (x, y) => { const d = diffEntries(x, y); return !d.added.length && !d.changed.length && !d.removed.length; };

/**
 * The attribution diff of a staging step: every path the step added must be one it reported (or its receipt), and every path it
 * reported must exist. Anything else is an unattributed write. `reported` and `receipt` are snapshot-relative posix paths.
 */
export function attributionProblems({ before, after, reported, receipt }) {
  const d = diffEntries(before, after);
  const allowed = new Set([...reported, ...(receipt ? [receipt] : [])]);
  const problems = [];
  for (const p of [...d.added, ...d.changed]) if (!allowed.has(p)) problems.push(`${p} was written by the staging step but it did not report it`);
  for (const p of d.removed) problems.push(`${p} was removed by the staging step`);
  const present = new Set(after.map((e) => e.path));
  for (const p of allowed) if (!present.has(p)) problems.push(`${p} was reported by the staging step but is not in the snapshot as a ledger entry`);
  return problems;
}

/** Every `path` string a staging receipt or its `--json` result carries (files, their notices), app-relative. */
export function reportedPaths(result) {
  const out = new Set();
  const visit = (v) => {
    if (Array.isArray(v)) { v.forEach(visit); return; }
    if (v && typeof v === "object") { for (const [k, x] of Object.entries(v)) { if (k === "path" && typeof x === "string") out.add(x.replaceAll("\\", "/")); else visit(x); } }
  };
  visit(result?.files ?? []);
  return out;
}

/**
 * The cross-check with `.rightkit/compile-inputs.json` (decision 2, Q1): for each declared input that covers ledger entries, the digest
 * the compile receipt names must equal the gate's own digest of those bytes (compile-inputs.mjs judgeCompileInputs, the broker's
 * algorithm). Returns problems; `judge` is judgeCompileInputs.
 */
export function declaredInputProblems({ snapshot, entries, receiptDigests, judge }) {
  const judged = judge(snapshot);
  const problems = judged.findings.map((x) => `declared compile input: ${x}`);
  const named = new Map((receiptDigests ?? []).map((d) => [d.path, d.sha256]));
  for (const input of judged.inputs) {
    const covers = entries.some((e) => e.path === input.path || e.path.startsWith(`${input.path}/`));
    if (!covers) continue;
    if (!named.has(input.path)) problems.push(`declared compile input ${input.path} covers ledger entries but the receipt carries no digest for it`);
    else if (named.get(input.path) !== input.sha256) problems.push(`declared compile input ${input.path}: the receipt's digest ${named.get(input.path)} differs from the gate's digest ${input.sha256} of the ledger bytes`);
  }
  return problems;
}

/**
 * Whether the entries the snapshot verify admitted (snapshot.mjs verifySnapshot / snapshot-check.mjs verifySnapshotDir with a ledger,
 * row 5.09d) agree with the entries this gate's own walk recorded. The verify's `admitted` lists FILES and the DIRECTORIES that parent them
 * (`type: "dir"`); the walk records files only (a directory is implied by its files). They agree when the admitted files are exactly the
 * walk's files (same path, size, digest and exec bit when the verify names them) and every admitted directory parents one of them.
 * Returns a message naming the first disagreement, or null.
 */
export function admittedMismatch(admitted, entries) {
  const files = admitted.filter((e) => (e.type ?? "file") !== "dir");
  const dirs = admitted.filter((e) => e.type === "dir");
  const mine = new Map(entries.map((e) => [e.path, e])); const theirs = new Map(files.map((e) => [e.path, e]));
  for (const p of mine.keys()) if (!theirs.has(p)) return `${p} is in the door's walk but the verify did not admit it`;
  for (const p of theirs.keys()) if (!mine.has(p)) return `${p} was admitted by the verify but the door's walk did not record it`;
  for (const [p, e] of theirs) {
    const w = mine.get(p);
    if (e.sha256 !== undefined && (e.sha256 !== w.sha256 || e.size !== w.size || Boolean(e.execBit) !== Boolean(w.execBit))) return `${p} differs between the verify's entry and the door's walk`;
  }
  const parents = new Set();
  for (const p of mine.keys()) { const parts = p.split("/"); for (let i = 1; i < parts.length; i++) parents.add(parts.slice(0, i).join("/")); }
  for (const d of dirs) if (!parents.has(d.path)) return `the directory ${d.path} was admitted by the verify but parents no ledger entry`;
  return null;
}

/** The receipt form of the ledger: no absolute path, no bytes. */
export const receiptLedger = (entries) => entries.map((e) => ({ path: e.path, kind: e.kind, type: e.type, size: e.size, sha256: e.sha256, execBit: e.execBit, producer: e.producer }));
