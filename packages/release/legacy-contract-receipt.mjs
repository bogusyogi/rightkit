// The reading of the old contract-test receipt (rightkit.contract-test-receipt/1), kept so results sealed before the twelve-line test stay readable
// (docs/GATE-CONTRACT.md GC-A27, rightkit#1514 step 9). Copied from the retired runner gate-acceptance.mjs (last at 1e478405c): the declarations
// gate-release-admission.mjs needs, and the ones they use, without the three that named the runner's own files (BUDGETS_FILE, RUNNER_FILE,
// runnerFileHashes): their hashes are `retiredRunner` in contract-nn.json. Nothing here runs a contract test.
import { createHash, randomBytes } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { keyPath, repoKey } from "./snapshot.mjs";

const RECEIPT_SCHEMA = "rightkit.gate-receipt/1";

const SOURCE_IDENTITY_SCHEMA = "rightkit.source-identity.v1";

const RENDER_RECEIPT_SCHEMA = "rightkit.render-views/1";

const here = path.dirname(fileURLToPath(import.meta.url));

const sha256 = (data) => createHash("sha256").update(data).digest("hex");

const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

const isSha = (v) => typeof v === "string" && /^[0-9a-f]{64}$/.test(v);

const isCommit = (v) => typeof v === "string" && /^[0-9a-f]{40}$/.test(v);

const NEEDS_FIXTURE = "needs fixture rows (criteria.md section 2, F1-F14) that do not exist in the witness fixture yet";

const NEEDS_INSTALL_APPROVAL = "needs install approval";

/** Unknown or missing keys of `obj` against `{ required, optional }`; fail closed on both. */
function strictKeys(obj, { required = [], optional = [] }, where) {
  if (!isObject(obj)) return [`${where}: not an object`];
  const problems = [];
  for (const k of required) if (!(k in obj)) problems.push(`${where}: missing field "${k}"`);
  const known = new Set([...required, ...optional]);
  for (const k of Object.keys(obj)) if (!known.has(k)) problems.push(`${where}: unknown field "${k}"`);
  return problems;
}

/** GC-A21: every non-JSON log line carries a UTC stamp; JSON and the one `verdict deterministic` marker line are exempt. */
const UTC_STAMP = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z /;

function checkUtcStamps(text) {
  const problems = [];
  const lines = String(text).split(/\r?\n/);
  let checked = 0;
  for (const [i, line] of lines.entries()) {
    if (!line.trim()) continue;
    if (/^rightkit-gate: verdict deterministic/.test(line)) continue;
    if (/^\s*[{}\[\]"]/.test(line)) continue; // pretty-printed JSON body
    checked += 1;
    if (!UTC_STAMP.test(line)) problems.push(`log line ${i + 1} has no UTC stamp: ${line.slice(0, 120)}`);
  }
  if (checked === 0) problems.push("the log has no lines to check");
  return { ok: problems.length === 0, problems, checked };
}

/** `[stage] passed in N.Ns` and `failed in` lines of a log: [{ stage, ok, seconds }]. */
function stageTimings(text) {
  const out = [];
  for (const m of String(text).matchAll(/\[([a-z:]+)\] (passed|failed) in (\d+(?:\.\d+)?)s/g)) out.push({ stage: m[1], ok: m[2] === "passed", seconds: Number(m[3]) });
  return out;
}

// `limitedEvidence`: the compile stage's list of type-check-only cross rows, which NN38 requires; the schema refused it until 2026-11-18, so NN38 could never pass.
const STAGE_FIELDS = ["ok", "at", "ruleTier", "direct", "qualifyOwed", "limitedEvidence", "outputs", "snapshot", "snapshotLock", "platform", "sealed", "manifestSha256", "voided", "track", "qaReceiptDigest", "tier"];

const BINDING_FIELDS = ["tree", "locks", "gate", "toolchain", "executor"];

/**
 * GC-A04, GC-I05 (door half), GC-A05: the door receipt `--receipt-out` writes. `expectedTree` is `git rev-parse <commit>^{tree}`. Fail closed on unknown
 * fields; `verdict` and `refreshed` must be absent (the broker never writes them into a gate receipt).
 */
function validateDoorReceipt(receipt, { commit, expectedTree, stages = [] }) {
  const problems = strictKeys(receipt, { required: ["schema", "commit", "app", "binding", "stages"], optional: ["brokerException"] }, "receipt");
  if (!isObject(receipt)) return { ok: false, problems };
  if (receipt.schema !== RECEIPT_SCHEMA) problems.push(`receipt.schema is ${JSON.stringify(receipt.schema)}, expected ${RECEIPT_SCHEMA}`);
  if (receipt.commit !== commit) problems.push(`receipt.commit ${receipt.commit} != ${commit}`);
  if ("brokerException" in receipt) problems.push("receipt.brokerException is set: the run was built outside the broker");
  for (const banned of ["verdict", "refreshed"]) if (banned in receipt) problems.push(`receipt carries "${banned}" (refused by the gate)`);
  problems.push(...strictKeys(receipt.binding, { required: BINDING_FIELDS }, "receipt.binding"));
  if (isObject(receipt.binding)) {
    if (receipt.binding.tree !== expectedTree) problems.push(`receipt.binding.tree ${receipt.binding.tree} != ${expectedTree} (commit^{tree})`);
    if (!isSha(receipt.binding.gate)) problems.push("receipt.binding.gate is not a sha256");
  }
  if (!isObject(receipt.stages)) { problems.push("receipt.stages is not an object"); return { ok: false, problems }; }
  for (const stage of stages) {
    const s = receipt.stages[stage];
    const where = `receipt.stages.${stage}`;
    if (!isObject(s)) { problems.push(`${where}: missing`); continue; }
    problems.push(...strictKeys(s, { required: ["ok", "at"], optional: STAGE_FIELDS.slice(2) }, where));
    if (s.ok !== true) problems.push(`${where}.ok is ${JSON.stringify(s.ok)}`);
    if ("voided" in s) problems.push(`${where}.voided is ${JSON.stringify(s.voided)}`);
    if (!Number.isFinite(Date.parse(s.at))) problems.push(`${where}.at is not a timestamp`);
    if (stage !== "check" && !isObject(s.snapshot)) problems.push(`${where}.snapshot is missing`);
    if (isObject(s.snapshot)) {
      if (s.snapshot.tree !== expectedTree) problems.push(`${where}.snapshot.tree ${s.snapshot.tree} != ${expectedTree}`);
      if (s.snapshot.intactAfter !== true) problems.push(`${where}.snapshot.intactAfter is ${JSON.stringify(s.snapshot.intactAfter)} (the snapshot must still be exactly the commit's bytes at stage exit)`);
      if (typeof s.snapshot.path !== "string" || !s.snapshot.path) problems.push(`${where}.snapshot.path is missing`);
    }
  }
  return { ok: problems.length === 0, problems };
}

/**
 * Whether the compile of a run really ran in that run. A recorded compile that "stands" is reused (`[compile] already passed ...`; the contract's E7
 * says no redundant compile is required, and the receipt keeps the OLD stage `at`), so only a run whose log has `[compile] passed in` and no
 * `already passed` line for the compile proves anything about ordering or about what a compile reads.
 */
function compileRanFresh(log) {
  const t = String(log ?? "");
  return /\[compile\] passed in/.test(t) && !/\[compile\] already passed/.test(t);
}

/**
 * GC-T05: Class A (stage check) is finished before the compile stage starts. With `log` (the run's own log) the proof is: the compile really ran in
 * this run, the `[check] passed in` line's UTC stamp is not later than the receipt's compile `at` (the moment the compile stage began), and the check
 * stage is ok. A reused compile is returned as `{ ok: false, skipped: true }`: nothing about ordering can be read from another run's `at`.
 * Without `log` only the receipt timestamps are compared (kept for callers that hold no log).
 */
function classABeforeCompile(receipt, log = null) {
  const check = Date.parse(receipt?.stages?.check?.at);
  const compile = Date.parse(receipt?.stages?.compile?.at);
  if (!Number.isFinite(check) || !Number.isFinite(compile)) return { ok: false, problems: ["receipt.stages.check.at and receipt.stages.compile.at are both needed"] };
  if (receipt.stages.check.ok !== true) return { ok: false, problems: [`receipt.stages.check.ok is ${JSON.stringify(receipt.stages.check.ok)}`] };
  if (log === null) return check <= compile ? { ok: true, problems: [] } : { ok: false, problems: [`check at ${receipt.stages.check.at} is after compile at ${receipt.stages.compile.at}`] };
  if (!compileRanFresh(log)) return { ok: false, skipped: true, problems: ["the compile was reused from an earlier run (`[compile] already passed`); its recorded `at` is not this run's, so no ordering can be read"] };
  const m = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z) rightkit-gate: \[check\] passed in/m.exec(String(log));
  if (!m) return { ok: false, problems: ["the log has no stamped `[check] passed in` line"] };
  const checkEnd = Date.parse(m[1]);
  return checkEnd <= compile ? { ok: true, problems: [] } : { ok: false, problems: [`Class A ended at ${m[1]}, after the compile stage began at ${receipt.stages.compile.at}`] };
}

/** GC-A15, GC-A07: the output dirs a receipt records (stage outputs.dirs) are one set across stages, and none is a debug dir. */
function outputDirsProblems(receipt) {
  const problems = [];
  const sets = [];
  for (const [stage, s] of Object.entries(receipt?.stages ?? {})) {
    const dirs = s?.outputs?.dirs;
    if (dirs === undefined) continue;
    if (!Array.isArray(dirs) || dirs.some((d) => typeof d !== "string")) { problems.push(`receipt.stages.${stage}.outputs.dirs is malformed`); continue; }
    sets.push([stage, [...dirs].sort()]);
    if (s.outputs.truncated === true) problems.push(`receipt.stages.${stage}.outputs.truncated is true`);
    for (const d of dirs) if (/(^|[\\/])debug([\\/]|$)/.test(d)) problems.push(`receipt.stages.${stage}.outputs.dirs names a debug directory: ${d}`);
  }
  if (sets.length === 0) problems.push("no stage of the receipt records outputs.dirs");
  for (const [stage, dirs] of sets.slice(1)) if (JSON.stringify(dirs) !== JSON.stringify(sets[0][1])) problems.push(`output dirs of ${stage} differ from ${sets[0][0]}`);
  return { ok: problems.length === 0, problems };
}

/**
 * The key GC-I01 names: sha256 of the canonical repository path, first 16 hex digits, over the path as the gate keys it. On Windows that is the
 * verbatim `\\?\` spelling, so this imports the gate's own `keyPath` and `repoKey` (snapshot.mjs) instead of hashing the bare path.
 */
const repositoryKey = (canonicalPath, platform = process.platform) => repoKey(keyPath(canonicalPath, platform));

/** GC-A23: the snapshot's `.rightkit-source-identity.json`. `trees` is `{ dir: tree id }` recomputed from git by the caller. */
function validateSourceIdentity(identity, { commit, trees }) {
  const problems = strictKeys(identity, { required: ["schema", "commit", "trees"] }, "source-identity");
  if (!isObject(identity)) return { ok: false, problems };
  if (identity.schema !== SOURCE_IDENTITY_SCHEMA) problems.push(`source-identity.schema is ${JSON.stringify(identity.schema)}`);
  if (identity.commit !== commit) problems.push(`source-identity.commit ${identity.commit} != ${commit}`);
  if (!isObject(identity.trees)) return { ok: false, problems: [...problems, "source-identity.trees is not an object"] };
  if (!("" in identity.trees)) problems.push('source-identity.trees has no "" (root) entry');
  for (const [dir, tree] of Object.entries(trees)) if (identity.trees[dir] !== tree) problems.push(`source-identity.trees[${JSON.stringify(dir)}] is ${identity.trees[dir]}, git says ${tree}`);
  for (const dir of Object.keys(identity.trees)) if (!(dir in trees)) problems.push(`source-identity.trees has an unexpected entry ${JSON.stringify(dir)}`);
  return { ok: problems.length === 0, problems };
}

/** GC-A22 render: the receipt a `--to render` run stores. `png(name)` returns the PNG bytes of a case (or null). */
function validateRenderReceipt(receipt, { commit, png }) {
  const problems = strictKeys(receipt, { required: ["schema", "commit", "tree", "appDir", "package", "bin", "features", "executable", "buildOutputDir", "host", "startedAt", "endedAt", "cases"], optional: ["buildRequestId"] }, "render-receipt");
  if (!isObject(receipt)) return { ok: false, problems };
  if (receipt.schema !== RENDER_RECEIPT_SCHEMA) problems.push(`render-receipt.schema is ${JSON.stringify(receipt.schema)}`);
  if (receipt.commit !== commit) problems.push(`render-receipt.commit ${receipt.commit} != ${commit}`);
  if (!isSha(receipt.executable?.sha256)) problems.push("render-receipt.executable.sha256 is not a sha256");
  if (typeof receipt.executable?.path !== "string" || typeof receipt.buildOutputDir !== "string" || !receipt.executable.path.startsWith(receipt.buildOutputDir)) problems.push("render-receipt.executable.path does not lie inside buildOutputDir");
  if (!Array.isArray(receipt.cases) || receipt.cases.length === 0) { problems.push("render-receipt.cases is empty"); return { ok: false, problems }; }
  for (const c of receipt.cases) {
    const where = `render-receipt.cases[${c?.name}]`;
    problems.push(...strictKeys(c, { required: ["name", "png", "sha256", "bytes", "width", "height"] }, where));
    const bytes = isObject(c) ? png(c.png) : null;
    if (!bytes) { problems.push(`${where}: PNG ${c?.png} cannot be read`); continue; }
    if (!(bytes.length > 24 && bytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])))) { problems.push(`${where}: not a PNG (bad magic)`); continue; }
    if (sha256(bytes) !== c.sha256 || bytes.length !== c.bytes) problems.push(`${where}: sha256 or size differs from the receipt`);
    if (bytes.readUInt32BE(16) !== c.width || bytes.readUInt32BE(20) !== c.height) problems.push(`${where}: PNG width/height differ from the receipt`);
  }
  return { ok: problems.length === 0, problems };
}

