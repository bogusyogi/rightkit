#!/usr/bin/env node
// Propose or regenerate dependency-allowlist baselines from each registered repository's CURRENT direct
// dependencies. This only edits the committed proposal file; it takes effect solely through Adrian's trust-sync.
//   node ownership-dependency-allowlist-seed.mjs --workspace <dir> [--write]
// Existing approvals are never removed (union with the current file); block-tier catalog dependencies
// and RightKit packages are never recorded as approvals.
import { existsSync, readdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { canonicalRepositoryIdentity } from "./ownership-approval.mjs";
import { directDependencies } from "./ownership.mjs";
import { ALLOWLIST_FILE, entryKey, isRightKitPackage, sourceKind, validateAllowlist } from "./ownership-dependency-allowlist.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const readJson = (name) => JSON.parse(readFileSync(path.join(here, name), "utf8"));

// Universally approved tooling: tiny on purpose. Everything else is per-repository.
export const SEED_GLOBAL = Object.freeze(["npm:@types/node", "npm:typescript", "npm:vitest"]);
// A renamed repository that has no checkout of its own reuses the baseline of its current name.
export const SEED_ALIASES = Object.freeze({ "https://github.com/orthic-labs/cockpit": "https://github.com/orthic-labs/pulse" });

function localCheckouts(workspace) {
  const found = new Map();
  for (const name of readdirSync(workspace)) {
    const dir = path.join(workspace, name);
    if (!existsSync(path.join(dir, ".git"))) continue;
    const origin = spawnSync("git", ["-C", dir, "config", "--get", "remote.origin.url"], { encoding: "utf8", windowsHide: true });
    if (origin.status !== 0) continue;
    try { found.set(canonicalRepositoryIdentity(origin.stdout.trim()), dir); } catch { /* not a canonical remote */ }
  }
  return found;
}

export function seedRepository(root, manifest) {
  const blocked = new Set(manifest.capabilities.flatMap((c) => c.fingerprints.filter((f) => f.type === "dependency" && (f.tier ?? c.tier ?? "block") === "block").map((f) => `${f.ecosystem}:${f.name}`)));
  const owners = manifest.capabilities.map((c) => c.package);
  const entries = new Set(); let skippedBlock = 0, skippedFirstParty = 0, skippedRightKit = 0;
  const deps = directDependencies(root);
  for (const d of deps) {
    const kind = sourceKind(d.source);
    if (kind === "path") { skippedFirstParty += 1; continue; }
    if (isRightKitPackage(d.name, owners)) { skippedRightKit += 1; continue; }
    if (blocked.has(`${d.ecosystem}:${d.name}`)) { skippedBlock += 1; continue; }
    entries.add(entryKey(d.ecosystem, d.name, kind));
  }
  return { entries: [...entries].sort(), direct: deps.length, skippedBlock, skippedFirstParty, skippedRightKit, unscanned: deps.coverage?.length ?? 0 };
}

export function buildSeed({ workspace, registry = readJson("ownership-repositories.json"), manifest = readJson("rightkit-ownership.json"), existing = null, log = () => {} } = {}) {
  const checkouts = localCheckouts(workspace);
  const previous = new Map((existing?.repositories ?? []).map((entry) => [entry.repositoryIdentity, entry]));
  const seeded = new Map(), stats = [];
  for (const { repositoryIdentity } of registry.repositories) {
    const root = checkouts.get(repositoryIdentity);
    if (!root) continue;
    const result = seedRepository(root, manifest);
    seeded.set(repositoryIdentity, result.entries);
    stats.push({ repositoryIdentity, root, ...result });
  }
  const repositories = registry.repositories.map(({ repositoryIdentity }) => {
    const prior = previous.get(repositoryIdentity)?.dependencies ?? [];
    let note;
    let fresh = seeded.get(repositoryIdentity);
    if (!fresh && SEED_ALIASES[repositoryIdentity] && seeded.has(SEED_ALIASES[repositoryIdentity])) { fresh = seeded.get(SEED_ALIASES[repositoryIdentity]); note = `seeded from ${SEED_ALIASES[repositoryIdentity]} (same repository, renamed)`; stats.push({ repositoryIdentity, alias: SEED_ALIASES[repositoryIdentity], entries: fresh }); }
    if (!fresh) { fresh = []; note = "no local checkout when seeded; baseline is empty until proposed"; stats.push({ repositoryIdentity, unseeded: true, entries: [] }); }
    return { repositoryIdentity, ...(note ? { note } : {}), dependencies: [...new Set([...prior, ...fresh])].sort() };
  });
  const allowlist = { schema: 1, global: [...new Set([...(existing?.global ?? []), ...SEED_GLOBAL])].sort(), repositories };
  const check = validateAllowlist(allowlist);
  if (!check.ok) throw new Error(`generated allowlist is invalid: ${check.problems.join("; ")}`);
  log(stats);
  return { allowlist, stats };
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const args = process.argv.slice(2);
  const flag = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  const workspace = path.resolve(flag("--workspace") ?? path.resolve(here, "../../.."));
  const file = path.join(here, ALLOWLIST_FILE);
  const existing = existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : null;
  const { allowlist, stats } = buildSeed({ workspace, existing });
  for (const s of stats) process.stdout.write(`${s.repositoryIdentity}: ${s.entries.length} entries${s.direct !== undefined ? ` (direct ${s.direct}, first-party ${s.skippedFirstParty}, rightkit ${s.skippedRightKit}, block-tier ${s.skippedBlock}, unscanned ${s.unscanned})` : s.alias ? ` (alias of ${s.alias})` : " (UNSEEDED)"}\n`);
  if (args.includes("--write")) { writeFileSync(file, `${JSON.stringify(allowlist, null, 2)}\n`); process.stdout.write(`wrote ${file}\n`); }
}
