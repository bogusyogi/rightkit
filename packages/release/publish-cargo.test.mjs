import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, writeFileSync, rmSync, mkdirSync, readFileSync, readdirSync, symlinkSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  buildCargoPublishPlan,
  packageFileViolations,
  parseCargoPublishArgs,
  runCargoPublish,
  runCargoReadOnly,
  scanCrateForSecrets,
} from "./publish-cargo.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { HOST_UPLOAD_REFUSED } from "./host-upload-refusal.mjs";
import { mintTestCapability } from "./test-support/publish-capability-fixture.mjs";
import { checkPublicationGate } from "./publication-gate.mjs";
import { localTestPlan } from "./local-publish.mjs";

test("cargo publish requires a safe explicit crate name", () => {
  assert.throws(() => parseCargoPublishArgs([]), /--crate/);
  assert.throws(() => parseCargoPublishArgs(["--crate", "../secret"]), /invalid crate/);
  assert.throws(() => parseCargoPublishArgs(["--crate", "rightkit-license", "--allow-dirty"]), /only valid with --dry-run/);
  assert.equal(parseCargoPublishArgs(["--crate", "rightkit-license"]).crate, "rightkit-license");
});

test("cargo publish plan always runs local safety gates before upload", () => {
  const plan = buildCargoPublishPlan({ crate: "rightkit-license", crateDir: "C:/rightkit/crates/rightkit-license", dryRun: false });
  assert.deepEqual(plan.map((step) => step.label), [
    "secret scan",
    "runtime budget",
    "format",
    "tests",
    "clippy",
    "package contents",
    "registry dry-run",
    "package archive",
    "package location",
    "archive hash check",
    "registry publish",
    "registry readback",
  ]);
  assert.deepEqual(plan.find((step) => step.label === "package archive").args, ["package", "--locked", "--no-verify", "-p", "rightkit-license"]);
  assert.equal(plan.find((step) => step.label === "archive hash check").internal, "verify-archive");
  assert.deepEqual(plan.find((step) => step.label === "registry publish"), {
    label: "registry publish", command: "cargo", args: ["publish", "--no-verify", "-p", "rightkit-license"],
  });
  assert.equal(plan.at(-1)?.internal, "registry-readback");
  assert.deepEqual(plan.find((step) => step.label === "format").args, ["fmt", "-p", "rightkit-license", "--check"]);
  assert.deepEqual(plan.find((step) => step.label === "tests").args, ["test", "-p", "rightkit-license", "--", "--test-threads=1"]);
});

test("local offline mode scopes offline to package inspection, then keeps upload online", () => {
  const plan = buildCargoPublishPlan({ crate: "rightkit-license", crateDir: "C:/rightkit/crates/rightkit-license", dryRun: false, offlineReadOnly: true, verifyOnPublish: true });
  assert.deepEqual(plan.find((step) => step.label === "package contents").args, ["package", "--list", "--offline", "-p", "rightkit-license"]);
  // The archive is packaged online and locked even in offline mode: an offline package resolves its
  // Cargo.lock from downloaded .crate files only and can drift from the qualified archive.
  assert.deepEqual(plan.find((step) => step.label === "package archive").args, ["package", "--locked", "--no-verify", "-p", "rightkit-license"]);
  assert.deepEqual(plan.find((step) => step.label === "package location").args, ["metadata", "--offline", "--format-version", "1", "--no-deps"]);
  assert.deepEqual(plan.find((step) => step.label === "format").args, ["fmt", "-p", "rightkit-license", "--check"]);
  assert.deepEqual(plan.find((step) => step.label === "tests").args, ["test", "-p", "rightkit-license", "--", "--test-threads=1"]);
  assert.deepEqual(plan.find((step) => step.label === "registry publish").args, ["publish", "--locked", "-p", "rightkit-license"]);
  assert.equal(plan.find((step) => step.label === "registry dry-run"), undefined);
});

test("offline inspection retries one explicit cache miss online and preserves both failures", async () => {
  const calls = [], logged = [];
  const execute = async (_command, args) => {
    calls.push(args);
    return calls.length === 1
      ? { status: 101, stdout: "first out", stderr: "attempting to make an HTTP request, but --offline was specified" }
      : { status: 101, stdout: "second out", stderr: "second error" };
  };
  const result = await runCargoReadOnly(["metadata", "--offline", "--format-version", "1"], {}, execute, (failure) => logged.push(failure.stderr));
  assert.deepEqual(calls, [["metadata", "--offline", "--format-version", "1"], ["metadata", "--format-version", "1"]]);
  assert.deepEqual(logged, ["attempting to make an HTTP request, but --offline was specified"]);
  assert.equal(result.offlineFallbackFailed, true);
  assert.match(result.stdout, /first out[\s\S]*second out/);
  assert.match(result.stderr, /attempting to make an HTTP request[\s\S]*second error/);
});

test("offline cache miss succeeds with one online retry", async () => {
  const calls = [];
  const execute = async (_command, args) => {
    calls.push(args);
    return calls.length === 1
      ? { status: 101, stderr: "As a reminder, you're using offline mode (--offline)" }
      : { status: 0, stdout: "{}", stderr: "" };
  };
  const result = await runCargoReadOnly(["package", "--offline", "--no-verify"], {}, execute);
  assert.equal(result.usedOfflineFallback, true);
  assert.deepEqual(calls, [["package", "--offline", "--no-verify"], ["package", "--no-verify"]]);
});

test("offline inspection does not retry ordinary failures or non-inspection commands", async () => {
  let calls = 0;
  const execute = async () => { calls += 1; return { status: 101, stdout: "compile failed", stderr: "build error" }; };
  await runCargoReadOnly(["package", "--offline", "--list"], {}, execute);
  await runCargoReadOnly(["test", "--offline", "-p", "x"], {}, execute);
  assert.equal(calls, 2);
});

test("dry-run plan never contains a real registry upload", () => {
  const plan = buildCargoPublishPlan({ crate: "rightkit-logs", crateDir: "C:/rightkit/crates/rightkit-logs", dryRun: true });
  assert.equal(plan.at(-1)?.label, "registry dry-run");
});

test("package inspection rejects secret and build-output paths", () => {
  assert.deepEqual(packageFileViolations([
    "Cargo.toml",
    "src/lib.rs",
    ".env",
    "keys/signing.p8",
    "target/release/app",
  ]), [".env", "keys/signing.p8", "target/release/app"]);
});

// ---- the secret scan: strict for crates, reviewed records for the repository gate ----------------------------------------
// Credentials are assembled from parts so this file holds none of the shapes it tests.
const SHAPES = {
  ghp: () => "gh" + "p_" + "A".repeat(24),
  akia: () => "AK" + "IA" + "ABCDEFGHIJKLMNOP",
  npm: () => "np" + "m_" + "a".repeat(30),
  pem: () => "-----BEGIN " + "PRIVATE KEY-----",
  pemRsa: () => "-----BEGIN RSA " + "PRIVATE KEY-----",
  pemEncrypted: () => "-----BEGIN ENCRYPTED " + "PRIVATE KEY-----",
  pemPgp: () => "-----BEGIN PGP " + "PRIVATE KEY BLOCK-----",
  githubPat: () => "github" + "_pat_" + "A1".repeat(15),
  stripe: () => "sk" + "_live_" + "a1B2c3D4e5F6g7H8",
  slack: () => "xox" + "b-" + "1234567890-abcdefghij",
  google: () => "AI" + "za" + "SyA1234567890abcdefghijklmnopqrstuv",
  webhook: () => "hooks.slack" + ".com/services/" + "T01234567/B01234567/abcdefGHIJKL",
};
const RECORDS = ".rightkit/secret-scan-records.json";
// A record digest reads CRLF as LF (publish-cargo.mjs recordDigest); LF content is unchanged by it.
const sha = (data) => digestBytes(Buffer.from((Buffer.isBuffer(data) ? data : Buffer.from(data)).toString("latin1").replace(/\r\n/g, "\n"), "latin1"));

