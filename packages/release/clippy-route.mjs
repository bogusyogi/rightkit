// The Clippy route: `rightkit-gate clippy [<cargo clippy args>]` (GATE-CONTRACT §1 "Clippy route & batching", E13 to E15; item 8 of
// the joint plan, rows 8.1 to 8.16 of the signed map). A gate-owned destination for a request such as
// `cargo clippy --workspace --all-targets -- -D warnings`: it builds HEAD's commit (never the checkout) from the gate-owned
// snapshot, under the host's build ownership, after FULL class A (D8-2), and stops after diagnostics. It has no stage chain, runs no
// package, sign, publish or release stage, and writes nothing a door receipt, qualification or the full gate could take as stage
// evidence (row 8.5).
//
//   argv          cargo's own arguments, verbatim. The gate adds only `--message-format=json`, `--keep-going` (unless present) and
//                 `--locked` (unless --locked, --frozen or --offline is present), each recorded as a transformation. Refused with
//                 exit 2, before class A, the snapshot sync or any compile: a second --message-format, --target-dir, --config,
//                 +toolchain, --fix, --allow-dirty, -Z (and -C, which moves the working directory), an absolute --manifest-path
//                 (a relative one resolves inside the snapshot and may not leave it) and any word that is not an option of
//                 `cargo clippy` (a subcommand).
//   sidecars      a requested package that declares sidecars (Tauri `bundle.externalBin`) refuses the whole request, BLOCKED with
//                 RK_CLIPPY_SIDECAR_BOUND, before ownership, class A, the sync or any compile, until item 7 row 7.05 stages sidecars
//                 from a snapshot. The request is never rewritten to leave such a package out (signed D8-1: three additions only).
//   Linux         direct, through the item 7 cargo wrapper (cargo-target-shim.mjs): every cargo launch is bound to the checkout's own
//                 target directory or refused, so nothing is written under the snapshot (row 8.12).
//   Mac and Dell  the broker carrier (item 7 row 7.12) is not wired into the gate yet. The route refuses BEFORE class A, the sync and
//                 any compile with a BLOCKED code naming 7.12, and never falls back to direct cargo on a broker-managed host
//                 (rows 8.3, 8.10, 8.13). The gate's half of the broker receipt (attribution and toolchain, rows 8.9 and 8.16) is
//                 `routeFromBrokerReceipt` below, unwired until the carrier is.
//   result        FAILED when any unit failed; diagnostics by package and target from cargo's JSON messages; a unit that never ran
//                 because a dependency failed is reported blocked and the result claims no completeness (row 8.4).
//   exit          0 PASSED, 1 FAILED or BLOCKED, 2 refused or could not run (RK_CHECK_ERROR: the snapshot moved).
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { resolveExecutor as defaultResolveExecutor } from "./gate-executor.mjs";
import { directPlatform } from "./managed-cargo.mjs";
import { defaultSpawnAsync, sidecarBoundCrate } from "./deterministic-scans.mjs";
import { gateStateDir } from "./gate-stages.mjs";
import { ProvenanceError, verifyAttributionDetail } from "./provenance-verify.mjs";

export const ROUTE_RECEIPT_SCHEMA = "rightkit.gate-clippy-route-receipt/1";
export const ROUTE_DIAGNOSTICS_SCHEMA = "rightkit.gate-clippy-route-diagnostics/1";
export const CLIPPY_USAGE = "usage: rightkit-gate clippy [<cargo clippy args>]   (HEAD's commit; the arguments are cargo's, verbatim; the gate adds only --message-format=json, --keep-going and --locked)";
/** The BLOCKED and refusal codes the route prints (each as `rightkit-gate: <code>: ...`). */
export const ROUTE_CODES = Object.freeze({
  notClippy: "RK_CLIPPY_NOT_CLIPPY", messageFormat: "RK_CLIPPY_MESSAGE_FORMAT", targetDir: "RK_CLIPPY_TARGET_DIR", config: "RK_CLIPPY_CONFIG", toolchain: "RK_CLIPPY_TOOLCHAIN_OVERRIDE",
  fix: "RK_CLIPPY_FIX", unstable: "RK_CLIPPY_UNSTABLE", chdir: "RK_CLIPPY_CHDIR", manifestAbsolute: "RK_CLIPPY_MANIFEST_ABSOLUTE", manifestEscapes: "RK_CLIPPY_MANIFEST_ESCAPES",
  outputInSnapshot: "RK_CLIPPY_OUTPUT_IN_SNAPSHOT", brokerCarrier: "RK_CLIPPY_BROKER_CARRIER_NOT_WIRED", brokerUnavailable: "RK_CLIPPY_BROKER_UNAVAILABLE", hostUnsupported: "RK_CLIPPY_HOST_UNSUPPORTED",
  sidecarBound: "RK_CLIPPY_SIDECAR_BOUND", receiptEvidence: "RK_CLIPPY_RECEIPT_EVIDENCE", toolchainUnreadable: "RK_CLIPPY_TOOLCHAIN_UNREADABLE",
});

const sha256 = (v) => crypto.createHash("sha256").update(v).digest("hex");
const posix = (p) => String(p).replaceAll("\\", "/");
const text = (r) => String(r?.stdout ?? "");
const failed = (r) => !r || Boolean(r.error) || r.status !== 0;
const why = (r) => String(r?.error?.message ?? r?.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | ");
const inside = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || (!!rel && !rel.startsWith("..") && !path.isAbsolute(rel)); };

// ---- argv (rows 8.1, 8.2) ------------------------------------------------------------------------------------------------------------

