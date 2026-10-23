import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { cargoPackage, lockPathPackages, RULES, runRules } from "./preflight.mjs";

const tree = (t, files) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-preflight-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [file, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), text);
  }
  return { root, files: Object.keys(files) };
};
const run = (only, t, files) => runRules({ ...tree(t, files), only: [only] }).failures;

test("cargoPackage reads [package] only, never a dependency's version", () => {
  const pkg = cargoPackage('[package]\nname = "rightkit-a"\nversion = "0.2.0"\npublish = false\n\n[dependencies]\nrightkit-b = { version = "=9.9.9" }\nversion = "1.0.0"\n');
  assert.deepEqual(pkg, { name: "rightkit-a", version: "0.2.0", publish: false });
  assert.equal(cargoPackage('[package]\nname = "x"\nversion = "1.0.0"\n').publish, true);
});

test("lockPathPackages keeps path packages and drops registry ones", () => {
  const lock = 'version = 4\n\n[[package]]\nname = "rightkit-a"\nversion = "0.6.0"\ndependencies = [\n "serde",\n]\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n';
  assert.deepEqual(lockPathPackages(lock), [{ name: "rightkit-a", version: "0.6.0", source: false }]);
});

test("consumer-locks refuses a consumer lock behind its path crate and ignores ungated locks", (t) => {
  const crate = '[package]\nname = "rightkit-a"\nversion = "0.6.1"\n';
  const stale = '[[package]]\nname = "rightkit-a"\nversion = "0.6.0"\n';
  const failures = run("consumer-locks", t, { "crates/rightkit-a/Cargo.toml": crate, "ci/consumers/wv2/Cargo.lock": stale, "spikes/old/Cargo.lock": stale });
  assert.equal(failures.length, 1);
  assert.match(failures[0].problem, /^ci\/consumers\/wv2\/Cargo\.lock lists rightkit-a 0\.6\.0 but crates\/rightkit-a\/Cargo\.toml is 0\.6\.1/);
  assert.match(failures[0].fix, /version = "0\.6\.1"/);
});

test("file-url refuses .pathname on an import.meta.url URL and accepts fileURLToPath", (t) => {
  // Joined at run time so this file does not itself carry the pattern the rule refuses.
  const meta = ["import", "meta", "url"].join(".");
  const failures = run("file-url", t, {
    "a.test.mjs": `const a = new URL(${meta}).pathname;\nconst b = new URL("../..", ${meta}).pathname.replace(/x/, "");\n`,
    "b.mjs": `const c = fileURLToPath(new URL("..", ${meta}));\n`,
  });
  assert.deepEqual(failures.map((f) => f.problem.split(" ")[0]), ["a.test.mjs:1", "a.test.mjs:2"]);
});

test("dispatch-pins refuses a changed helper and a pin it cannot map to a file", (t) => {
  const helper = "export const x = 1;\n";
  const dispatch = (pin) => `const PREFLIGHT_SHA256 = "${pin}";\nconst ADMISSION_DEPENDENCIES = {\n};\n`;
  const changed = run("dispatch-pins", t, { "scripts/ci/rightkit-dispatch.mjs": dispatch("0".repeat(64)), "scripts/ci/rk-preflight.mjs": helper });
  assert.equal(changed.length, 1);
  assert.match(changed[0].problem, /^scripts\/ci\/rk-preflight\.mjs changed/);
  const unknown = run("dispatch-pins", t, { "scripts/ci/rightkit-dispatch.mjs": `const NEW_HELPER_SHA256 = "${"0".repeat(64)}";\n` });
  assert.match(unknown[0].problem, /NEW_HELPER_SHA256, which preflight cannot map to a file/);
});

test("version-pins refuses a crate's dispatch exception left at the old version", (t) => {
  const failures = run("version-pins", t, {
    "crates/rightkit-a/Cargo.toml": '[package]\nname = "rightkit-a"\nversion = "0.6.1"\n',
    "ci/rk-pipeline.json": JSON.stringify({ dispatch: { exceptions: { "rightkit-a": { version: "0.6.0", allow: [] } } } }),
  });
  assert.equal(failures.length, 1);
  assert.match(failures[0].problem, /dispatch\.exceptions\["rightkit-a"\]\.version is "0\.6\.0" but crates\/rightkit-a\/Cargo\.toml is 0\.6\.1/);
});

test("a rule that throws is a FAIL, never a pass", (t) => {
  const { root, files } = tree(t, {});
  const { failures } = runRules({ root, files, commit: "not-a-commit", only: ["eol-policy"] });
  assert.equal(failures.length, 1);
  assert.match(failures[0].problem, /^the rule itself failed/);
});

test("every rule names its scan-gap family", () => {
  for (const rule of RULES) assert.match(rule.gap, /^G\d{2}$/, rule.id);
});