function tree(t, files) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-secret-scan-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const [name, body] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
    writeFileSync(path.join(root, name), body);
  }
  return root;
}
const putRecords = (root, text) => { mkdirSync(path.join(root, ".rightkit"), { recursive: true }); writeFileSync(path.join(root, RECORDS), text); };
const records = (...list) => JSON.stringify({ schema: 1, records: list.map((r) => ({ note: "reviewed: placeholders only", ...r })) });
const record = (root, name, kind, over = {}) => ({ path: name, sha256: sha(readFileSync(path.join(root, name))), kind, ...over });
const gate = (root, dir = ".") => scanCrateForSecrets(path.join(root, dir), { envTemplates: true, repoRoot: root });
const recordLineOf = (violation) => JSON.parse(violation.slice(violation.indexOf("{")));

test("secret scan: a crate package stays strict; it refuses every template by name and ignores the records file", (t) => {
  const root = tree(t, {
    ".env.example": "API_URL=\nAPI_TOKEN=\n",
    "sub/.env.test.example": "A=\n",
    ".env.sample": "A=\n", ".env.template": "A=\n", ".env": "A=1\n", ".env.local": "A=1\n", "keys/cert.pem": "x", "keys/a.p12": "x",
    "src/vector.rs": `// fake ${SHAPES.ghp()}\n`,
    "src/lib.rs": "pub fn a() {}\n",
  });
  putRecords(root, records(record(root, ".env.example", "env-template"), record(root, "src/vector.rs", "fixture")));
  assert.deepEqual(scanCrateForSecrets(root), [".env", ".env.example", ".env.local", ".env.sample", ".env.template", "keys/a.p12", "keys/cert.pem", "src/vector.rs", "sub/.env.test.example"],
    "templates are refused by name and the recorded fixture is still flagged: the records file admits nothing outside the repository gate");
});

test("secret scan: a skip predicate excludes a directory and a file from the walk, in both the crate and the repository modes", (t) => {
  const root = tree(t, {
    "src/lib.rs": "pub fn a() {}\n",
    "ignored/vendor/vec.rs": `// fake ${SHAPES.ghp()}\n`,
    "dist/out.js": `// fake ${SHAPES.ghp()}\n`,
    "src/vector.rs": `// fake ${SHAPES.ghp()}\n`,
  });
  const skip = (absolute) => { const rel = path.relative(root, absolute).replaceAll("\\", "/"); return rel === "ignored" || rel.startsWith("ignored/") || rel === "dist/out.js"; };
  assert.deepEqual(scanCrateForSecrets(root), ["dist/out.js", "ignored/vendor/vec.rs", "src/vector.rs"], "without a skip every file is read");
  assert.deepEqual(scanCrateForSecrets(root, { skip }), ["src/vector.rs"], "crate mode honours the skip");
  const repo = scanCrateForSecrets(root, { envTemplates: true, repoRoot: root, skip });
  assert.ok(repo.some((v) => v.startsWith("src/vector.rs")) && !repo.some((v) => v.includes("ignored/") || v.includes("dist/out.js")), repo.join("; "));
});

test("secret scan: a record reads CRLF as LF, so the same record holds on an LF host and on a Windows CRLF export", (t) => {
  const lf = "API_URL=http://localhost:3000\nAPI_TOKEN=\n";
  const vector = `// fake ${SHAPES.ghp()}\nfn f() {}\n`;
  const root = tree(t, { ".env.example": lf, "src/vector.rs": vector });
  putRecords(root, records(record(root, ".env.example", "env-template"), record(root, "src/vector.rs", "fixture")));
  assert.deepEqual(gate(root), [], "recorded on the LF export");
  writeFileSync(path.join(root, ".env.example"), lf.replace(/\n/g, "\r\n"));
  writeFileSync(path.join(root, "src/vector.rs"), vector.replace(/\n/g, "\r\n"));
  assert.deepEqual(gate(root), [], "the same records admit the CRLF export (git archive with core.autocrlf=true)");
  writeFileSync(path.join(root, ".env.example"), lf.replace("\n", "\r"));
  assert.match(gate(root).join("\n"), /\.env\.example: env template changed since it was recorded/, "a lone CR is a real change, not a line ending");
});

test("secret scan: in the repository gate an env template is admitted only by a record of its exact bytes", (t) => {
  const root = tree(t, { ".env.example": "API_URL=http://localhost:3000\nAPI_TOKEN=\n", "apps/web/.env.production.example": "X=1\n" });
  const unrecorded = gate(root);
  assert.equal(unrecorded.length, 2, "both templates are unrecorded");
  const first = unrecorded.find((v) => v.startsWith(".env.example: "));
  assert.match(first, /^\.env\.example: env template \(sha256 [0-9a-f]{64}\) is not recorded in \.rightkit\/secret-scan-records\.json; after reviewing/);
  assert.deepEqual(recordLineOf(first), { path: ".env.example", sha256: sha(readFileSync(path.join(root, ".env.example"))), kind: "env-template", note: "<what it holds and why it is safe>" },
    "the violation prints the exact record to add after review");
  assert.ok(first.includes(sha(readFileSync(path.join(root, ".env.example")))), "and names the file's sha256");
  // The record line as printed is not yet a record: its note is still the placeholder.
  putRecords(root, JSON.stringify({ schema: 1, records: [recordLineOf(first)] }));
  assert.match(gate(root).join("\n"), /record 0 \("\.env\.example"\): "note" must say what the file holds/);
  putRecords(root, records(record(root, ".env.example", "env-template"), record(root, "apps/web/.env.production.example", "env-template", { sha256: sha(readFileSync(path.join(root, "apps/web/.env.production.example"))).toUpperCase() })));
  assert.deepEqual(gate(root), [], "recorded with the matching sha256 (hex case does not matter) the templates pass");
  assert.deepEqual(gate(root, "apps/web"), [], "scanning a sub-directory reads the same repository records, by repository path");
  writeFileSync(path.join(root, ".env.example"), "API_URL=http://localhost:3000\nAPI_TOKEN=\nNEW=\n");
  const changed = gate(root);
  assert.equal(changed.length, 1);
  assert.match(changed[0], /^\.env\.example: env template changed since it was recorded \(recorded sha256 [0-9a-f]{64}, now [0-9a-f]{64}\); after reviewing the change, replace its record/);
  assert.ok(changed[0].includes(sha(readFileSync(path.join(root, ".env.example")))), "the new sha256 is named");
});

test("secret scan: a populated template is not guessed at; a recorded one passes however it reads, and the content heuristics are gone", (t) => {
  // These are the shapes the 740ca5f heuristics flagged or missed by name and value. Content no longer decides; the record does.
  const body = "POSTGRES_PASSWORD=postgres\nACCESS_TOKEN_TTL=3600\nDATABASE_URL=postgres://cp:cp@127.0.0.1:5433/app\nREDIS_URL=redis://:pw@cache\nAPP_KEY=base64:abc\n";
  const root = tree(t, { ".env.example": body });
  assert.equal(gate(root).length, 1, "unrecorded");
  putRecords(root, records(record(root, ".env.example", "env-template")));
  assert.deepEqual(gate(root), []);
});

