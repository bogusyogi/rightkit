import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { CONTRACT_PATH, MJS_PATH, RUST_PATH, TEST_VECTOR_PATH, TS_PATH, parseContract, renderProjections, suiteApiProblems } from "./gen-suite-api.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SCRIPT = path.join(ROOT, "scripts", "gen-suite-api.mjs");
const HOST = "api.orthiclabs.com";
const LEGACY = "api.spoares.com";
const PROJECTIONS = [RUST_PATH, TS_PATH, MJS_PATH, TEST_VECTOR_PATH];
const real = (rel) => readFileSync(path.join(ROOT, rel), "utf8");

// A temporary RightKit tree holding only the contract. Tests never touch the real generated files.
function tempRoot() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-suite-api-"));
  mkdirSync(path.join(dir, "contracts"), { recursive: true });
  copyFileSync(path.join(ROOT, CONTRACT_PATH), path.join(dir, CONTRACT_PATH));
  return dir;
}
const run = (root, ...args) => spawnSync(process.execPath, [SCRIPT, "--root", root, ...args], { encoding: "utf8" });
const readAll = (root) => Object.fromEntries(PROJECTIONS.map((p) => [p, readFileSync(path.join(root, p), "utf8")]));

test("the generator is deterministic: two runs over the same contract write identical bytes", () => {
  const a = tempRoot(); const b = tempRoot();
  try {
    assert.equal(run(a).status, 0);
    assert.equal(run(b).status, 0);
    assert.deepEqual(readAll(a), readAll(b));
    assert.equal(renderProjections(real(CONTRACT_PATH)).get(RUST_PATH), readAll(a)[RUST_PATH]);
    for (const text of Object.values(readAll(a))) {
      assert.ok(!text.includes("\r"), "LF line endings only");
      assert.ok(text.endsWith("\n") && !text.endsWith("\n\n"), "exactly one trailing newline");
    }
  } finally {
    rmSync(a, { recursive: true, force: true }); rmSync(b, { recursive: true, force: true });
  }
});

test("--check passes after generation and writes nothing", () => {
  const root = tempRoot();
  try {
    assert.equal(run(root).status, 0);
    const before = readAll(root);
    const check = run(root, "--check");
    assert.equal(check.status, 0, check.stderr);
    assert.deepEqual(readAll(root), before);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("--check fails after a one-byte edit to the contract and after a one-byte edit to a projection", () => {
  const root = tempRoot();
  try {
    assert.equal(run(root).status, 0);
    const contractPath = path.join(root, CONTRACT_PATH);
    const original = readFileSync(contractPath, "utf8");
    const edited = original.replace('"Canonical', '"canonical');
    assert.equal(edited.length, original.length);
    assert.equal([...original].filter((ch, i) => ch !== edited[i]).length, 1, "exactly one byte differs");
    writeFileSync(contractPath, edited);
    const afterContractEdit = run(root, "--check");
    assert.equal(afterContractEdit.status, 1);
    assert.match(afterContractEdit.stderr, new RegExp(TEST_VECTOR_PATH.replaceAll(".", "\\.")));
    writeFileSync(contractPath, original);

    const tsPath = path.join(root, TS_PATH);
    const ts = readFileSync(tsPath, "utf8");
    writeFileSync(tsPath, ts.replace(`"${HOST}";`, `"${HOST} ";`));
    const afterProjectionEdit = run(root, "--check");
    assert.equal(afterProjectionEdit.status, 1);
    assert.match(afterProjectionEdit.stderr, new RegExp(TS_PATH.replaceAll(".", "\\.").replaceAll("/", "\\/")));
    assert.equal(readFileSync(tsPath, "utf8").includes(`"${HOST} ";`), true, "--check never rewrites a projection");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("each projection contains the contract host", () => {
  for (const p of PROJECTIONS) assert.ok(real(p).includes(HOST), `${p} carries ${HOST}`);
});

test("the generated Rust and TS mention the legacy host only in the LEGACY_HOSTS constant", () => {
  const lines = (p) => real(p).split("\n").filter((line) => line.includes(LEGACY));
  assert.deepEqual(lines(RUST_PATH), [`pub const LEGACY_HOSTS: &[&str] = &[${JSON.stringify(LEGACY)}];`]);
  assert.deepEqual(lines(TS_PATH), [`export const RIGHT_SUITE_LEGACY_HOSTS = [${JSON.stringify(LEGACY)}] as const;`]);
  assert.deepEqual(lines(MJS_PATH), [`export const RIGHT_SUITE_LEGACY_HOSTS = [${JSON.stringify(LEGACY)}];`]);
});

test("the test-vector copy is byte-identical to the contract", () => {
  assert.equal(real(TEST_VECTOR_PATH), real(CONTRACT_PATH));
});

test("suiteApiProblems reports a missing projection and passes a complete set", () => {
  const files = renderProjections(real(CONTRACT_PATH));
  files.set(CONTRACT_PATH, real(CONTRACT_PATH));
  const readFrom = (map) => (rel) => { if (!map.has(rel)) throw new Error(`ENOENT ${rel}`); return map.get(rel); };
  assert.deepEqual(suiteApiProblems(readFrom(files)), []);
  files.delete(MJS_PATH);
  const problems = suiteApiProblems(readFrom(files));
  assert.equal(problems.length, 1);
  assert.match(problems[0], /suite-api\.generated\.mjs: missing/);
});

test("parseContract refuses a legacy host equal to the host, a foreign schema and an unknown artifact set", () => {
  const c = JSON.parse(real(CONTRACT_PATH));
  assert.throws(() => parseContract(JSON.stringify({ ...c, legacyHosts: [HOST] })), /legacyHosts/);
  assert.throws(() => parseContract(JSON.stringify({ ...c, schema: "right-suite-api.v2" })), /schema/);
  assert.throws(() => parseContract(JSON.stringify({ ...c, apps: { viewright: { artifactHosts: ["other"] } } })), /artifactHosts/);
});
