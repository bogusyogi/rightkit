import "./test-support/broker-guard.mjs";
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

// GB-15 (GATE 9/86, E17) rows: an app or unit stage runs the executing Class A scans for its dependency closure only; file-only scans stay
// repository-wide. Unreadable or unparseable manifests refuse (RK_SCOPE_CLOSURE), never widen or narrow.
import { doorFixture } from "./test-support/door-fixture.mjs";

const POSIX_SIM = process.platform === "win32" ? "simulates a Linux host, whose ownership reads ps" : false;
const files = {
  "pnpm-workspace.yaml": "packages:\n  - 'apps/*'\n  - 'packages/*'\n",
  "package.json": '{"name":"root","private":true}\n',
  "apps/a/package.json": '{"name":"app-a","scripts":{"test":"vitest run"},"dependencies":{"lib":"workspace:*"}}\n',
  "apps/a/right-release.config.json": "{}\n",
  "apps/b/package.json": '{"name":"app-b","scripts":{"test":"vitest run"}}\n',
  "packages/lib/package.json": '{"name":"lib","scripts":{"test":"vitest run"}}\n',
  "packages/other/package.json": '{"name":"other","scripts":{"test":"vitest run"}}\n',
};
const npmUnits = [{ name: "app-a", dir: "apps/a", root: "." }, { name: "app-b", dir: "apps/b", root: "." }, { name: "lib", dir: "packages/lib", root: "." }, { name: "other", dir: "packages/other", root: "." }];
const readFiles = (rel) => { if (!(rel in files)) throw new Error(`no ${rel}`); return files[rel]; };
const scansOf = (...ids) => ids.map((id) => ({ id }));

test("[GB-15] the closure of an app is its units plus their workspace dependencies; an unrelated unit is outside", () => {
  const c = closureOf({ scopeDirs: ["apps/a"], npmUnits, rustUnits: [], readFile: (r) => (r === "apps/a/package.json" ? '{"name":"app-a","dependencies":{"lib":"workspace:*"}}' : r === "packages/lib/package.json" ? '{"name":"lib"}' : readFiles(r)) });
  assert.deepEqual([...c.npm].sort(), ["app-a", "lib"]);
  const kept = scopeScans(scansOf("secret-scan", "license", "test:npm:app-a", "test:npm:app-b", "tsc:lib", "eslint:other", "shellcheck"), c);
  assert.deepEqual(kept.scans.map((s) => s.id), ["secret-scan", "license", "test:npm:app-a", "tsc:lib", "shellcheck"], "file-only scans and the closure's executing scans stay");
  assert.deepEqual(kept.dropped.map((s) => s.id), ["test:npm:app-b", "eslint:other"]);
  const whole = closureOf({ scopeDirs: ["."], npmUnits, rustUnits: [], readFile: readFiles });
  assert.equal(whole.npm.size, 4, "the repository-root scope is the whole repository");
});

test("[GB-15] Rust closure follows path dependencies; an inherited workspace dependency without a workspace entry or an unreadable manifest refuses", () => {
  const rustUnits = [{ name: "app", dir: "apps/a/src-tauri", cargoDir: "apps/a/src-tauri" }, { name: "core", dir: "crates/core", cargoDir: "crates/core" }, { name: "x", dir: "crates/x", cargoDir: "crates/x" }, { name: "w1", dir: "ws/w1", cargoDir: "ws" }, { name: "w2", dir: "ws/w2", cargoDir: "ws" }];
  const manifests = {
    "apps/a/src-tauri/Cargo.toml": '[package]\nname="app"\n[dependencies]\ncore = { path = "../../../crates/core" }\n[lib]\npath = "src/lib.rs"\n',
    "crates/core/Cargo.toml": '[package]\nname="core"\nversion.workspace = true\n[dependencies]\nserde = "1"\n',
  };
  const c = closureOf({ scopeDirs: ["apps/a"], npmUnits: [], rustUnits, readFile: (r) => { if (r in manifests) return manifests[r]; throw new Error("absent"); } });
  assert.deepEqual([...c.rust].sort(), ["app", "core"], "path dependency followed; [package] workspace inheritance and an unrelated crate do not widen");
  assert.throws(() => closureOf({ scopeDirs: ["ws/w1"], npmUnits: [], rustUnits, readFile: (r) => (r === "ws/w1/Cargo.toml" ? '[dependencies]\nfoo = { workspace = true }\n' : "") }), /RK_SCOPE_CLOSURE.*foo/, "an inherited dependency the workspace does not define refuses, never widens");
  assert.throws(() => closureOf({ scopeDirs: ["crates/x"], npmUnits: [], rustUnits, readFile: () => { throw new Error("absent"); } }), /RK_SCOPE_CLOSURE/, "an unreadable manifest refuses");
  assert.deepEqual(scopeScans(scansOf("test:rust:app", "clippy:core:1", "check:x", "doc:w2", "cargo-deny", "test:rust-serial:x"), c, { rustUnits }).dropped.map((s) => s.id), ["check:x", "doc:w2", "test:rust-serial:x"]);
  assert.deepEqual(manifestDependencyFacts('[dependencies]\na = { path = "../a" }\n[target.\'cfg(unix)\'.dev-dependencies]\nb = { workspace = true }\n'), { paths: ["../a"], inherits: true });
});