test("secret scan: a file whose content matches SECRET_PATTERNS is admitted only by a fixture record of its exact bytes", (t) => {
  const root = tree(t, {
    "engine/redaction.rs": `fn a() {}\n\n// test vector\nlet t = "${SHAPES.ghp()}";\nlet k = "${SHAPES.akia()}";\n`,
    "engine/ok.rs": "fn b() {}\n",
  });
  const flagged = gate(root);
  assert.equal(flagged.length, 1);
  assert.match(flagged[0], /^engine\/redaction\.rs:4: content matches a GitHub token \(gh\*_\) \(line 4\); an AWS access key id \(AKIA\) \(line 5\); sha256 [0-9a-f]{64}\. If this is a redaction or screening test vector and not a credential, review it and add this record to \.rightkit\/secret-scan-records\.json: \{/);
  assert.ok(!flagged[0].includes(SHAPES.ghp()) && !flagged[0].includes(SHAPES.akia()), "a violation names the line and the shape, never the matched text");
  assert.deepEqual(recordLineOf(flagged[0]), { path: "engine/redaction.rs", sha256: sha(readFileSync(path.join(root, "engine/redaction.rs"))), kind: "fixture", note: "<what it holds and why it is safe>" });
  putRecords(root, records(record(root, "engine/redaction.rs", "fixture")));
  assert.deepEqual(gate(root), [], "recorded as a fixture it passes");
  writeFileSync(path.join(root, "engine/redaction.rs"), readFileSync(path.join(root, "engine/redaction.rs"), "utf8") + "// more\n");
  assert.match(gate(root).join("\n"), /engine\/redaction\.rs:4: content matches .*its fixture record is out of date \(recorded sha256 [0-9a-f]{64}, now [0-9a-f]{64}\)/);
});

test("secret scan: a record admits only its own path and kind", (t) => {
  const root = tree(t, { "a.rs": `// ${SHAPES.ghp()}\n`, "b.rs": `// ${SHAPES.ghp()}\n`, ".env.example": "A=\n", "same-bytes.txt": `// ${SHAPES.ghp()}\n` });
  putRecords(root, records(record(root, "a.rs", "fixture")));
  assert.deepEqual(gate(root).map((v) => v.split(":")[0]).sort(), [".env.example", "b.rs", "same-bytes.txt"], "identical bytes at another path are not admitted");
  // An env-template record cannot admit a non-template, and a fixture record cannot admit a template: both are rejected as records.
  putRecords(root, records(record(root, "b.rs", "env-template"), record(root, ".env.example", "fixture")));
  const out = gate(root).join("\n");
  assert.match(out, /record 0 \("b\.rs"\): kind "env-template" names a path that is not an env template/);
  assert.match(out, /record 1 \("\.env\.example"\): kind "fixture" cannot admit an env template/);
  assert.match(out, /^b\.rs:1: content matches/m, "the rejected record admits nothing");
  assert.match(out, /^\.env\.example: env template .* is not recorded/m);
});

test("secret scan: .env, every non-template .env.*, and key files are never recordable", (t) => {
  const names = [".env", ".env.local", ".env.production", ".env.example.bak", "config/.env", "keys/a.pem", "keys/b.p8", "keys/c.p12", "keys/d.key"];
  const root = tree(t, Object.fromEntries(names.map((n) => [n, "A=1\n"])));
  const refused = gate(root);
  assert.deepEqual(refused.map((v) => v.split(":")[0]).sort(), [...names].sort());
  for (const v of refused) assert.match(v, /: refused by name \(an \.env file, a key or certificate file, or build output\); it is never committed and cannot be recorded$/);
  putRecords(root, records(...names.map((n) => record(root, n, "fixture"))));
  const again = gate(root);
  assert.equal(again.filter((v) => /: refused by name/.test(v)).length, names.length, "a record does not admit any of them");
  for (const n of names) assert.match(again.join("\n"), new RegExp(`record \\d+ \\("${n.replaceAll(".", "\\.")}"\\): names a path that can never be recorded`), `a record naming ${n} is itself a violation`);
});

test("secret scan: a template with a PEM private key or another high-precision credential shape is refused even when recorded", (t) => {
  const shapes = { pem: SHAPES.pem(), pemRsa: SHAPES.pemRsa(), pemEncrypted: SHAPES.pemEncrypted(), pemPgp: SHAPES.pemPgp(), githubPat: SHAPES.githubPat(), stripe: SHAPES.stripe(),
    slack: SHAPES.slack(), google: SHAPES.google(), webhook: SHAPES.webhook(), ghp: SHAPES.ghp(), akia: SHAPES.akia(), npm: SHAPES.npm() };
  const files = Object.fromEntries(Object.entries(shapes).map(([name, value]) => [`.env.${name}.example`, `# placeholders\nFIRST=1\nCRED=${value}\n`]));
  const root = tree(t, { ...files, ".env.clean.example": "SECRET_KEY=<generate with openssl rand -hex 32>\nSTRIPE=sk_live_xxxx\nSLACK=xoxb-token\nGOOGLE=AIza...\n" });
  putRecords(root, records(...Object.keys(files).map((n) => record(root, n, "env-template")), record(root, ".env.clean.example", "env-template")));
  const out = gate(root);
  assert.equal(out.length, Object.keys(files).length, "every credential template is refused although recorded; the clean one passes");
  for (const v of out) {
    assert.match(v, /^\.env\.[a-zA-Z]+\.example:3: env template matches .* \(line 3\); refused even when recorded\. Replace the value with a placeholder$/);
    for (const value of Object.values(shapes)) assert.ok(!v.includes(value), "the matched text is never printed");
  }
  assert.match(out.find((v) => v.startsWith(".env.pemEncrypted.")), /a PEM private key header \(line 3\)/);
  assert.match(out.find((v) => v.startsWith(".env.pemPgp.")), /a PEM private key header/);
  assert.match(out.find((v) => v.startsWith(".env.webhook.")), /a Slack webhook URL/);
});

test("secret scan: a template that is not UTF-8 is refused even when recorded", (t) => {
  const utf16 = Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from("DB_PASSWORD=Tr0ub4dor\nGITHUB=" + SHAPES.ghp() + "\n", "utf16le")]);
  const root = tree(t, {
    ".env.utf16.example": utf16,
    ".env.nul.example": Buffer.concat([Buffer.from("A=1\n"), Buffer.from([0]), Buffer.from("B=2\n")]),
    ".env.latin.example": Buffer.from([0x41, 0x3d, 0xc3, 0x28, 0x0a]),
    ".env.bom.example": Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from("A=1\n")]),
  });
  putRecords(root, records(...[".env.utf16.example", ".env.nul.example", ".env.latin.example", ".env.bom.example"].map((n) => record(root, n, "env-template"))));
  const out = gate(root);
  assert.deepEqual(out.map((v) => v.split(":")[0]), [".env.latin.example", ".env.nul.example", ".env.utf16.example"]);
  assert.match(out[0], /env template is not valid UTF-8, so it cannot be read as UTF-8 text; refused even when recorded\. Save it as UTF-8$/);
  assert.match(out[1], /env template contains NUL bytes/);
  assert.match(out[2], /env template starts with a UTF-16 byte-order mark/);
  assert.ok(!out.join("\n").includes(SHAPES.ghp()));
});

