// release-version-pins: every place a unit's version is written down by hand, checked against the one source of truth
// (the unit's own manifest). Stage 0 of the @rightkit/release canary and a plan-check of the npm pre-flight run it.
//
// Why it exists: 0.6.3 was bumped in packages/release/package.json (035a439e) while the bundled version feed still
// listed 0.6.2 as the staged release, and again (85ea11d4) while ci/consumers/npm/release/consumer.mjs still asserted
// 0.6.2; each cost a hosted run (39966188759). A pin that is a copy of the version is a defect waiting for the next bump.
//
// Built-ins only: the pinned dispatcher digests this file, and the canary must not import the code it judges.
import fs from "node:fs";
import path from "node:path";

export const RELEASE_UNIT = "@rightkit/release";
const FEED = "packages/release/rightkit-versions.json";
const SESSION_COPY = "packages/build-control-policy/src/ownership-session-versions.json";
const PIPELINE = "ci/rk-pipeline.json";

const read = (root, rel) => { try { return fs.readFileSync(path.join(root, rel)); } catch { return null; } };
const readJson = (root, rel) => { const bytes = read(root, rel); if (!bytes) return { missing: true }; try { return { value: JSON.parse(bytes.toString("utf8")) }; } catch (error) { return { invalid: String(error.message ?? error) }; } };

const triple = (v) => { const m = /^(\d+)\.(\d+)\.(\d+)$/.exec(String(v).trim()); return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null; };
const cmp = (a, b) => { for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1; return 0; };

/** Does an exact x.y.z satisfy an npm range? Supports ||, ^, ~, comparators, x/* wildcards and `workspace:` prefixes.
 * Anything it cannot read is NOT satisfied (fail closed). */
export function satisfiesNpm(version, range) {
  const v = triple(version);
  if (!v) return false;
  const text = String(range).trim().replace(/^workspace:/, "").replace(/(\^|~|>=|<=|>|<|=)\s+/g, "$1");
  if (text === "" || text === "*" || text === "x" || text === "X" || text === "^" || text === "~") return true;
  return text.split("||").some((alt) => alt.trim().split(/\s+/).filter(Boolean).every((token) => token === "*" || satisfiesToken(v, token)));
}
function satisfiesToken(v, token) {
  const m = /^(\^|~|>=|<=|>|<|=)?(\d+)(?:\.(\d+|x|X|\*))?(?:\.(\d+|x|X|\*))?$/.exec(token);
  if (!m) return false;
  const [, op = "", major, minorRaw, patchRaw] = m;
  const wild = (x) => x === undefined || x === "x" || x === "X" || x === "*";
  const base = [Number(major), wild(minorRaw) ? 0 : Number(minorRaw), wild(patchRaw) ? 0 : Number(patchRaw)];
  if (op === "" || op === "=") {
    if (wild(minorRaw)) return v[0] === base[0];
    if (wild(patchRaw)) return v[0] === base[0] && v[1] === base[1];
    return cmp(v, base) === 0;
  }
  if (op === ">=") return cmp(v, base) >= 0;
  if (op === ">") return cmp(v, base) > 0;
  if (op === "<=") return cmp(v, base) <= 0;
  if (op === "<") return cmp(v, base) < 0;
  if (op === "~") return cmp(v, base) >= 0 && v[0] === base[0] && (wild(minorRaw) ? true : v[1] === base[1]);
  // caret: the leftmost non-zero component is the one that may not change
  if (cmp(v, base) < 0) return false;
  if (base[0] > 0 || wild(minorRaw)) return v[0] === base[0];
  if (base[1] > 0 || wild(patchRaw)) return v[0] === 0 && v[1] === base[1];
  return v[0] === 0 && v[1] === 0 && v[2] === base[2];
}

function workspacePackage(root, unit) {
  const dir = path.join(root, "packages");
  let entries = [];
  try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch { return { error: "packages/ is missing" }; }
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const file = `packages/${entry.name}/package.json`;
    const { value } = readJson(root, file);
    if (value?.name === unit) return { dir: `packages/${entry.name}`, file, version: value.version };
  }
  return { error: `${unit} is not a workspace package under packages/` };
}

function npmDependents(root, unit) {
  const out = [];
  let entries = [];
  try { entries = fs.readdirSync(path.join(root, "packages"), { withFileTypes: true }); } catch { return out; }
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const file = `packages/${entry.name}/package.json`;
    const { value } = readJson(root, file);
    if (!value || value.name === unit) continue;
    for (const kind of ["dependencies", "peerDependencies", "optionalDependencies"]) {
      if (typeof value[kind]?.[unit] === "string") out.push({ file, kind, name: value.name, range: value[kind][unit] });
    }
  }
  return out;
}

function cratePin(root, unit) {
  const manifest = read(root, `crates/${unit}/Cargo.toml`);
  if (!manifest) return { error: `crates/${unit}/Cargo.toml is missing` };
  const section = /\[package\]([\s\S]*?)(?:\n\[|$)/.exec(manifest.toString("utf8"));
  const version = section && /^\s*version\s*=\s*"([^"]+)"/m.exec(section[1]);
  return version ? { version: version[1] } : { error: `crates/${unit}/Cargo.toml has no literal package version` };
}

