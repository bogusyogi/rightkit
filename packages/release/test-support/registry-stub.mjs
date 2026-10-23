// Test-only preload (node --import): answers the canonical registry URLs from the lock files of the fixture app, so a
// spawned release.mjs / doctor runs offline. Every locked RightKit version is "published" with its locked digest unless
// RK_TEST_UNPUBLISHED / RK_TEST_YANKED name it; RK_TEST_LATEST ("name@version,...") moves a package's latest.
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectLockedRightKit, ownedRightKitNames } from "../rightkit-registry-resolver.mjs";

const dir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const owned = ownedRightKitNames(JSON.parse(readFileSync(path.join(dir, "rightkit-versions.json"), "utf8")), JSON.parse(readFileSync(path.join(dir, "rightkit-ownership.json"), "utf8")));
const list = (name) => String(process.env[name] ?? "").split(",").filter(Boolean);
const semver = (v) => v.split(/[.-]/).slice(0, 3).map(Number);
const newer = (a, b) => { const x = semver(a); const y = semver(b); for (let i = 0; i < 3; i += 1) if (x[i] !== y[i]) return x[i] - y[i]; return 0; };
const json = (body, status = 200) => new Response(typeof body === "string" ? body : JSON.stringify(body), { status });

function published() {
  const rows = new Map();
  const gone = new Set(list("RK_TEST_UNPUBLISHED"));
  for (const root of String(process.env.RK_TEST_REGISTRY_ROOT ?? "").split(path.delimiter).filter(Boolean)) {
    for (const entry of collectLockedRightKit(root, owned, { extraLocks: list("RK_TEST_EXTRA_LOCKS") }).entries) {
      if (gone.has(`${entry.name}@${entry.version}`)) continue;
      const key = `${entry.ecosystem}\0${entry.name}`;
      rows.set(key, [...(rows.get(key) ?? []).filter((r) => r.version !== entry.version), { version: entry.version, digest: entry.digest.value }]);
    }
  }
  for (const item of list("RK_TEST_LATEST")) {
    const [name, version] = item.split("@@");
    for (const eco of ["cargo", "npm"]) if (rows.has(`${eco}\0${name}`)) rows.get(`${eco}\0${name}`).push({ version, digest: "f".repeat(64) });
  }
  return rows;
}
const latestOf = (versions, name) => list("RK_TEST_LATEST").map((i) => i.split("@@")).find(([n]) => n === name)?.[1] ?? versions.map((r) => r.version).sort(newer).at(-1);

const realFetch = globalThis.fetch;
globalThis.fetch = async (input, init) => {
  const url = new URL(typeof input === "string" ? input : input.url);
  const rows = published();
  const yanked = new Set(list("RK_TEST_YANKED"));
  if (url.host === "index.crates.io") {
    const name = url.pathname.split("/").at(-1);
    const versions = rows.get(`cargo\0${name}`);
    return versions ? json(versions.map((r) => JSON.stringify({ name, vers: r.version, cksum: r.digest, yanked: yanked.has(`${name}@${r.version}`) })).join("\n")) : json("", 404);
  }
  if (url.host === "crates.io") {
    const name = url.pathname.split("/").at(-1);
    const versions = rows.get(`cargo\0${name}`);
    return versions ? json({ crate: { max_stable_version: latestOf(versions, name) } }) : json("", 404);
  }
  if (url.host === "registry.npmjs.org") {
    const name = decodeURIComponent(url.pathname.slice(1));
    const versions = rows.get(`npm\0${name}`);
    return versions ? json({ "dist-tags": { latest: latestOf(versions, name) }, versions: Object.fromEntries(versions.map((r) => [r.version, { dist: { integrity: r.digest }, ...(yanked.has(`${name}@${r.version}`) ? { deprecated: "test" } : {}) }])) }) : json("", 404);
  }
  return realFetch(input, init);
};