test("secret scan: a malformed records file is a violation and a bad record admits nothing", (t) => {
  const root = tree(t, { ".env.example": "A=\n", ".env.other.example": "B=\n" });
  const good = record(root, ".env.example", "env-template");
  const other = record(root, ".env.other.example", "env-template");
  const problems = (doc) => { putRecords(root, typeof doc === "string" ? doc : JSON.stringify(doc)); return gate(root).filter((v) => v.startsWith(`${RECORDS}: `)).map((v) => v.slice(RECORDS.length + 2)); };
  const bad = [
    ["bad JSON", "{ nope", /^is not valid JSON/],
    ["not an object", "[]", /^must be an object/],
    ["unknown schema", { schema: 2, records: [good] }, /^schema must be 1 \(found 2\); no record is read/],
    ["no schema", { records: [good] }, /^schema must be 1 \(found undefined\)/],
    ["records not an array", { schema: 1, records: {} }, /^"records" must be an array/],
    ["unknown top-level key", { schema: 1, records: [good], extra: 1 }, /^has an unknown key "extra"/],
    ["unknown kind", { schema: 1, records: [{ ...good, kind: "secret", note: "n" }] }, /^record 0 \("\.env\.example"\): "kind" must be one of env-template, fixture/],
    ["missing note", { schema: 1, records: [good] }, /^record 0 \("\.env\.example"\): "note" must say what the file holds and why it is safe/],
    ["empty note", { schema: 1, records: [{ ...good, note: "   " }] }, /"note" must say/],
    ["non-hex sha", { schema: 1, records: [{ ...good, note: "n", sha256: "z".repeat(64) }] }, /"sha256" must be 64 hex characters/],
    ["short sha", { schema: 1, records: [{ ...good, note: "n", sha256: "abc" }] }, /"sha256" must be 64 hex characters/],
    ["unknown record key", { schema: 1, records: [{ ...good, note: "n", reason: "x" }] }, /has an unknown key "reason"/],
    ["not an object record", { schema: 1, records: ["x"] }, /^record 0 must be an object/],
    ["parent path", { schema: 1, records: [{ ...good, note: "n", path: "../.env.example" }] }, /"path" must be a normalised repository-relative path/],
    ["absolute path", { schema: 1, records: [{ ...good, note: "n", path: "/.env.example" }] }, /"path" must be a normalised/],
    ["backslash path", { schema: 1, records: [{ ...good, note: "n", path: "a\\.env.example" }] }, /"path" must be a normalised/],
    ["dot-slash path", { schema: 1, records: [{ ...good, note: "n", path: "./.env.example" }] }, /"path" must be a normalised/],
  ];
  for (const [name, doc, expected] of bad) assert.match(problems(doc).join("\n"), expected, name);
  // Duplicate paths: reported once, and neither record admits.
  const dup = problems({ schema: 1, records: [{ ...good, note: "one" }, { ...good, note: "two" }, { ...other, note: "n" }] });
  assert.deepEqual(dup, ['".env.example" is recorded 2 times; keep one record per path']);
  assert.match(gate(root).join("\n"), /^\.env\.example: env template .* is not recorded/m, "a duplicated path admits nothing");
  assert.ok(!gate(root).some((v) => v.startsWith(".env.other.example")), "while a valid record next to the bad ones is honoured");
  // Bad JSON or an unknown schema admits nothing at all.
  putRecords(root, "{ nope");
  assert.equal(gate(root).filter((v) => /is not recorded/.test(v)).length, 2);
  // The strict scan never reads the file, so a malformed one does not matter there.
  assert.ok(!scanCrateForSecrets(root).some((v) => v.startsWith(".rightkit")));
});

test("secret scan: a symbolic link or a non-file in place of the records file admits nothing", (t) => {
  const root = tree(t, { ".env.example": "A=\n" });
  const outside = tree(t, { "records.json": records({ path: ".env.example", sha256: sha("A=\n"), kind: "env-template" }) });
  mkdirSync(path.join(root, ".rightkit"));
  try { symlinkSync(path.join(outside, "records.json"), path.join(root, RECORDS)); } catch { t.skip("symbolic links are not available here"); return; }
  const out = gate(root).join("\n");
  assert.match(out, /\.rightkit\/secret-scan-records\.json: is not a regular file/);
  assert.match(out, /\.env\.example: env template .* is not recorded/);
  rmSync(path.join(root, RECORDS));
  mkdirSync(path.join(root, RECORDS));
  assert.match(gate(root).join("\n"), /\.rightkit\/secret-scan-records\.json: is not a regular file/);
});

test("secret scan: a template inside a publishable crate or package is refused by name even when recorded", (t) => {
  const root = tree(t, {
    "package.json": '{"name":"app","private":true}',
    ".env.example": "A=\n",
    "apps/priv/package.json": '{"name":"priv","private":true}',
    "apps/priv/.env.example": "A=\n",
    "apps/priv/src-tauri/Cargo.toml": '[package]\nname = "priv"\npublish = false\n',
    "apps/priv/src-tauri/.env.example": "A=\n",
    "apps/priv/src-tauri/crates/lib/Cargo.toml": '[package]\nname = "lib"\npublish = []\n',
    "apps/priv/src-tauri/crates/lib/.env.example": "A=\n",
    "packages/pub/package.json": '{"name":"pub","version":"1.0.0"}',
    "packages/pub/.env.example": "A=\n",
    "packages/pub/deep/er/.env.example": "A=\n",
    "crates/pub/Cargo.toml": '[package]\nname = "pub"\nversion = "1.0.0"\n',
    "crates/pub/.env.example": "A=\n",
    "crates/virtual/Cargo.toml": '[workspace]\nmembers = []\n',
    "crates/virtual/.env.example": "A=\n",
    "crates/broken/Cargo.toml": '[package]\nname = "broken"\npublish = false\n',
    "crates/broken/package.json": "{ not json",
    "crates/broken/.env.example": "A=\n",
  });
  const templates = Object.keys({ ".env.example": 0 }).concat(["apps/priv/.env.example", "apps/priv/src-tauri/.env.example", "apps/priv/src-tauri/crates/lib/.env.example", "packages/pub/.env.example", "packages/pub/deep/er/.env.example", "crates/pub/.env.example", "crates/virtual/.env.example", "crates/broken/.env.example"]);
  putRecords(root, records(...templates.map((n) => record(root, n, "env-template"))));
  const out = gate(root);
  const byPath = Object.fromEntries(out.map((v) => [v.split(":")[0], v]));
  assert.deepEqual(Object.keys(byPath).sort(), ["crates/broken/.env.example", "crates/pub/.env.example", "packages/pub/.env.example", "packages/pub/deep/er/.env.example"],
    "private apps, unpublished crates (publish = false or []) and a virtual workspace carry templates; a publishable crate or package, or an unreadable manifest, does not");
  assert.match(byPath["crates/pub/.env.example"], /env template inside the publishable crate at crates\/pub \(its Cargo\.toml \[package\] does not say publish = false\); a publishable unit never carries one \(refused by name, as at publish time/);
  assert.match(byPath["packages/pub/deep/er/.env.example"], /inside the publishable package at packages\/pub \(its package\.json is not "private": true\)/);
  assert.match(byPath["crates/broken/.env.example"], /inside the publishable package at crates\/broken/);
});

test("secret scan: violations name the repository path and the line, in a unit below the repository root", (t) => {
  const root = tree(t, {
    "services/api/package.json": '{"name":"api","private":true}',
    "services/api/src/vec.ts": `export const a = 1;\r\nexport const b = "${SHAPES.npm()}";\r\n\r\nexport const c = "${SHAPES.ghp()}";\r\n`,
  });
  const out = gate(root, "services/api");
  assert.equal(out.length, 1);
  assert.match(out[0], /^services\/api\/src\/vec\.ts:2: content matches an npm token \(npm_\) \(line 2\); a GitHub token \(gh\*_\) \(line 4\); sha256 /);
  putRecords(root, records({ path: "services/api/src/vec.ts", sha256: sha(readFileSync(path.join(root, "services/api/src/vec.ts"))), kind: "fixture" }));
  assert.deepEqual(gate(root, "services/api"), [], "the record is by repository path, read from the repository root");
});

test("secret scan: every expression is linear; a 1 MB line of adversarial input scans in well under a second", (t) => {
  const MB = 1_000_000;
  const inputs = {
    pemWords: "-----BEGIN " + "A ".repeat(MB / 2),
    pemStarts: "-----BEGIN ".repeat(MB / 11),
    spaces: "A=" + " ".repeat(MB) + "x",
    ghpRuns: ("gh" + "p_").repeat(MB / 4),
    ghpMatches: ("gh" + "p_" + "A".repeat(24) + " ").repeat(MB / 29),
    patRun: "github" + "_pat_" + "_".repeat(MB),
    webhook: "hooks.slack" + ".com/services/" + "T" + "A".repeat(MB),
    webhookRepeat: ("hooks.slack" + ".com/services/T").repeat(MB / 26),
    xoxRuns: ("xox" + "b-").repeat(MB / 5),
    aizaRuns: ("AI" + "za").repeat(MB / 4),
    stripeRun: "sk" + "_live_" + "a".repeat(15) + "-".repeat(MB),
    manyLines: "x\n".repeat(MB / 2) + "gh" + "p_" + "A".repeat(24),
  };
  const files = {};
  for (const [name, body] of Object.entries(inputs)) { files[`.env.${name}.example`] = body; files[`vec/${name}.txt`] = body; }
  const root = tree(t, files);
  const started = performance.now();
  const out = gate(root);
  const elapsed = performance.now() - started;
  assert.ok(elapsed < 5000, `scanned ${Object.keys(files).length} adversarial files in ${Math.round(elapsed)} ms`);
  assert.ok(out.length >= 2, "the files that do hold a shape are still reported");
  assert.match(out.find((v) => v.startsWith("vec/manyLines.txt")), /^vec\/manyLines\.txt:500001: content matches a GitHub token \(gh\*_\) \(line 500001\)/);
});

test("secret scan: the shapes of SECRET_PATTERNS each carry a label", (t) => {
  const root = tree(t, { "a.txt": SHAPES.pem(), "b.txt": SHAPES.akia(), "c.txt": SHAPES.ghp(), "d.txt": SHAPES.npm() });
  const out = gate(root);
  assert.equal(out.length, 4);
  for (const v of out) assert.doesNotMatch(v, /undefined/);
  assert.deepEqual(out.map((v) => /content matches (.*?) \(line 1\)/.exec(v)[1]), ["a PEM private key header", "an AWS access key id (AKIA)", "a GitHub token (gh*_)", "an npm token (npm_)"]);
});

test("secret scan detects credentials beside NUL bytes while accepting benign binary assets", (t) => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-secrets-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const credential = Buffer.from("gh" + "p_" + "A".repeat(24));
  writeFileSync(path.join(root, "prefix.bin"), Buffer.concat([Buffer.from([0]), credential]));
  writeFileSync(path.join(root, "suffix.bin"), Buffer.concat([credential, Buffer.from([0])]));
  writeFileSync(path.join(root, "benign.bin"), Buffer.from([0, 255, 128, 1, 0, 254]));
  assert.deepEqual(scanCrateForSecrets(root), ["prefix.bin", "suffix.bin"]);
});

