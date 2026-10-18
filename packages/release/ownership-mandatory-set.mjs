#!/usr/bin/env node
// right-release ownership-mandatory: Table 1 mandatory-owner-set check (plan task 5.1).
//
// This evaluates declarations only; it does not establish active target/features, lock resolution or production adoption.
// A registered app must declare every Table 1 owner (rows M1-M23) at or above the required version,
// from the registry. Report mode (default) prints per-app findings and exits 0; --enforce exits 1
// on any missing owner, below-required version, or path/git/patch override ("not-on-registry").
// Mode switching of the ownership gate itself (Report -> Enforce) is a separate step; this check
// is read-only and never changes ownership mode.
//
//   right-release ownership-mandatory [--workspace <dir>] [--app <name|dir>]... [--profile desktop|apple]
//                                     [--amendment v2.1] [--enforce] [--json]
//
// App-side declaration (optional): <app>/rightkit-mandatory.json
//   { "profile": "desktop", "notApplicable": { "M16": "reason text" } }
import { existsSync, readFileSync, readdirSync, realpathSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parse as parseToml } from "smol-toml";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
export const DATA_FILE = "ownership-mandatory-set.json";
export const loadData = () => JSON.parse(readFileSync(path.join(packageRoot, DATA_FILE), "utf8"));

const SKIP_DIRS = new Set(["node_modules", "target", ".git", "rk-snapshot", "vendor", "dist", "build", "examples", "fixtures", "benchmarks", ".pnpm-store"]);
const MAX_MANIFESTS = 2000;

// ---- versions -------------------------------------------------------------------------------
/**
 * Proven minimum of a Cargo/npm requirement: a version every satisfying version is guaranteed >=, or null when
 * none can be proven (upper-only `<9`, `*`, `latest`, malformed, or a `||` branch with no proven minimum).
 * Space/comma comparators AND (proven minimum = max of lower contributions; `<`/`<=` contribute none); `||`
 * branches union (proven minimum = min of branch minima, null if any branch is unprovable). Hyphen ranges
 * contribute their lower end. `>`, `>=`, `=`, `^`, `~`, bare & wildcard partials contribute their version
 * (missing/`x`/`*` components read as 0), so `>1.2.3` proves 1.2.3 (sound, never above the true floor).
 */
export function lowerBound(spec) {
  if (typeof spec !== "string") return null;
  const s = spec.trim().replace(/^npm:(?:@[^@/]+\/)?[^@]+@/, "").trim();
  if (!s || /^(\*|latest|x)$/i.test(s)) return null;
  const part = (v) => (v === undefined || /[xX*]/.test(v) ? 0 : Number(v));
  const versionOf = (text) => {
    const m = /^v?(\d+)(?:\.(\d+|[xX*]))?(?:\.(\d+|[xX*]))?(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(text);
    if (m?.[4] && m[4].split(".").some((id) => !id || (/^\d+$/.test(id) && id.length > 1 && id.startsWith("0")))) return null;
    return m ? { major: Number(m[1]), minor: part(m[2]), patch: part(m[3]), pre: m[4] ?? null } : null;
  };
  const branchBound = (branch) => {
    const raw = branch.split(/[\s,]+/).filter(Boolean);
    const tokens = [];
    for (let i = 0; i < raw.length; i += 1) {
      if (/^(>=|<=|>|<|=|\^|~)$/.test(raw[i]) && i + 1 < raw.length) { tokens.push(raw[i] + raw[i + 1]); i += 1; }
      else tokens.push(raw[i]);
    }
    if (!tokens.length) return null;
    const lowers = [];
    let skipUpperEnd = false;
    for (const token of tokens) {
      if (token === "-") { skipUpperEnd = true; continue; }
      if (skipUpperEnd) { skipUpperEnd = false; continue; }
      if (/^(\*|x|latest)$/i.test(token)) continue;
      const m = /^(>=|<=|>|<|=|\^|~)?(.+)$/.exec(token);
      const version = m && versionOf(m[2]);
      if (!version) return null;
      if (m[1] === "<" || m[1] === "<=") continue;
      lowers.push(version);
    }
    return lowers.length ? lowers.reduce((best, v) => (compareVersions(v, best) > 0 ? v : best)) : null;
  };
  const branches = s.split("||").map((b) => b.trim());
  if (branches.some((b) => !b)) return null;
  const bounds = branches.map(branchBound);
  if (bounds.some((b) => b === null)) return null;
  return bounds.reduce((best, v) => (compareVersions(v, best) < 0 ? v : best));
}
export const parseVersion = lowerBound;
export function compareVersions(a, b) {
  for (const k of ["major", "minor", "patch"]) if (a[k] !== b[k]) return a[k] < b[k] ? -1 : 1;
  if (a.pre === b.pre) return 0;
  if (a.pre === null) return 1;
  if (b.pre === null) return -1;
  const left = a.pre.split("."), right = b.pre.split(".");
  for (let i = 0; i < Math.max(left.length, right.length); i += 1) {
    if (left[i] === undefined) return -1;
    if (right[i] === undefined) return 1;
    if (left[i] === right[i]) continue;
    const an = /^\d+$/.test(left[i]), bn = /^\d+$/.test(right[i]);
    if (an && bn) {
      const av = BigInt(left[i]), bv = BigInt(right[i]);
      if (av !== bv) return av < bv ? -1 : 1;
    } else if (an !== bn) return an ? -1 : 1;
    else return left[i] < right[i] ? -1 : 1;
  }
  return 0;
}
const show = (v) => `${v.major}.${v.minor}.${v.patch}${v.pre ? `-${v.pre}` : ""}`;

// ---- manifest discovery & parsing ---------------------------------------------------------
export function findManifests(root) {
  const found = [];
  const walk = (dir, depth) => {
    if (found.length >= MAX_MANIFESTS || depth > 8) return;
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); } catch { return; }
    for (const e of entries) {
      if (e.isSymbolicLink()) continue;
      const full = path.join(dir, e.name);
      if (e.isDirectory()) { if (!SKIP_DIRS.has(e.name) && !e.name.startsWith(".")) walk(full, depth + 1); }
      else if (e.name === "Cargo.toml" || e.name === "package.json") found.push(full);
    }
  };
  walk(root, 0);
  return found.sort();
}

