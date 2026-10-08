import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { applyRegistryLatest, compareVersions, recordPublished, registryLatest, rightkitPublishables, stageVersion, syncConsumerCopies, syncRightkitVersions } from "./rightkit-sync.mjs";

test("versions compare numerically, not lexically", () => {
  assert.ok(compareVersions("0.2.10", "0.2.9") > 0);
  assert.equal(compareVersions("0.1.0", "0.1.0"), 0);
  assert.ok(compareVersions("0.1.6", "0.2.0") < 0);
  const ordered = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0"];
  assert.deepEqual([...ordered].reverse().sort(compareVersions), ordered);
  assert.equal(compareVersions("1.0.0+build.1", "1.0.0+build.2"), 0);
});

test("newer published versions become staged; the replaced staged version stays allowed", () => {
  const versions = { npm: { "@rightkit/qa": "0.1.0" }, stagedNpm: { "@rightkit/qa": "0.2.3" }, cargo: { "rightkit-db": "0.1.0" }, stagedCargo: { "rightkit-db": "0.2.1", "rightkit-control": "0.1.5" }, registry: { npm: { "@rightkit/qa": "0.2.3" }, cargo: { "rightkit-control": "0.1.5" } } };
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

test("a registry never downgrades staged, and a published floor is retained in legacy", () => {
  const { versions: next, changes } = applyRegistryLatest({ cargo: { "rightkit-db": "0.2.0" }, stagedCargo: { "rightkit-db": "0.2.0" }, registry: { cargo: { "rightkit-db": "0.2.0" } } }, [{ registry: "cargo", name: "rightkit-db", version: "0.2.1" }, { registry: "cargo", name: "rightkit-fs", version: "0.0.9" }]);
  assert.equal(next.stagedCargo["rightkit-db"], "0.2.1"); assert.deepEqual(next.legacyCargo["rightkit-db"], ["0.2.0"]);
  const { changes: none } = applyRegistryLatest({ stagedCargo: { "rightkit-db": "0.3.0" } }, [{ registry: "cargo", name: "rightkit-db", version: "0.2.1" }]);
  assert.equal(none.length, 0); assert.equal(changes.length, 2);
});

test("publishables skip private crates and packages; registry lookups use both registries", async () => {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "rk-sync-"))); try {
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
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "rk-sync-copies-"))); try {
    for (const dir of ["release", "build-control-clients/src", "build-control-policy/src"]) await mkdir(path.join(root, "packages", dir), { recursive: true });
    await writeFile(path.join(root, "packages", "release", "rightkit-ownership.json"), '{"capabilities":[]}\n');
    await writeFile(path.join(root, "packages", "release", "rightkit-versions.json"), '{"schema":2}\n');
    await writeFile(path.join(root, "packages", "release", "rightkit-drift.mjs"), "export {};\n");
    await writeFile(path.join(root, "packages", "release", "background-process.mjs"), "export {};\n");
    await writeFile(path.join(root, "packages", "release", "ownership-source.mjs"), "export {};\n");
    await writeFile(path.join(root, "packages", "release", "ownership-availability.mjs"), "export {};\n");
    assert.equal(syncConsumerCopies(root).length, 10);
    assert.equal(await readFile(path.join(root, "packages", "build-control-policy", "src", "ownership-session-versions.json"), "utf8"), '{"schema":2}\n');
    assert.deepEqual(syncConsumerCopies(root), []);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("checked-in consumer copies match the canonical catalog and versions (run: node rightkit-sync.mjs)", async () => {
  const packages = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const pairs = [
    ["release/rightkit-ownership.json", "build-control-clients/src/ownership-catalog.json"],
    ["release/rightkit-versions.json", "build-control-clients/src/rightkit-versions.json"],
    ["release/rightkit-ownership.json", "build-control-policy/src/ownership-session-catalog.json"],
    ["release/rightkit-ownership.json", "build-control-policy/src/ownership-fingerprints.json"],
    ["release/rightkit-versions.json", "build-control-policy/src/ownership-session-versions.json"],
    ["release/rightkit-drift.mjs", "build-control-policy/src/rightkit-drift.mjs"],
    ["release/background-process.mjs", "build-control-policy/src/background-process.mjs"],
    ["release/ownership-source.mjs", "build-control-policy/src/ownership-source.mjs"],
    ["release/ownership-availability.mjs", "build-control-policy/src/ownership-availability.mjs"],
    ["release/ownership-availability.mjs", "build-control-clients/src/ownership-availability.mjs"],
  ];
  for (const [source, copy] of pairs) assert.equal(await readFile(path.join(packages, copy), "utf8"), await readFile(path.join(packages, source), "utf8"), `${copy} is stale`);
});

test("the registry block records what the registries serve, independent of staging", () => {
  const { versions: next, verified } = applyRegistryLatest({ stagedNpm: { "@rightkit/release": "0.2.122" } }, [{ registry: "npm", name: "@rightkit/release", version: "0.2.121" }, { registry: "cargo", name: "rightkit-x", version: null }]);
  assert.equal(next.stagedNpm["@rightkit/release"], "0.2.122", "staged is never downgraded");
  assert.deepEqual(next.registry, { cargo: {}, npm: { "@rightkit/release": "0.2.121" } }); assert.equal(verified, 1);
  assert.deepEqual(next.legacyNpm["@rightkit/release"], ["0.2.121"]);
});

const noFetch = async () => { throw new Error("unexpected registry request"); };
const latestFetch = (registry, version) => async (url) => {
  assert.ok(url.startsWith(registry === "npm" ? "https://registry.npmjs.org/" : "https://crates.io/api/v1/crates/"));
  return version === null ? { ok: false, status: 404 }
    : { ok: true, status: 200, json: async () => registry === "npm" ? { "dist-tags": { latest: version } } : { crate: { max_stable_version: version } } };
};

async function syncFixture(context, versions) {
  // Resolve /var -> /private/var on macOS before deriving any fixture paths.
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "rk-stage-")));
  context.after(() => rm(root, { recursive: true, force: true }));
  for (const dir of ["release", "build-control-clients/src", "build-control-policy/src"]) await mkdir(path.join(root, "packages", dir), { recursive: true });
  await mkdir(path.join(root, "crates"));
  const release = path.join(root, "packages", "release");
  const file = path.join(release, "rightkit-versions.json");
  await writeFile(file, `${JSON.stringify(versions, null, 2)}\n`);
  await writeFile(path.join(release, "rightkit-ownership.json"), '{"capabilities":[]}\n');
  for (const name of ["rightkit-drift.mjs", "background-process.mjs", "ownership-source.mjs", "ownership-availability.mjs"]) await writeFile(path.join(release, name), "export {};\n");
  syncConsumerCopies(root);
  return { root, file, log: () => {}, fetchImpl: noFetch };
}

