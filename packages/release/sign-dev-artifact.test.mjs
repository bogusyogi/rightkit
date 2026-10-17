import assert from "node:assert/strict";
import { appendFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileEvidence } from "./macos-file-evidence.mjs";
import { SignDevArtifactError, readDevArtifactSignManifest, signDevArtifact } from "./sign-dev-artifact.mjs";

const IDENTITY = "Developer ID Application: Orthic Labs (6KLGD3LLKF)";
const TEAM = "6KLGD3LLKF";
const NAMES = ["legion", "legion-hook", "legion-mcp"];


function prepare() {
  const f = (() => {
    const root = mkdtempSync(path.join(os.tmpdir(), "sign-dev-artifact-"));
    const artifact = path.join(root, "artifact");
    return { root, artifact };
  })();
  mkdirSync(f.artifact, { recursive: true });
  for (const name of NAMES) writeFileSync(path.join(f.artifact, name), `bytes of ${name}`);
  const metadata = path.join(f.artifact, "payload.json");
  const record = () => ({ schema: 1, files: NAMES.map((name) => { const e = fileEvidence(path.join(f.artifact, name)); return { path: name, sha256: e.sha256, sizeBytes: e.sizeBytes }; }) });
  writeFileSync(metadata, JSON.stringify(record()));
  const manifest = path.join(f.root, "sign.json");
  writeFileSync(manifest, JSON.stringify({ schema: 1, entries: NAMES.map((name) => ({ file: name, identifier: `com.example.${name}` })) }));
  return { ...f, metadata, manifest, evidence: path.join(f.root, "out", "evidence.json") };
}


// Fake codesign: signing appends a marker (so the bytes change) and remembers the identifier.
function fakes() {
  const state = { identifiers: new Map(), signed: [], argv: [] };
  return {
    state,
    resolveSigning: () => ({ identity: IDENTITY, teamIdentifier: TEAM, certificateSha1: "AA" }),
    signOne: (args) => {
      state.argv.push(args);
      const file = args.at(-1);
      state.identifiers.set(file, args[args.indexOf("--identifier") + 1]);
      appendFileSync(file, "\nSIGNED");
      state.signed.push(file);
      return { status: 0, stdout: "", stderr: "" };
    },
    inspect: (command, args) => {
      assert.equal(command, "codesign");
      const file = args.at(-1);
      if (args.includes("--verify")) return "valid on disk\nsatisfies its designated requirement";
      if (args.includes("-r-")) return `designated => identifier "${state.identifiers.get(file)}" and anchor apple generic and certificate leaf[subject.OU] = ${TEAM}\n`;
      return `Identifier=${state.identifiers.get(file)}\nAuthority=${IDENTITY}\nTeamIdentifier=${TEAM}\nTimestamp=Oct 17, 2026\n`;
    },
  };
}

const run = (p, extra = {}) => signDevArtifact({ artifact: p.artifact, manifestPath: p.manifest, evidencePath: p.evidence, platform: "darwin", ...fakes(), ...extra });

