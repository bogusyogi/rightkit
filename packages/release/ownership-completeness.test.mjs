import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { parse as parseToml } from "smol-toml";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { capabilityTable, scanApp, validateManifest } from "./ownership.mjs";
import { CATALOG_CONFIG, generateOwnershipCatalog } from "./ownership-catalog.mjs";
import { OWNERSHIP_APPS, publishedAvailability } from "./ownership-report.mjs";

const rightkitRoot = path.resolve(import.meta.dirname, "../..");
const catalog = JSON.parse(readFileSync(new URL("./rightkit-ownership.json", import.meta.url)));
const versions = JSON.parse(readFileSync(new URL("./rightkit-versions.json", import.meta.url)));
// Standalone native harnesses exercise shared crates; they are not owners.
const TEST_CRATES = Object.fromEntries(CATALOG_CONFIG.testOwners.map((entry) => [entry.path, { name: entry.package, reason: entry.reason }]));

function manifests(root, group, basename) {
  const files = [];
  function visit(dir) {
    if (!existsSync(dir)) return;
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory() && !["node_modules", "target", ".git"].includes(entry.name)) visit(path.join(dir, entry.name));
      else if (entry.isFile() && entry.name === basename) files.push(path.relative(root, path.join(dir, entry.name)).split(path.sep).join("/"));
    }
  }
  visit(path.join(root, group));
  return files;
}

function gaps(root, manifest) {
  const owners = new Set(manifest.capabilities.map((entry) => entry.package));
  const missing = [];
  for (const file of manifests(root, "crates", "Cargo.toml")) {
    const pkg = parseToml(readFileSync(path.join(root, file), "utf8")).package;
    if (!pkg) continue;
    const exception = TEST_CRATES[file];
    if (exception) {
      assert.equal(pkg.name, exception.name, `stale test-only allowlist: ${file}`);
      assert.equal(pkg.publish, false, `allowlisted test crate must stay unpublished: ${file}`);
      assert.ok(exception.reason);
    } else if (!owners.has(pkg.name)) missing.push(`${file}: ${pkg.name}`);
  }
  for (const file of manifests(root, "packages", "package.json")) {
    const pkg = JSON.parse(readFileSync(path.join(root, file), "utf8"));
    if (!pkg.private && !owners.has(pkg.name)) missing.push(`${file}: ${pkg.name}`);
  }
  const swift = path.join(root, "swift/RightKitSwift/Package.swift");
  if (existsSync(swift) && !owners.has("RightKitSwift")) missing.push("swift/RightKitSwift/Package.swift: RightKitSwift");
  return missing;
}

test("every crate and public npm package has a catalog owner, except named unpublished test hosts", () => {
  assert.deepEqual(gaps(rightkitRoot, catalog), []);
  for (const file of Object.keys(TEST_CRATES)) assert.ok(existsSync(path.join(rightkitRoot, file)), `remove stale test-only allowlist: ${file}`);
  assert.ok(existsSync(path.join(rightkitRoot, "swift/RightKitSwift/Package.swift")));
  assert.deepEqual(catalog, generateOwnershipCatalog(rightkitRoot));
});

