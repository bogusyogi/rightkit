import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { RULES, runRules } from "./app-check.mjs";
import { canonicalJson, readDeclaration, safeRelative, SOURCE_CLOSURE_RULES, sourceClosure, transformedDigest } from "./source-closure-rules.mjs";
import { spawnSync } from "./background-process.mjs";

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
  "src/registry/providers.json": "{\"providers\":[{\"id\":\"a\",\"benchmark\":{\"status\":\"qualified\",\"requiredForCleanClaim\":true}}]}\n",
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
const MOBILE_ID = "legion-provider-registry/without-qualification-metadata/v1";
// A record sealed the way Legion's mobile producer seals providers.json (audit-mobile-native-enrollment.py:74-89): benchmark without
// status, qualificationDigest and qualification.
const projected = (text) => sha(canonicalJson((({ providers, ...rest }) => ({ ...rest, providers: providers.map(({ benchmark: { status: _s, qualificationDigest: _d, qualification: _q, ...b }, ...p }) => ({ ...p, benchmark: b })) }))(JSON.parse(text))));
const mobileProjected = (files) => {
  const entry = mobile(files);
  return { ...entry, sourceClosure: seal({ ...entry.sourceClosure.sources, "src/registry/providers.json": projected(files["src/registry/providers.json"]) }) };
};
const declare = (normalized) => ({ schema: 1, normalized });
const MOBILE_DECLARATION = declare([{ file: INDEX, transform: MOBILE_ID }]);
const providers = (value) => `${JSON.stringify(value)}\n`;
const gateCheck = (t, files) => {
  const { root } = tree(t, files);
  const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
  git("init", "-q"); git("add", "-A"); git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "fixture");
  return spawnSync(process.execPath, [GATE, "check", "--repo", root, "--commit", "HEAD", "--only", "source-closure"], { encoding: "utf8" });
};