/** GC-I22: vendored vector files equal the sha256 their vectors-source.json pins and RustRight's own bytes; a missing upstream directory fails, it is not skipped. */
function vectorsProblems({ source, files, upstream = null, upstreamDir = null }) {
  const problems = strictKeys(source, { required: ["schema", "note", "source", "files"] }, "vectors-source");
  if (!isObject(source?.files)) return { ok: false, problems: [...problems, "vectors-source.files missing"] };
  for (const [name, meta] of Object.entries(source.files)) {
    const bytes = files[name];
    if (!bytes) { problems.push(`${name}: pinned but not vendored`); continue; }
    if (sha256(bytes) !== meta.sha256) problems.push(`${name}: vendored sha256 ${sha256(bytes).slice(0, 12)} != pinned ${String(meta.sha256).slice(0, 12)}`);
    if (!upstream) continue;
    if (!upstream[name]) problems.push(`${name}: RustRight's docs/contracts has no copy to compare with`);
    else if (sha256(upstream[name]) !== sha256(bytes)) problems.push(`${name}: differs from RustRight's docs/contracts copy`);
  }
  if (!upstream) problems.push(`RustRight's docs/contracts${upstreamDir ? ` (${upstreamDir})` : ""} was not found: the vendored vectors cannot be compared with their source`);
  for (const name of Object.keys(files)) if (name !== "vectors-source.json" && !(name in source.files)) problems.push(`${name}: vendored but not pinned`);
  return { ok: problems.length === 0, problems };
}

/** GC-I18 (static half): the digest the gate pins is the installed runtime's. */
function brokerDigestProblems({ manifest, pins }) {
  const problems = [];
  if (!isObject(manifest) || !isSha(manifest.brokerDigest)) problems.push("runtime manifest has no brokerDigest sha256");
  if (!isObject(pins) || !isSha(pins.brokerDigest)) problems.push("broker pins record has no brokerDigest sha256");
  if (!problems.length && manifest.brokerDigest !== pins.brokerDigest) problems.push(`pins brokerDigest ${pins.brokerDigest.slice(0, 12)} != runtime manifest brokerDigest ${manifest.brokerDigest.slice(0, 12)}`);
  return { ok: problems.length === 0, problems };
}

/**
 * S2 (GC-A24, GC-A19, NN01): a warm rerun of the same commit. Against the first run `prev` (id -> row) and the rerun `next`:
 *  - every Class A scan of the rerun answered `hit` (a `ran` there is a failure, a skipped row is a failure). There is no uncached family any more:
 *    pnpm-lockfile and advisory-exceptions took GC-A19/GC-A24 keys (commit af0704a05, 2026-11-05), so NN01 ("any Class A row is ran") covers them too;
 *  - the key of every row equals the first run's key byte for byte.
 */