const isNonRegistry = (v) => v && typeof v === "object" && ("path" in v || "git" in v);

/** Collect dependency facts: { owner, ecosystem, kind: "registry"|"override", dev, spec, via, file }. */
export function readManifest(file, root) {
  const rel = path.relative(root, file).split(path.sep).join("/");
  const text = readFileSync(file, "utf8");
  const facts = [];
  let workspaceDeps;
  if (path.basename(file) === "Cargo.toml") {
    let doc;
    try { doc = parseToml(text); } catch (error) { return { rel, facts, error: `unparseable Cargo.toml: ${error.message}` }; }
    workspaceDeps = doc.workspace?.dependencies;
    const wsTable = workspaceDeps ?? {};
    const sections = (table, dev, target = null) => {
      for (const [section, isDev] of [["dependencies", dev], ["build-dependencies", dev], ["dev-dependencies", true]]) {
        for (const [key, raw] of Object.entries(table?.[section] ?? {})) add(key, raw, isDev, target);
      }
    };
    const add = (key, raw, dev, target) => {
      let value = raw;
      if (value && typeof value === "object" && value.workspace === true && wsTable[key] !== undefined) value = { ...(typeof wsTable[key] === "object" ? wsTable[key] : { version: wsTable[key] }), ...value };
      const name = typeof value === "object" && typeof value.package === "string" ? value.package : key;
      const spec = typeof value === "string" ? value : value?.version;
      const unresolved = raw && typeof raw === "object" && raw.workspace === true && wsTable[key] === undefined;
      facts.push({ target, optional: Boolean(value?.optional), owner: name, ecosystem: "cargo", kind: isNonRegistry(value) ? "override" : "registry", dev, spec: spec ?? null, via: isNonRegistry(value) ? ("path" in value ? "path dependency" : "git dependency") : "dependency", file: rel, ...(unresolved ? { inheritsWorkspace: key } : {}) });
    };
    sections(doc, false);
    for (const [target, table] of Object.entries(doc.target ?? {})) sections(table, false, target);
    for (const [key, raw] of Object.entries(wsTable)) {
      // workspace.dependencies only counts through members that opt in (above); record path/git overrides here.
      if (isNonRegistry(raw)) facts.push({ owner: typeof raw.package === "string" ? raw.package : key, ecosystem: "cargo", kind: "override", dev: false, spec: raw.version ?? null, via: "workspace.dependencies path/git", file: rel, declarationOnly: true });
    }
    for (const [registry, table] of Object.entries(doc.patch ?? {})) {
      for (const [key, raw] of Object.entries(table ?? {})) {
        if (isNonRegistry(raw)) facts.push({ owner: typeof raw.package === "string" ? raw.package : key, ecosystem: "cargo", kind: "override", dev: false, spec: null, via: `[patch.${registry}]`, file: rel, patch: true });
      }
    }
  } else {
    let doc;
    try { doc = JSON.parse(text); } catch (error) { return { rel, facts, error: `unparseable package.json: ${error.message}` }; }
    for (const [section, dev] of [["dependencies", false], ["optionalDependencies", false], ["peerDependencies", false], ["devDependencies", true]]) {
      for (const [name, spec] of Object.entries(doc[section] ?? {})) {
        const alias = typeof spec === "string" ? /^npm:((?:@[^@/]+\/)?[^@]+)@/.exec(spec) : null;
        const owner = alias ? alias[1] : name;
        const local = typeof spec === "string" && /^(workspace:|file:|link:|portal:|git\+|git:|github:|https?:)/.test(spec);
        facts.push({ declarationSection: section, optional: section === "optionalDependencies", peer: section === "peerDependencies", owner, ecosystem: "npm", kind: local ? "override" : "registry", dev, spec: typeof spec === "string" ? spec : null, via: local ? `${spec.split(":")[0]}: specifier` : "dependency", file: rel });
      }
    }
    for (const table of [doc.pnpm?.overrides, doc.overrides, doc.resolutions]) {
      for (const [key, spec] of Object.entries(table ?? {})) {
        if (typeof spec === "string" && /^(workspace:|file:|link:|portal:|git\+|github:)/.test(spec)) {
          facts.push({ owner: key.replace(/@[^@/]*$/, ""), ecosystem: "npm", kind: "override", dev: false, spec, via: "package override", file: rel, patch: true });
        }
      }
    }
  }
  return { rel, facts, workspaceDeps };
}