async function assertVersionCopies({ root, file }) {
  const canonical = await readFile(file, "utf8");
  for (const copy of ["build-control-clients/src/rightkit-versions.json", "build-control-policy/src/ownership-session-versions.json"]) {
    assert.equal(await readFile(path.join(root, "packages", copy), "utf8"), canonical, `${copy} is stale`);
  }
  return JSON.parse(canonical);
}

for (const [registry, name, stagedKey, legacyKey] of [
  ["npm", "@rightkit/release", "stagedNpm", "legacyNpm"],
  ["cargo", "rightkit-shell", "stagedCargo", "legacyCargo"],
]) {
  test(`stage ${registry} retains recorded published pins, sorts, deduplicates & refreshes copies`, async (context) => {
    const options = await syncFixture(context, {
      [registry]: { [name]: "0.1.0" }, [stagedKey]: { [name]: "0.2.9" },
      registry: { [registry]: { [name]: "0.2.9" } },
      [legacyKey]: { [name]: ["0.2.9", "0.2.2", "0.2.1"] },
    });
    const result = await stageVersion({ registry, name, version: "0.2.10" }, options);
    const next = await assertVersionCopies(options);
    assert.equal(next[stagedKey][name], "0.2.10");
    assert.deepEqual(next[legacyKey][name], ["0.2.1", "0.2.2", "0.2.9"]);
    assert.equal(next[registry][name], "0.1.0", "floor remains unchanged");
    assert.equal(next.registry[registry][name], "0.2.9", "staging does not claim publication");
    assert.equal(result.written, true);
    assert.equal(result.copies.length, 2);
    assert.deepEqual(result.changes, [{ registry, name, from: "0.2.9", to: "0.2.10" }]);
    const repeat = await stageVersion({ registry, name, version: "0.2.10" }, options);
    assert.equal(repeat.written, false);
    assert.deepEqual(repeat.copies, []);
  });

  for (const source of ["staged", "floor"]) {
    test(`stage ${registry} retains ${source} when live registry latest proves publication`, async (context) => {
      const options = await syncFixture(context, {
        [registry]: { [name]: source === "floor" ? "0.2.9" : "0.1.0" },
        ...(source === "staged" ? { [stagedKey]: { [name]: "0.2.9" } } : {}),
      });
      await stageVersion({ registry, name, version: "0.2.10" }, { ...options, fetchImpl: latestFetch(registry, "0.2.9") });
      const next = await assertVersionCopies(options);
      assert.deepEqual(next[legacyKey][name], ["0.2.9"]);
      assert.equal(next[stagedKey][name], "0.2.10");
    });
  }

  test(`stage ${registry} retains a registry-recorded floor without a lookup`, async (context) => {
    const options = await syncFixture(context, { [registry]: { [name]: "0.2.9" }, registry: { [registry]: { [name]: "0.2.9" } } });
    await stageVersion({ registry, name, version: "0.2.10" }, options);
    const next = await assertVersionCopies(options);
    assert.deepEqual(next[legacyKey][name], ["0.2.9"]);
  });

  for (const published of ["0.1.0", null]) {
    test(`stage ${registry} skips unpublished previous staging (latest ${published}) & preserves legacy`, async (context) => {
      const options = await syncFixture(context, { [stagedKey]: { [name]: "0.2.9" }, [legacyKey]: { [name]: ["0.1.0"] } });
      await stageVersion({ registry, name, version: "0.2.10" }, { ...options, fetchImpl: latestFetch(registry, published) });
      const next = await assertVersionCopies(options);
      assert.deepEqual(next[legacyKey][name], ["0.1.0"]);
      assert.equal(next[stagedKey][name], "0.2.10");
    });
  }

  test(`stage ${registry} refuses downgrades before fetch or any writes`, async (context) => {
    const options = await syncFixture(context, { [stagedKey]: { [name]: "0.2.10" } });
    const before = await readFile(options.file, "utf8");
    for (const version of ["0.2.9", "0.2.10-rc.1"]) {
      await assert.rejects(stageVersion({ registry, name, version }, options), /refusing to stage/);
      assert.equal(await readFile(options.file, "utf8"), before);
      await assertVersionCopies(options);
    }
  });

  test(`recordPublished ${registry} retains published staged versions before overwriting registry evidence`, async (context) => {
    const options = await syncFixture(context, {
      [stagedKey]: { [name]: "0.2.9" }, registry: { [registry]: { [name]: "0.2.9" } },
      [legacyKey]: { [name]: ["0.2.1"] },
    });
    recordPublished({ registry, name, version: "0.2.10" }, options);
    const next = await assertVersionCopies(options);
    assert.deepEqual(next[legacyKey][name], ["0.2.1", "0.2.9"]);
    assert.equal(next[stagedKey][name], "0.2.10");
    assert.equal(next.registry[registry][name], "0.2.10");
  });

  test(`recordPublished ${registry} does not promote unpublished previous staging to legacy`, async (context) => {
    const options = await syncFixture(context, { [stagedKey]: { [name]: "0.2.9" } });
    recordPublished({ registry, name, version: "0.2.10" }, options);
    const next = await assertVersionCopies(options);
    assert.equal(next[legacyKey]?.[name], undefined);
    assert.equal(next[stagedKey][name], "0.2.10");
  });

  test(`recordPublished ${registry} retains the old registry pin even if previous staging was unpublished`, async (context) => {
    const options = await syncFixture(context, {
      [stagedKey]: { [name]: "0.2.9" }, registry: { [registry]: { [name]: "0.2.8" } },
    });
    recordPublished({ registry, name, version: "0.2.10" }, options);
    const next = await assertVersionCopies(options);
    assert.deepEqual(next[legacyKey][name], ["0.2.8"]);
    assert.equal(next.registry[registry][name], "0.2.10");
  });
}