/** Every pin of `unit`'s version that disagrees with its manifest. Empty array = consistent. Never throws on a missing file:
 * a pin that cannot be read is a problem, not a pass. */
export function versionPinProblems(root, unit) {
  const problems = [];
  if (typeof unit !== "string" || !unit) return ["versionPinProblems needs a unit name"];

  if (unit.startsWith("@")) {
    const pkg = workspacePackage(root, unit);
    if (pkg.error) return [pkg.error];
    const version = pkg.version;
    if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(String(version))) return [`${pkg.file} version ${JSON.stringify(version)} is not semver`];

    const feed = readJson(root, FEED);
    if (unit === RELEASE_UNIT) {
      if (feed.missing || feed.invalid) problems.push(`${FEED} cannot be read${feed.invalid ? `: ${feed.invalid}` : ""}`);
      else if (feed.value.stagedNpm?.[unit] !== version) problems.push(`${FEED} stagedNpm[${unit}] is ${JSON.stringify(feed.value.stagedNpm?.[unit])} but ${pkg.file} is ${version}; stage the version in the feed (node packages/release/rightkit-sync.mjs) before publishing`);
      const feedBytes = read(root, FEED);
      const copyBytes = read(root, SESSION_COPY);
      if (!copyBytes) problems.push(`${SESSION_COPY} is missing`);
      else if (!feedBytes || !feedBytes.equals(copyBytes)) problems.push(`${SESSION_COPY} is not byte-identical to ${FEED}; copy the feed over it`);
    } else if (feed.value?.stagedNpm?.[unit] !== undefined && feed.value.stagedNpm[unit] !== version) {
      problems.push(`${FEED} stagedNpm[${unit}] is ${feed.value.stagedNpm[unit]} but ${pkg.file} is ${version}`);
    }

    const pipeline = readJson(root, PIPELINE);
    const exception = pipeline.value?.dispatch?.exceptions?.[unit];
    if (exception && exception.version !== version) problems.push(`${PIPELINE} dispatch.exceptions[${unit}].version is ${JSON.stringify(exception.version)} but ${pkg.file} is ${version}; the exception would silently stop applying`);

    if (unit === RELEASE_UNIT) {
      for (const dep of npmDependents(root, unit)) {
        if (!satisfiesNpm(version, dep.range)) problems.push(`${dep.file} ${dep.kind}["${unit}"] is "${dep.range}", which does not include ${version}`);
      }
    }

    const consumer = `ci/consumers/npm/${unit.slice("@rightkit/".length)}/consumer.mjs`;
    const consumerBytes = read(root, consumer);
    if (consumerBytes) {
      // The recipe receives the version as RIGHTKIT_CONSUMER_EXPECTED_VERSION; a literal copy goes stale on every bump.
      for (const m of consumerBytes.toString("utf8").matchAll(/assert\.equal\(\s*version\s*,\s*['"](\d+\.\d+\.\d+)['"]\s*\)/g)) {
        if (m[1] !== version) problems.push(`${consumer} hard-codes version ${m[1]} but ${pkg.file} is ${version}; use RIGHTKIT_CONSUMER_EXPECTED_VERSION`);
      }
    }
    return problems;
  }

  // Cargo unit
  const crate = cratePin(root, unit);
  if (crate.error) return [crate.error];
  // The version feed (packages/release/rightkit-versions.json) lists what the doctor admits: a crate published at a
  // version the feed does not list is refused by every app's doctor (rightkit-fs 0.4.7 against a feed at 0.4.6).
  const feed = readJson(root, FEED);
  if (feed.value) {
    const listed = feed.value.stagedCargo?.[unit] ?? feed.value.cargo?.[unit];
    if (listed === undefined) problems.push(`${FEED} lists ${unit} in neither stagedCargo nor cargo; stage ${crate.version} in the feed`);
    else if (listed !== crate.version) problems.push(`${FEED} lists ${unit} ${listed} (${feed.value.stagedCargo?.[unit] !== undefined ? "stagedCargo" : "cargo"}) but crates/${unit}/Cargo.toml is ${crate.version}; stage the version in the feed`);
  } else problems.push(`${FEED} cannot be read`);
  const consumer = read(root, `ci/consumers/${unit}/Cargo.toml`);
  if (!consumer) return [...problems, `ci/consumers/${unit}/Cargo.toml is missing`];
  const pin = new RegExp(`^${unit.replace(/[-.]/g, "\\$&")}\\s*=\\s*\\{[^\\n]*version\\s*=\\s*"=([^"]+)"`, "m").exec(consumer.toString("utf8"));
  if (!pin) problems.push(`ci/consumers/${unit}/Cargo.toml does not pin ${unit} with an exact =x.y.z version`);
  else if (pin[1] !== crate.version) problems.push(`ci/consumers/${unit}/Cargo.toml pins =${pin[1]} but crates/${unit}/Cargo.toml is ${crate.version}`);
  return problems;
}