/** A member's `{ workspace = true }` names a table in an ancestor manifest's [workspace.dependencies]; resolve it across files. */
export function resolveWorkspaceInheritance(manifests) {
  const roots = manifests.filter((m) => m.workspaceDeps && Object.keys(m.workspaceDeps).length);
  for (const m of manifests) {
    for (const fact of m.facts) {
      if (!fact.inheritsWorkspace) continue;
      const dir = path.posix.dirname(m.rel);
      const owner = roots
        .filter((r) => r.workspaceDeps[fact.inheritsWorkspace] !== undefined)
        .filter((r) => { const rd = path.posix.dirname(r.rel); return rd === "." || dir === rd || dir.startsWith(`${rd}/`); })
        .sort((a, b) => path.posix.dirname(b.rel).length - path.posix.dirname(a.rel).length)[0];
      if (!owner) continue;
      const declared = owner.workspaceDeps[fact.inheritsWorkspace];
      const table = declared && typeof declared === "object" ? declared : { version: declared };
      fact.spec = table.version ?? null;
      fact.kind = isNonRegistry(table) ? "override" : "registry";
      if (isNonRegistry(table)) fact.via = "path" in table ? "path dependency" : "git dependency";
      if (typeof table.package === "string") fact.owner = table.package;
    }
  }
}

// ---- evaluation --------------------------------------------------------------------------
/** Merge row owners into one requirement per (ecosystem,name): max min, any allowDev, union of rows. */
function requirements(rows) {
  const map = new Map();
  for (const row of rows) for (const o of row.owners) {
    const key = `${o.ecosystem}:${o.name}`;
    const cur = map.get(key) ?? { ...o, rows: [] };
    if (compareVersions(lowerBound(o.min), lowerBound(cur.min)) > 0) cur.min = o.min;
    cur.allowDev = Boolean(cur.allowDev || o.allowDev);
    cur.rows.push(row.id);
    map.set(key, cur);
  }
  return map;
}

