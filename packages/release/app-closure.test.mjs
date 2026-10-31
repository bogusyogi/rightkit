import assert from "node:assert/strict";
import test from "node:test";
import { cargoMetadataFromManifests, closureOf, manifestDependencyFacts, scopedPlan, scopeScans } from "./app-closure.mjs";
import { digestScanSet } from "./scan-digest.mjs";

const rustUnits = ["app", "core", "other"].map((name) => ({ name, dir: `crates/${name}`, cargoDir: `crates/${name}` }));
const sources = {
  "crates/app/Cargo.toml": "[package]\nname='app'\n[workspace]\n[dependencies]\nrenamed = { package = 'core', path = '../core' }\n",
  "crates/core/Cargo.toml": "[package]\nname='core'\n[workspace]\n",
  "crates/other/Cargo.toml": "[package]\nname='other'\n[workspace]\n",
};
const read = (file) => { if (!(file in sources)) throw new Error(`missing ${file}`); return sources[file]; };
const closure = () => closureOf({ scopeDirs: ["crates/app"], rustUnits, readFile: read });

test("unit scope follows parsed single-quoted, dotted, renamed & target Cargo dependency paths", () => {
  assert.deepEqual(manifestDependencyFacts("[dependencies]\na.path = '../a#literal'\n[target.'cfg(unix)'.build-dependencies.renamed]\npackage='b'\npath='../b'\n[dev-dependencies]\nc = { workspace = true }\n"), { paths: ["../a#literal", "../b"], inherits: true });
  assert.deepEqual([...closure().rust].sort(), ["app", "core"]);
});

test("workspace inherited paths resolve only named dependencies & include transitive edges", () => {
  const files = {
    "Cargo.toml": "[workspace]\nmembers=['crates/app','crates/core','crates/other']\n[workspace.dependencies]\nrenamed={package='core',path='crates/core'}\nunused={path='crates/other'}\n",
    "crates/app/Cargo.toml": "[package]\nname='app'\n[dependencies]\nrenamed.workspace=true\n",
    "crates/core/Cargo.toml": "[package]\nname='core'\n",
  };
  const c = closureOf({ scopeDirs: ["crates/app"], rustUnits: rustUnits.map((u) => ({ ...u, cargoDir: "." })), readFile: (file) => { if (!(file in files)) throw new Error(file); return files[file]; } });
  assert.deepEqual([...c.rust].sort(), ["app", "core"]);
});

test("unknown, unreadable, invalid & escaped dependency closures refuse instead of omitting or widening execution", () => {
  for (const body of ["[dependencies]\na={path='../missing'}", "[dependencies]\na={workspace=true}", "[dependencies]\na={path='../../../escape'}", "[dependencies]\na={path="]) {
    assert.throws(() => closureOf({ scopeDirs: ["crates/app"], rustUnits, readFile: (file) => file === "crates/app/Cargo.toml" ? body : read(file) }), /RK_SCOPE_CLOSURE/);
  }
  assert.throws(() => closureOf({ scopeDirs: ["crates/app"], rustUnits, readFile: () => { throw new Error("unreadable"); } }), /RK_SCOPE_CLOSURE.*unreadable/);
  assert.throws(() => closureOf({ scopeDirs: ["absent"], rustUnits, readFile: read }), /contains no committed/);
});

test("selected Rust scope retains dependencies & repository file-only scans, excludes unrelated Rust/npm execution", () => {
  const ids = ["secret-scan", "license", "cargo-deny:other", "pnpm-audit:templates/desktop", "pnpm-lockfile:templates/desktop", "rustfmt:2021", "check:app", "test:rust:core", "test:rust:other", "tsc:desktop", "test:npm:desktop", "generated-docs", "suite-api", "test:scripts-ci"];
  const { scans, dropped } = scopeScans(ids.map((id) => ({ id })), closure(), { rustUnits });
  assert.deepEqual(scans.map((s) => s.id), ids.slice(0, 8));
  assert.deepEqual(dropped.map((s) => s.id), ids.slice(8));
  const blockedBy = (red) => scans.some((s) => red.includes(s.id));
  assert.equal(blockedBy(["test:rust:other", "test:npm:desktop"]), false);
  assert.equal(blockedBy(["test:rust:core"]), true);
  assert.equal(blockedBy(["secret-scan"]), true);
});

