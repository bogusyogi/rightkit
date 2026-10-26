import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { RULES, runRules } from "./app-check.mjs";
import { canonicalJson, readDeclaration, safeRelative, SOURCE_CLOSURE_RULES, sourceClosure, transformedDigest } from "./source-closure-rules.mjs";
import { spawnSync } from "node:child_process";

// The Legion source-closure failure (Orthic-Labs/legion run 40158717069 job 121065535787, run 40149636641 job 121038474276:
// "AssertionError: source closure stale/incomplete" from scripts/ci/audit-qualification-records.py, after the Windows build).
// Fixtures mirror its shape: qualification/provider-accuracy/index.json { entries: [{ provider, family, sourceClosure: { sources, digest } }] }.

const sha = (text) => `sha256:${createHash("sha256").update(text).digest("hex")}`;
const seal = (sources) => ({ sources, digest: sha(canonicalJson(sources)) });
const INDEX = "qualification/provider-accuracy/index.json";
const V1 = {
  ".github/workflows/ci.yml": "on: push\n",
  "scripts/ci/audit-qualification-records.py": "print('gate v1')\n",
  "scripts/ci/tests/test_audit_qualification_records.py": "def test(): pass\n",
  "src/registry/providers.json": "{\"providers\":[{\"id\":\"a\",\"benchmark\":1}]}\n",
  "engine/crates/legion-audit/src/native_providers/code/go.rs": "pub fn go() {}\n",
};
const closureOf = (files, paths) => seal(Object.fromEntries(paths.map((p) => [p, sha(files[p])])));
const tree = (t, files) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-closure-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [file, body] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), typeof body === "string" ? body : JSON.stringify(body, null, 2));
  }
  return { root, files: Object.keys(files) };
};
const index = (entries) => ({ schema: "legion-provider-accuracy-index/1", entries });
const architecture = (files) => ({ provider: "architecture.core", family: "file", sourceClosure: closureOf(files, [".github/workflows/ci.yml", "scripts/ci/audit-qualification-records.py", "scripts/ci/tests/test_audit_qualification_records.py"]) });
const dotnet = (files) => ({ provider: "code.dotnet", family: "dotnet-sdk", sourceClosure: closureOf(files, ["scripts/ci/audit-qualification-records.py", "engine/crates/legion-audit/src/native_providers/code/go.rs"]) });
const mobile = (files) => ({ provider: "code.mobile", family: "code-mobile-sdk", sourceClosure: closureOf(files, ["scripts/ci/audit-qualification-records.py", "src/registry/providers.json"]) });

test("source-closure: registered in the app check with gap SF-01, before the ownership class rules", () => {
  assert.deepEqual(SOURCE_CLOSURE_RULES.map((r) => [r.id, r.gap]), [["source-closure", "SF-01"]]);
  const ids = RULES.map((r) => r.id);
  assert.ok(ids.includes("source-closure"));
  assert.ok(ids.indexOf("source-closure") < ids.indexOf("mandatory-foundation"));
});

test("source-closure: original negative: a sealed source edited after sealing fails, as Legion's Windows job did after its build", (t) => {
  const sealed = { ...V1, [INDEX]: index([architecture(V1)]) };
  const edited = { ...sealed, "scripts/ci/audit-qualification-records.py": "print('gate v2')\n" };
  const { failures } = runRules({ ...tree(t, edited), only: ["source-closure"] });
  assert.equal(failures.length, 1);
  assert.equal(failures[0].identity, `source-closure|${INDEX}|architecture.core (/entries/0)`);
  assert.match(failures[0].problem, /sealed source closure stale\/incomplete: 1 source\(s\) changed since the record was sealed: scripts\/ci\/audit-qualification-records\.py/);
  assert.match(failures[0].fix, /never rewrite recorded digests by hand/);
});

test("source-closure: original negative: a sealed source deleted from the tree fails (Legion 1bd8f71 removed providers without re-sealing)", (t) => {
  const { "engine/crates/legion-audit/src/native_providers/code/go.rs": _gone, ...rest } = V1;
  const found = sourceClosure(tree(t, { ...rest, [INDEX]: index([dotnet(V1)]) }));
  assert.equal(found.length, 1);
  assert.match(found[0].problem, /1 source\(s\) no longer in the tree: engine\/crates\/legion-audit\/src\/native_providers\/code\/go\.rs/);
});