test("signs each loose executable with its fixed identifier, refreshes the payload metadata and writes evidence", () => {
  const p = prepare();
  try {
    const f = fakes();
    const before = new Map(NAMES.map((name) => [name, fileEvidence(path.join(p.artifact, name)).sha256]));
    const result = run(p, f);
    assert.deepEqual(f.state.argv.map((args) => args[args.indexOf("--identifier") + 1]), NAMES.map((name) => `com.example.${name}`));
    assert.ok(f.state.argv.every((args) => !args.includes("--deep")), "no --deep");
    assert.deepEqual(result.files.map((file) => file.path), NAMES);
    for (const file of result.files) {
      assert.equal(file.sha256Before, before.get(file.path));
      assert.notEqual(file.sha256After, file.sha256Before);
      assert.equal(file.sha256After, fileEvidence(path.join(p.artifact, file.path)).sha256);
      assert.equal(file.teamIdentifier, TEAM);
      assert.equal(file.identifier, `com.example.${file.path}`);
      assert.match(file.designatedRequirement, /^identifier /);
    }
    const metadata = JSON.parse(readFileSync(p.metadata, "utf8"));
    assert.deepEqual(metadata.files.map((item) => item.sha256), result.files.map((file) => file.sha256After));
    assert.equal(metadata.signing.teamIdentifier, TEAM);
    assert.deepEqual(JSON.parse(readFileSync(p.evidence, "utf8")), result);
    assert.equal(result.notarized, false);
    assert.equal(result.published, false);
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("refuses to sign when a recorded hash no longer matches, and signs nothing", () => {
  const p = prepare();
  try {
    appendFileSync(path.join(p.artifact, "legion-hook"), "tampered");
    const f = fakes();
    assert.throws(() => run(p, f), (error) => error instanceof SignDevArtifactError && error.code === "SIGN_DEV_ARTIFACT_HASH_MISMATCH");
    assert.equal(f.state.signed.length, 0);
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("a manifest file whose hash was never recorded is refused", () => {
  const p = prepare();
  try {
    writeFileSync(p.manifest, JSON.stringify({ schema: 1, entries: [{ file: "extra", identifier: "com.example.extra" }] }));
    writeFileSync(path.join(p.artifact, "extra"), "x");
    assert.throws(() => run(p), (error) => error.code === "SIGN_DEV_ARTIFACT_NOT_RECORDED");
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("manifest validation: identifier required, valid, relative paths only, nested code before its bundle", () => {
  const p = prepare();
  try {
    const read = (entries) => { writeFileSync(p.manifest, JSON.stringify({ schema: 1, entries })); return readDevArtifactSignManifest(p.manifest, p.artifact); };
    assert.throws(() => read([{ file: "legion" }]), (error) => error.code === "SIGN_DEV_ARTIFACT_IDENTIFIER_REQUIRED");
    assert.throws(() => read([{ file: "legion", identifier: "bad id" }]), (error) => error.code === "MAC_SIGN_IDENTIFIER_INVALID");
    assert.throws(() => read([{ file: "../outside", identifier: "a.b" }]), (error) => error.code === "SIGN_DEV_ARTIFACT_PATH_INVALID");
    assert.throws(() => read([{ file: path.join(p.artifact, "legion"), identifier: "a.b" }]), (error) => error.code === "SIGN_DEV_ARTIFACT_PATH_INVALID");
    assert.throws(() => read([{ file: "Installer.app", identifier: "a.installer" }, { file: "Installer.app/Contents/MacOS/x", identifier: "a.x" }]), (error) => error.code === "SIGN_DEV_ARTIFACT_ORDER_INVALID");
    const ok = read([{ file: "Installer.app/Contents/MacOS/x", identifier: "a.x" }, { file: "Installer.app", identifier: "a.installer" }]);
    assert.equal(ok.length, 2);
    assert.equal(ok[0].identifier, "a.x");
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("a missing Developer ID is the typed DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED error, with no signing", () => {
  const p = prepare();
  try {
    const f = fakes();
    const typed = Object.assign(new Error("none"), { code: "DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED" });
    assert.throws(() => run(p, { ...f, resolveSigning: () => { throw typed; } }), (error) => error === typed);
    assert.throws(() => run(p, { ...f, resolveSigning: () => ({ identity: "-", adhoc: true }) }), (error) => error.code === "DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED");
    assert.throws(() => run(p, { ...f, resolveSigning: () => ({ identity: "Apple Development: x (ABC)", teamIdentifier: TEAM }) }), (error) => error.code === "DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED");
    assert.equal(f.state.signed.length, 0);
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("a wrong team or a cdhash-bound designated requirement fails the read-back", () => {
  const p = prepare();
  try {
    const f = fakes();
    assert.throws(() => run(p, { ...f, inspect: (c, a) => f.inspect(c, a).replace(`TeamIdentifier=${TEAM}`, "TeamIdentifier=OTHER") }), (error) => error.code === "SIGN_DEV_ARTIFACT_TEAM_MISMATCH");
  } finally { rmSync(p.root, { recursive: true, force: true }); }
  const q = prepare();
  try {
    const f = fakes();
    assert.throws(() => run(q, { ...f, inspect: (c, a) => a.includes("-r-") ? "designated => cdhash H\"abc\"\n" : f.inspect(c, a) }), (error) => error.code === "SIGN_DEV_ARTIFACT_UNSTABLE_REQUIREMENT");
  } finally { rmSync(q.root, { recursive: true, force: true }); }
});

test("dry run verifies hashes only: no identity lookup, no signing, no metadata change", () => {
  const p = prepare();
  try {
    const f = fakes();
    const metadataBefore = readFileSync(p.metadata, "utf8");
    const result = signDevArtifact({ artifact: p.artifact, manifestPath: p.manifest, dryRun: true, platform: "linux", ...f, resolveSigning: () => { throw new Error("must not resolve"); } });
    assert.equal(result.dryRun, true);
    assert.equal(f.state.signed.length, 0);
    assert.equal(readFileSync(p.metadata, "utf8"), metadataBefore);
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("a non-macOS host is refused before any signing", () => {
  const p = prepare();
  try {
    assert.throws(() => run(p, { platform: "win32" }), (error) => error.code === "SIGN_DEV_ARTIFACT_PLATFORM");
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});