function assertWarmRerun(prev, next) {
  const problems = [];
  let hits = 0;
  for (const [id, row] of next) {
    if (row.coverage === "skipped" && row.status === "not-in-pass") continue;
    if (row.coverage === "skipped") { problems.push(`${id}: skipped (${row.reason}); a skipped row is never a pass`); continue; }
    if (row.class === "A" && row.coverage !== "hit") problems.push(`${id}: Class A row ${row.coverage} on an unchanged commit, expected hit${row.key === null ? " (no cache key)" : ""}`);
    if (row.class === "A" && row.coverage === "hit") hits += 1;
    const before = prev.get(id);
    if (row.class === "A" && before && before.key !== row.key) problems.push(`${id}: key changed between runs on one commit (${before.key} -> ${row.key})`);
    if (row.class === "A" && !before) problems.push(`${id}: not present in the first run`);
  }
  if (hits === 0) problems.push("no Class A row hit: the rerun proves nothing about warmth");
  return { ok: problems.length === 0, problems, hits };
}

/**
 * S3 (GC-A24): one `.rs` edit. Exactly one `rustfmt` row `ran` (that file's), plus any per-file rows that read it (secret-scan, licence); every other
 * Class A key equals the previous run. `perFile` is a RegExp of the row ids allowed to run besides rustfmt.
 */
function assertOneFileEdit(prev, next, { perFile = /^(secret-scan|licen[cs]e)/ } = {}) {
  const problems = [];
  const ranA = [...next.values()].filter((r) => r.class === "A" && r.coverage === "ran");
  const rustfmt = ranA.filter((r) => r.id.startsWith("rustfmt"));
  if (rustfmt.length !== 1) problems.push(`${rustfmt.length} rustfmt rows ran, expected exactly 1 (${rustfmt.map((r) => r.id).join(", ")})`);
  for (const r of ranA) if (!r.id.startsWith("rustfmt") && !perFile.test(r.id)) problems.push(`${r.id}: ran but does not read the edited file`);
  // A row that ran because its input changed has a new key; a ran row that kept its key is an unexplained miss (and a row without a key cannot show a change).
  for (const r of ranA) {
    const before = prev.get(r.id);
    if (!before) problems.push(`${r.id}: ran but is absent from the previous run, so no changed key can be shown`);
    else if (r.key === null || before.key === r.key) problems.push(`${r.id}: ran with ${r.key === null ? "no cache key" : "the same key as the previous run"}: an edit changes the key of every row that reads it`);
  }
  for (const [id, row] of next) {
    if (row.class !== "A" || row.coverage === "skipped") continue;
    const before = prev.get(id);
    if (row.coverage === "hit" && before && before.key !== row.key) problems.push(`${id}: key changed but the row answered hit`);
    if (row.coverage === "hit" && !before) problems.push(`${id}: hit but absent from the previous run`);
  }
  return { ok: problems.length === 0, problems, ran: ranA.map((r) => r.id) };
}

const metadataLabel = (id) => (id === "cargo-metadata" ? "." : id.startsWith("cargo-metadata:") ? id.slice("cargo-metadata:".length) : null);

const packageListUnit = (id) => (id.startsWith("cargo-package-list:") ? id.slice("cargo-package-list:".length) : id === "cargo-package-list" ? "" : null);

/**
 * S4 (GC-A19): a manifest edit reruns `cargo-metadata` and `cargo-package-list` for the edited manifest's workspace ONLY. `workspace` is that
 * workspace's directory ("." for the root), `workspaces` every workspace directory of the commit and `crates` `{ workspaceDir: [crate names] }`.
 * The workspace's own `cargo-metadata` row must have run with a new key; any `cargo-metadata` or `cargo-package-list` row of another workspace that
 * ran, and any such row that cannot be attributed to a workspace, fails.
 */
function assertManifestEdit(prev, next, { workspace, workspaces = [], crates = {} } = {}) {
  const problems = [];
  if (typeof workspace !== "string") return { ok: false, problems: ["the edited manifest's workspace is not known"], ran: [] };
  const wsOfLabel = (label) => {
    if (workspaces.includes(label)) return label;
    const byCrate = workspaces.find((w) => w === `crates/${label}`);
    return byCrate ?? null;
  };
  const crateWs = (unit) => {
    const name = unit.includes(":") ? unit.slice(unit.lastIndexOf(":") + 1) : unit;
    const owners = Object.entries(crates).filter(([, names]) => names.includes(name)).map(([w]) => w);
    return owners.length === 1 ? owners[0] : null;
  };
  const ran = [];
  let ownMetadata = false;
  for (const row of next.values()) {
    const label = metadataLabel(row.id);
    const unit = packageListUnit(row.id);
    if (label === null && unit === null) continue;
    const ws = label !== null ? (workspaces.length === 0 && label === "." ? "." : wsOfLabel(label)) : crateWs(unit);
    const own = ws === workspace;
    if (row.coverage === "skipped" && row.status !== "not-in-pass") problems.push(`${row.id}: skipped (${row.reason}); a skipped row is never a pass`);
    if (row.coverage === "ran") {
      ran.push(row.id);
      if (ws === null) problems.push(`${row.id}: ran but belongs to no known workspace`);
      else if (!own) problems.push(`${row.id}: ran for workspace ${JSON.stringify(ws)}, which the edit (workspace ${JSON.stringify(workspace)}) does not touch`);
      else {
        const before = prev?.get(row.id);
        if (!before || before.key === row.key) problems.push(`${row.id}: ran without a changed key (${before ? "same key as before" : "absent before"})`);
      }
    }
    if (own && label !== null) {
      ownMetadata = true;
      if (row.coverage !== "ran") problems.push(`${row.id}: the edited workspace's cargo-metadata answered ${row.coverage}, expected ran`);
    }
    if (own && unit !== null && row.coverage === "hit") problems.push(`${row.id}: a cargo-package-list row of the edited workspace answered hit`);
  }
  if (!ownMetadata) problems.push(`no cargo-metadata row for workspace ${JSON.stringify(workspace)} exists in the run`);
  return { ok: problems.length === 0, problems, ran };
}

/** S9: two git listings taken at different times must be byte equal. */
function sameListing(name, a, b) {
  return a === b ? { ok: true, problems: [] } : { ok: false, problems: [`${name} changed: ${JSON.stringify(a).slice(0, 160)} -> ${JSON.stringify(b).slice(0, 160)}`] };
}

/** S9 and GC-A01: the one-worktree rule (`git worktree list --porcelain` has one `worktree ` row). */
const worktreeCount = (porcelain) => String(porcelain).split(/\r?\n/).filter((l) => l.startsWith("worktree ")).length;

/** GC-T20: no hook of the fixture's hooks directory (`.sample` files excepted) mentions the gate. `files` is `{ name: text }`. */
function hookProblems({ files, hooksPath = null }) {
  const problems = [];
  for (const [name, text] of Object.entries(files)) {
    if (name.endsWith(".sample")) continue;
    if (/rightkit-gate|rightkit-host-gate|rightkit_gate|gate\.mjs/i.test(String(text))) problems.push(`hook ${name} runs the gate`);
  }
  return { ok: problems.length === 0, problems };
}

/**
 * How to start a gate launcher. A `.cmd`/`.bat` launcher cannot be spawned directly on Windows (Node refuses it): it goes through `cmd.exe /d /s /c`
 * with verbatim arguments. Every argument and the launcher path must be free of cmd metacharacters (`" % ^ & | < > CR LF`); one that is not throws.
 */