test("[GB-15] Cargo path dependencies in TOML literal strings, inline-table spacing variants, dotted and quoted keys are all read; a manifest or path key it cannot read refuses", () => {
  const facts = (body) => manifestDependencyFacts(`[dependencies]\n${body}\n`).paths;
  assert.deepEqual(facts("a = { path = '../a' }"), ["../a"], "single-quoted literal string");
  assert.deepEqual(facts("a = {path='../a'}"), ["../a"], "no spaces");
  assert.deepEqual(facts('a = {   path   =   "../a" , version = "1" }'), ["../a"], "extra spaces and a following key");
  assert.deepEqual(facts("a = { version = '1', path = '../a' } # path = '../ignored'"), ["../a"], "comment text is not read");
  assert.deepEqual(facts(`a.path = '../a'\n"b" = { "path" = "../b" }\nc = { 'path' = '../c' }`), ["../a", "../b", "../c"], "dotted and quoted keys");
  assert.deepEqual(facts(String.raw`a = { path = "../we\\ird#x" }`), ["../we\\ird#x"], "escapes and # inside a basic string");
  assert.deepEqual(manifestDependencyFacts("[dependencies.a]\npath = '../a'\n[workspace.dependencies]\nb = { path = '../b' }\n").paths, ["../a"], "a key in a dependency table is read; [workspace.dependencies] edges are followed through the inheriting unit, not read here");
  assert.deepEqual(facts(`a = { git = "https://x/path = '../no'" }`), [], "a path inside another string is not a key");
  assert.deepEqual(facts("a = { path = '''../a''' }"), ["../a"], "a TOML multi-line literal string is a valid path value");
  assert.deepEqual(facts('a = { path = """../a""" }'), ["../a"], "a TOML multi-line basic string is a valid path value");
  const unparseable = ["a = { path = ../a }", 'a = { path = "../a }', 'a = { path = ["../a"] }', String.raw`a = { path = "../\q" }`, "a = { path ="];
  for (const bad of unparseable) {
    assert.throws(() => manifestDependencyFacts(`[package]\nname = "x"\n[dependencies]\n${bad}\n`), /RK_SCOPE_CLOSURE/, bad);
  }
  assert.throws(() => manifestDependencyFacts("[lib]\npath = unquoted\n"), /RK_SCOPE_CLOSURE.*cannot be parsed/, "an unparseable manifest refuses even outside a dependency table");
  const rustUnits = [{ name: "app", dir: "app", cargoDir: "." }, { name: "core", dir: "core", cargoDir: "." }, { name: "other", dir: "other", cargoDir: "." }];
  const literal = closureOf({ scopeDirs: ["app"], npmUnits: [], rustUnits, readFile: (r) => (r === "app/Cargo.toml" ? "[dependencies]\ncore = { path = '../core' }\n" : "") });
  assert.deepEqual([...literal.rust].sort(), ["app", "core"], "a literal-string path dependency is followed");
  assert.throws(() => closureOf({ scopeDirs: ["app"], npmUnits: [], rustUnits, readFile: () => "[dependencies]\ncore = { path = ../core }\n" }), /RK_SCOPE_CLOSURE/, "an unparseable path key refuses, never omits or widens");
});

test("[GB-15] a red unit outside the app's closure does not block the app; a red one inside does; repository-wide scans keep their coverage (real snapshot route)", { skip: POSIX_SIM }, async (t) => {
  const fx = doorFixture(t, files, "rk-gb15-");
  const planned = [];
  const seams = (red) => ({ cwd: `${fx.repo}/apps/a`, runCheck: undefined, runScans: async (plan) => { planned.push(plan.scans.map((s) => s.id)); return { scans: plan.scans.map((s) => ({ id: s.id, class: s.class, status: red.includes(s.id) ? "fail" : "pass", ...(red.includes(s.id) ? { code: 1, output: "red" } : {}) })), notes: [], cacheHits: [], deferred: [] }; } });
  let r = await fx.door(["--to", "check", "--commit", fx.commit()], seams(["test:npm:app-b"]));
  assert.equal(r.code, 0, r.all);
  const ids = planned.at(-1);
  assert.ok(ids.includes("test:npm:app-a") && ids.includes("secret-scan"), "the app's own test and the repository-wide scans run");
  assert.ok(!ids.includes("test:npm:app-b") && !ids.includes("test:npm:other"), "unrelated units' tests are not part of the stage");
  assert.match(r.out, /scope apps\/a: executing scans run for/);
  r = await fx.door(["--to", "check", "--commit", fx.commit()], seams(["test:npm:app-a"]));
  assert.notEqual(r.code, 0, "a red test inside the closure blocks");
  r = await fx.door(["--to", "check", "--commit", fx.commit()], seams(["secret-scan"]));
  assert.notEqual(r.code, 0, "a red repository-wide scan blocks");
});