function fixture(t, { verifyOnPublish = false } = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-publish-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const name = "rightkit-fixture", version = "1.0.0", bytes = Buffer.from("fixture crate archive");
  const crateDir = path.join(root, "crates", name), target = path.join(root, "generated-target");
  mkdirSync(crateDir, { recursive: true });
  writeFileSync(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${name}"\nversion = "${version}"\n[package.metadata.rightkit]\ncontinuous = false\ncontinuous_reason = "fixture library with no continuous work"\n`);
  mkdirSync(path.join(target, "package"), { recursive: true });
  const crateFile = path.join(target, "package", `${name}-${version}.crate`);
  writeFileSync(crateFile, bytes);
  const evidence = { artifactSha256: digestBytes(bytes) }, events = [];
  const execute = (command, args, settings) => {
    assert.equal(command, "cargo");
    assert.equal(settings.cwd, root);
    assert.equal(settings.windowsHide, true);
    if (args[0] === "package" && args.includes("--no-verify")) events.push("package");
    if (args[0] === "publish" && !args.includes("--dry-run")) {
      assert.deepEqual(args, ["publish", verifyOnPublish ? "--locked" : "--no-verify", "-p", name]);
      events.push("publish");
    }
    return { status: 0, stdout: args[0] === "metadata" ? JSON.stringify({ target_directory: target })
      : args.includes("--list") ? "Cargo.toml\nsrc/lib.rs\n" : "" };
  };
  let gateCalls = 0;
  const deps = {
    enforceGate: () => { if (++gateCalls === 2) events.push("hash check"); return { ok: true, evidence }; },
    fetch: async () => {
      events.push("readback");
      return { ok: true, text: async () => JSON.stringify({ name, vers: version, cksum: evidence.artifactSha256 }) };
    },
  };
  // A capability for exactly this crate archive, created through the production admission validator.
  const capability = mintTestCapability({ registry: "crates", archives: [{ name, version, sha256: evidence.artifactSha256 }] });
  return { root, name, version, bytes, crateFile, evidence, events, execute, deps, capability, options: { workspaceRoot: root, crate: name, capability } };
}

test("Cargo publisher packages, checks evidence hash, publishes through same runner & reads back", async (t) => {
  const fx = fixture(t);
  const result = await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.deepEqual(fx.events, ["package", "hash check", "publish", "readback"]);
  assert.deepEqual(result, { name: fx.name, version: fx.version, sha256: fx.evidence.artifactSha256 });
});

test("local offline inspections capture output while registry publish stays online", async (t) => {
  const fx = fixture(t, { verifyOnPublish: true }), seen = [];
  const execute = (command, args, settings) => {
    seen.push({ args, stdio: settings.stdio });
    return fx.execute(command, args, settings);
  };
  await runCargoPublish({ ...fx.options, offlineReadOnly: true, verifyOnPublish: true }, execute, fx.deps);
  // The archive step (`package --no-verify`) is packaged online and locked; only inspections are offline.
  for (const entry of seen.filter(({ args }) => ["package", "metadata"].includes(args[0]) && !args.includes("--no-verify"))) {
    assert.ok(entry.args.includes("--offline"));
    assert.equal(entry.stdio, "pipe");
  }
  const upload = seen.find(({ args }) => args[0] === "publish" && !args.includes("--dry-run"));
  assert.ok(upload);
  assert.equal(upload.args.includes("--offline"), false);
});

test("runtime-budget crate without a measured runtime-budget.json is refused before packaging or upload", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${fx.name}"\nversion = "${fx.version}"\n[package.metadata.rightkit]\nruntime_budget = true\n`);
  writeFileSync(path.join(crateDir, "README.md"), "# fixture\n\n## Runtime budget\n\n- Resident memory: 40 MB per 1,000 entries\n");
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), (error) => {
    assert.equal(error.code, "RUNTIME_BUDGET_MISSING");
    assert.match(error.message, /runtime budget refused/);
    assert.match(error.message, /runtime-budget\.json is missing/);
    return true;
  });
  assert.deepEqual(fx.events, []);
});