test("completeness detects new unpublished crates, nested crates and public npm packages", () => {
  const root = mkdtempSync(path.join(tmpdir(), "rk-owner-gaps-"));
  try {
    for (const dir of ["crates/new", "crates/new/child", "packages/new", "swift/RightKitSwift"]) mkdirSync(path.join(root, dir), { recursive: true });
    writeFileSync(path.join(root, "crates/new/Cargo.toml"), '[package]\nname = "new-owner"\npublish = false\n');
    writeFileSync(path.join(root, "crates/new/child/Cargo.toml"), '[package]\nname = "nested-owner"\n');
    writeFileSync(path.join(root, "packages/new/package.json"), '{"name":"@rightkit/new"}');
    writeFileSync(path.join(root, "swift/RightKitSwift/Package.swift"), 'let package = Package(name: "RightKitSwift")');
    assert.equal(gaps(root, { capabilities: [] }).length, 4);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("build-control packages are reasoned private internal owners, excluded from app capabilities", () => {
  const internal = catalog.internalOwners;
  assert.ok(internal.length);
  for (const entry of internal) {
    assert.ok(entry.reason);
    const pkg = JSON.parse(readFileSync(path.join(rightkitRoot, entry.path), "utf8"));
    assert.equal(pkg.name, entry.package);
    assert.equal(pkg.private, true);
    assert.ok(!catalog.capabilities.some((owner) => owner.package === entry.package));
  }
  for (const file of manifests(rightkitRoot, "packages", "package.json")) {
    const pkg = JSON.parse(readFileSync(path.join(rightkitRoot, file), "utf8"));
    if (pkg.name?.startsWith("@rightkit/build-control-")) assert.ok(internal.some((entry) => entry.path === file && entry.package === pkg.name), file);
  }
});

test("all catalog owners resolve availability from the registry manifest without invented workspace versions", () => {
  for (const entry of catalog.capabilities) {
    const recorded = entry.package === "RightKitSwift" ? versions.swift["rightkit-swift"].version
      : entry.package.startsWith("@rightkit/") ? versions.registry?.npm?.[entry.package] ?? versions.npm?.[entry.package] ?? versions.stagedNpm?.[entry.package]
      : versions.registry?.cargo?.[entry.package] ?? versions.cargo?.[entry.package] ?? versions.stagedCargo?.[entry.package];
    assert.equal(publishedAvailability(entry, versions)[0]?.version, recorded, entry.package);
  }
});

test("declaration-only owners are valid and grouped owners detect every declared member", () => {
  assert.doesNotThrow(() => validateManifest(catalog));
  const clean = { violations: [], covered: [], coverage: [] };
  for (const entry of catalog.capabilities) {
    const rows = capabilityTable(catalog, [{ name: entry.package }], clean);
    assert.equal(rows.find((row) => row.capability === entry.capability).status, "declared-dependency", entry.package);
    if (!entry.fingerprints.length) assert.equal(entry.detection, "declaration-only");
  }
});

test("Pulse includes native and hub code; PostRight and Legion use repository-wide roots", () => {
  for (const [key, root] of [["pulse", "cockpit"], ["postright", "postright"], ["legion", "legion"]]) {
    assert.deepEqual(OWNERSHIP_APPS.find((app) => app.key === key), { key, root, scope: "repository" });
  }
});

test("provider request fingerprint ignores comments, examples, unrelated domains and adoption aliases", () => {
  const root = mkdtempSync(path.join(tmpdir(), "rk-owner-precision-"));
  try {
    assert.equal(spawnSync("git", ["-C", root, "init", "-q"], { windowsHide: true }).status, 0);
    mkdirSync(path.join(root, "src"));
    writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "consumer"\nversion = "0.1.0"\n[dependencies]\nqts = { package = "rightkit-tts-qwen", version = "0.2.1" }\n');
    writeFileSync(path.join(root, "src/main.rs"), [
      '// client.post("https://api.anthropic.com/v1/messages");',
      'let example = r#"client.post("https://api.openai.com/v1/responses")"#;',
      'client.post("https://example.com/v1/messages");',
      'client.post("https://api.anthropic.com.example.com/v1/messages");',
      'let docs = "https://api.openai.com/v1/responses";',
      'client.post("https://api.anthropic.com/v1/messages");',
    ].join("\n"));
    const findings = scanApp(root, catalog, { trust: null });
    assert.equal(findings.filter((f) => f.fingerprint === "llm.endpoint.provider-request").length, 1);
    assert.ok(!findings.some((f) => f.fingerprint === "qts.cargo.qts"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("new dependency fingerprints match exact Cargo/npm declarations, not shared runtime dependencies", () => {
  const root = mkdtempSync(path.join(tmpdir(), "rk-owner-deps-"));
  try {
    assert.equal(spawnSync("git", ["-C", root, "init", "-q"], { windowsHide: true }).status, 0);
    const fingerprints = catalog.capabilities.flatMap((entry) => entry.fingerprints.filter((f) => f.type === "dependency"));
    const cargo = [...new Set(fingerprints.filter((f) => f.ecosystem === "cargo").map((f) => f.name))];
    const npm = [...new Set(fingerprints.filter((f) => f.ecosystem === "npm").map((f) => f.name))];
    writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "consumer"\nversion = "0.1.0"\n[dependencies]\n' + cargo.map((name) => `"${name}" = "1"`).join("\n"));
    writeFileSync(path.join(root, "package.json"), JSON.stringify({ dependencies: Object.fromEntries(npm.map((name) => [name, "1"])) }));
    const swift = [...new Set(fingerprints.filter((f) => f.ecosystem === "swiftpm").map((f) => f.name))];
    writeFileSync(path.join(root, "Package.swift"), `// swift-tools-version:5.9\nimport PackageDescription\nlet package = Package(name: "consumer", dependencies: [\n${swift.map((name) => `  .package(url: "https://github.com/example/${name}.git", from: "1.0.0"),`).join("\n")}\n])\n`);
    const found = new Set(scanApp(root, catalog, { trust: null }).map((f) => f.fingerprint));
    for (const fingerprint of fingerprints) assert.ok(found.has(fingerprint.id), fingerprint.id);
    writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "consumer"\nversion = "0.1.0"\n[dependencies]\nserde = "1"\ntokio = "1"\nsha2 = "1"\ntauri = "2"\n');
    writeFileSync(path.join(root, "package.json"), '{"dependencies":{"react":"19","typescript":"5"}}');
    writeFileSync(path.join(root, "Package.swift"), '// swift-tools-version:5.9\nimport PackageDescription\nlet package = Package(name: "consumer", dependencies: [\n  .package(url: "https://github.com/apple/swift-argument-parser.git", from: "1.0.0"),\n])\n');
    assert.deepEqual([...scanApp(root, catalog, { trust: null })], []);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
