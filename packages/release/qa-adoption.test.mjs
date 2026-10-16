import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { assertQaAdoption, cargoQaPins, evaluateQaAdoption, normalizeRepositoryIdentity, qaExemption, qaManifests } from "./qa-contract.mjs";

const VERSIONS = { registry: { npm: { "@rightkit/qa": "0.4.8", "@rightkit/qa-chrome": "0.1.6" }, cargo: { "rightkit-qa": "0.4.0" } } };
const noExemption = { exemptions: { nonAppRepositories: [] }, remote: () => null };

function appFixture(files) {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-qa-adoption-"));
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    writeFileSync(path.join(root, file), typeof text === "string" ? text : JSON.stringify(text));
  }
  return root;
}
const evaluate = (root, extra = {}) => evaluateQaAdoption({ appRoot: root, app: "Demo" }, { versions: VERSIONS, ...noExemption, ...extra });

test("an app with no QA lane is refused, naming the rule and how to adopt", () => {
  const root = appFixture({ "package.json": { name: "demo", devDependencies: { vitest: "1.0.0" } }, "src-tauri/Cargo.toml": "[dependencies]\nserde = \"1\"\n" });
  const result = evaluate(root);
  assert.equal(result.refusals.length, 1);
  assert.match(result.refusals[0], /RightKit QA is mandatory for every Orthic Labs app, and Demo has no QA lane/);
  assert.match(result.refusals[0], /add @rightkit\/qa to devDependencies/);
  assert.match(result.refusals[0], /an app cannot opt out/);
  assert.throws(() => assertQaAdoption({ appRoot: root, app: "Demo" }, { versions: VERSIONS, ...noExemption }), /QA adoption contract failed:\n- RightKit QA is mandatory/);
});

test("each recognised lane passes at a current pin", () => {
  assert.deepEqual(evaluate(appFixture({ "package.json": { name: "d", devDependencies: { "@rightkit/qa": "0.4.8" } } })).refusals, []);
  assert.deepEqual(evaluate(appFixture({ "package.json": { name: "d", dependencies: { "@rightkit/qa": "^0.4.8" } } })).refusals, []);
  assert.deepEqual(evaluate(appFixture({ "src-tauri/Cargo.toml": "[dev-dependencies]\nrightkit-qa = \"0.4.0\"\n" })).refusals, []);
  assert.deepEqual(evaluate(appFixture({ "src-tauri/Cargo.toml": "[dev-dependencies.rightkit-qa]\nversion = \"0.4.0\"\n" })).refusals, []);
  assert.deepEqual(evaluate(appFixture({ "src-tauri/Cargo.toml": "[dependencies]\nrightkit-qa = { path = \"../../rightkit/crates/rightkit-qa\" }\n" })).refusals, []);
  const native = evaluate(appFixture({ "right-qa.config.mjs": "export default {};\n", "package.json": { name: "d" } }));
  assert.deepEqual(native.refusals, []);
  assert.match(native.lane[0], /native wdio lane/);
});

test("a QA mention in a comment, a node_modules package or a vendored tree is not adoption", () => {
  const root = appFixture({
    "package.json": { name: "d" },
    "src-tauri/Cargo.toml": "[dependencies]\n# rightkit-qa = \"0.4.0\"\n",
    "node_modules/x/package.json": { devDependencies: { "@rightkit/qa": "0.4.8" } },
    "node_modules.medium-1/x/package.json": { devDependencies: { "@rightkit/qa": "0.4.8" } },
    "vendor/y/Cargo.toml": "[dependencies]\nrightkit-qa = \"0.4.0\"\n",
  });
  assert.equal(evaluate(root).refusals.length, 1);
});

test("a package.json in a nested workspace package counts", () => {
  assert.deepEqual(evaluate(appFixture({ "package.json": { name: "d" }, "packages/app/package.json": { devDependencies: { "@rightkit/qa": "0.4.8" } } })).refusals, []);
  assert.deepEqual(qaManifests(appFixture({ "a/package.json": {}, "node_modules/b/package.json": {}, "Cargo.toml": "" })), ["Cargo.toml", "a/package.json"]);
});