test("runtime-budget crate dry-run reports the failing check and still validates", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${fx.name}"\nversion = "${fx.version}"\n[package.metadata.rightkit]\nruntime_budget = true\n`);
  writeFileSync(path.join(crateDir, "README.md"), "# fixture\n");
  const lines = [];
  const write = process.stdout.write.bind(process.stdout);
  process.stdout.write = (chunk) => { lines.push(String(chunk)); return true; };
  let result;
  try {
    result = await runCargoPublish({ ...fx.options, dryRun: true }, fx.execute, fx.deps);
  } finally {
    process.stdout.write = write;
  }
  assert.deepEqual(result, { name: fx.name, version: fx.version, dryRun: true });
  assert.match(lines.join(""), /\[FAIL\] runtime-budget: rightkit-fixture: runtime-budget\.json is missing/);
  assert.deepEqual(fx.events, []);
});

test("archive hash mismatch refuses publication before Cargo upload or readback", async (t) => {
  const fx = fixture(t);
  writeFileSync(fx.crateFile, "different archive");
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), { code: "PUBLICATION_HASH_MISMATCH" });
  assert.deepEqual(fx.events, ["package", "hash check"]);
});

test("changed publication evidence refuses Cargo upload", async (t) => {
  const fx = fixture(t);
  const execute = (command, args, settings) => {
    const result = fx.execute(command, args, settings);
    if (args[0] === "package" && args.includes("--no-verify")) fx.evidence.artifactSha256 = "b".repeat(64);
    return result;
  };
  await assert.rejects(runCargoPublish(fx.options, execute, fx.deps), /evidence changed during packaging/);
  assert.deepEqual(fx.events, ["package", "hash check"]);
});

test("failed Cargo publish prevents registry readback", async (t) => {
  const fx = fixture(t);
  const execute = (command, args, settings) => {
    const result = fx.execute(command, args, settings);
    return args[0] === "publish" && !args.includes("--dry-run") ? { status: 1 } : result;
  };
  await assert.rejects(runCargoPublish(fx.options, execute, fx.deps), /registry publish failed/);
  assert.deepEqual(fx.events, ["package", "hash check", "publish"]);
});

test("registry mismatch propagates from publisher without executing yank", async (t) => {
  const fx = fixture(t);
  fx.deps.fetch = async () => ({ ok: true, text: async () => JSON.stringify({ name: fx.name, vers: fx.version, cksum: "b".repeat(64) }) });
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), { code: "PUBLICATION_REGISTRY_MISMATCH" });
  assert.deepEqual(fx.events, ["package", "hash check", "publish"]);
});

// A direct import must not publish: a real run without a capability from a validated admission refuses before the gate, packaging
// or any command; a capability for other bytes refuses before the registry upload.
test("a real publish refuses without a publish capability from a validated admission, before the gate or any command", async (t) => {
  const fx = fixture(t);
  let gated = 0;
  const deps = { ...fx.deps, enforceGate: () => { gated += 1; return { ok: true, evidence: fx.evidence }; } };
  const other = mintTestCapability({ registry: "crates", archives: [{ name: "rightkit-other", version: fx.version, sha256: fx.evidence.artifactSha256 }] });
  for (const [label, capability] of Object.entries({
    "no capability": undefined,
    "an object literal shaped like one": { kind: "rightkit-publish-capability", registry: "crates" },
    "a copy of a real capability": { ...fx.capability },
    "the capability of another crate": other,
    "an npm capability": mintTestCapability({ registry: "npm", archives: [{ name: fx.name, version: fx.version, sha256: fx.evidence.artifactSha256 }] }),
  })) {
    await assert.rejects(runCargoPublish({ ...fx.options, capability }, fx.execute, deps), (error) => error.code === HOST_UPLOAD_REFUSED && /host uploads are retired/.test(error.message), label);
  }
  assert.equal(gated, 0, "refused before the gate");
  assert.deepEqual(fx.events, [], "and before any command");
  await assert.rejects(runCargoPublish({ ...fx.options, fromEvidence: true, evidence: "gate.json", capability: undefined }, fx.execute, deps), { code: HOST_UPLOAD_REFUSED });
});

test("a capability for other bytes refuses before the registry upload and readback", async (t) => {
  const fx = fixture(t);
  const wrongBytes = mintTestCapability({ registry: "crates", archives: [{ name: fx.name, version: fx.version, sha256: "e".repeat(64) }] });
  await assert.rejects(runCargoPublish({ ...fx.options, capability: wrongBytes }, fx.execute, fx.deps), (error) => error.code === HOST_UPLOAD_REFUSED && /are not the admitted archive/.test(error.message));
  assert.deepEqual(fx.events, ["package", "hash check"], "packaged and hash-checked, never uploaded");
  const wrongVersion = mintTestCapability({ registry: "crates", archives: [{ name: fx.name, version: "9.9.9", sha256: fx.evidence.artifactSha256 }] });
  await assert.rejects(runCargoPublish({ ...fx.options, capability: wrongVersion }, fx.execute, fx.deps), (error) => error.code === HOST_UPLOAD_REFUSED && /is not the admitted 9\.9\.9/.test(error.message));
});

test("a dry run needs no capability", async (t) => {
  const fx = fixture(t);
  assert.deepEqual(await runCargoPublish({ ...fx.options, capability: undefined, dryRun: true }, fx.execute, fx.deps), { name: fx.name, version: fx.version, dryRun: true });
});

test("dry-run publisher never checks an archive, uploads or reads registry", async (t) => {
  const fx = fixture(t);
  assert.deepEqual(await runCargoPublish({ ...fx.options, dryRun: true }, fx.execute, fx.deps),
    { name: fx.name, version: fx.version, dryRun: true });
  assert.deepEqual(fx.events, []);
});

test("publication gate compares actual artifact buffer to evidence", () => {
  const commit = "a".repeat(40), bytes = Buffer.from("fixture crate archive"), name = "rightkit-fixture", version = "1.0.0";
  const options = { kind: "cargo", name, version, packagePath: "/fixture", evidencePath: "/evidence", artifactBytes: bytes,
    git: (args) => args[0] === "rev-parse" ? commit : "",
    readFile: () => JSON.stringify({ schema: 1, kind: "cargo", name, version, commit, artifactSha256: digestBytes(bytes),
      runs: ["darwin", "win32"].map((os) => ({ host: { os, arch: "arm64" }, commit, features: ["default"], artifactSha256: digestBytes(bytes), command: "node --test", passed: 1, failed: 0, operator: "test", timestamp: "2026-10-08" })) }),
  };
  assert.equal(checkPublicationGate(options).ok, true);
  assert.equal(checkPublicationGate({ ...options, artifactBytes: Buffer.from("changed") }).ok, false);
});

test("release source never accesses Cargo credential files or registry token environment variables", () => {
  const root = fileURLToPath(new URL(".", import.meta.url));
  const forbidden = /\bCARGO_REGISTRY_TOKEN\b|\bCARGO_REGISTRIES_[A-Z0-9_]+_TOKEN\b|["'`]credentials\.toml["'`]|\.cargo[/\\]credentials(?:\.toml)?/;
  const inspect = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory() && !["node_modules", "fixtures", "test"].includes(entry.name)) inspect(file);
      else if (entry.isFile() && /\.(?:mjs|js|ts|py|sh|ps1)$/.test(entry.name) && !/\.test\./.test(entry.name)) {
        assert.doesNotMatch(readFileSync(file, "utf8"), forbidden, path.relative(root, file));
      }
    }
  };
  inspect(root);
});

test("publish refuses a Cargo.toml whose [package].name is not the requested crate", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(crateDir, "Cargo.toml"), '[package]\nname = "rightkit-renamed"\nversion = "1.0.0"\n');
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /\[package\]\.name "rightkit-renamed" differs from --crate rightkit-fixture/);
  assert.deepEqual(fx.events, []);
});