// `cargo clippy` options that take a value as the next word (or after `=`), and the short letters that do.
const LONG_WITH_VALUE = new Set(["--package", "--exclude", "--jobs", "--bin", "--example", "--test", "--bench", "--features", "--profile", "--target", "--manifest-path", "--color", "--lockfile-path", "--artifact-dir", "--out-dir", "--build-dir"]);
const SHORT_WITH_VALUE = new Set(["p", "j", "F"]);
const REFUSED = new Map([
  ["--message-format", [ROUTE_CODES.messageFormat, "the gate requests --message-format=json itself, exactly once; a request that carries its own is refused, never rewritten"]],
  ["--target-dir", [ROUTE_CODES.targetDir, "the gate passes no output path of its own: outputs go to the checkout's existing target directory (GATE-CONTRACT §1 output directory & cache)"]],
  ["--config", [ROUTE_CODES.config, "a cargo configuration override can move tools, outputs or flags the gate pins"]],
  ["--fix", [ROUTE_CODES.fix, "the route reports diagnostics only; the snapshot is never written"]],
  ["--allow-dirty", [ROUTE_CODES.fix, "the route reports diagnostics only; the snapshot is never written"]],
]);
const refusal = (code, message) => ({ refusal: { code, message } });
const absolutePath = (p) => path.posix.isAbsolute(p) || path.win32.isAbsolute(p) || /^[A-Za-z]:/.test(p);

/**
 * The route's cargo argv for a request: `{ refusal }`, `{ help: true }` or
 * `{ request, before, after, final, additions, transformations, already, manifestPath }`. `before` and `after` are the request's words
 * on either side of its `--`. The final argv is `before`, the additions, then `after`, so removing the additions gives the request
 * back, word for word, and lint arguments after `--` are untouched.
 */
export function parseClippyArgv(args) {
  const request = [...args];
  const sep = request.indexOf("--");
  const before = sep < 0 ? request : request.slice(0, sep);
  const after = sep < 0 ? [] : request.slice(sep);
  let keepGoing = false; let lockLike = false; let help = false; let manifestPath = null;
  for (let i = 0; i < before.length; i += 1) {
    const w = before[i];
    if (w.startsWith("+")) return refusal(ROUTE_CODES.toolchain, `${w}: the toolchain is pinned by the gate (BROKER-CONTRACT B5), never chosen by the request`);
    if (!w.startsWith("-") || w === "-") return refusal(ROUTE_CODES.notClippy, `${JSON.stringify(w)} is not an option of \`cargo clippy\`: the route runs Clippy only (a subcommand such as build, check, test or fmt is refused)`);
    if (w.startsWith("--")) {
      const eq = w.indexOf("=");
      const name = eq < 0 ? w : w.slice(0, eq);
      const refused = REFUSED.get(name);
      if (refused) return refusal(refused[0], `${name}: ${refused[1]}`);
      if (name === "--help" || name === "--version") help = true;
      else if (name === "--keep-going") keepGoing = true;
      else if (name === "--locked" || name === "--frozen" || name === "--offline") lockLike = true;
      if (LONG_WITH_VALUE.has(name)) {
        const value = eq >= 0 ? w.slice(eq + 1) : (before[i + 1] ?? null);
        if (eq < 0) i += 1;
        if (name === "--manifest-path" && value !== null) {
          if (absolutePath(value)) return refusal(ROUTE_CODES.manifestAbsolute, `--manifest-path ${value} is absolute: it would name a file outside the snapshot; give a path relative to the working directory (it resolves inside the snapshot)`);
          manifestPath = value;
        }
      }
      continue;
    }
    // A short cluster: the first value-taking letter takes the rest of the word (or the next word); Z and C are refused wherever they stand.
    for (let j = 1; j < w.length; j += 1) {
      const letter = w[j];
      if (letter === "Z") return refusal(ROUTE_CODES.unstable, `${w}: -Z unstable flags change what cargo does in ways the gate pins`);
      if (letter === "C") return refusal(ROUTE_CODES.chdir, `${w}: -C moves cargo's working directory, which must stay inside the snapshot`);
      if (letter === "h" || letter === "V") help = true;
      if (SHORT_WITH_VALUE.has(letter)) { if (j === w.length - 1) i += 1; break; }
    }
  }
  if (help) return { help: true };
  const additions = []; const transformations = []; const already = [];
  const add = (arg, reason) => { additions.push(arg); transformations.push({ op: "add", arg, reason }); };
  add("--message-format=json", "structured diagnostics: package and target attribution (E15)");
  if (keepGoing) already.push("--keep-going"); else add("--keep-going", "collect every feasible independent diagnostic when a unit fails (E15)");
  if (lockLike) already.push("--locked|--frozen|--offline"); else add("--locked", "the commit-only build: a lockfile rewrite inside the snapshot would surface only as a late tree mismatch (GATE-CONTRACT §1)");
  return { request, before, after, final: [...before, ...additions, ...after], additions, transformations, already, manifestPath };
}

// ---- cargo messages, units and the aggregate (rows 8.4, 8.9) ---------------------------------------------------------------------------

/** A package's name from its id, in either of cargo's spellings (`name 1.2.3 (source)`, `source#name@1.2.3`, `path+file:///x/name#1.2.3`). */
export function packageNameOf(packageId) {
  const id = String(packageId ?? "");
  const hash = id.lastIndexOf("#");
  if (hash >= 0) {
    const fragment = id.slice(hash + 1);
    const at = fragment.indexOf("@");
    if (at >= 0) return fragment.slice(0, at);
    return id.slice(0, hash).replace(/\/+$/, "").split(/[\\/]/).pop() || id;
  }
  return id.split(" ")[0] || id;
}

/** `{ messages, nonJson }`: the JSON objects of cargo's `--message-format=json` stream, one per line; other lines are counted. */
export function parseCargoMessages(body) {
  const messages = []; let nonJson = 0;
  for (const line of String(body ?? "").split(/\r?\n/)) {
    const t = line.trim();
    if (!t) continue;
    if (t.startsWith("{")) { try { messages.push(JSON.parse(t)); continue; } catch { /* counted below */ } }
    nonJson += 1;
  }
  return { messages, nonJson };
}

const SUMMARY_MESSAGE = /^(aborting due to|could not compile|\d+ warnings? emitted|build failed)/i;
const targetKind = (t) => (Array.isArray(t?.kind) ? t.kind.join(",") : String(t?.kind ?? ""));
const primarySpan = (spans) => (spans ?? []).find((s) => s.is_primary) ?? spans?.[0] ?? null;

/**
 * What a cargo message stream says. `units`: one row per package and target (package, packageId, target, kind, errors, warnings,
 * artifact, fresh); `diagnostics`: every error and warning with its package, target, code and primary span, plus cargo's own
 * rendering; `artifacts`: every compiler-artifact with the feature set it was built with; `buildFinished`: `{ success }` or null.
 */