function launchSpec(gate, args, { platform = process.platform, comspec = process.env.ComSpec || "cmd.exe" } = {}) {
  if (/\.mjs$/i.test(gate)) return { command: process.execPath, args: [gate, ...args], options: {} }; // as the host launcher starts a sealed gate: node <entry> <args>
  if (platform !== "win32" || !/\.(cmd|bat)$/i.test(gate)) return { command: gate, args, options: {} };
  const unsafe = /["%^&|<>\r\n]/;
  for (const a of [gate, ...args]) if (unsafe.test(a)) throw new Error(`argument ${JSON.stringify(a)} is not safe to pass through cmd.exe`);
  const quote = (a) => (a === "" || /\s/.test(a) ? `"${a}"` : a);
  return { command: comspec, args: ["/d", "/s", "/c", `"${[gate, ...args].map(quote).join(" ")}"`], options: { windowsVerbatimArguments: true } };
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// The run sequence (criteria.md section 3) and the clauses each step asserts.
// ---------------------------------------------------------------------------------------------------------------------------------------------
// ---------------------------------------------------------------------------------------------------------------------------------------------
// Non-negotiables NN01-NN19 (GATE-CONTRACT [GC-A27]). Adrian, 2026-11-05: "each implement like cache etc should be written into part of an e2e test i.e.
// final state. so anything that breaks it next time fails"; "no reruns, no repeat calls. no going over things that haven't changed. fully optimizations".
// `steps` are the S-steps that produce each one's evidence. A non-negotiable is never "unchecked" on a host that can run it: when its step ran, missing
// evidence is a failure with the reason; when its step was not selected (a `--steps` subset) it is "not-evaluated" and the verdict says the run is partial.
// ---------------------------------------------------------------------------------------------------------------------------------------------
const NN = Object.freeze([
  { id: "NN01", steps: ["S2"], title: "An unchanged commit runs no Class A row", evidence: "warm --to check and --to compile coverage, twice" },
  { id: "NN02", steps: ["S2"], title: "An unchanged commit runs no Class B row", evidence: "warm --to compile coverage, twice" },
  { id: "NN03", steps: ["S3"], title: "One edited .rs file reruns only the rows that read it", evidence: "S3 coverage" },
  { id: "NN04", steps: ["S4"], title: "One edited manifest reruns only its workspace's rows", evidence: "S4 coverage" },
  { id: "NN05", steps: ["S2"], title: "Keys are stable across identical runs", evidence: "coverage keys of two warm runs of check and of compile" },
  { id: "NN06", steps: ["S1", "S2"], title: "Only executing rows carry the environment digest", evidence: "coverage keys, the log's env digest" },
  { id: "NN07", steps: ["S1", "S2", "S3", "S4", "S10"], title: "No identical child process twice in one run", evidence: "spawn ledger" },
  { id: "NN08", steps: ["S1", "S2", "S3", "S4", "S10"], title: "rustfmt is one process per bucket; no cargo fmt", evidence: "spawn ledger, coverage" },
  { id: "NN09", steps: ["S1", "S2", "S3", "S4", "S10"], title: "Non-compiling cargo never goes through the broker", evidence: "spawn ledger" },
  { id: "NN10", steps: ["S2"], title: "Inside a snapshot the resident gate answers cargo metadata", evidence: "spawn ledger of the warm runs" },
  { id: "NN11", steps: ["S1", "S2"], title: "A warm run proves the snapshot from the resident index, without re-hashing it", evidence: "gate log (`snapshot proof:` line)" },
  { id: "NN12", steps: ["S2"], title: "Installed dependencies are identified by pnpm's install record and reused", evidence: "gate log (reuse-match=hit), spawn ledger, env digest" },
  { id: "NN13", steps: ["S1", "S2", "S3", "S4", "S10"], title: "Class A takes no exclusion of its own: one snapshot lock, no build-ownership lease", evidence: "gate log of every --to check" },
  { id: "NN14", steps: ["S2"], title: "Preflight and app rules run once per tree", evidence: "gate log of the warm runs" },
  { id: "NN15", steps: ["S2"], title: "Advice scans are cached per lock set and UTC day or hour", evidence: "warm coverage" },
  { id: "NN16", steps: ["S10"], title: "A unit list shares one Class A and runs no per-unit format check", evidence: "--units-file log, spawn ledger" },
  { id: "NN17", steps: ["S1", "S2"], title: "Class A lanes equal the host's cores; Class B batches compatible rows", evidence: "gate log, spawn ledger" },
  { id: "NN18", steps: ["S2"], title: "Warm time stays inside the host's budget", evidence: "run wall time and stamped log lines against ci/gate-acceptance-budgets.json" },
  { id: "NN19", steps: ["S0"], always: true, title: "The gate under test is the installed sealed gate", evidence: "--version, launcher" },
]);

/** NN20-NN43 (GC-A27 as extended 2026-11-05). `post` ones are judged from the finished result (NN34, NN35). */
const NN_EXT = Object.freeze([
  { id: "NN20", steps: ["S12"], title: "One edited crate recompiles only that crate and its dependents", evidence: "S12 crate-edit compile coverage" },
  { id: "NN21", steps: ["S12"], title: "One edited lockfile reruns only the rows that read it", evidence: "S12 lock-edit coverage" },
  { id: "NN22", steps: ["S2"], title: "A warm snapshot sync writes and removes nothing", evidence: "sync lines and spawn ledger of the warm runs" },
  { id: "NN23", steps: ["S13"], title: "A gate upgrade that changes no key-relevant code keeps every hit", evidence: "coverage of the previous and the newest sealed gate" },
  { id: "NN24", steps: ["S14"], title: "The cache survives the resident gate's restart and idle exit", evidence: "coverage of a run after the resident gate was killed" },
  { id: "NN25", steps: ["S2"], title: "A warm run starts no git-after-proof, version probe, pnpm, process probe or real cargo", evidence: "spawn ledger of the warm runs" },
  { id: "NN26", steps: ["S2"], title: "The gate asks the resident gate over its socket, in process", evidence: "spawn ledger" },
  { id: "NN27", steps: ["S1"], title: "Never two compiler passes over one crate, target and feature set", evidence: "S1 compile coverage" },
  { id: "NN28", steps: ["S2"], title: "Change detection comes from RightKit's file index and watcher", evidence: "gate log (change-source), spawn ledger" },
  { id: "NN29", steps: ["S16"], title: "Per-run identities never enter a key or a reuse record", evidence: "coverage keys of a run with different identities" },
  { id: "NN30", steps: ["S0"], always: true, title: "The releasing host passes (per host)", evidence: "this host's own run; another host's receipt is never required" },
  { id: "NN31", steps: ["S0"], always: true, title: "No gate is released or installed as trusted without a passing contract test", evidence: "release and install tooling calls --verify-release" },
  { id: "NN32", steps: ["S8"], title: "A killed run's snapshot lock is reclaimed by the next run on both hosts", evidence: "S8 kill cases" },
  { id: "NN33", steps: ["S0"], always: true, title: "The contract and the test match", evidence: "docs/GATE-CONTRACT.md NN table" },
  { id: "NN34", steps: ["S0"], always: true, post: true, title: "Missing evidence fails; a partial run never passes", evidence: "the finished result" },
  { id: "NN35", steps: ["S0"], always: true, post: true, title: "The verdict shows coverage, and a release needs all of it", evidence: "the finished result, receipt" },
  { id: "NN36", steps: ["S0"], always: true, title: "The test is locked", evidence: "git history of the runner, the contract table, the budgets" },
  { id: "NN37", steps: ["S0"], always: true, title: "The test's author is not the implementer", evidence: "budgets.reviews" },
  { id: "NN38", steps: ["S1"], title: "Cross-target rows built with stand-ins are limited evidence", evidence: "coverage and receipt of the compile" },
  { id: "NN39", steps: ["S11"], title: "The parallelism is real, not configured", evidence: "spawn ledger start and end times of the cold runs" },
  { id: "NN40", steps: ["S2"], title: "Docs build as one command per workspace and feature round, on every compile", evidence: "spawn ledger and doc coverage rows of every compile run" },
  { id: "NN41", steps: ["S17"], title: "A deterministic failure is cached like a pass", evidence: "coverage of the failing commit run three times (same inputs twice, then a changed toolchain identity)" },
  { id: "NN42", steps: ["S2"], title: "Cross targets of one stage go in one broker command naming every --target", evidence: "spawn ledger and log of every compile run" },
  { id: "NN43", steps: ["S10"], title: "A unit-list run admits grouped rows by lockstep, not by whole-unit order", evidence: "S10 unit-list compile coverage (grouped rows) and log solo notes; per-unit receipts for blocked-whole" },
]);

const NN_ALL = Object.freeze([...NN, ...NN_EXT]);

const NN_ALL_IDS = Object.freeze(NN_ALL.map((n) => n.id));

const LADDER = Object.freeze(["check", "compile", "test", "render", "dev", "package", "qualify"]);

const RECEIPT_STAGES = new Set(["check", "compile", "package", "qualify"]);

// ---------------------------------------------------------------------------------------------------------------------------------------------
// Clause assertions. Each CHECKS entry is `(ctx) => { status: "pass"|"fail"|"unchecked", evidence: [..], problems: [..] }` or a list of them. `ctx.runs`
// holds the runs of the sequence by label; a clause needing a run that is absent is "unchecked". The summary of a clause across checks is: any fail
// -> fail; else all real checks pass and no blocking "missing" part -> pass; else unchecked.
// ---------------------------------------------------------------------------------------------------------------------------------------------
const verdict = (res, evidence) => ({ status: res.ok ? "pass" : "fail", evidence, problems: res.problems });

const unchecked = (why) => ({ status: "unchecked", evidence: [], problems: [why] });

/** An unchecked part that keeps the clause from passing although other parts passed. */
const missing = (why) => ({ status: "unchecked", blocksPass: true, evidence: [], problems: [why] });

const needRun = (ctx, label) => ctx.runs[label] ?? null;

/** A run whose nonzero exit is the demanded outcome (a negative, a judged refusal, or an S17 cached-failure leg): clause checks judge its evidence, never its exit. */
const expectedFailure = (r) => Boolean(r.negative) || r.refusal !== undefined || r.expectedFailure === true;

const firstRows = (ctx, ...labels) => { for (const l of labels) if (ctx.runs[l]?.rows) return ctx.runs[l]; return null; };

/**
 * The running broker's `{ pid, startedMs }` for GC-I15 (S0 and S9 compare it), or null when it cannot be established (fail safe). Mac: exactly one of the two launchd
 * entries the broker alternates between at a cutover (`dev.rustright.broker`, `dev.rustright.broker.b`) is running. Windows: exactly one `rustright-broker` process.
 * Linux has no broker. `run` and `uid` are test seams.
 */
function runningBrokerProcess({ platform = process.platform, run = spawnSync, uid = process.getuid?.() } = {}) {
  if (platform === "win32") {
    try {
      const script = "$p = @(Get-Process -Name rustright-broker -ErrorAction SilentlyContinue); if ($p.Count -eq 1) { \"$($p[0].Id) $($p[0].StartTime.ToUniversalTime().ToString('o'))\" }";
      const r = run("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")], { encoding: "utf8", timeout: 15000, windowsHide: true });
      const m = /^(\d+) (\S+)\s*$/m.exec(String(r.stdout ?? ""));
      if (!m) return null;
      const pid = Number(m[1]); const startedMs = Date.parse(m[2]);
      return Number.isInteger(pid) && pid > 0 && Number.isFinite(startedMs) ? { pid, startedMs } : null;
    } catch { return null; }
  }
  if (platform !== "darwin") return null;
  try {
    const pids = ["dev.rustright.broker", "dev.rustright.broker.b"]
      .map((label) => Number(/^\s*pid = (\d+)$/m.exec(String(run("launchctl", ["print", `gui/${uid}/${label}`], { encoding: "utf8", timeout: 5000 }).stdout ?? ""))?.[1]))
      .filter((pid) => Number.isInteger(pid) && pid > 0);
    if (pids.length !== 1) return null;
    const pid = pids[0];
    const ps = run("ps", ["-o", "lstart=", "-p", String(pid)], { encoding: "utf8", timeout: 5000, env: { ...process.env, LC_ALL: "C", LANG: "C" } });
    const startedMs = Date.parse(String(ps.stdout ?? "").trim());
    return Number.isFinite(startedMs) ? { pid, startedMs } : null;
  } catch { return null; }
}

