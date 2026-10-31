// GB-15 (GATE 9/86, E17): an app or unit stage runs the executing Class A scans for its dependency closure only; file-only scans stay
// repository-wide; an unreadable manifest widens, never narrows.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { closureOf, ManifestParseError, manifestDependencyFacts, scopeScans } from "./app-closure.mjs";
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
const read = (rel) => { if (!(rel in files)) throw new Error(`no ${rel}`); return files[rel]; };
const scansOf = (...ids) => ids.map((id) => ({ id }));

test("[GB-15] the closure of an app is its units plus their workspace dependencies; an unrelated unit is outside", () => {
  const c = closureOf({ scopeDirs: ["apps/a"], npmUnits, rustUnits: [], readFile: (r) => (r === "apps/a/package.json" ? '{"name":"app-a","dependencies":{"lib":"workspace:*"}}' : r === "packages/lib/package.json" ? '{"name":"lib"}' : read(r)) });
  assert.deepEqual([...c.npm].sort(), ["app-a", "lib"]);
  const kept = scopeScans(scansOf("secret-scan", "license", "test:npm:app-a", "test:npm:app-b", "tsc:lib", "eslint:other", "shellcheck"), c);
  assert.deepEqual(kept.scans.map((s) => s.id), ["secret-scan", "license", "test:npm:app-a", "tsc:lib", "shellcheck"], "file-only scans and the closure's executing scans stay");
  assert.deepEqual(kept.dropped.map((s) => s.id), ["test:npm:app-b", "eslint:other"]);
  const whole = closureOf({ scopeDirs: ["."], npmUnits, rustUnits: [], readFile: read });
  assert.equal(whole.npm.size, 4, "the repository-root scope is the whole repository");
});

test("[GB-15] Rust closure follows path dependencies; an inherited workspace dependency or an unreadable manifest widens to the whole Cargo workspace", () => {
  const rustUnits = [{ name: "app", dir: "apps/a/src-tauri", cargoDir: "apps/a/src-tauri" }, { name: "core", dir: "crates/core", cargoDir: "crates/core" }, { name: "x", dir: "crates/x", cargoDir: "crates/x" }, { name: "w1", dir: "ws/w1", cargoDir: "ws" }, { name: "w2", dir: "ws/w2", cargoDir: "ws" }];
  const manifests = {
    "apps/a/src-tauri/Cargo.toml": '[package]\nname="app"\n[dependencies]\ncore = { path = "../../../crates/core" }\n[lib]\npath = "src/lib.rs"\n',
    "crates/core/Cargo.toml": '[package]\nname="core"\nversion.workspace = true\n[dependencies]\nserde = "1"\n',
  };
  const c = closureOf({ scopeDirs: ["apps/a"], npmUnits: [], rustUnits, readFile: (r) => { if (r in manifests) return manifests[r]; throw new Error("absent"); } });
  assert.deepEqual([...c.rust].sort(), ["app", "core"], "path dependency followed; [package] workspace inheritance and an unrelated crate do not widen");
  const widened = closureOf({ scopeDirs: ["ws/w1"], npmUnits: [], rustUnits, readFile: (r) => (r === "ws/w1/Cargo.toml" ? '[dependencies]\nfoo = { workspace = true }\n' : "") });
  assert.deepEqual([...widened.rust].sort(), ["w1", "w2"], "an inherited dependency puts the whole workspace in scope");
  assert.ok(widened.widened.length);
  const unreadable = closureOf({ scopeDirs: ["crates/x"], npmUnits: [], rustUnits, readFile: () => { throw new Error("absent"); } });
  assert.deepEqual([...unreadable.rust], ["x"], "a single-crate workspace widens to itself");
  assert.deepEqual(scopeScans(scansOf("test:rust:app", "clippy:core:1", "check:x", "doc:w2", "cargo-deny", "test:rust-serial:x"), c, { rustUnits }).dropped.map((s) => s.id), ["check:x", "doc:w2", "test:rust-serial:x"]);
  assert.deepEqual(manifestDependencyFacts('[dependencies]\na = { path = "../a" }\n[target.\'cfg(unix)\'.dev-dependencies]\nb = { workspace = true }\n'), { paths: ["../a"], inherits: true });
});

test("[GB-15] Cargo path dependencies in TOML literal strings, inline-table spacing variants, dotted and quoted keys are all read; a path key it cannot read throws, naming the line", () => {
  const facts = (body) => manifestDependencyFacts(`[dependencies]\n${body}\n`).paths;
  assert.deepEqual(facts("a = { path = '../a' }"), ["../a"], "single-quoted literal string");
  assert.deepEqual(facts("a = {path='../a'}"), ["../a"], "no spaces");
  assert.deepEqual(facts('a = {   path   =   "../a" , version = "1" }'), ["../a"], "extra spaces and a following key");
  assert.deepEqual(facts("a = { version = '1', path = '../a' } # path = '../ignored'"), ["../a"], "comment text is not read");
  assert.deepEqual(facts(`a.path = '../a'\n"b" = { "path" = "../b" }\nc = { 'path' = '../c' }`), ["../a", "../b", "../c"], "dotted and quoted keys");
  assert.deepEqual(facts(String.raw`a = { path = "../we\\ird#x" }`), ["../we\\ird#x"], "escapes and # inside a basic string");
  assert.deepEqual(manifestDependencyFacts("[dependencies.a]\npath = '../a'\n[workspace.dependencies]\nb = { path = '../b' }\n").paths, ["../a", "../b"], "key in a dependency table and in workspace.dependencies");
  assert.deepEqual(facts(`a = { git = "https://x/path = '../no'" }`), [], "a path inside another string is not a key");
  const unparseable = ["a = { path = ../a }", 'a = { path = "../a }', "a = { path = '''../a''' }", 'a = { path = """../a""" }', 'a = { path = ["../a"] }', String.raw`a = { path = "../\q" }`, "a = { path ="];
  for (const bad of unparseable) {
    assert.throws(() => manifestDependencyFacts(`[package]\nname = "x"\n[dependencies]\n${bad}\n`), (error) => error instanceof ManifestParseError && error.code === "APP_CLOSURE_MANIFEST_UNPARSEABLE" && /line 4/.test(error.message) && error.message.includes(bad.trim()), bad);
  }
  assert.doesNotThrow(() => manifestDependencyFacts("[lib]\npath = unquoted\n"), "a path key outside a dependency table is not a dependency");
  const rustUnits = [{ name: "app", dir: "app", cargoDir: "." }, { name: "core", dir: "core", cargoDir: "." }, { name: "other", dir: "other", cargoDir: "." }];
  const literal = closureOf({ scopeDirs: ["app"], npmUnits: [], rustUnits, readFile: (r) => (r === "app/Cargo.toml" ? "[dependencies]\ncore = { path = '../core' }\n" : "") });
  assert.deepEqual([...literal.rust].sort(), ["app", "core"], "a literal-string path dependency is followed");
  const bad = closureOf({ scopeDirs: ["app"], npmUnits: [], rustUnits, readFile: () => "[dependencies]\ncore = { path = ../core }\n" });
  assert.deepEqual([...bad.rust].sort(), ["app", "core", "other"], "an unparseable path key widens to the whole Cargo workspace, never omits");
  assert.match(bad.widened.join("\n"), /Cargo\.toml line 2.*core = \{ path = \.\.\/core \}/);
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