function judge(req, facts) {
  const mine = facts.filter((f) => f.owner === req.name && f.ecosystem === req.ecosystem);
  const overrides = mine.filter((f) => f.kind === "override");
  const counted = mine.filter((f) => f.kind === "registry" && (req.allowDev || !f.dev));
  const devOnly = !counted.length && mine.some((f) => f.kind === "registry" && f.dev);
  const min = lowerBound(req.min);
  const base = { evidenceKind: "declaration", qualification: "unverified", activeTarget: null, lockResolution: null, productionUse: null, declarations: mine.map((f) => ({ file: f.file, spec: f.spec, dev: f.dev, target: f.target ?? null, optional: f.optional ?? false, peer: f.peer ?? false })), owner: req.name, ecosystem: req.ecosystem, rows: [...new Set(req.rows)], required: req.min };
  const bounds = counted.map((f) => ({ f, v: lowerBound(f.spec) }));
  const best = bounds.filter((b) => b.v).sort((a, b) => compareVersions(b.v, a.v))[0];
  const declaredAt = best ? show(best.v) : null;
  const registryExempt = req.registry === false;
  if (overrides.length && !registryExempt) {
    return { ...base, status: "not-on-registry", declared: declaredAt, evidence: overrides.map((f) => `${f.file}: ${f.via}${f.spec ? ` (${f.spec})` : ""}`) };
  }
  if (!counted.length && !(registryExempt && overrides.length)) {
    return { ...base, status: "missing", declared: null, evidence: devOnly ? ["declared only under dev-dependencies"] : [] };
  }
  if (!counted.length) return { ...base, status: "ok", declared: "path/git (registry not required)", evidence: overrides.map((f) => `${f.file}: ${f.via}`) };
  if (!best) return { ...base, status: "below-required", declared: null, evidence: counted.map((f) => `${f.file}: version not provable from ${JSON.stringify(f.spec)}`) };
  if (compareVersions(best.v, min) < 0) return { ...base, status: "below-required", declared: declaredAt, evidence: [`${best.f.file}: ${best.f.spec} < ${req.min}`] };
  return { ...base, status: "ok", declared: declaredAt, evidence: [] };
}

export function loadAppDeclaration(root, data) {
  const file = path.join(root, data.appDeclarationFile);
  if (!existsSync(file)) return { profile: data.defaultProfile, notApplicable: {}, problems: [] };
  const problems = [];
  let doc = {};
  try { doc = JSON.parse(readFileSync(file, "utf8")); } catch (error) { problems.push(`${data.appDeclarationFile} unparseable: ${error.message}`); }
  const notApplicable = {};
  for (const [id, reason] of Object.entries(doc.notApplicable ?? {})) {
    if (typeof reason === "string" && reason.trim().length >= 10) notApplicable[id] = reason.trim();
    else problems.push(`${id}: N/A declaration needs a reason (>= 10 chars); ignored`);
  }
  return { profile: doc.profile ?? data.defaultProfile, notApplicable, problems };
}

export function checkApp(root, { data = loadData(), profile, amendments = [] } = {}) {
  const name = path.basename(root);
  if (!existsSync(root) || !statSync(root).isDirectory()) return { app: name, root, scanned: false, profile: profile ?? data.defaultProfile, owners: [], pending: [], notApplicable: [], problems: ["app root missing"], violations: 0 };
  const declaration = loadAppDeclaration(root, data);
  const active = profile ?? declaration.profile;
  const problems = [...declaration.problems];
  if (!data.profiles[active]) problems.push(`unknown profile "${active}"`);
  const rows = data.rows.filter((r) => r.profile === active).map((r) => ({ ...r, owners: [...r.owners] }));
  for (const key of amendments) {
    const amendment = data.amendments?.[key];
    if (!amendment) { problems.push(`unknown amendment "${key}"`); continue; }
    for (const extra of amendment.rows.filter((r) => r.profile === active)) {
      const row = rows.find((r) => r.id === extra.id);
      if (row) row.owners.push(...extra.owners);
    }
  }
  const notApplicable = Object.entries(declaration.notApplicable).map(([id, reason]) => ({ row: id, reason }));
  const applicable = rows.filter((r) => !(r.id in declaration.notApplicable));
  const pending = data.pendingRows.filter((r) => r.profile === active).map((r) => ({ row: r.id, name: r.name, status: "owner-pending", note: r.ownerPending }));
  const manifests = findManifests(root).map((f) => readManifest(f, root));
  resolveWorkspaceInheritance(manifests);
  for (const m of manifests) if (m.error) problems.push(`${m.rel}: ${m.error}`);
  const facts = manifests.flatMap((m) => m.facts);
  const owners = [...requirements(applicable).values()].map((req) => { const result = judge(req, facts); return { ...result, declarationStatus: result.status }; });
  // A profile that is unknown, marked unratified, or has no rows checks nothing; an app may not select it, so it fails rather than passing empty.
  const profileStatus = !data.profiles[active] ? "unknown-profile" : (data.profiles[active].ratified === false || !data.rows.some((r) => r.profile === active)) ? "unratified" : null;
  if (profileStatus) {
    problems.push(`profile "${active}" is ${profileStatus === "unratified" ? "unratified or has no rows" : "unknown"}; no mandatory set is checked`);
    owners.push({ owner: `profile:${active}`, ecosystem: "profile", rows: [], required: "ratified profile", status: profileStatus, declared: null, evidence: ["an app may not select a profile that is not ratified"] });
  }
  const violations = owners.filter((o) => o.status !== "ok").length;
  return { app: name, root, scanned: true, profile: active, manifests: manifests.length, owners, pending, notApplicable, problems, violations };
}