/** GC-I15: the broker process (pid and start) is the same at S0 and S9; the gate launched none. */
function brokerUnchanged(a, b) {
  if (!a || !b) return { ok: false, problems: ["the broker process was not established at S0 or at S9"] };
  return a.pid === b.pid && a.startedMs === b.startedMs ? { ok: true, problems: [] } : { ok: false, problems: [`broker pid/start changed: ${JSON.stringify(a)} -> ${JSON.stringify(b)}`] };
}

/** GC-T19: the check stage's recorded rule tier is the tier the stage owes (compile below qualify, release at qualify). */
function ruleTierProblems(receipt, stage) {
  const tier = receipt?.stages?.check?.ruleTier;
  const want = stage === "qualify" || stage === "release" ? "release" : "compile";
  return tier === want ? { ok: true, problems: [] } : { ok: false, problems: [`receipt.stages.check.ruleTier is ${JSON.stringify(tier)}, expected ${JSON.stringify(want)} for --to ${stage}`] };
}

const CONTRACT_FILE = path.resolve(here, "..", "..", "docs", "GATE-CONTRACT.md");

// ---- NN19 / NN30 / NN31: the receipt ---------------------------------------------------------------------------------------------------------------
const CONTRACT_RECEIPT_SCHEMA = "rightkit.contract-test-receipt/1";

const validWaiver = (w) => isObject(w) && typeof w.clause === "string" && /^GC-/.test(w.clause) && w.by === "Adrian" && typeof w.words === "string" && w.words.trim().length > 0;

/** `--verify-release`: the receipt of the host that releases or installs passes, for the gate commit and sealed gate, with the contract and runner it was
 * judged by. Per host (Adrian 2026-11-07: "why would both machines be tied to each other in such a way for anything?"): one host's receipt never
 * stands for or blocks the other. `b` is optional; when given, each receipt is judged on its own. */
// The same bindings the installer's promotion makes (rustright-install trusted-gate promote, GB-98), so the two checks cannot disagree about a receipt:
// `budgetsSha256` the budgets it was judged against, `runnerFiles` every file of the runner ([{ name, sha256 }], exact set), `brokerRuntimeDigest` the
// broker installed on this host. Each is compared when given; the command line always gives all of them.
// `archive`: { sha256, integrity } of a package archive (archiveIdentity). When given, the receipt must be a candidate's and must name exactly that
// archive: this is the comparison a publication makes between the bytes it is about to publish and the bytes that were tested.
function verifyReleaseReceipts(a, b = null, { commit = null, contractSha256 = null, runnerSha256 = null, waivers = [], host = null, sealedSha256 = null, budgetsSha256 = null, runnerFiles = null, brokerRuntimeDigest = null, archive = null } = {}) {
  const problems = [];
  if (host && a?.host !== host) problems.push(`the receipt is from ${a?.host ?? "no host"}, this host is ${host}`);
  for (const [name, r] of (b ? [["first", a], ["second", b]] : [["the", a]])) {
    const w = `${name} receipt (${r?.host})`;
    if (!isObject(r) || r.schema !== CONTRACT_RECEIPT_SCHEMA) { problems.push(`${w}: not a ${CONTRACT_RECEIPT_SCHEMA}`); continue; }
    if (r.ok !== true || !/^PASS /.test(String(r.verdict))) problems.push(`${w}: verdict ${JSON.stringify(r.verdict)} is not a PASS`);
    if (!isCommit(r.commit)) problems.push(`${w}: commit is not a 40-hex sha`);
    // A candidate's receipt is what promotion keeps (NN31: tested as a candidate, then promoted), so both kinds are a sealed gate's receipt.
    if (!["installed", "candidate"].includes(r.gate?.kind) || !isSha(r.gate?.sealedSha256)) problems.push(`${w}: not a sealed gate's receipt (kind ${JSON.stringify(r.gate?.kind)})`);
    // The receipt must be for the very gate being released or installed, not for some other gate of the same commit.
    else if (sealedSha256 !== null && r.gate.sealedSha256 !== sealedSha256) problems.push(`${w}: it is for the sealed gate ${r.gate.sealedSha256}, the gate in question is ${sealedSha256}`);
    for (const id of Object.keys(r.nn ?? {})) if (r.nn[id] !== "pass") problems.push(`${w}: ${id} is ${r.nn[id]}`);
    // The exact set, by name: a count let a receipt with other or fewer rules through (the list had grown to 42 while this line still said 38).
    const have = Object.keys(isObject(r.nn) ? r.nn : {}); const missing = NN_ALL_IDS.filter((id) => !have.includes(id)); const unknown = have.filter((id) => !NN_ALL_IDS.includes(id));
    if (missing.length || unknown.length) problems.push(`${w}: its non-negotiables are not the contract's ${NN_ALL_IDS.length}${missing.length ? `; missing ${missing.join(", ")}` : ""}${unknown.length ? `; unknown ${unknown.join(", ")}` : ""}`);
    // The list must be there, as the installer's promotion requires: a receipt that leaves it out is not read as "nothing unchecked".
    if (!Array.isArray(r.uncheckedClauses)) problems.push(`${w}: it does not state which operative clauses were left unchecked (uncheckedClauses is missing)`);
    const open = (Array.isArray(r.uncheckedClauses) ? r.uncheckedClauses : []).filter((id) => !waivers.some((x) => validWaiver(x) && x.clause === id));
    if (open.length) problems.push(`${w}: ${open.length} operative clause(s) unchecked and not waived by Adrian by name (${open.slice(0, 8).join(", ")}${open.length > 8 ? ", ..." : ""})`);
  }
  for (const r of (b ? [a, b] : [a]).filter(isObject)) {
    if (commit && r.commit !== commit) problems.push(`the ${r.host} receipt is for commit ${r.commit}, the release is ${commit}`);
    if (contractSha256 && r.contractSha256 !== contractSha256) problems.push(`the contract changed after the ${r.host} receipt was written`);
    if (runnerSha256 && r.runnerSha256 !== runnerSha256) problems.push(`the runner changed after the ${r.host} receipt was written`);
    if (budgetsSha256 && r.budgetsSha256 !== budgetsSha256) problems.push(`the ${r.host} receipt was judged against other budgets (${r.budgetsSha256 ?? "none stated"})`);
    if (runnerFiles) {
      const got = new Map((Array.isArray(r.runnerFiles) ? r.runnerFiles : []).filter(isObject).map((f) => [f.name, f.sha256])); const want = new Map(runnerFiles.map((f) => [f.name, f.sha256]));
      const missing = [...want.keys()].filter((n) => !got.has(n)); const extra = [...got.keys()].filter((n) => !want.has(n)); const differ = [...want.keys()].filter((n) => got.has(n) && got.get(n) !== want.get(n));
      if (missing.length || extra.length || differ.length) problems.push(`the ${r.host} receipt's runner files are not this runner's${missing.length ? `; not listed: ${missing.join(", ")}` : ""}${extra.length ? `; not part of this runner: ${extra.join(", ")}` : ""}${differ.length ? `; changed: ${differ.join(", ")}` : ""}`);
    }
    if (archive) {
      if (!isSha(r.gate?.archiveSha256) || typeof r.gate?.archiveIntegrity !== "string") problems.push(`the ${r.host} receipt does not say which archive its gate was installed from, so it cannot be matched to the archive in question`);
      else if (r.gate.archiveSha256 !== archive.sha256 || r.gate.archiveIntegrity !== archive.integrity) problems.push(`the ${r.host} receipt is for the archive ${r.gate.archiveSha256}, the archive in question is ${archive.sha256}`);
    }
    if (brokerRuntimeDigest && r.broker?.runtimeDigest !== brokerRuntimeDigest) problems.push(`the ${r.host} receipt was made with the broker ${r.broker?.runtimeDigest ?? "(not stated)"}, this host runs ${brokerRuntimeDigest}`);
  }
  return { ok: problems.length === 0, problems };
}