test("npm local/workspace dependencies keep their install root; unresolved workspace dependency refuses", () => {
  const units = [{ name: "app", dir: "apps/app" }, { name: "core", dir: "packages/core" }, { name: "desktop", dir: "templates/desktop", root: "templates/desktop" }];
  const docs = { "apps/app/package.json": '{"name":"app","dependencies":{"alias":"workspace:core@*"}}', "packages/core/package.json": '{"name":"core"}', "templates/desktop/package.json": '{"name":"desktop"}' };
  const c = closureOf({ scopeDirs: ["apps/app"], npmUnits: units, readFile: (p) => docs[p] });
  assert.deepEqual([...c.npm].sort(), ["app", "core"]);
  assert.deepEqual([...new Set(units.filter((u) => c.npm.has(u.name)).map((u) => u.root ?? "."))], ["."]);
  docs["apps/app/package.json"] = '{"name":"app","dependencies":{"unknown":"workspace:*"}}';
  assert.throws(() => closureOf({ scopeDirs: ["apps/app"], npmUnits: units, readFile: (p) => docs[p] }), /unknown.*no committed npm unit/);
});

test("source-only Cargo inventory preserves workspace ownership & independent component identity", () => {
  const metadata = cargoMetadataFromManifests({ manifests: Object.keys(sources), readFile: read });
  assert.deepEqual(metadata("crates/app"), { cargoDir: "crates/app", members: [{ name: "app", dir: "crates/app", manifest: "crates/app/Cargo.toml", publish: true }] });
  const docs = { "Cargo.toml": "[workspace]\nmembers=['crates/*']\nexclude=['crates/other']\n", "crates/app/Cargo.toml": "[package]\nname='app'\n", "crates/other/Cargo.toml": "[package]\nname='other'\n[workspace]\n" };
  const ws = cargoMetadataFromManifests({ manifests: Object.keys(docs), readFile: (f) => docs[f] });
  assert.equal(ws("crates/app").cargoDir, ".");
  assert.equal(ws("crates/other").cargoDir, "crates/other");
});

test("scope/cache evidence stays reusable for its own scope, never another unit or unscoped admission", () => {
  const scans = [{ id: "secret-scan", class: "A" }];
  const plan = { inputDigest: "same-tree", deferred: [], scans };
  const one = scopedPlan(plan, scans, closure());
  const again = scopedPlan(plan, scans, closure());
  const other = scopedPlan(plan, scans, closureOf({ scopeDirs: ["crates/other"], rustUnits, readFile: read }));
  assert.equal(one.scopeDigest, again.scopeDigest);
  assert.equal(one.inputDigest, again.inputDigest);
  assert.notEqual(one.inputDigest, other.inputDigest);
  assert.notEqual(one.inputDigest, plan.inputDigest);
  assert.notEqual(one.scopeDigest, other.scopeDigest);
  assert.equal(digestScanSet(one.scans), digestScanSet(again.scans));
});

test("scoped TypeScript maps only closure siblings & unknown executing rows refuse", () => {
  const c = { dirs: ["apps/a"], npm: new Set(["a", "core"]), rust: new Set(), dependencyDirs: ["apps/a", "packages/core"] };
  const kept = scopeScans([{ id: "tsc:a", class: "A", executes: true, tscWorkspace: ["apps/a", "packages/core", "packages/unrelated"] }, { id: "swift-build:unrelated", class: "B" }], c);
  assert.deepEqual(kept.scans[0].tscWorkspace, ["apps/a", "packages/core"]);
  assert.deepEqual(kept.dropped.map((s) => s.id), ["swift-build:unrelated"]);
  assert.throws(() => scopeScans([{ id: "unknown-execution", class: "A", executes: true }], c), /no exact scope owner/);
});