export function checkApps(roots, options = {}) {
  const data = options.data ?? loadData();
  const apps = roots.map((r) => checkApp(r, { ...options, data }));
  return { schema: 1, mode: options.enforce ? "enforce" : "report", amendments: options.amendments ?? [], apps, failed: apps.some((a) => !a.scanned || a.violations > 0) };
}

export function renderText(report) {
  const out = [`ownership-mandatory (${report.mode}) — Table 1 owner declarations (active-target, lock & production qualification unverified)`];
  for (const app of report.apps) {
    const count = (s) => app.owners.filter((o) => o.status === s).length;
    out.push("", `${app.app} [${app.profile}] ${app.scanned ? `ok=${count("ok")} missing=${count("missing")} below-required=${count("below-required")} not-on-registry=${count("not-on-registry")}` : "UNSCANNED"}`
      + (app.pending.length ? ` pending=${app.pending.map((p) => p.row).join(",")}` : "") + (app.notApplicable.length ? ` n/a=${app.notApplicable.map((n) => n.row).join(",")}` : ""));
    for (const o of app.owners.filter((x) => x.status !== "ok")) out.push(`  ${o.status.padEnd(15)} ${o.owner} (${o.rows.join(",")}) requires ${o.required}${o.declared ? `, declared ${o.declared}` : ""}${o.evidence.length ? ` — ${o.evidence.join("; ")}` : ""}`);
    for (const n of app.notApplicable) out.push(`  n/a             ${n.row}: ${n.reason}`);
    for (const p of app.problems) out.push(`  problem         ${p}`);
  }
  out.push("", `${report.apps.filter((a) => !a.scanned || a.violations).length}/${report.apps.length} apps fail the mandatory set${report.mode === "enforce" ? "" : " (report mode: exit 0)"}`);
  return `${out.join("\n")}\n`;
}

const flags = (args, name) => args.flatMap((a, i) => (a === name ? [args[i + 1]] : []));
export async function main(args, io = {}) {
  const stdout = io.stdout ?? process.stdout, stderr = io.stderr ?? process.stderr;
  const data = loadData();
  const workspace = path.resolve(flags(args, "--workspace")[0] ?? path.resolve(packageRoot, "../../.."));
  const requested = flags(args, "--app");
  const roots = (requested.length ? requested : data.defaultApps).map((a) => (path.isAbsolute(a) || a.includes(path.sep) ? path.resolve(a) : path.join(workspace, a)));
  const amendments = flags(args, "--amendment").map((a) => a.replace(/^v?/, "v"));
  const profile = flags(args, "--profile")[0];
  if (profile && !data.profiles[profile]) { stderr.write(`[right-release] ownership-mandatory: unknown profile "${profile}"\n`); return 2; }
  const unknownAmendment = amendments.find((a) => !data.amendments?.[a]);
  if (unknownAmendment) { stderr.write(`[right-release] ownership-mandatory: unknown amendment "${unknownAmendment}"\n`); return 2; }
  const report = checkApps(roots, { data, profile, amendments, enforce: args.includes("--enforce") });
  stdout.write(args.includes("--json") ? `${JSON.stringify(report, null, 2)}\n` : renderText(report));
  return args.includes("--enforce") && report.failed ? 1 : 0;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try { process.exitCode = await main(process.argv.slice(2)); }
  catch (error) { console.error(`[right-release] ownership-mandatory: ${error.message}`); process.exitCode = 1; }
}