/**
 * The launches that cannot be served from a cache, so a run that really happened shows broker jobs in them: the two S12 compiles of commits
 * the runner has just made. S11, the run with the cache moved aside, is NOT one: by design it stops for approval before anything runs and
 * sends no broker job. Sealed with the gate, with what the broker's signed claim for each must say (contract-nn.json `signedLaunches`). `label` is the launch's
 * step label as the nonce carries it; `commands`: the stage words of which at least one job must have run in it; `exitCode`: the expected
 * result of every job of the launch; `commit`: "any" means the runner made the commit for that launch (an edit): every job of the launch must
 * name one same 40-hex commit, and it must not be the fixture commit the receipt states.
 */
const CONTRACT_SIGNED_LAUNCHES = Object.freeze([
  Object.freeze({ label: "S12:crate", commands: Object.freeze(["clippy", "test", "build", "check", "doc"]), exitCode: 0, commit: "any" }),
  Object.freeze({ label: "S12:manifest", commands: Object.freeze(["clippy", "test", "build", "check", "doc"]), exitCode: 0, commit: "any" }),
]);

/**
 * Which package archive a sealed gate folder was installed from, worked out from the seal itself and not from anyone's statement.
 * The seal is the sha256 of the folder's manifest (one line per file: path, a tab, kind, a tab, "-", a tab, sha256). The installer writes
 * `trusted-gate-source.json` at the folder's root before sealing, so the manifest has a line for it. So: the manifest must hash to `sealedSha256`;
 * it must list that file exactly once; `sourceBytes` must hash to the listed value; and the record must be for @rightkit/release.
 * Returns { ok, problems, archive: { sha256, integrity, version } | null }.
 */
function archiveOfSeal({ sealedSha256, manifestBytes, sourceBytes }) {
  const problems = [];
  if (!isSha(sealedSha256)) problems.push("no seal to check the manifest against");
  else if (sha256(manifestBytes) !== sealedSha256) problems.push(`the manifest handed over hashes to ${sha256(manifestBytes)}, the tested gate's seal is ${sealedSha256}`);
  const lines = String(manifestBytes).split("\n").filter((l) => l.split("\t")[0] === "./trusted-gate-source.json");
  if (lines.length !== 1) problems.push(`the manifest lists trusted-gate-source.json ${lines.length} time(s); a gate sealed with its source record lists it once`);
  const [, kind, , listed] = (lines[0] ?? "").split("\t");
  if (lines.length === 1 && (kind !== "file" || !isSha(String(listed ?? "").trim()))) problems.push("the manifest's line for trusted-gate-source.json is not a file with a sha256");
  else if (lines.length === 1 && sha256(sourceBytes) !== String(listed).trim()) problems.push("the source record handed over is not the one under the seal (sha256 differs from the manifest's line)");
  let src = null; try { src = JSON.parse(String(sourceBytes)); } catch { problems.push("the source record is not JSON"); }
  if (src && (src.schema !== "rustright.trusted-gate-source/1" || src.package !== "@rightkit/release" || !isSha(src.sha256) || typeof src.integrity !== "string" || !/^sha512-[A-Za-z0-9+/]+=*$/.test(src.integrity) || typeof src.version !== "string")) { problems.push("the source record is not a rustright.trusted-gate-source/1 for @rightkit/release with a sha256 and an integrity"); src = null; }
  return { ok: problems.length === 0, problems, archive: problems.length === 0 ? { sha256: src.sha256, integrity: src.integrity, version: src.version } : null };
}

/** The identity of a package archive as a receipt states it: sha256 of its bytes (hex) and the npm integrity (sha512, base64). */
function archiveIdentity(bytes) {
  return { sha256: createHash("sha256").update(bytes).digest("hex"), integrity: `sha512-${createHash("sha512").update(bytes).digest("base64")}` };
}

const bad = (label, problems, evidence = []) => ({ label, ok: false, problems, evidence });