test("a pin behind the latest version warns, and refuses only on a breaking lag", () => {
  const soft = evaluate(appFixture({ "package.json": { name: "d", devDependencies: { "@rightkit/qa": "0.4.2", "@rightkit/qa-chrome": "0.1.6" } } }));
  assert.deepEqual(soft.refusals, []);
  assert.equal(soft.warnings.length, 1);
  assert.match(soft.warnings[0], /Demo runs RightKit QA behind the latest published version: @rightkit\/qa 0\.4\.2 -> 0\.4\.8 \(package\.json\)\. Update with: node rightkit-drift\.mjs --apply/);
  const hard = evaluate(appFixture({ "package.json": { name: "d", devDependencies: { "@rightkit/qa": "0.3.9" } } }));
  assert.match(hard.refusals.join("\n"), /behind a breaking version: @rightkit\/qa 0\.3\.9 -> 0\.4\.8 \(breaking\)/);
  const cargo = evaluate(appFixture({ "Cargo.toml": "[dev-dependencies]\nrightkit-qa = \"0.3.1\"\n" }));
  assert.match(cargo.refusals.join("\n"), /rightkit-qa 0\.3\.1 -> 0\.4\.0 \(breaking\)/);
  const chrome = evaluate(appFixture({ "package.json": { name: "d", devDependencies: { "@rightkit/qa": "0.4.8", "@rightkit/qa-chrome": "0.0.9" } } }));
  assert.match(chrome.refusals.join("\n"), /@rightkit\/qa-chrome 0\.0\.9 -> 0\.1\.6 \(breaking\)/);
  const warnings = [];
  assertQaAdoption({ appRoot: appFixture({ "package.json": { name: "d", devDependencies: { "@rightkit/qa": "0.4.2" } } }), app: "Demo" }, { versions: VERSIONS, ...noExemption, warn: (line) => warnings.push(line) });
  assert.match(warnings[0], /^\[right-release\] QA warning: Demo runs RightKit QA behind/);
});

test("workspace, path and git specs carry no version to lag", () => {
  assert.deepEqual(evaluate(appFixture({ "package.json": { name: "d", devDependencies: { "@rightkit/qa": "workspace:*" } } })), { exempt: null, lane: ["@rightkit/qa"], refusals: [], warnings: [] });
  assert.deepEqual(cargoQaPins("[dependencies]\nrightkit-qa.workspace = true\nother = \"1.0.0\"\n"), [{ version: null }]);
});

test("only a non-app repository is exempt, by RightKit-owned identity or the RightKit source tree", () => {
  const bare = appFixture({ "package.json": { name: "lib" } });
  const exemptions = { nonAppRepositories: [{ repositoryIdentity: "https://github.com/Orthic-Labs/some-lib", kind: "library", reason: "a library" }] };
  assert.deepEqual(qaExemption({ appRoot: bare }, { exemptions, remote: () => "git@github.com:orthic-labs/some-lib.git" }), { kind: "library", reason: "a library" });
  assert.equal(qaExemption({ appRoot: bare }, { exemptions, remote: () => "https://github.com/orthic-labs/an-app" }), null);
  assert.equal(evaluate(bare, { exemptions, remote: () => "https://github.com/orthic-labs/some-lib" }).refusals.length, 0);
  assert.equal(normalizeRepositoryIdentity("git@github.com:Orthic-Labs/X.git"), normalizeRepositoryIdentity("https://github.com/orthic-labs/x/"));
  // An entry without a recognised kind or a reason is ignored: no silent blanket exemption.
  for (const entry of [{ repositoryIdentity: "https://github.com/a/b", reason: "x" }, { repositoryIdentity: "https://github.com/a/b", kind: "app", reason: "x" }, { repositoryIdentity: "https://github.com/a/b", kind: "library" }]) {
    assert.equal(qaExemption({ appRoot: bare }, { exemptions: { nonAppRepositories: [entry] }, remote: () => "https://github.com/a/b" }), null);
  }
  const tree = appFixture({ "packages/release/package.json": { name: "@rightkit/release" } });
  assert.equal(qaExemption({ appRoot: tree }, noExemption)?.kind, "tooling");
  assert.equal(evaluate(tree).refusals.length, 0);
  // A package merely sitting at that path under another name does not exempt an app.
  assert.equal(qaExemption({ appRoot: appFixture({ "packages/release/package.json": { name: "not-rightkit" } }) }, noExemption), null);
});

test("the shipped exemption file lists only non-app kinds with a reason, and no env switch exists", () => {
  const shipped = JSON.parse(readFileSync(new URL("./qa-adoption-exemptions.json", import.meta.url), "utf8"));
  for (const entry of shipped.nonAppRepositories) {
    assert.ok(["library", "tooling"].includes(entry.kind) && entry.reason && entry.repositoryIdentity, JSON.stringify(entry));
  }
  assert.doesNotMatch(readFileSync(new URL("./qa-contract.mjs", import.meta.url), "utf8"), /process\.env/);
});