export function summarizeMessages(messages) {
  const units = new Map(); const diagnostics = []; const artifacts = []; let buildFinished = null;
  const unitOf = (m) => {
    const packageId = String(m.package_id ?? "unknown");
    const key = `${packageNameOf(packageId)}\0${m.target?.name ?? "unknown"}\0${targetKind(m.target)}`;
    if (!units.has(key)) units.set(key, { package: packageNameOf(packageId), packageId, target: m.target?.name ?? "unknown", kind: targetKind(m.target), errors: 0, warnings: 0, artifact: false, fresh: null });
    return units.get(key);
  };
  for (const m of messages) {
    if (m.reason === "build-finished") buildFinished = { success: m.success === true };
    else if (m.reason === "compiler-artifact") {
      const u = unitOf(m); u.artifact = true; u.fresh = m.fresh === true;
      artifacts.push({ package: u.package, packageId: u.packageId, target: u.target, kind: u.kind, test: m.profile?.test === true, features: [...(m.features ?? [])].sort(), fresh: m.fresh === true });
    } else if (m.reason === "compiler-message") {
      const u = unitOf(m); const msg = m.message ?? {};
      const level = String(msg.level ?? "");
      if (SUMMARY_MESSAGE.test(String(msg.message ?? ""))) continue;
      if (level === "error" || level === "error: internal compiler error") u.errors += 1;
      else if (level === "warning") u.warnings += 1;
      else continue;
      const span = primarySpan(msg.spans);
      diagnostics.push({ package: u.package, packageId: u.packageId, target: u.target, kind: u.kind, level, code: msg.code?.code ?? null, message: String(msg.message ?? ""), span: span ? { file: span.file_name, line: span.line_start, column: span.column_start, lineEnd: span.line_end, columnEnd: span.column_end } : null, rendered: String(msg.rendered ?? "") });
    }
  }
  return { units: [...units.values()], diagnostics, artifacts, buildFinished };
}

