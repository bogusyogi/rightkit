import assert from "node:assert/strict";
import { execFileSync } from "./background-process.mjs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { applyRightkitAdoption, breakingBump, formatDrift, rightkitDrift } from "./rightkit-drift.mjs";

const versions = { npm: { "@rightkit/logs": "0.1.3" }, stagedNpm: { "@rightkit/logs": "0.1.4", "@rightkit/theme": "0.2.1" }, cargo: { "rightkit-db": "0.1.0" }, stagedCargo: { "rightkit-db": "0.2.1", "rightkit-logs": "0.2.0", "rightkit-device": "0.1.1" } };

async function app() {
  const root = await mkdtemp(path.join(tmpdir(), "rk-drift-"));
  await mkdir(path.join(root, "src-tauri"), { recursive: true }); await mkdir(path.join(root, "rightkit"), { recursive: true });
  await writeFile(path.join(root, "package.json"), JSON.stringify({ name: "app", dependencies: { "@rightkit/logs": "0.1.3", "@rightkit/theme": "workspace:*", react: "19.0.0" }, devDependencies: { "@rightkit/theme": "^0.1.0" } }, null, 2) + "\n");
  await writeFile(path.join(root, "src-tauri", "Cargo.toml"), [
    "[dependencies]",
    'rightkit-db = { version = "=0.2.0", features = ["x"] }',
    'rightkit-device = "0.1.0"',
    'rightkit-logs = "=0.1.0"',
    'rightkit-local = { version = "=0.1.0", path = "../local" }',
    'serde = "1"', "",
  ].join("\n"));
  await writeFile(path.join(root, "rightkit", "Cargo.toml"), '[dependencies]\nrightkit-db = "=0.1.0"\n');
  const git = (...args) => execFileSync("git", ["-C", root, ...args], { stdio: "ignore" });
  git("init"); git("add", ".");
  return root;
}

test("breaking bumps follow 0.x semver", () => {
  assert.equal(breakingBump("0.1.0", "0.1.4"), false);
  assert.equal(breakingBump("0.1.0", "0.2.0"), true);
  assert.equal(breakingBump("1.2.0", "1.9.0"), false);
  assert.equal(breakingBump("1.2.0", "2.0.0"), true);
});

test("drift lists behind pins only, skipping workspace, path and RightKit's own tree", async () => {
  const root = await app(); try {
    const drift = rightkitDrift(root, versions).map((d) => `${d.name}:${d.current}->${d.latest}:${d.breaking}:${d.file}`).sort();
    assert.deepEqual(drift, [
      "@rightkit/logs:0.1.3->0.1.4:false:package.json",
      "@rightkit/theme:0.1.0->0.2.1:true:package.json",
      "rightkit-db:0.2.0->0.2.1:false:src-tauri/Cargo.toml",
      "rightkit-device:0.1.0->0.1.1:false:src-tauri/Cargo.toml",
      "rightkit-logs:0.1.0->0.2.0:true:src-tauri/Cargo.toml",
    ]);
    assert.match(formatDrift(rightkitDrift(root, versions)), /rightkit-logs 0\.1\.0 -> 0\.2\.0 \(breaking\)/);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("apply adopts compatible bumps, keeps pin prefixes, and leaves breaking bumps for --all", async () => {
  const root = await app(); try {
    const adopted = applyRightkitAdoption(root, versions, { refreshLocks: false });
    assert.deepEqual(adopted.map((d) => d.name).sort(), ["@rightkit/logs", "rightkit-db", "rightkit-device"]);
    const cargo = await readFile(path.join(root, "src-tauri", "Cargo.toml"), "utf8");
    assert.match(cargo, /rightkit-db = \{ version = "=0\.2\.1", features = \["x"\] \}/);
    assert.match(cargo, /rightkit-device = "0\.1\.1"/); assert.match(cargo, /rightkit-logs = "=0\.1\.0"/); assert.match(cargo, /rightkit-local = \{ version = "=0\.1\.0", path/);
    const pkg = JSON.parse(await readFile(path.join(root, "package.json"), "utf8"));
    assert.equal(pkg.dependencies["@rightkit/logs"], "0.1.4"); assert.equal(pkg.devDependencies["@rightkit/theme"], "^0.1.0"); assert.equal(pkg.dependencies.react, "19.0.0");
    applyRightkitAdoption(root, versions, { refreshLocks: false, includeBreaking: true });
    assert.equal(JSON.parse(await readFile(path.join(root, "package.json"), "utf8")).devDependencies["@rightkit/theme"], "^0.2.1");
    assert.deepEqual(rightkitDrift(root, versions), []);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("with a registry block, only versions the registry serves are recommended", async () => {
  const root = await app(); try {
    const registryOnly = { ...versions, registry: { cargo: { "rightkit-db": "0.2.1" }, npm: {} } };
    assert.deepEqual(rightkitDrift(root, registryOnly).map((d) => `${d.name}:${d.latest}`), ["rightkit-db:0.2.1"]);
  } finally { await rm(root, { recursive: true, force: true }); }
});