test("publish refuses an undeclared crate: neither runtime-budget.json nor continuous = false", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(crateDir, "Cargo.toml"), '[package]\nname = "rightkit-fixture"\nversion = "1.0.0"\n');
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), (error) => {
    assert.equal(error.code, "RUNTIME_BUDGET_MISSING");
    assert.match(error.message, /undeclared/);
    return true;
  });
  assert.deepEqual(fx.events, []);
});

test("publish refuses continuous = false when src spawns a thread and the reason does not name it", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  mkdirSync(path.join(crateDir, "src"), { recursive: true });
  writeFileSync(path.join(crateDir, "src", "lib.rs"), "pub fn go() { std::thread::spawn(|| {}); }\n");
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /thread-spawn \(src\/lib\.rs\)/);
  assert.deepEqual(fx.events, []);
});

test("version.workspace = true resolves through the workspace root instead of a regex miss", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(fx.root, "Cargo.toml"), '[workspace]\nmembers = ["crates/*"]\n[workspace.package]\nversion = "1.0.0"\n');
  writeFileSync(path.join(crateDir, "Cargo.toml"), '[package]\nname = "rightkit-fixture"\nversion.workspace = true\n[package.metadata.rightkit]\ncontinuous = false\ncontinuous_reason = "fixture library"\n');
  const result = await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.equal(result.version, "1.0.0");
});

test("proven steps are removed from the plan, never the runtime budget, package contents, archive hash or readback", () => {
  const all = { tests: true, clippy: true, fmt: true, secretScan: true };
  const plan = buildCargoPublishPlan({ crate: "rightkit-license", crateDir: "C:/rightkit/crates/rightkit-license", dryRun: false, proven: all, verifyOnPublish: true });
  assert.deepEqual(plan.map((step) => step.label), [
    "runtime budget", "package contents", "package archive", "package location", "archive hash check", "registry publish", "registry readback",
  ]);
  assert.deepEqual(plan.find((step) => step.label === "registry publish").args, ["publish", "--locked", "-p", "rightkit-license"]);
  // Partial proof removes only what is proven; an unproven step still runs.
  const partial = buildCargoPublishPlan({ crate: "rightkit-license", crateDir: "x", dryRun: false, proven: { tests: true, clippy: true } });
  assert.deepEqual(partial.map((step) => step.label).slice(0, 4), ["secret scan", "runtime budget", "format", "package contents"]);
  // No proof (the plain publisher) keeps every step; a dry-run keeps its dry-run even with verifyOnPublish.
  assert.equal(buildCargoPublishPlan({ crate: "c", crateDir: "x", dryRun: false, proven: { tests: false } }).length, 12);
  const dry = buildCargoPublishPlan({ crate: "c", crateDir: "x", dryRun: true, proven: all, verifyOnPublish: true });
  assert.equal(dry.at(-1).label, "registry dry-run");
  assert.equal(buildCargoPublishPlan({ crate: "c", crateDir: "x", dryRun: false, verifyOnPublish: true }).some((step) => step.label === "registry dry-run"), false);
});

test("trustEvidenceSteps skips only what the evidence proves; verifyOnPublish publishes with Cargo's verify build", async (t) => {
  const fx = fixture(t);
  const commit = "a".repeat(40);
  const stepsRun = [];
  const execute = (command, args) => {
    stepsRun.push(args.join(" "));
    return { status: 0, stdout: args[0] === "metadata" ? JSON.stringify({ target_directory: path.join(fx.root, "generated-target") }) : args.includes("--list") ? "Cargo.toml\nsrc/lib.rs\n" : "" };
  };
  const evidence = {
    commit, name: fx.name, artifactSha256: fx.evidence.artifactSha256,
    runs: [{ host: { os: "darwin" }, commit, steps: [{ command: `cargo test -p ${fx.name} --no-fail-fast -- --show-output`, ok: true, executed: 3, failed: 0 }, { command: `cargo clippy -p ${fx.name} --no-deps --all-targets -- -D warnings`, ok: true }] }],
  };
  const deps = { ...fx.deps, enforceGate: () => ({ ok: true, evidence }) };
  const lines = [];
  const write = process.stdout.write.bind(process.stdout);
  process.stdout.write = (chunk) => { lines.push(String(chunk)); return true; };
  try { await runCargoPublish({ ...fx.options, trustEvidenceSteps: true, verifyOnPublish: true }, execute, deps); } finally { process.stdout.write = write; }
  // fmt has no batch receipt, so it still runs; tests and clippy are proven; the dry-run is gone.
  assert.deepEqual(stepsRun, [`fmt -p ${fx.name} --check`, `package --list -p ${fx.name}`, `package --locked --no-verify -p ${fx.name}`, "metadata --format-version 1 --no-deps", `publish --locked -p ${fx.name}`]);
  assert.match(lines.join(""), /cargo tests: proven by the gate evidence/);
  // Without trustEvidenceSteps the same evidence skips nothing (a plain publish proved nothing in advance).
  stepsRun.length = 0;
  await runCargoPublish({ ...fx.options }, (command, args) => { const result = execute(command, args); stepsRun.splice(stepsRun.length - 1, 1, args[0]); return result; }, deps).catch(() => {});
  assert.ok(stepsRun.includes("test") && stepsRun.includes("clippy") && stepsRun.includes("fmt"));
});

test("an async execute is awaited", async (t) => {
  const fx = fixture(t);
  const seen = [];
  const execute = async (command, args, settings) => { await new Promise((resolve) => setTimeout(resolve, 1)); seen.push(args[0]); return fx.execute(command, args, settings); };
  await runCargoPublish(fx.options, execute, fx.deps);
  assert.ok(seen.includes("publish"));
  assert.deepEqual(fx.events, ["package", "hash check", "publish", "readback"]);
});

test('from-evidence is explicit and refuses ambiguous or dry-run options', () => {
  const base = ['--crate', 'rightkit-fixture', '--from-evidence'];
  assert.throws(() => parseCargoPublishArgs(base), /requires a gate evidence file/);
  assert.throws(() => parseCargoPublishArgs([...base, '--dry-run']), /requires a gate evidence file/);
  assert.throws(() => parseCargoPublishArgs([...base, 'gate.json', '--dry-run']), /cannot be combined/);
  assert.throws(() => parseCargoPublishArgs([...base, 'gate.json', '--evidence', 'other.json']), /cannot be combined/);
  const parsed = parseCargoPublishArgs([...base, 'gate.json']);
  assert.equal(parsed.evidence, 'gate.json');
  assert.equal(parsed.fromEvidence, true);
});