test("source-closure: repaired: the record re-sealed over the current bytes passes", (t) => {
  const v2 = { ...V1, "scripts/ci/audit-qualification-records.py": "print('gate v2')\n" };
  const { "engine/crates/legion-audit/src/native_providers/code/go.rs": _gone, ...withoutGo } = v2;
  const resealedDotnet = { provider: "code.dotnet", family: "dotnet-sdk", sourceClosure: closureOf(v2, ["scripts/ci/audit-qualification-records.py"]) };
  assert.deepEqual(sourceClosure(tree(t, { ...withoutGo, [INDEX]: index([architecture(v2), resealedDotnet]) })), []);
});

test("source-closure: a closure digest that does not cover its sources fails (hand-edited source digests)", (t) => {
  const entry = architecture(V1);
  const v2 = { ...V1, "scripts/ci/audit-qualification-records.py": "print('gate v2')\n" };
  entry.sourceClosure.sources["scripts/ci/audit-qualification-records.py"] = sha(v2["scripts/ci/audit-qualification-records.py"]);
  const found = sourceClosure(tree(t, { ...v2, [INDEX]: index([entry]) }));
  assert.equal(found.length, 1);
  assert.match(found[0].problem, /sourceClosure\.digest is not sha256 over the canonical JSON of sourceClosure\.sources/);
});

test("source-closure: an unsafe or malformed source is a finding and is never read", (t) => {
  const sources = { "../outside.txt": sha("x"), "scripts/ci/audit-qualification-records.py": "md5:abc" };
  const found = sourceClosure(tree(t, { ...V1, [INDEX]: index([{ provider: "evil", sourceClosure: seal(sources) }]) }));
  assert.equal(found.length, 1);
  assert.match(found[0].problem, /unsafe path or malformed digest: \.\.\/outside\.txt, scripts\/ci\/audit-qualification-records\.py/);
});

const GATE = path.join(path.dirname(fileURLToPath(import.meta.url)), "bin", "rightkit-gate.mjs");
const DROP_BENCHMARK = { kind: "json-drop", drop: ["/providers/*/benchmark"] };
const benchmarkFree = (text) => sha(canonicalJson((({ providers }) => ({ providers: providers.map(({ benchmark: _b, ...p }) => p) }))(JSON.parse(text))));
// A record sealed the way Legion's mobile/compatibility producers seal providers.json: over the registry without benchmark metadata.
const mobileProjected = (files) => {
  const entry = mobile(files);
  const sources = { ...entry.sourceClosure.sources, "src/registry/providers.json": benchmarkFree(files["src/registry/providers.json"]) };
  return { ...entry, sourceClosure: seal(sources) };
};
const declare = (normalized) => ({ schema: 1, normalized });

test("source-closure: red, through rightkit-gate check: a mutated source declared normalised with only a reason keeps its old digest and is refused", (t) => {
  // The bypass: list a changed source as "normalised" with a plausible reason and leave the sealed map and digest untouched.
  const mutated = { ...V1, "src/registry/providers.json": "{\"providers\":[{\"id\":\"b\",\"benchmark\":1}]}\n" };
  const declaration = declare([{ file: INDEX, where: { family: "code-mobile-sdk" }, paths: ["src/registry/providers.json"], reason: "the producer digests a projection of providers.json" }]);
  const { root } = tree(t, { ...mutated, ".rightkit/source-closures.json": declaration, [INDEX]: index([mobile(V1)]) });
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  git("init", "-q"); git("add", "-A"); git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "fixture");
  const run = spawnSync(process.execPath, [GATE, "check", "--repo", root, "--commit", "HEAD", "--only", "source-closure"], { encoding: "utf8" });
  assert.equal(run.status, 1, run.stdout + run.stderr);
  assert.match(run.stdout, /FAIL source-closure \(SF-01\): \.rightkit\/source-closures\.json normalized #0: transform must be one of lf, json-drop .*a reason alone exempts nothing/);
  assert.match(run.stdout, /FAIL source-closure \(SF-01\): qualification\/provider-accuracy\/index\.json code\.mobile \(\/entries\/0\): sealed source closure stale\/incomplete: 1 source\(s\) changed since the record was sealed: src\/registry\/providers\.json/);
});