/** Packages cargo's stderr names as failed to build (`could not compile` and a failed build script), by name. */
export function failedPackagesFromStderr(stderr) {
  const names = new Set();
  for (const m of String(stderr ?? "").matchAll(/could not compile `([^`]+)`/g)) names.add(m[1]);
  for (const m of String(stderr ?? "").matchAll(/failed to run custom build command for `([^`\s]+)/g)) names.add(m[1]);
  return [...names].sort();
}

/**
 * The packages a request selects, from `cargo metadata --no-deps` of the snapshot: `-p`/`--package` (name, `name@version`, glob),
 * `--workspace`/`--all` less `--exclude`, else the package at the working directory, else the workspace's default members.
 * `{ members: [pkg], all: [pkg], unresolved: [spec] }`; a spec that matches nothing is unresolved and the scope is then not fully known.
 */
export function resolveScope({ before, metadata, directory }) {
  const specs = []; const excludes = []; let workspace = false;
  for (let i = 0; i < before.length; i += 1) {
    const w = before[i];
    if (w === "--workspace" || w === "--all") { workspace = true; continue; }
    if (w === "-p" || w === "--package") { specs.push(before[i + 1] ?? ""); i += 1; continue; }
    if (w.startsWith("--package=")) { specs.push(w.slice(10)); continue; }
    if (w === "--exclude") { excludes.push(before[i + 1] ?? ""); i += 1; continue; }
    if (w.startsWith("--exclude=")) { excludes.push(w.slice(10)); continue; }
    if (/^-p./.test(w) && !w.startsWith("--")) specs.push(w.slice(2));
  }
  const packages = metadata?.packages ?? [];
  const byId = new Map(packages.map((p) => [p.id, p]));
  const all = (metadata?.workspace_members ?? []).map((id) => byId.get(id)).filter(Boolean);
  const globOf = (spec) => new RegExp(`^${spec.replace(/[.+^${}()|\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".")}$`);
  const matches = (p, spec) => p.name === spec || `${p.name}@${p.version}` === spec || p.id === spec || (/[*?]/.test(spec) && globOf(spec).test(p.name));
  const unresolved = [];
  let members;
  if (workspace) members = all.filter((p) => !excludes.some((x) => matches(p, x)));
  else if (specs.length) {
    members = [];
    for (const spec of specs) { const hit = all.filter((p) => matches(p, spec)); if (!hit.length) unresolved.push(spec); for (const p of hit) if (!members.includes(p)) members.push(p); }
  } else {
    const here = all.find((p) => path.dirname(p.manifest_path) === directory);
    members = here ? [here] : (metadata?.workspace_default_members ?? metadata?.workspace_members ?? []).map((id) => byId.get(id)).filter(Boolean);
  }
  return { members, all, unresolved };
}

/**
 * The route's result for one cargo run. Aggregate FAILED on any failed unit or a build that did not finish successfully (E15). A
 * requested package that built nothing because a dependency failed is `blocked`, never clean, and `complete` is false then (row 8.4).
 */
export function summarizeRun({ messages, stderr = "", exit = null, signal = null, startError = null, scope = null }) {
  const run = summarizeMessages(messages);
  const memberNames = scope ? new Set(scope.members.map((p) => p.name)) : null;
  const failedNames = new Set(failedPackagesFromStderr(stderr));
  const units = run.units.filter((u) => !memberNames || memberNames.has(u.package)).map((u) => ({ package: u.package, packageId: u.packageId, target: u.target, kind: u.kind, errors: u.errors, warnings: u.warnings, status: u.errors > 0 ? "failed" : u.artifact ? "clean" : "unbuilt", fresh: u.fresh }));
  for (const u of run.units) if (u.errors > 0) failedNames.add(u.package);
  const success = exit === 0 && !signal && !startError && run.buildFinished?.success === true;
  const blocked = [];
  if (scope && !success && run.buildFinished !== null) {
    const deps = new Map(scope.all.map((p) => [p.name, (p.dependencies ?? []).map((d) => d.name)]));
    const reachable = (name) => { const seen = new Set(); const stack = [...(deps.get(name) ?? [])]; while (stack.length) { const n = stack.pop(); if (seen.has(n) || !deps.has(n)) continue; seen.add(n); stack.push(...deps.get(n)); } return seen; };
    for (const p of scope.members) {
      const mine = units.filter((u) => u.package === p.name);
      if (failedNames.has(p.name) || mine.some((u) => u.status === "failed" || u.status === "clean")) continue;
      const blockedBy = [...reachable(p.name)].filter((n) => failedNames.has(n)).sort();
      blocked.push({ package: p.name, blockedBy, ...(blockedBy.length ? {} : { failedElsewhere: [...failedNames].filter((n) => !memberNames.has(n)).sort() }) });
    }
  }
  const aggregate = success ? "PASSED" : "FAILED";
  const scopeKnown = Boolean(scope) && scope.unresolved.length === 0;
  const complete = success || (blocked.length === 0 && scopeKnown && run.buildFinished !== null && !startError && !signal);
  const completeness = success ? "every requested unit was built"
    : startError ? `not claimed: cargo did not run (${startError})`
      : signal ? `not claimed: cargo was stopped by ${signal}`
        : !scope ? "not claimed: the requested package set could not be read"
          : scope.unresolved.length ? `not claimed: the request names packages the snapshot does not hold (${scope.unresolved.join(", ")})`
            : blocked.length ? `not claimed: ${blocked.length} requested package(s) were blocked by a failed dependency and built nothing`
              : run.buildFinished === null ? "not claimed: cargo reported no build-finished message"
                : "every requested unit ran; some failed";
  return { aggregate, complete, completeness, units, blocked, failedPackages: [...failedNames].sort(), diagnostics: run.diagnostics, artifacts: run.artifacts, buildFinished: run.buildFinished };
}

// ---- the broker's half of the receipt: attribution and toolchain (rows 8.9, 8.16) ------------------------------------------------------

/**
 * The gate's reading of a broker receipt for a Clippy request (BROKER-CONTRACT B4 to B6; the gate side only, wired when the carrier is). The receipt's
 * `attribution` is COMPACT (counts and a `detail` descriptor `{path, sha256}`); the per package and per target rows live in the evidence file the descriptor names
 * (`detailBytes`, read by the caller from that path). GB-50: the per-unit result is derived only through `verifyAttributionDetail` (RustRight's
 * verify_attribution_detail, vector attribution-detail-vectors.v1.json): the file's bytes hash to the signed descriptor, its schema, request id and toolchain are
 * this receipt's, it is available and not truncated, every row is well formed and the compact counts equal the rows. Counts alone never pass (`units: 2` with exit 0
 * and no matching detail is FAILED). Missing or unverifiable evidence makes the route result FAILED, never a partial success:
 * `{ aggregate, code, units, failedUnits, problems, toolchain, complete }`. A unit with errors, or a nonzero exit, is a failure.
 * The Clippy carrier stays explicitly refused (RK_CLIPPY_BROKER_CARRIER_NOT_WIRED) until the whole route is wired.
 */
export function routeFromBrokerReceipt(receipt, detailBytes = null) {
  const problems = [];
  const attribution = receipt?.attribution;
  const toolchain = typeof receipt?.toolchain === "string" && receipt.toolchain.trim() ? receipt.toolchain : null;
  let rows = null; let failedUnits = 0;
  if (!attribution || typeof attribution !== "object") problems.push("the receipt carries no attribution");
  else if (attribution.available !== true) problems.push(`the receipt's attribution is unavailable (${attribution.reason ?? "no reason"})`);
  else if (detailBytes === null || detailBytes === undefined) problems.push("the receipt's attribution is compact counts only and no detail file was supplied: counts alone cannot satisfy attribution");
  else {
    try { ({ rows, failedUnits } = verifyAttributionDetail(receipt, detailBytes)); } catch (e) {
      if (!(e instanceof ProvenanceError)) throw e;
      problems.push(`the receipt's attribution is not verified against its detail rows (${e.code}: ${e.message})`);
    }
  }
  if (toolchain === null) problems.push("the receipt carries no toolchain identity");
  const units = rows ? rows.map((u) => ({ package: u.package, packageId: u.packageId ?? null, target: u.target, kind: u.kind ?? "", errors: Number(u.errors ?? 0), warnings: Number(u.warnings ?? 0), status: Number(u.errors ?? 0) > 0 ? "failed" : "clean", fresh: u.fresh ?? null })) : [];
  const exit = receipt?.exit;
  const exitCode = typeof exit === "number" ? exit : typeof exit?.code === "number" ? exit.code : typeof exit?.status === "number" ? exit.status : null;
  if (exitCode === null) problems.push("the receipt carries no exit status");
  const aggregate = problems.length || exitCode !== 0 || failedUnits > 0 ? "FAILED" : "PASSED";
  return { aggregate, code: problems.length ? ROUTE_CODES.receiptEvidence : null, units, failedUnits, problems, toolchain, complete: aggregate === "PASSED" };
}

// ---- sidecar-bound scope (row 8.8) -----------------------------------------------------------------------------------------------------

/**
 * The requested packages that declare sidecars (a Tauri `bundle.externalBin`): their build needs the sidecars staged first, which the
 * snapshot route cannot do until item 7 row 7.05 lands. The route never rewrites the request to leave them out (signed D8-1 allows
 * only its three additions): it refuses the whole request, BLOCKED with RK_CLIPPY_SIDECAR_BOUND, before class A, the sync or any
 * compile. `dirOf(pkg)` is the package's directory as `exists` and `readFile` read it (the commit's tree, or the snapshot).
 */
export function sidecarBoundMembers({ scope, dirOf, exists, readFile }) {
  if (!scope) return [];
  return scope.members.filter((p) => sidecarBoundCrate({ dir: dirOf(p), exists, readFile }).bound).map((p) => p.name).sort();
}

const sidecarRefusal = (tag, bound, where) => `${tag} BLOCKED: ${ROUTE_CODES.sidecarBound}: the requested scope holds sidecar-bound package(s) ${bound.join(", ")} (${where}); they need their sidecars staged before they build, which a commit snapshot cannot do until item 7 row 7.05; the route does not rewrite the request to leave them out; nothing compiled`;

// ---- the route ----------------------------------------------------------------------------------------------------------------------

const BLOCKED = 1; const REFUSED_EXIT = 2;
const resultLine = (o, tag, value) => o.out(`${tag} clippy route result ${JSON.stringify(value)}`);

/**
 * `rightkit-gate clippy`: the entry. `args` are the words after `clippy`. `deps` (from the gate's main): `main` (the gate entry, for
 * the door's own full class A), `io` (its overrides), `absoluteEnv` (the inherited locations a snapshot compile requires absolute),
 * and, for tests, overrides of `resolveExecutor`, `snapshot` (snapshot.mjs), `snapshotCheck` (snapshot-check.mjs), `ownership`
 * (build-ownership.mjs), `acquireOwnership`, `syncSnapshot`, `runClassA`, `runCargo` and `shim` (cargo-target-shim.mjs).
 */
export async function clippyCommand(o, args, deps = {}) {
  const tag = "rightkit-gate:";
  const parsed = parseClippyArgv(args);
  if (parsed.help) { o.out(CLIPPY_USAGE); return 0; }
  if (parsed.refusal) { o.err(`${tag} ${parsed.refusal.code}: ${parsed.refusal.message}; nothing ran`); return REFUSED_EXIT; }

  // The checkout: where HEAD's commit and the object database come from. Its working files are never read.
  const top = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: o.cwd });
  if (failed(top) || !text(top).trim()) { o.err(`${tag} not inside a git repository; nothing ran`); return REFUSED_EXIT; }
  let root; let cwdReal;
  try { root = fs.realpathSync.native(path.resolve(text(top).trim())); cwdReal = fs.realpathSync.native(path.resolve(o.cwd)); } catch (e) { o.err(`${tag} cannot resolve the checkout: ${e.message}; nothing ran`); return REFUSED_EXIT; }
  const relCwd = inside(cwdReal, root) ? posix(path.relative(root, cwdReal)) : "";
  // A relative --manifest-path resolves inside the snapshot and may not leave it (resolved lexically against the working directory).
  if (parsed.manifestPath !== null) {
    const resolved = path.posix.normalize(path.posix.join(relCwd || ".", posix(parsed.manifestPath)));
    if (resolved === ".." || resolved.startsWith("../")) { o.err(`${tag} ${ROUTE_CODES.manifestEscapes}: --manifest-path ${parsed.manifestPath} resolves outside the checkout's tree, so outside the snapshot; nothing ran`); return REFUSED_EXIT; }
  }

  // The executor is the gate's, never the request's: Mac and Dell are broker hosts, Linux is always direct (E2, E3).
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (deps.resolveExecutor ?? defaultResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile });
  if (executor?.env && o.env) Object.assign(o.env, executor.env);
  if (executor?.note) o.out(`${tag} ${executor.note}`);
  if (executor.kind === "refuse") {
    o.err(`${tag} BLOCKED: ${ROUTE_CODES.brokerUnavailable}: ${executor.problem}; the route never compiles around the broker, and no launch was attempted (no class A, no sync, no compile)`);
    resultLine(o, tag, { result: "BLOCKED", code: ROUTE_CODES.brokerUnavailable, launched: false });
    return BLOCKED;
  }
  if (executor.kind === "broker") {
    o.err(`${tag} BLOCKED: ${ROUTE_CODES.brokerCarrier}: on this broker host a Clippy route compiles the commit snapshot only through the RustRight snapshot carrier (item 7 row 7.12), which the gate does not wire yet; nothing ran and cargo is never run directly here (rows 8.3, 8.10, 8.13)`);
    resultLine(o, tag, { result: "BLOCKED", code: ROUTE_CODES.brokerCarrier, blockedOn: ["item7:7.12"], launched: false });
    return BLOCKED;
  }
  if (!directPlatform(o.platform) || o.env.GITHUB_ACTIONS === "true") {
    o.err(`${tag} BLOCKED: ${ROUTE_CODES.hostUnsupported}: the route builds a gate-owned commit snapshot on a Linux execution host; hosted CI runs Clippy through its own workflow; nothing ran`);
    return BLOCKED;
  }

  const snap = deps.snapshot ?? await import("./snapshot.mjs");
  const sc = deps.snapshotCheck ?? await import("./snapshot-check.mjs");
  const ownershipApi = deps.ownership ?? await import("./build-ownership.mjs");
  let selected; let snapshotsAt; let snapshotDir;
  try {
    selected = snap.resolveCommit(root, "HEAD");
    snapshotsAt = o.snapshotRoot ?? snap.snapshotRoot({ platform: o.platform, env: o.env });
    snapshotDir = path.join(snapshotsAt, snap.repositoryOf(root).key);
  } catch (e) { o.err(`${tag} cannot select HEAD's commit: ${e.message}; nothing ran`); return REFUSED_EXIT; }

  // The output directory is the checkout's, resolved by the gate (row 8.12): a caller's CARGO_TARGET_DIR that points into the snapshot would
  // be a second output directory, and CARGO_HOME, sccache and temp are inherited only as absolute values.
  for (const name of ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"]) {
    const v = o.env[name];
    if (typeof v === "string" && v !== "" && inside(path.resolve(cwdReal, v), snapshotDir)) { o.err(`${tag} ${ROUTE_CODES.outputInSnapshot}: ${name}=${v} points into the gate-owned snapshot ${snapshotDir}; outputs go to the checkout's target directory, never under the snapshot; nothing ran`); return REFUSED_EXIT; }
  }
  const relative = (deps.absoluteEnv ?? []).filter((n) => typeof o.env[n] === "string" && o.env[n] !== "" && !path.isAbsolute(o.env[n]));
  if (relative.length) { o.err(`${tag} BLOCKED: ${relative.map((n) => `${n}=${o.env[n]}`).join(", ")} ${relative.length > 1 ? "are" : "is"} relative; a snapshot compile inherits CARGO_HOME, sccache and temp only as absolute paths; nothing ran`); return BLOCKED; }

  // Sidecar-bound scope, refused BEFORE ownership, the sync, class A or any compile (row 8.8, lead decision 2026-10-29). Each Tauri
  // configuration is read from HEAD's commit, never from the checkout's files. A commit that declares no sidecar anywhere needs no
  // further look (no cargo process starts before class A). Otherwise the requested packages come from the checkout's
  // `cargo metadata --no-deps --offline --locked` (no compile, nothing written) and the request is refused when one of them declares
  // sidecars. The snapshot's own metadata re-checks it below, so a checkout whose workspace differs from the commit cannot slip a
  // sidecar-bound package through.
  const commitExists = (f) => !failed(o.spawn("git", ["cat-file", "-e", `${selected.commit}:${posix(f)}`], { cwd: root }));
  const commitRead = (f) => { const r = o.spawn("git", ["cat-file", "-p", `${selected.commit}:${posix(f)}`], { cwd: root }); if (failed(r)) throw new Error(`cannot read ${f} in ${selected.commit}`); return text(r); };
  const listed = o.spawn("git", ["ls-tree", "-r", "--name-only", selected.commit], { cwd: root });
  const confDirs = failed(listed) ? [""] : [...new Set(text(listed).split(/\r?\n/).filter((f) => /(^|\/)(tauri\.conf\.json5?|Tauri\.toml)$/.test(f)).map((f) => path.posix.dirname(f)).map((d) => (path.posix.basename(d) === "src-tauri" ? path.posix.dirname(d) : d)))];
  const commitDeclaresSidecars = confDirs.some((d) => d === "" || sidecarBoundCrate({ dir: d, exists: commitExists, readFile: commitRead }).bound);
  let preBound = [];
  if (commitDeclaresSidecars) {
    const preMeta = o.spawn("cargo", ["metadata", "--no-deps", "--offline", "--locked", "--format-version", "1", ...(parsed.manifestPath ? ["--manifest-path", parsed.manifestPath] : [])], { cwd: cwdReal, env: o.env });
    let preScope = null;
    try { if (!failed(preMeta)) preScope = resolveScope({ before: parsed.before, metadata: JSON.parse(text(preMeta)), directory: parsed.manifestPath ? path.dirname(path.resolve(cwdReal, parsed.manifestPath)) : cwdReal }); } catch { preScope = null; }
    preBound = sidecarBoundMembers({ scope: preScope, dirOf: (p) => posix(path.relative(root, path.dirname(p.manifest_path))) || ".", exists: commitExists, readFile: commitRead });
  }
  if (preBound.length) { o.err(sidecarRefusal(tag, preBound, "read from the commit")); resultLine(o, tag, { result: "BLOCKED", code: ROUTE_CODES.sidecarBound, blockedOn: ["item7:7.05"], bound: preBound, launched: false }); return BLOCKED; }

  const timings = {}; const lap = (name, since) => { timings[name] = Date.now() - since; };
  // Build ownership first (E6): held from before the sync until the diagnostics are written. A busy host runs nothing and syncs nothing.
  let own;
  try { own = (deps.acquireOwnership ?? ownershipApi.acquireBuildOwnership)({ file: ownershipApi.ownershipFile({ platform: o.platform, beside: o.snapshotRoot }), stage: "clippy", snapshot: snapshotDir, commit: selected.commit, platform: o.platform, env: o.env }); }
  catch (e) { if (e.code !== "busy" && e.code !== "refused") throw e; o.err(`${tag} BLOCKED: ${e.message}; ${e.code === "refused" ? "nothing ran, nothing created" : "the route never re-syncs a snapshot another request holds; nothing ran"}`); return BLOCKED; }
  o.out(`${tag} build ownership ${own.joined ? "joined" : "held"} ${own.handle}`);
  const abort = o.abort ?? new AbortController();
  let shimDir = null; let messagesFile = null;
  const stopped = []; const onSignal = (sig) => () => { stopped.push(sig); abort.abort(); };
  const handlers = o.abort ? [] : [["SIGINT", onSignal("SIGINT")], ["SIGTERM", onSignal("SIGTERM")]];
  for (const [sig, h] of handlers) process.on(sig, h);
  try {
    let t = Date.now();
    let synced;
    try { synced = (deps.syncSnapshot ?? snap.syncSnapshot)({ checkout: root, rev: selected.commit, root: snapshotsAt }); }
    catch (e) { o.err(`${tag} ${e.code === "error" ? "RK_CHECK_ERROR" : "BLOCKED"}: snapshot ${e.message}; nothing ran`); return e.code === "error" ? REFUSED_EXIT : BLOCKED; }
    lap("sync", t);
    o.out(`${tag} snapshot ${synced.path} is commit ${synced.commit} tree ${synced.tree} (written=${synced.written} removed=${synced.removed} unchanged=${synced.unchanged})`);

    // FULL class A first (D8-2, E1): the door's own check of the snapshot. A red class A starts no Clippy.
    t = Date.now();
    const classA = await (deps.runClassA ?? (() => deps.main(["check", "--commit", synced.commit, "--full-a", "--snapshot", synced.path, "--rule-tier", "release"], { ...(deps.io ?? {}), cwd: root, buildOwnership: own })))();
    lap("classA", t);
    if (classA !== 0) { o.err(`${tag} FAILED: class A is red for ${synced.commit.slice(0, 12)}; Clippy was not started`); resultLine(o, tag, { result: "FAILED", reason: "class-a", exit: classA, commit: synced.commit, clippyStarted: false }); return classA === REFUSED_EXIT ? REFUSED_EXIT : BLOCKED; }

    const realCheckout = fs.realpathSync.native(root);
    const realSnapshot = fs.realpathSync.native(synced.path);
    let treeAtStart;
    try { treeAtStart = sc.verifySnapshotDir({ checkout: realCheckout, dir: synced.path, commit: synced.commit, tree: synced.tree, when: "before Clippy" }); }
    catch (e) { o.err(`${tag} RK_CHECK_ERROR: ${e.message}; nothing was compiled`); return REFUSED_EXIT; }

    // The snapshot's working directory for the request, and the Cargo workspace it names, resolved from the commit's own manifests.
    const snapCwd = relCwd ? path.join(synced.path, ...relCwd.split("/")) : synced.path;
    if (!fs.existsSync(snapCwd)) { o.err(`${tag} BLOCKED: the working directory ${relCwd} is not in commit ${synced.commit.slice(0, 12)}; run from a directory the commit holds; nothing compiled`); return BLOCKED; }
    const realCargo = which("cargo").find((c) => path.isAbsolute(c));
    if (!realCargo) { o.err(`${tag} BLOCKED: no cargo on PATH to wrap for the snapshot compile; nothing compiled`); return BLOCKED; }
    const located = o.spawn("cargo", ["locate-project", "--workspace", "--message-format", "plain", ...(parsed.manifestPath ? ["--manifest-path", parsed.manifestPath] : [])], { cwd: snapCwd, env: o.env });
    if (failed(located) || !path.isAbsolute(text(located).trim())) { o.err(`${tag} BLOCKED: the request's Cargo workspace cannot be resolved in the snapshot (${why(located) || "no manifest"}); nothing compiled`); return BLOCKED; }
    const workspaceDir = fs.realpathSync.native(path.dirname(text(located).trim()));
    if (!inside(workspaceDir, realSnapshot)) { o.err(`${tag} BLOCKED: the Cargo workspace ${workspaceDir} is outside the snapshot; nothing compiled`); return BLOCKED; }
    const cargoDir = posix(path.relative(realSnapshot, workspaceDir)) || ".";
    const checkoutManifest = path.join(root, ...(cargoDir === "." ? [] : cargoDir.split("/")), "Cargo.toml");
    if (!(o.exists ?? fs.existsSync)(checkoutManifest)) { o.err(`${tag} BLOCKED: the checkout has no ${posix(path.relative(root, checkoutManifest))}, so the output directory of workspace ${cargoDir} cannot be resolved; nothing compiled`); return BLOCKED; }
    // The same resolution the door's snapshot compile makes (bin/rightkit-gate.mjs, "output directory for workspace").
    const metaOut = o.spawn("cargo", ["metadata", "--offline", "--format-version", "1", "--no-deps", "--manifest-path", checkoutManifest], { cwd: path.dirname(checkoutManifest), env: o.env });
    let outputDir = null;
    try { outputDir = failed(metaOut) ? null : JSON.parse(text(metaOut)).target_directory; } catch { outputDir = null; }
    if (typeof outputDir !== "string" || !path.isAbsolute(outputDir)) { o.err(`${tag} BLOCKED: cargo metadata in the checkout reported no absolute target_directory for ${cargoDir}${failed(metaOut) ? ` (${why(metaOut)})` : ""}; nothing compiled`); return BLOCKED; }
    outputDir = path.resolve(outputDir);
    if (inside(outputDir, realSnapshot)) { o.err(`${tag} ${ROUTE_CODES.outputInSnapshot}: the resolved output directory ${outputDir} is inside the snapshot; nothing compiled`); return REFUSED_EXIT; }
    o.out(`${tag} output directory for workspace ${cargoDir}: ${outputDir} (the checkout's)`);

    // The item 7 cargo wrapper binds every cargo launch to that directory or refuses it.
    const shimApi = deps.shim ?? await import("./cargo-target-shim.mjs");
    let shim;
    try { shim = shimApi.installCargoShim({ base: os.tmpdir(), snapshotRoot: synced.path, targetDirs: { [cargoDir]: outputDir }, realCargo, platform: o.platform, onDir: (dir) => { shimDir = dir; } }); }
    catch (e) { o.err(`${tag} BLOCKED: the runtime cargo wrapper cannot be installed (${e.message}); nothing compiled`); return BLOCKED; }
    const scanEnv = shimApi.shimScanEnv(o.env, shim);
    o.out(`${tag} runtime cargo wrapper ${shim.shim} (identity ${shim.identity.slice(0, 16)}) -> ${realCargo}`);

    // The toolchain identity this run compiled with (BROKER-CONTRACT B5: the same engine and toolchain for batch and per-crate runs).
    const toolchain = toolchainIdentity({ spawn: o.spawn, env: o.env, cwd: snapCwd });
    if (!toolchain) { o.err(`${tag} BLOCKED: ${ROUTE_CODES.toolchainUnreadable}: \`cargo -vV\` or \`rustc -vV\` printed nothing the gate can read; nothing compiled`); return BLOCKED; }

    // The requested package set (blocked-unit detection and the sidecar re-check), read from the snapshot's own metadata.
    let scope = null;
    const metaScope = o.spawn(shim.shim, ["metadata", "--no-deps", "--format-version", "1", "--locked", ...(parsed.manifestPath ? ["--manifest-path", parsed.manifestPath] : [])], { cwd: snapCwd, env: scanEnv });
    try { if (!failed(metaScope)) scope = resolveScope({ before: parsed.before, metadata: JSON.parse(text(metaScope)), directory: parsed.manifestPath ? path.dirname(path.resolve(snapCwd, parsed.manifestPath)) : snapCwd }); } catch { scope = null; }

    // Sidecar-bound scope (row 8.8): refused before the sync above; this re-check on the snapshot's own metadata is the backstop.
    const bound = sidecarBoundMembers({ scope, dirOf: (p) => path.dirname(p.manifest_path) });
    if (bound.length) { o.err(sidecarRefusal(tag, bound, "found in the snapshot, after the checkout's own metadata showed none")); resultLine(o, tag, { result: "BLOCKED", code: ROUTE_CODES.sidecarBound, blockedOn: ["item7:7.05"], bound, launched: false }); return BLOCKED; }
    const invocations = [];

    t = Date.now();
    let run = { messages: [], stderr: "", exit: null, signal: null, startError: null };
    const stateDir = path.join(gateStateDir(commonDirOf(o, root)), "clippy-route");
    fs.mkdirSync(stateDir, { recursive: true });
    const requestId = crypto.randomUUID();
    messagesFile = path.join(stateDir, `${requestId}.messages.jsonl`);
    {
      const r = await (deps.runCargo ?? defaultSpawnAsync)(shim.shim, ["clippy", ...parsed.final], { cwd: snapCwd, env: scanEnv, signal: abort.signal, stdoutFile: messagesFile });
      invocations.push({ role: "requested", argv: ["clippy", ...parsed.final], exit: r.status ?? null });
      let body = "";
      try { body = fs.readFileSync(messagesFile, "utf8"); } catch { body = String(r.stdout ?? ""); }
      run = { messages: parseCargoMessages(body).messages, stderr: String(r.stderr ?? ""), exit: r.status ?? null, signal: stopped[0] ?? null, startError: r.error && !r.cancelled ? r.error.message : null };
    }
    lap("cargo", t);
    if (stopped.length) { fs.rmSync(messagesFile, { force: true }); o.err(`${tag} stopped by ${stopped[0]}; no result`); return stopped[0] === "SIGINT" ? 130 : 143; }

    // The result covers the commit only if the snapshot is still its tree after cargo exits (E6): otherwise the diagnostics are discarded.
    t = Date.now();
    let treeAtExit;
    try { treeAtExit = sc.verifySnapshotDir({ checkout: realCheckout, dir: synced.path, commit: synced.commit, tree: synced.tree, when: "after Clippy" }); }
    catch (e) { fs.rmSync(messagesFile, { force: true }); o.err(`${tag} RK_CHECK_ERROR: ${e.message}; the diagnostics are discarded, they cover no known bytes`); resultLine(o, tag, { result: "FAILED", code: "RK_CHECK_ERROR", discarded: true, commit: synced.commit }); return REFUSED_EXIT; }
    lap("afterExitBind", t);
    if (!own.held()) { fs.rmSync(messagesFile, { force: true }); o.err(`${tag} RK_CHECK_ERROR: the build ownership ${own.handle} is no longer held; the diagnostics are discarded`); return REFUSED_EXIT; }

    t = Date.now();
    const summary = summarizeRun({ ...run, scope });
    const diagnosticsPath = path.join(stateDir, `${requestId}.diagnostics.json`);
    fs.writeFileSync(diagnosticsPath, `${JSON.stringify({ schema: ROUTE_DIAGNOSTICS_SCHEMA, requestId, commit: { sha: synced.commit, tree: synced.tree }, units: summary.units, blocked: summary.blocked, diagnostics: summary.diagnostics }, null, 2)}\n`);
    fs.rmSync(messagesFile, { force: true });
    lap("diagnostics", t);
    const receipt = {
      schema: ROUTE_RECEIPT_SCHEMA, requestId, executor: "direct", issuedAt: new Date().toISOString(),
      commit: { sha: synced.commit, tree: synced.tree },
      snapshot: { path: synced.path, treeAtStart, treeAtExit },
      ownership: { handle: own.handle },
      outputDir, outputDirs: { [cargoDir]: outputDir },
      cargoWrapper: { identity: shim.identity, workspaces: shim.workspaces },
      toolchain,
      argv: { request: parsed.request, final: parsed.final, transformations: parsed.transformations, already: parsed.already },
      invocations,
      exit: { status: run.exit, signal: run.signal }, diagnosticsPath, timingsMs: timings,
      result: { aggregate: summary.aggregate, complete: summary.complete, completeness: summary.completeness, units: summary.units, blocked: summary.blocked, failedPackages: summary.failedPackages },
      rebuilt: null,
    };
    const receiptPath = path.join(stateDir, `${requestId}.json`);
    fs.writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`);
    reportRoute(o, tag, receipt, summary, receiptPath);
    return summary.aggregate === "PASSED" ? 0 : BLOCKED;
  } finally {
    for (const [sig, h] of handlers) process.off(sig, h);
    if (shimDir) fs.rmSync(shimDir, { recursive: true, force: true });
    const r = own.release();
    if (r?.released === false && !own.joined) o.err(`${tag} build ownership ${own.handle} kept: ${r.why}`);
  }
}

function commonDirOf(o, root) {
  const r = o.spawn("git", ["rev-parse", "--path-format=absolute", "--git-common-dir"], { cwd: root });
  return failed(r) ? path.join(root, ".git") : text(r).trim();
}

/** The toolchain identity in the broker's shape (`name|cargo:<sha256>|rustc:<sha256>`, over `-vV`), or null when unreadable. */
export function toolchainIdentity({ spawn, env, cwd }) {
  const cargo = spawn("cargo", ["-vV"], { cwd, env });
  const rustc = spawn("rustc", ["-vV"], { cwd, env });
  if (failed(cargo) || failed(rustc) || !text(cargo).trim() || !text(rustc).trim()) return null;
  return `${env?.RUSTUP_TOOLCHAIN ?? "direct"}|cargo:${sha256(text(cargo).trim())}|rustc:${sha256(text(rustc).trim())}`;
}

const SHOWN_DIAGNOSTICS = 400;
function reportRoute(o, tag, receipt, summary, receiptPath) {
  const shown = summary.diagnostics.slice(0, SHOWN_DIAGNOSTICS);
  for (const d of shown) if (d.rendered) o.err(d.rendered.trimEnd());
  if (summary.diagnostics.length > shown.length) o.err(`${tag} ${summary.diagnostics.length - shown.length} more diagnostic(s) are in ${receipt.diagnosticsPath}`);
  const failedUnits = summary.units.filter((u) => u.status === "failed");
  o.out(`${tag} clippy route ${summary.aggregate} for ${receipt.commit.sha.slice(0, 12)}: ${summary.units.length} unit(s), ${failedUnits.length} failed, ${summary.blocked.length} blocked; ${summary.completeness}`);
  for (const u of failedUnits) o.out(`  failed  ${u.package} (${u.target}${u.kind ? `, ${u.kind}` : ""}): ${u.errors} error(s), ${u.warnings} warning(s)`);
  for (const b of summary.blocked) o.out(`  blocked ${b.package}${b.blockedBy?.length ? `: dependency ${b.blockedBy.join(", ")} failed` : b.reason ? `: ${b.reason}` : ": a dependency failed to build"}`);
  o.out(`${tag} clippy route receipt ${receiptPath}`);
  resultLine(o, tag, { result: summary.aggregate, complete: summary.complete, commit: receipt.commit.sha, failed: failedUnits.length, blocked: summary.blocked.length, receipt: receiptPath });
}