test("source-closure: red, through rightkit-gate check: a mutated source declared normalised with only a reason keeps its old digest and is refused", (t) => {
  const mutated = { ...V1, "src/registry/providers.json": providers({ providers: [{ id: "b", benchmark: { status: "qualified", requiredForCleanClaim: true } }] }) };
  const declaration = declare([{ file: INDEX, where: { family: "code-mobile-sdk" }, paths: ["src/registry/providers.json"], reason: "the producer digests a projection of providers.json" }]);
  const run = gateCheck(t, { ...mutated, ".rightkit/source-closures.json": declaration, [INDEX]: index([mobile(V1)]) });
  assert.equal(run.status, 1, run.stdout + run.stderr);
  assert.match(run.stdout, /FAIL source-closure \(SF-01\): \.rightkit\/source-closures\.json normalized #0: transform must name an approved RightKit identity .*a repository never supplies its own transform or pointers/);
  assert.match(run.stdout, /FAIL source-closure \(SF-01\): qualification\/provider-accuracy\/index\.json code\.mobile \(\/entries\/0\): sealed source closure stale\/incomplete: 1 source\(s\) changed since the record was sealed: src\/registry\/providers\.json/);
});

test("source-closure: red, through rightkit-gate check: repository-chosen json-drop pointers cannot drop a new consumed field", (t) => {
  // The audit counterexample on d45185a (exit 0 there): seal {"id":"p"}, add a field, declare json-drop of that field.
  const sources = { "src/p.json": sha(canonicalJson({ id: "p" })) };
  const run = gateCheck(t, {
    "src/p.json": JSON.stringify({ id: "p", newConsumedField: true }),
    "index.json": { entries: [{ provider: "p", family: "f", sourceClosure: seal(sources) }] },
    ".rightkit/source-closures.json": declare([{ file: "index.json", paths: ["src/p.json"], transform: { kind: "json-drop", drop: ["/newConsumedField"] }, reason: "the producer ignores this field entirely" }]),
  });
  assert.equal(run.status, 1, run.stdout + run.stderr);
  assert.match(run.stdout, /FAIL source-closure \(SF-01\): \.rightkit\/source-closures\.json normalized #0: transform must name an approved RightKit identity/);
  assert.match(run.stdout, /FAIL source-closure \(SF-01\): index\.json p \(\/entries\/0\): sealed source closure stale\/incomplete: 1 source\(s\) changed since the record was sealed: src\/p\.json/);
});

test("source-closure: red: an approved identity never excuses a deleted source", (t) => {
  const { "src/registry/providers.json": _gone, ...deleted } = V1;
  const found = sourceClosure(tree(t, { ...deleted, ".rightkit/source-closures.json": MOBILE_DECLARATION, [INDEX]: index([mobileProjected(V1)]) }));
  assert.match(found[0].problem, /1 source\(s\) no longer in the tree: src\/registry\/providers\.json/);
});

test("source-closure: red: an approved identity keeps every field it does not drop (a provider id, a new provider field)", (t) => {
  for (const changed of [{ id: "b", benchmark: { status: "qualified", requiredForCleanClaim: true } }, { id: "a", newConsumedField: true, benchmark: { status: "qualified", requiredForCleanClaim: true } }]) {
    const found = sourceClosure(tree(t, { ...V1, "src/registry/providers.json": providers({ providers: [changed] }), ".rightkit/source-closures.json": MOBILE_DECLARATION, [INDEX]: index([mobileProjected(V1)]) }));
    assert.match(found[0].problem, /changed since the record was sealed: src\/registry\/providers\.json \(under legion-provider-registry\/without-qualification-metadata\/v1\)/);
  }
});

test("source-closure: red: a field the identity's schema does not know is refused, not dropped", (t) => {
  const found = sourceClosure(tree(t, { ...V1, "src/registry/providers.json": providers({ providers: [{ id: "a", benchmark: { status: "qualified", requiredForCleanClaim: true, newConsumedField: true } }] }), ".rightkit/source-closures.json": MOBILE_DECLARATION, [INDEX]: index([mobileProjected(V1)]) }));
  assert.match(found[0].problem, /outside their approved transform: src\/registry\/providers\.json providers\[0\]\.benchmark has key\(s\) newConsumedField the approved projection does not know/);
});

test("source-closure: red: a declaration may only name an identity; paths, where or an unknown identity are findings and exempt nothing", (t) => {
  const changed = { ...V1, "src/registry/providers.json": providers({ providers: [{ id: "a", benchmark: { status: "x", requiredForCleanClaim: true } }] }) };
  const declaration = declare([{ file: INDEX, transform: MOBILE_ID, paths: ["src/registry/providers.json"] }, { file: INDEX, transform: "my-own/v1" }]);
  const found = sourceClosure(tree(t, { ...changed, ".rightkit/source-closures.json": declaration, [INDEX]: index([mobileProjected(V1)]) }));
  assert.deepEqual(found.map((f) => f.subject).sort(), ["code.mobile (/entries/0)", "declaration", "declaration"]);
  assert.equal(readDeclaration("{").problems.length, 1);
  assert.match(transformedDigest(Buffer.from("not json"), MOBILE_ID).problem, /is not JSON/);
  assert.match(transformedDigest(Buffer.from('{"providers":[{"benchmark":{"status":1.5}}]}'), MOBILE_ID).problem, /non-integer number/);
});

test("source-closure: positive control: a change only inside what the identity drops passes, and only for the identity's families", (t) => {
  const benchmarkOnly = { ...V1, "src/registry/providers.json": providers({ providers: [{ id: "a", benchmark: { status: "candidate", qualificationDigest: "sha256:0", requiredForCleanClaim: true } }] }) };
  const otherFamily = { ...mobileProjected(V1), provider: "code.other", family: "file" };
  const found = sourceClosure(tree(t, { ...benchmarkOnly, ".rightkit/source-closures.json": MOBILE_DECLARATION, [INDEX]: index([mobileProjected(V1), otherFamily]) }));
  assert.deepEqual(found.map((f) => f.subject), ["code.other (/entries/1)"], "the identity binds its families; another family is compared on raw bytes");
});

test("source-closure: positive control: text-lf/v1 accepts a CRLF checkout of the sealed LF bytes, and nothing else", (t) => {
  const lf = { provider: "docs", family: "file", sourceClosure: closureOf(V1, [".github/workflows/ci.yml"]) };
  const declaration = declare([{ file: INDEX, transform: "text-lf/v1", paths: [".github/workflows/ci.yml"] }]);
  assert.deepEqual(sourceClosure(tree(t, { ...V1, ".github/workflows/ci.yml": "on: push\r\n", ".rightkit/source-closures.json": declaration, [INDEX]: index([lf]) })), []);
  assert.equal(sourceClosure(tree(t, { ...V1, ".github/workflows/ci.yml": "on: pull_request\r\n", ".rightkit/source-closures.json": declaration, [INDEX]: index([lf]) })).length, 1);
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