test("stage preserves prerelease legacy entries in semver order", async (context) => {
  const name = "@rightkit/release";
  const options = await syncFixture(context, {
    stagedNpm: { [name]: "1.0.0-rc.1" }, registry: { npm: { [name]: "1.0.0-rc.1" } },
    legacyNpm: { [name]: ["1.0.0-beta.11", "1.0.0-beta.2", "0.2.10", "0.2.9"] },
  });
  await stageVersion({ registry: "npm", name, version: "1.0.0" }, options);
  const next = await assertVersionCopies(options);
  assert.deepEqual(next.legacyNpm[name], ["0.2.9", "0.2.10", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1"]);
});

test("stage adds new names without network & validates inputs before writes", async (context) => {
  const options = await syncFixture(context, {});
  const before = await readFile(options.file, "utf8");
  for (const entry of [
    { registry: "swift", name: "rightkit-swift", version: "0.1.0" },
    { registry: "npm", name: "", version: "0.1.0" },
    { registry: "npm", name: "@rightkit/x", version: "next" },
    { registry: "npm", name: "@rightkit/x", version: "01.0.0" },
  ]) {
    await assert.rejects(stageVersion(entry, options), /unknown registry|requires.*name|invalid semver/);
    assert.equal(await readFile(options.file, "utf8"), before);
  }
  await stageVersion({ registry: "npm", name: "@rightkit/x", version: "0.1.0" }, options);
  const next = await assertVersionCopies(options);
  assert.equal(next.stagedNpm["@rightkit/x"], "0.1.0");
  assert.equal(next.legacyNpm, undefined);
});

test("stage registry failure leaves table & copies unchanged", async (context) => {
  const options = await syncFixture(context, { stagedNpm: { "@rightkit/release": "0.2.9" } });
  const before = await readFile(options.file, "utf8");
  await assert.rejects(stageVersion({ registry: "npm", name: "@rightkit/release", version: "0.2.10" }, {
    ...options, fetchImpl: async () => ({ ok: false, status: 503 }),
  }), /HTTP 503/);
  assert.equal(await readFile(options.file, "utf8"), before);
  await assertVersionCopies(options);
});

test("sync writes legacy-only repairs even when registry & staging already match recorded values", async (context) => {
  const name = "@rightkit/release";
  const options = await syncFixture(context, { stagedNpm: { [name]: "0.2.10" }, registry: { npm: { [name]: "0.2.9" } } });
  await writeFile(path.join(options.root, "packages", "release", "package.json"), JSON.stringify({ name }));
  const fetchImpl = latestFetch("npm", "0.2.9");
  const before = await readFile(options.file, "utf8");
  const checked = await syncRightkitVersions({ ...options, fetchImpl, check: true });
  assert.equal(checked.written, false);
  assert.equal(await readFile(options.file, "utf8"), before);
  const result = await syncRightkitVersions({ ...options, fetchImpl });
  assert.equal(result.written, true);
  assert.deepEqual(result.changes, []);
  const next = await assertVersionCopies(options);
  assert.deepEqual(next.legacyNpm[name], ["0.2.9"]);
});

test("stage CLI dispatches npm & cargo staging, rejects malformed args & downgrades", async (context) => {
  const options = await syncFixture(context, {
    stagedNpm: { "@rightkit/release": "0.2.9" }, stagedCargo: { "rightkit-shell": "0.2.9" },
    registry: { npm: { "@rightkit/release": "0.2.9" }, cargo: { "rightkit-shell": "0.2.9" } },
  });
  const script = path.join(options.root, "packages", "release", "rightkit-sync.mjs");
  await writeFile(script, await readFile(new URL("./rightkit-sync.mjs", import.meta.url)));
  const run = (...args) => spawnSync(process.execPath, [script, ...args], { cwd: options.root, encoding: "utf8", timeout: 5_000 });
  for (const [registry, name, stagedKey, legacyKey] of [["npm", "@rightkit/release", "stagedNpm", "legacyNpm"], ["cargo", "rightkit-shell", "stagedCargo", "legacyCargo"]]) {
    const result = run("stage", registry, name, "0.2.10");
    assert.equal(result.status, 0, result.stderr || result.error?.message);
    const next = await assertVersionCopies(options);
    assert.equal(next[stagedKey][name], "0.2.10");
    assert.deepEqual(next[legacyKey][name], ["0.2.9"]);
  }
  const before = await readFile(options.file, "utf8");
  for (const args of [["stage", "npm"], ["stage", "swift", "rightkit-swift", "0.1.0"], ["stage", "npm", "@rightkit/release", "0.2.9"]]) {
    const result = run(...args);
    assert.equal(result.status, 1, result.stderr || result.error?.message);
    assert.match(result.stderr, /usage:|unknown registry|refusing to stage/);
    assert.equal(await readFile(options.file, "utf8"), before);
  }
});