test("source-closure: red: a declared transform never excuses a deleted source or a change outside what it discards", (t) => {
  const declaration = declare([{ file: INDEX, where: { family: "code-mobile-sdk" }, paths: ["src/registry/providers.json"], transform: DROP_BENCHMARK, reason: "the producer digests providers.json without providers[].benchmark" }]);
  const { "src/registry/providers.json": _gone, ...deleted } = V1;
  const gone = sourceClosure(tree(t, { ...deleted, ".rightkit/source-closures.json": declaration, [INDEX]: index([mobileProjected(V1)]) }));
  assert.match(gone[0].problem, /1 source\(s\) no longer in the tree: src\/registry\/providers\.json/);
  const renamed = { ...V1, "src/registry/providers.json": "{\"providers\":[{\"id\":\"b\",\"benchmark\":1}]}\n" };
  const changed = sourceClosure(tree(t, { ...renamed, ".rightkit/source-closures.json": declaration, [INDEX]: index([mobileProjected(V1)]) }));
  assert.match(changed[0].problem, /changed since the record was sealed: src\/registry\/providers\.json/);
});

test("source-closure: positive control: a change only inside the declared transform passes, for the declared family only", (t) => {
  const benchmarkOnly = { ...V1, "src/registry/providers.json": "{\"providers\":[{\"id\":\"a\",\"benchmark\":{\"status\":\"qualified\"}}]}\n" };
  const declaration = declare([{ file: INDEX, where: { family: "code-mobile-sdk" }, paths: ["src/registry/providers.json"], transform: DROP_BENCHMARK, reason: "the producer digests providers.json without providers[].benchmark" }]);
  const otherFamily = { ...mobileProjected(V1), provider: "code.other", family: "file" };
  const found = sourceClosure(tree(t, { ...benchmarkOnly, ".rightkit/source-closures.json": declaration, [INDEX]: index([mobileProjected(V1), otherFamily]) }));
  assert.deepEqual(found.map((f) => f.subject), ["code.other (/entries/1)"], "the transform is scoped to its family; another family is compared on raw bytes");
});

test("source-closure: positive control: line-ending normalisation (lf) accepts a CRLF checkout of the sealed LF bytes, and nothing else", (t) => {
  const lf = { provider: "docs", family: "file", sourceClosure: closureOf(V1, [".github/workflows/ci.yml"]) };
  const declaration = declare([{ file: INDEX, paths: [".github/workflows/ci.yml"], transform: { kind: "lf" }, reason: "sealed on Linux; Windows checkouts write CRLF" }]);
  assert.deepEqual(sourceClosure(tree(t, { ...V1, ".github/workflows/ci.yml": "on: push\r\n", ".rightkit/source-closures.json": declaration, [INDEX]: index([lf]) })), []);
  assert.equal(sourceClosure(tree(t, { ...V1, ".github/workflows/ci.yml": "on: pull_request\r\n", ".rightkit/source-closures.json": declaration, [INDEX]: index([lf]) })).length, 1);
});

test("source-closure: a malformed declaration is a finding and exempts nothing", (t) => {
  const changedRegistry = { ...V1, "src/registry/providers.json": "{}\n" };
  const declaration = { normalized: [{ file: INDEX, paths: ["src/registry/providers.json"], transform: { kind: "semantic" }, reason: "short" }] };
  const found = sourceClosure(tree(t, { ...changedRegistry, ".rightkit/source-closures.json": declaration, [INDEX]: index([mobile(V1)]) }));
  assert.deepEqual(found.map((f) => f.subject).sort(), ["code.mobile (/entries/0)", "declaration"]);
  assert.deepEqual(readDeclaration("{").problems.length, 1);
  assert.equal(transformedDigest(Buffer.from("not json"), DROP_BENCHMARK), null);
});

test("source-closure: JSON without a sealed record, and vendored trees, are not read", (t) => {
  const files = { ...V1, "package.json": { name: "x" }, "node_modules/dep/index.json": index([architecture({ ...V1, ".github/workflows/ci.yml": "other\n" })]) };
  assert.deepEqual(sourceClosure(tree(t, files)), []);
  assert.equal(safeRelative("a/../b"), false);
  assert.equal(safeRelative("C:/x"), false);
  assert.equal(safeRelative("a/b.json"), true);
});

test("source-closure: canonical JSON matches Python json.dumps(sort_keys=True, ensure_ascii=False, separators=(',', ':'))", () => {
  assert.equal(canonicalJson({ b: "é", a: ["x", { d: 1, c: null }] }), '{"a":["x",{"c":null,"d":1}],"b":"é"}');
});