const CHECKS = {
  "GC-A21": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.ladder) continue;
      if (run.args.includes("--route")) continue;
      if (run.exit !== 0 && !expectedFailure(run)) out.push({ label, status: "fail", evidence: [run.logPath], problems: [`the run exited ${run.exit}${run.timedOut ? " (timed out)" : ""}`] });
      const stamps = checkUtcStamps(run.log);
      out.push({ label, ...verdict(stamps, [`${run.logPath} (every line)`]) });
      if (run.exit === 0 && ["check", "compile", "package", "qualify"].includes(run.stage)) {
        // A reused stage prints `[stage] already passed ...` instead of a timing line; only a stage that ran owes `passed in`.
        const reused = new RegExp(`\\[${run.stage}\\] already passed`).test(run.log);
        const timings = stageTimings(run.log).filter((t) => t.stage === run.stage);
        if (!reused || timings.length) out.push({ label, ...verdict({ ok: timings.length > 0 && timings.every((t) => t.ok), problems: timings.length ? [] : [`no "[${run.stage}] passed in" line`] }, [`${run.logPath} ([${run.stage}] passed in)`]) });
      }
      if (run.coverageProblems.length) out.push({ label, status: "fail", evidence: run.coveragePaths, problems: run.coverageProblems });
      else if (run.coveragePaths.length) out.push({ label, status: "pass", evidence: run.coveragePaths, problems: [] });
      else if (run.exit === 0 && run.stage !== "render") out.push({ label, status: "fail", evidence: [run.logPath], problems: ["the log names no coverage report"] });
    }
    return out.length ? out : unchecked("no ladder run executed");
  },
  "GC-A04": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receiptPath || expectedFailure(run)) continue;
      if (run.exit !== 0) { out.push({ label, status: "fail", evidence: [run.logPath], problems: [`the run exited ${run.exit}${run.timedOut ? " (timed out)" : ""}: no receipt can stand for it`] }); continue; }
      if (!run.receipt) { out.push({ label, status: "fail", evidence: [run.receiptPath], problems: ["the run exited 0 but wrote no readable receipt (--receipt-out)"] }); continue; }
      const stages = LADDER.filter((s) => RECEIPT_STAGES.has(s) && run.stageIndex >= LADDER.indexOf(s)).filter((s) => run.receipt.stages?.[s]);
      out.push({ label, ...verdict(validateDoorReceipt(run.receipt, { commit: run.commit, expectedTree: run.tree, stages }), [run.receiptPath]) });
    }
    if (ctx.s6) out.push(ctx.s6.fresh && ctx.s6.proof ? { label: "S6", ...verdict(ctx.s6.proof, ["the S6 snapshot file vs the commit's blob, the checkout's dirty file and its source identity"]) } : unchecked(`S6: ${ctx.s6.why ?? "the dirty-checkout proof was not made"}`));
    return out.length ? out : unchecked("no run wrote a receipt (--receipt-out)");
  },
  "GC-T05": (ctx) => {
    const out = []; let reused = 0;
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receipt || run.exit !== 0 || !run.receipt.stages?.compile || expectedFailure(run)) continue;
      const r = classABeforeCompile(run.receipt, run.log);
      if (r.skipped) { reused += 1; continue; }
      out.push({ label, ...verdict(r, [`${run.receiptPath} (stages.compile.at), ${run.logPath} ([check] passed in stamp)`]) });
    }
    for (const [label, run] of Object.entries(ctx.runs)) if (run.negative && ["N01", "N02"].includes(run.negative)) out.push({ label, ...verdict(run.negativeResult, [run.logPath]) });
    return out.length ? out : unchecked(`no compile ran in a run of its own${reused ? ` (${reused} run(s) reused a recorded compile) ` : ""} and no Class A negative ran`);
  },
  "GC-A15": (ctx) => {
    const run = [...Object.values(ctx.runs)].reverse().find((r) => r.receipt && r.exit === 0 && !expectedFailure(r) && Object.values(r.receipt.stages ?? {}).some((s) => s?.outputs));
    const out = [];
    if (run) out.push({ status: outputDirsProblems(run.receipt).ok ? "pass" : "fail", evidence: [run.receiptPath], problems: outputDirsProblems(run.receipt).problems });
    const n15 = Object.values(ctx.runs).find((r) => r.negative === "N15");
    if (n15) out.push({ ...verdict(n15.negativeResult, [n15.logPath]) });
    return out.length ? out : unchecked("no receipt records stage outputs");
  },
  "GC-A23": (ctx) => {
    // Read right after each run (the snapshot is re-synced by every later step), never at the end.
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.sourceIdentity) continue;
      const si = run.sourceIdentity;
      if (si.error) { out.push({ label, status: "fail", evidence: [si.file], problems: [si.error] }); continue; }
      out.push({ label, ...verdict(validateSourceIdentity(si.identity, { commit: run.commit, trees: si.trees }), [`${si.file} (read right after ${label})`, "git rev-parse <commit>:<dir>"]) });
    }
    return out.length ? out : unchecked("no run produced a receipt that names a snapshot path");
  },
  "GC-A22-render": (ctx) => {
    const run = needRun(ctx, "S1:render");
    if (!run) return unchecked("--to render was not run");
    if (run.exit !== 0) return { status: "fail", evidence: [run.logPath], problems: [`--to render exited ${run.exit}`] };
    const dir = (/(?:render(?:ed)?|stored|wrote|receipt)[^\n]*?(\S*rightkit-gate-render\S*)/.exec(run.log) ?? [])[1]?.replace(/[,.;)]+$/, "");
    if (!dir) return { status: "fail", evidence: [run.logPath], problems: ["the log names no rightkit-gate-render directory"] };
    const file = path.join(dir.endsWith(".json") ? path.dirname(dir) : dir, "render-receipt.json");
    let receipt;
    try { receipt = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { return { status: "fail", evidence: [file], problems: [`cannot read ${file}: ${e.message}`] }; }
    return verdict(validateRenderReceipt(receipt, { commit: run.commit, png: (rel) => { try { return fs.readFileSync(path.join(path.dirname(file), rel)); } catch { return null; } } }), [file]);
  },
  "GC-I22": (ctx) => {
    const dir = path.join(here, "test-support", "vectors");
    let source; const files = {};
    try { source = JSON.parse(fs.readFileSync(path.join(dir, "vectors-source.json"), "utf8")); for (const n of fs.readdirSync(dir)) if (n.endsWith(".json")) files[n] = fs.readFileSync(path.join(dir, n)); } catch (e) { return { status: "fail", evidence: [dir], problems: [e.message] }; }
    const up = ctx.rustrightContracts;
    let upstream = null;
    // GC-I22 pins copies of RustRight's COMMITTED blobs; the working tree's bytes are the checkout's (CRLF on Windows), never the pin's.
    if (up && fs.existsSync(up)) { upstream = {}; const repoRoot = path.resolve(up, "..", ".."); for (const n of Object.keys(source.files ?? {})) { const pin = source.files[n]?.commit ?? source.source?.commit; try { if (pin) upstream[n] = git(repoRoot, ["cat-file", "blob", `${pin}:docs/contracts/${n}`], { buffer: true }); } catch { /* reported as "no copy" by vectorsProblems */ } } }
    return verdict(vectorsProblems({ source, files: { ...files }, upstream, upstreamDir: up }), [path.join(dir, "vectors-source.json"), up ?? "RustRight docs/contracts"]);
  },
  "GC-I18": (ctx) => {
    const m = ctx.broker.manifest; const p = ctx.broker.pins;
    if (!m || !p) return { status: "fail", evidence: [ctx.broker.manifestPath ?? "runtime-manifest.json", ctx.broker.pinsPath ?? "pins/broker-<platform>.json"], problems: [ctx.broker.problem ?? "runtime manifest or pins record unreadable"] };
    return verdict(brokerDigestProblems({ manifest: m, pins: p }), [ctx.broker.manifestPath, ctx.broker.pinsPath]);
  },
  "GC-I01": (ctx) => {
    const key = repositoryKey(ctx.git.canonical, ctx.platform);
    const dir = path.join(ctx.managedRoot, "snapshots", key);
    const out = [];
    out.push({ ...verdict({ ok: fs.existsSync(dir), problems: fs.existsSync(dir) ? [] : [`${dir} does not exist`] }, [dir]) });
    const run = Object.values(ctx.runs).find((r) => r.receipt?.stages?.check?.snapshot?.path);
    if (run) { const sp = run.receipt.stages.check.snapshot.path; const ok = path.resolve(sp).startsWith(path.resolve(dir)); out.push({ ...verdict({ ok, problems: ok ? [] : [`receipt snapshot path ${sp} is not under ${dir}`] }, [run.receiptPath]) }); }
    return out;
  },
  "GC-A01": (ctx) => {
    const out = [];
    const n13 = Object.values(ctx.runs).filter((r) => r.negative === "N13");
    for (const r of n13) out.push({ ...verdict(r.negativeResult, [r.logPath]) });
    if (ctx.s9) {
      const wt = worktreeCount(ctx.s9.worktree);
      out.push({ ...verdict({ ok: wt === 1, problems: wt === 1 ? [] : [`${wt} worktrees listed, expected 1`] }, ["git worktree list --porcelain"]) });
      out.push({ ...verdict(ctx.s9.snapshotDirCheck, [path.join(ctx.managedRoot, "snapshots")]) });
    }
    return out.length ? out : unchecked("neither N13 nor S9 ran");
  },
  "GC-T14": (ctx) => CHECKS["GC-A01"](ctx),
  "GC-A02": (ctx) => {
    const out = [];
    if (ctx.s9) out.push({ ...sameVerdict(sameListing("git stash list", ctx.s0?.stash, ctx.s9.stash), ["git stash list at S0 and S9"], ctx.s0) });
    if (ctx.s6?.stash) out.push({ ...verdict(ctx.s6.stash, ["git stash list before and after S6"]) });
    if (ctx.s6?.restore) out.push({ ...verdict(ctx.s6.restore, ["the S6 checkout restore"]) });
    return out.length ? out : unchecked("S6 and S9 did not run");
  },
  "GC-T06": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) if (run.recovery) out.push({ label, ...verdict(run.recovery, [run.logPath, run.lockPath ?? "snapshot.lock"]) });
    if (ctx.s6?.fresh && ctx.s6.proof) out.push({ label: "S6", ...verdict(ctx.s6.proof, ["S6: a dirty tree and an unpushed commit build the commit's bytes"]) });
    if (ctx.s9) out.push({ ...verdict(ctx.s9.lockClean, [ctx.s9.lockPath]) });
    return out.length ? out : unchecked("S6, S8 and S9 did not run");
  },
  "GC-A19": (ctx) => {
    const out = [];
    const s4 = needRun(ctx, "S4:check");
    const base = firstRows(ctx, "S2:check", "S1:check");
    if (s4?.rows && base && s4.manifestInfo) out.push({ ...verdict(assertManifestEdit(base.rows, s4.rows, s4.manifestInfo), [...base.coveragePaths, ...s4.coveragePaths]) });
    const s2 = needRun(ctx, "S2:check");
    if (s2?.rows) {
      const aud = [...s2.rows.values()].filter((r) => /^(cargo-deny|cargo-audit|pnpm-audit|ghsa-audit)/.test(r.id));
      const bad = aud.filter((r) => r.coverage === "ran");
      out.push({ ...verdict({ ok: aud.length > 0 && bad.length === 0, problems: aud.length === 0 ? ["no audit rows in the warm rerun"] : bad.map((r) => `${r.id}: ran on an unchanged commit (key ${r.key})`) }, s2.coveragePaths) });
    }
    return out.length ? out : unchecked("S2 and S4 did not run");
  },
  "GC-A24": (ctx) => {
    const out = [];
    const s1 = needRun(ctx, "S1:check"); const s2 = needRun(ctx, "S2:check"); const s3 = needRun(ctx, "S3:check");
    if (s1?.rows && s2?.rows) out.push({ ...verdict(assertWarmRerun(s1.rows, s2.rows), [...s1.coveragePaths, ...s2.coveragePaths]) });
    if (s2?.rows && s3?.rows) out.push({ ...verdict(assertOneFileEdit(s2.rows, s3.rows), [...s2.coveragePaths, ...s3.coveragePaths]) });
    for (const [label, run] of Object.entries(ctx.runs)) if (run.kill === "K02") out.push({ label, ...verdict(run.recovery, [run.logPath]) });
    return out.length ? out : unchecked("S1/S2/S3 coverage pairs are missing");
  },
  "GC-T04": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receipt || run.exit !== 0 || expectedFailure(run)) continue;
      const problems = [];
      if (run.receipt.binding?.executor !== "broker") problems.push(`receipt.binding.executor is ${JSON.stringify(run.receipt.binding?.executor)}, expected "broker"`);
      if ("brokerException" in run.receipt) problems.push("receipt.brokerException is set");
      out.push({ label, ...verdict({ ok: problems.length === 0, problems }, [`${run.receiptPath} (binding.executor)`]) });
    }
    return out.length ? out : unchecked("no run wrote a receipt");
  },
  "GC-T19": (ctx) => {
    const out = []; let release = false;
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.ladder || expectedFailure(run) && run.refusal === undefined) continue;
      if (run.refusal !== undefined) { if (run.refusal.ok) { release = true; out.push({ label, status: "pass", evidence: [run.logPath], problems: [] }); } else out.push({ label, ...verdict(run.refusal, [run.logPath]) }); continue; }
      if (!run.receipt || run.exit !== 0 || !run.receipt.stages?.check) continue;
      if (run.stage === "qualify") release = true;
      out.push({ label, ...verdict(ruleTierProblems(run.receipt, run.stage), [`${run.receiptPath} (stages.check.ruleTier)`]) });
    }
    if (!out.length) return unchecked("no run recorded a check stage");
    if (!release) out.push(missing(ctx.allowInstall ? "no --to qualify run: the release-tier half is unchecked" : `release-tier half (--to qualify): ${NEEDS_INSTALL_APPROVAL}`));
    return out;
  },
  "GC-A22-classA": (ctx) => {
    const src = firstRows(ctx, "S1:check", "S1:compile", "S2:check");
    const out = [];
    if (src) {
      const find = (re) => [...src.rows.values()].filter((r) => re.test(r.id));
      for (const [name, re, need] of [["ghsa-audit", /^ghsa-audit(:|$)/, true], ["secret-scan", /^secret-scan$/, true], ["feature-coverage", /^feature-coverage$/, false]]) {
        const rows = find(re);
        if (!rows.length) { out.push(need ? { status: "fail", evidence: src.coveragePaths, problems: [`no ${name} row in the coverage report`] } : missing(`no ${name} row: the fixture has no crate with declared features (${NEEDS_FIXTURE})`)); continue; }
        const bad = rows.filter((r) => !(["hit", "ran"].includes(r.coverage) && r.status === "pass")).map((r) => `${r.id}: ${r.coverage}/${r.status}${r.reason ? ` (${r.reason})` : ""}`);
        out.push({ status: bad.length ? "fail" : "pass", evidence: src.coveragePaths, problems: bad });
      }
    } else out.push(unchecked("no coverage report (S1 check did not run)"));
    const n02 = Object.values(ctx.runs).find((r) => r.negative === "N02");
    if (n02) out.push({ ...verdict(n02.negativeResult, [n02.logPath]) });
    return out;
  },
  "GC-I03": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receipt || run.exit !== 0 || expectedFailure(run)) continue;
      const problems = [];
      for (const stage of ["check", "compile"]) {
        const sn = run.receipt.stages?.[stage]?.snapshot;
        if (stage === "check" && !run.receipt.stages?.check) { problems.push("receipt.stages.check is missing"); continue; }
        if (run.receipt.stages?.[stage] === undefined) continue;
        if (!isObject(sn)) problems.push(`receipt.stages.${stage}.snapshot is missing`);
        else { if (sn.tree !== run.tree) problems.push(`${stage}: snapshot tree ${sn.tree} != commit^{tree} ${run.tree}`); if (sn.intactAfter !== true) problems.push(`${stage}: snapshot.intactAfter is ${JSON.stringify(sn.intactAfter)}`); }
      }
      out.push({ label, ...verdict({ ok: problems.length === 0, problems }, [`${run.receiptPath} (door half: snapshot tree at stage exit vs commit^{tree})`]) });
    }
    return out.length ? out : unchecked("no run wrote a receipt");
  },
  "GC-I05": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receipt || run.exit !== 0 || expectedFailure(run) || !run.receiptPath) continue;
      const stages = LADDER.filter((s) => RECEIPT_STAGES.has(s) && run.stageIndex >= LADDER.indexOf(s)).filter((s) => run.receipt.stages?.[s]);
      out.push({ label, ...verdict(validateDoorReceipt(run.receipt, { commit: run.commit, expectedTree: run.tree, stages }), [`${run.receiptPath} (door half: no verdict/refreshed, strict fields; the broker's receipt fields are not read)`]) });
    }
    return out.length ? out : unchecked("no run wrote a receipt");
  },
  "GC-I15": (ctx) => {
    if (!ctx.s0 || !ctx.s9) return unchecked("S0 and S9 did not both run");
    return verdict(brokerUnchanged(ctx.s0.broker, ctx.s9.broker), ["runningBrokerProcess() at S0 and at S9 (pid and start time)"]);
  },
  "GC-T20": (ctx) => (ctx.hooks ? verdict(hookProblems(ctx.hooks), [`${ctx.hooks.dir} (the hooks half only; the no-gate-process-on-commit half is not run)`]) : unchecked("the fixture's hooks were not read")),
};

const sameVerdict = (res, evidence, s0) => (s0 ? verdict(res, evidence) : unchecked("S0 listing missing"));

function git(cwd, args, { env = {}, input, buffer = false } = {}) {
  const r = spawnSync("git", args, { cwd, encoding: buffer ? "buffer" : "utf8", env: { ...process.env, ...env }, input, maxBuffer: 1 << 28 });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${String(r.stderr || r.stdout || "").trim() || `exit ${r.status}`}`);
  return r.stdout;
}

let liveCandidateOutput = false;

function launch({ gate, args, cwd, env }) {
  const spec = launchSpec(gate, args);
  const child = spawn(spec.command, spec.args, { cwd, env, stdio: ["ignore", "pipe", "pipe"], detached: process.platform !== "win32", windowsHide: true, ...spec.options });
  if (liveCandidateOutput) {
    child.stdout.pipe(process.stdout, { end: false });
    child.stderr.pipe(process.stdout, { end: false });
  }
  return child;
}

export { CONTRACT_FILE, archiveIdentity, archiveOfSeal, validWaiver, verifyReleaseReceipts, NN_ALL_IDS, CONTRACT_RECEIPT_SCHEMA, CONTRACT_SIGNED_LAUNCHES };