function qualifiedFixture(t) {
  const fx = fixture(t);
  const commit = 'a'.repeat(40);
  mkdirSync(path.join(fx.root, 'ci'), { recursive: true });
  writeFileSync(path.join(fx.root, 'ci', 'gate.config.json'), '{}');
  const planned = localTestPlan({ kind: 'cargo', unit: fx.name, dir: `crates/${fx.name}`, config: {}, platform: 'win32' });
  const evidence = {
    schema: 1, kind: 'cargo', name: fx.name, version: fx.version, commit,
    artifactSha256: fx.evidence.artifactSha256, hostPolicy: 'win32-only', hostPolicyReason: 'local-build grant',
    runs: [{ host: { os: 'win32', arch: 'x64' }, commit, artifactSha256: fx.evidence.artifactSha256,
      features: ['default'], command: 'qualification', operator: 'agent', timestamp: '2026-10-18T00:00:00Z', passed: 3, failed: 0,
      steps: [
        ...planned.steps.map((step) => ({ command: `${step.command} ${step.args.join(' ')}`, ok: true, ...(step.test ? { executed: 3, failed: 0 } : {}) })),
        { command: `cargo package --locked -p ${fx.name}`, ok: true },
      ],
    }],
  };
  const file = path.join(fx.root, 'gate.json');
  const save = () => writeFileSync(file, JSON.stringify(evidence));
  save();
  const commands = [];
  const execute = (_command, args) => {
    commands.push(args);
    return { status: 0, stdout: args[0] === 'metadata' ? JSON.stringify({ target_directory: path.join(fx.root, 'generated-target') }) : args.includes('--list') ? 'Cargo.toml\nsrc/lib.rs\n' : '' };
  };
  const deps = { ...fx.deps, workspaceStatus: () => ({ status: 0, stdout: '' }), enforceGate: (options) => {
    const result = checkPublicationGate({ ...options, git: (args) => args[0] === 'rev-parse' ? commit : '' });
    if (!result.ok) throw new Error(`publication gate not satisfied: ${result.missing.join('; ')}`);
    return result;
  } };
  return { ...fx, evidence, save, commands, execute, deps, options: { ...fx.options, evidence: file, fromEvidence: true } };
}

test('Windows qualification can publish with no repeated tests, clippy or compile; locked archive hash remains enforced', async (t) => {
  const fx = qualifiedFixture(t);
  await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.equal(fx.commands.some((args) => ['test', 'clippy', 'check', 'build'].includes(args[0])), false);
  assert.equal(fx.commands.some((args) => args.includes('--dry-run')), false);
  assert.deepEqual(fx.commands.find((args) => args[0] === 'publish'), ['publish', '--locked', '--no-verify', '-p', fx.name]);
  assert.ok(fx.commands.some((args) => args[0] === 'package' && args.includes('--no-verify')));
});

test('from-evidence refuses unverified packaging before any command', async (t) => {
  const fx = qualifiedFixture(t);
  fx.evidence.runs[0].steps.pop(); fx.save();
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /requires a verified cargo package/);
  assert.equal(fx.commands.length, 0);
});

test('from-evidence refuses changed commit or version before any command', async (t) => {
  const fx = qualifiedFixture(t);
  fx.evidence.commit = 'b'.repeat(40); fx.save();
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /current HEAD/);
  fx.evidence.commit = 'a'.repeat(40); fx.evidence.version = '9.9.9'; fx.save();
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /evidence version/);
  assert.equal(fx.commands.length, 0);
});

test('from-evidence refuses differing local archive bytes before upload', async (t) => {
  const fx = qualifiedFixture(t);
  writeFileSync(fx.crateFile, 'different cross-host archive');
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), { code: 'PUBLICATION_HASH_MISMATCH' });
  assert.equal(fx.commands.some((args) => args[0] === 'publish'), false);
});


test('from-evidence refuses dirty workspace manifests before any command', async (t) => {
  const fx = qualifiedFixture(t);
  fx.deps.workspaceStatus = () => ({ status: 0, stdout: ' M Cargo.toml\n' });
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /requires clean workspace/);
  assert.equal(fx.commands.length, 0);
});


test('from-evidence refuses missing configured feature, serial or all-feature checks', async (t) => {
  const fx = qualifiedFixture(t);
  const entry = { featureSets: [[], ['--features', 'async']], alsoSerial: true };
  writeFileSync(path.join(fx.root, 'ci', 'gate.config.json'), JSON.stringify({ rust: { [fx.name]: entry } }));
  const planned = localTestPlan({ kind: 'cargo', unit: fx.name, dir: `crates/${fx.name}`, config: { rust: { [fx.name]: entry } }, platform: 'win32' });
  const complete = [
    ...planned.steps.map((step) => ({ command: `${step.command} ${step.args.join(' ')}`, ok: true, ...(step.test ? { executed: 3, failed: 0 } : {}) })),
    { command: `cargo package --locked -p ${fx.name}`, ok: true },
  ];
  for (const absent of complete.filter((row) => /--features async|--test-threads=1|^cargo check/.test(row.command))) {
    fx.evidence.runs[0].steps = complete.filter((row) => row !== absent); fx.save();
    await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /missing configured qualification step/);
  }
  assert.equal(fx.commands.length, 0);
});

test("from-evidence refuses uncommitted gate config edits, which decide the replayed steps (0.4.1 review A3)", async (t) => {
  const fx = qualifiedFixture(t);
  const seen = [];
  fx.deps.workspaceStatus = (workspaceRoot) => { seen.push(workspaceRoot); return { status: 0, stdout: " M ci/gate.config.json\n" }; };
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /requires clean workspace Cargo\.toml, Cargo\.lock and ci gate config/);
  assert.equal(fx.commands.length, 0);
  // The default status command names the gate config files beside the workspace manifest and lock.
  const source = readFileSync(new URL("./publish-cargo.mjs", import.meta.url), "utf8");
  assert.match(source, /"status", "--porcelain", "--", "Cargo\.toml", "Cargo\.lock", "ci\/gate\.config\.json", "ci\/asset-gated\.json"/);
});

test("from-evidence refuses a batch receipt that lints only some feature sets of the crate, and accepts one row per set (0.4.1 review A2)", async (t) => {
  const fx = qualifiedFixture(t);
  const commit = "a".repeat(40);
  const entry = { featureSets: [[], ["--features", "async"]] };
  const config = { rust: { [fx.name]: entry } };
  writeFileSync(path.join(fx.root, "ci", "gate.config.json"), JSON.stringify(config));
  const planned = localTestPlan({ kind: "cargo", unit: fx.name, dir: `crates/${fx.name}`, config, batch: true, platform: "win32" });
  const check = (name, command, extra = {}) => ({ name, command, ok: true, commit, tree: commit, ...extra });
  const lint = (features = []) => check("clippy", `cargo clippy -p ${fx.name} --message-format=json-render-diagnostics --no-deps --all-targets${features.length ? ` ${features.join(" ")}` : ""} -- -D warnings`, { crates: [fx.name] });
  const run = fx.evidence.runs[0];
  run.steps = [
    ...planned.steps.map((step) => ({ command: `${step.command} ${step.args.join(" ")}`, ok: true, ...(step.test ? { executed: 3, failed: 0 } : {}) })),
    { command: `cargo package --locked -p ${fx.name}`, ok: true },
  ];
  assert.equal(run.steps.some((row) => / clippy /.test(row.command)), false, "a batch run records no per-crate clippy step");
  run.batch = { id: "batch-1", commit, tree: commit, crates: [fx.name], checks: [
    check("fmt", "cargo fmt --all --check"), check("secret-scan", `scanCrateForSecrets crates/${fx.name}`, { crates: [fx.name] }), lint(),
  ] };
  fx.save();
  // A 0.4.0 batch receipt: clippy ran on the first feature set only. The gate still accepts it; replay must not.
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /missing batch clippy for feature set: --features async/);
  assert.equal(fx.commands.length, 0);
  run.batch.checks.push(lint(["--features", "async"])); fx.save();
  await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.ok(fx.commands.some((args) => args[0] === "publish"));
  assert.equal(fx.commands.some((args) => ["test", "clippy", "check", "build"].includes(args[0])), false);
});

test('from-evidence refuses any failed recorded step even beside complete passing evidence', async (t) => {
  const fx = qualifiedFixture(t);
  fx.evidence.runs[0].steps.push({ command: 'cargo check --features broken', ok: false }); fx.save();
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /unsuccessful qualification step/);
  assert.equal(fx.commands.length, 0);
});
