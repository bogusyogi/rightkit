import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { applyRegistryLatest, compareVersions, registryLatest, rightkitPublishables, syncConsumerCopies } from "./rightkit-sync.mjs";

test("versions compare numerically, not lexically", () => {
  assert.ok(compareVersions("0.2.10", "0.2.9") > 0);
  assert.equal(compareVersions("0.1.0", "0.1.0"), 0);
  assert.ok(compareVersions("0.1.6", "0.2.0") < 0);
});

test("newer published versions become staged; the replaced staged version stays allowed", () => {
  const versions = { npm: { "@rightkit/qa": "0.1.0" }, stagedNpm: { "@rightkit/qa": "0.2.3" }, cargo: { "rightkit-db": "0.1.0" }, stagedCargo: { "rightkit-db": "0.2.1", "rightkit-control": "0.1.5" } };
  const { versions: next, changes } = applyRegistryLatest(versions, [
    { registry: "npm", name: "@rightkit/qa", version: "0.2.4" },
    { registry: "cargo", name: "rightkit-control", version: "0.1.6" },
    { registry: "cargo", name: "rightkit-db", version: "0.2.1" },
    { registry: "cargo", name: "rightkit-embed", version: "0.2.0" },
    { registry: "cargo", name: "rightkit-llm", version: null },
  ]);
  assert.equal(next.stagedNpm["@rightkit/qa"], "0.2.4"); assert.deepEqual(next.legacyNpm["@rightkit/qa"], ["0.2.3"]);
  assert.equal(next.stagedCargo["rightkit-control"], "0.1.6"); assert.deepEqual(next.legacyCargo["rightkit-control"], ["0.1.5"]);
  assert.equal(next.stagedCargo["rightkit-embed"], "0.2.0"); assert.equal(next.legacyCargo["rightkit-embed"], undefined);
  assert.equal(next.stagedCargo["rightkit-llm"], undefined);
  assert.deepEqual(changes.map((c) => `${c.name}:${c.from}->${c.to}`), ["@rightkit/qa:0.2.3->0.2.4", "rightkit-control:0.1.5->0.1.6", "rightkit-embed:null->0.2.0"]);
  assert.equal(versions.stagedCargo["rightkit-control"], "0.1.5", "input is not mutated");
});

test("a registry never downgrades staged, and the floor is never moved to legacy", () => {
  const { versions: next, changes } = applyRegistryLatest({ cargo: { "rightkit-db": "0.2.0" }, stagedCargo: { "rightkit-db": "0.2.0" } }, [{ registry: "cargo", name: "rightkit-db", version: "0.2.1" }, { registry: "cargo", name: "rightkit-fs", version: "0.0.9" }]);
  assert.equal(next.stagedCargo["rightkit-db"], "0.2.1"); assert.equal(next.legacyCargo, undefined);
  const { changes: none } = applyRegistryLatest({ stagedCargo: { "rightkit-db": "0.3.0" } }, [{ registry: "cargo", name: "rightkit-db", version: "0.2.1" }]);
  assert.equal(none.length, 0); assert.equal(changes.length, 2);
});

test("publishables skip private crates and packages; registry lookups use both registries", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "rk-sync-")); try {
    await mkdir(path.join(root, "crates", "a"), { recursive: true }); await mkdir(path.join(root, "crates", "b"), { recursive: true });
    await mkdir(path.join(root, "packages", "p"), { recursive: true }); await mkdir(path.join(root, "packages", "q"), { recursive: true });
    await writeFile(path.join(root, "crates", "a", "Cargo.toml"), '[package]\nname = "rightkit-a"\nversion = "0.1.0"\n\n[dependencies]\nx = "1"\n');
    await writeFile(path.join(root, "crates", "b", "Cargo.toml"), '[package]\nname = "rightkit-b"\npublish = false\n');
    await writeFile(path.join(root, "packages", "p", "package.json"), JSON.stringify({ name: "@rightkit/p" }));
    await writeFile(path.join(root, "packages", "q", "package.json"), JSON.stringify({ name: "@rightkit/q", private: true }));
    const publishables = rightkitPublishables(root);
    assert.deepEqual(publishables, { crates: ["rightkit-a"], packages: ["@rightkit/p"] });
    const seen = [];
    const fetchImpl = async (url) => { seen.push(url); return url.includes("crates.io") ? { ok: true, status: 200, json: async () => ({ crate: { max_stable_version: "0.1.0" } }) } : { ok: false, status: 404 }; };
    assert.deepEqual(await registryLatest(publishables, { fetchImpl }), [{ registry: "cargo", name: "rightkit-a", version: "0.1.0" }, { registry: "npm", name: "@rightkit/p", version: null }]);
    assert.deepEqual(seen.sort(), ["https://crates.io/api/v1/crates/rightkit-a", "https://registry.npmjs.org/@rightkit%2fp"]);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("consumer copies are byte-exact and only rewritten when they differ", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "rk-sync-copies-")); try {
    for (const dir of ["release", "build-control-clients/src", "build-control-policy/src"]) await mkdir(path.join(root, "packages", dir), { recursive: true });
    await writeFile(path.join(root, "packages", "release", "rightkit-ownership.json"), '{"capabilities":[]}\n');
    await writeFile(path.join(root, "packages", "release", "rightkit-versions.json"), '{"schema":2}\n');
    assert.equal(syncConsumerCopies(root).length, 5);
    assert.equal(await readFile(path.join(root, "packages", "build-control-policy", "src", "ownership-session-versions.json"), "utf8"), '{"schema":2}\n');
    assert.deepEqual(syncConsumerCopies(root), []);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("checked-in consumer copies match the canonical catalog and versions (run: node rightkit-sync.mjs)", async () => {
  const packages = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1")), "..");
  const pairs = [
    ["release/rightkit-ownership.json", "build-control-clients/src/ownership-catalog.json"],
    ["release/rightkit-versions.json", "build-control-clients/src/rightkit-versions.json"],
    ["release/rightkit-ownership.json", "build-control-policy/src/ownership-session-catalog.json"],
    ["release/rightkit-ownership.json", "build-control-policy/src/ownership-fingerprints.json"],
    ["release/rightkit-versions.json", "build-control-policy/src/ownership-session-versions.json"],
  ];
  for (const [source, copy] of pairs) assert.equal(await readFile(path.join(packages, copy), "utf8"), await readFile(path.join(packages, source), "utf8"), `${copy} is stale`);
});
