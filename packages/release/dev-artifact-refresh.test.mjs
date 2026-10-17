import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { sha256TreeV1 } from "./dev-artifact-refresh.mjs";
import { fileEvidence } from "./macos-file-evidence.mjs";
import { signDevArtifact } from "./sign-dev-artifact.mjs";

const IDENTITY = "Developer ID Application: Orthic Labs (6KLGD3LLKF)";
const TEAM = "6KLGD3LLKF";
const APP = "Legion Installer.app";
const PAYLOAD = `${APP}/Contents/Resources/payload`;
const NAMES = ["legion", "legion-hook", "legion-mcp"];
const IDENTIFIERS = { legion: "com.orthiclabs.legion", "legion-hook": "com.orthiclabs.legion.hook", "legion-mcp": "com.orthiclabs.legion.mcp" };
const WRAPPER_IDENTIFIER = "com.orthiclabs.legion.installer";

const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const jsonText = (value) => `${JSON.stringify(value, null, 2)}\n`;

// An independent statement of the tree digest: sorted relative paths, each as path, NUL, 8-byte big-endian length, bytes.
function treeDigest(root) {
  const files = [];
  const walk = (directory) => {
    for (const name of readdirSync(directory)) {
      const absolute = path.join(directory, name);
      if (statSync(absolute).isDirectory()) walk(absolute); else files.push(path.relative(root, absolute).split(path.sep).join("/"));
    }
  };
  walk(root);
  files.sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
  const digest = createHash("sha256");
  for (const relative of files) {
    const bytes = readFileSync(path.join(root, ...relative.split("/")));
    const length = Buffer.alloc(8);
    length.writeBigUInt64BE(BigInt(bytes.length));
    digest.update(Buffer.from(relative)).update(Buffer.from([0])).update(length).update(bytes);
  }
  return digest.digest("hex");
}

// A fixture shaped like the Legion macOS installer artifact: a wrapper .app holding the portable release as its payload.
function prepare({ provenance, portableCoreSha256, metadataInsideWrapper = false } = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), "dev-artifact-refresh-"));
  const artifact = path.join(root, "artifact");
  const at = (relative) => path.join(artifact, ...relative.split("/"));
  const put = (relative, text) => { mkdirSync(path.dirname(at(relative)), { recursive: true }); writeFileSync(at(relative), text); };
  for (const name of NAMES) put(`${PAYLOAD}/bin/${name}`, `unsigned ${name}`);
  put(`${APP}/Contents/MacOS/Legion Installer`, "installer executable");
  put(`${PAYLOAD}/plugin/skills/demo/SKILL.md`, "a skill\n");
  put(`${PAYLOAD}/plugin/rightax-portable-core.json`, jsonText({ schemaVersion: 1, publicFiles: ["release-binding.json"] }));
  const unsigned = sha(readFileSync(at(`${PAYLOAD}/bin/legion`)));
  const locator = provenance ?? `local-build://macos-aarch64/${unsigned}`;
  put(`${PAYLOAD}/plugin/release-binding.json`, jsonText({ schemaVersion: 1, kind: "legion-installed-plugin-release-binding", plugin: "legion", releaseVersion: "0.3.21", runtimeSha256: unsigned, runtimeProvenance: locator, releaseManifestPath: "../share/legion/release.json" }));
  put(`${PAYLOAD}/share/legion/release.json`, jsonText({
    releaseVersion: "0.3.21",
    runtime: { platform: "macos", architecture: "aarch64", sha256: unsigned, provenance: locator },
    capabilityCatalogSha256: "a".repeat(64),
    stateSchemaVersion: 1,
    portableCoreSha256: portableCoreSha256 ?? sha(readFileSync(at(`${PAYLOAD}/plugin/rightax-portable-core.json`))),
    pluginSha256: treeDigest(at(`${PAYLOAD}/plugin`)),
  }));
  put(`${PAYLOAD}/share/legion/composition.json`, jsonText({ schemaVersion: 1, kind: "legion-m1-composition", releaseBinding: { runtimeProvenance: locator, declarativeAssetsKind: "directory" } }));
  const metadataRelative = metadataInsideWrapper ? `${APP}/Contents/Resources/payload.json` : "payload.json";
  const recorded = [...NAMES.map((name) => `${PAYLOAD}/bin/${name}`), `${PAYLOAD}/plugin/release-binding.json`, `${PAYLOAD}/share/legion/release.json`, `${PAYLOAD}/share/legion/composition.json`, ...(metadataInsideWrapper ? [] : [APP])];
  const record = () => ({ schema: 1, files: recorded.map((relative) => { const e = fileEvidence(at(relative)); return { path: relative, sha256: e.sha256, sizeBytes: e.sizeBytes }; }) });
  put(metadataRelative, JSON.stringify(record()));
  const ops = (of = "bin/legion") => [
    { file: "plugin/release-binding.json", pointer: "/runtimeSha256", compute: "sha256-file", of },
    { file: "plugin/release-binding.json", pointer: "/runtimeProvenance", compute: "sha256-file", of, format: "local-build://macos-aarch64/{value}", currentMatches: "^local-build://macos-aarch64/" },
    { file: "share/legion/release.json", pointer: "/runtime/sha256", compute: "sha256-file", of },
    { file: "share/legion/release.json", pointer: "/runtime/provenance", compute: "sha256-file", of, format: "local-build://macos-aarch64/{value}", currentMatches: "^local-build://macos-aarch64/" },
    { file: "share/legion/release.json", pointer: "/portableCoreSha256", compute: "verify-sha256-file", of: "plugin/rightax-portable-core.json" },
    { file: "share/legion/release.json", pointer: "/pluginSha256", compute: "sha256-tree-v1", of: "plugin" },
    { file: "share/legion/composition.json", pointer: "/releaseBinding/runtimeProvenance", compute: "sha256-file", of, format: "local-build://macos-aarch64/{value}", currentMatches: "^local-build://macos-aarch64/" },
  ];
  const writeManifest = (patch = {}) => {
    const manifest = path.join(root, "sign.json");
    writeFileSync(manifest, JSON.stringify({
      schema: 1,
      refreshRoot: PAYLOAD,
      entries: [...NAMES.map((name) => ({ file: `${PAYLOAD}/bin/${name}`, identifier: IDENTIFIERS[name] })), { file: APP, identifier: WRAPPER_IDENTIFIER }],
      refresh: ops(),
      ...patch,
    }));
    return manifest;
  };
  return { root, artifact, at, ops, writeManifest, manifest: writeManifest(), metadata: at(metadataRelative), evidence: path.join(root, "out", "evidence.json"), unsigned };
}

// Fake codesign. Signing a file appends a marker; signing the wrapper bundle seals a snapshot of the files it contains.
function fakes(p) {
  const state = { identifiers: new Map(), signed: [], sealed: null };
  return {
    state,
    resolveSigning: () => ({ identity: IDENTITY, teamIdentifier: TEAM, certificateSha1: "AA" }),
    signOne: (args) => {
      const file = args.at(-1);
      state.identifiers.set(file, args[args.indexOf("--identifier") + 1]);
      if (statSync(file).isDirectory()) {
        state.sealed = Object.fromEntries(["share/legion/release.json", "share/legion/composition.json", "plugin/release-binding.json"].map((relative) => [relative, readFileSync(path.join(p.at(PAYLOAD), ...relative.split("/")), "utf8")]));
        writeFileSync(path.join(file, "Contents", "_CodeSignature"), "seal");
      } else appendFileSync(file, "\nSIGNED");
      state.signed.push(path.basename(file));
      return { status: 0, stdout: "", stderr: "" };
    },
    inspect: (command, args) => {
      const file = args.at(-1);
      if (args.includes("--verify")) return "valid on disk\nsatisfies its designated requirement";
      if (args.includes("-r-")) return `designated => identifier "${state.identifiers.get(file)}" and anchor apple generic and certificate leaf[subject.OU] = ${TEAM}\n`;
      return `Identifier=${state.identifiers.get(file)}\nAuthority=${IDENTITY}\nTeamIdentifier=${TEAM}\nTimestamp=Oct 17, 2026\n`;
    },
  };
}

const run = (p, f = fakes(p), extra = {}) => signDevArtifact({ artifact: p.artifact, manifestPath: p.manifest, evidencePath: p.evidence, platform: "darwin", ...f, ...extra });
const cleanup = (p) => rmSync(p.root, { recursive: true, force: true });
const read = (p, relative) => JSON.parse(readFileSync(p.at(`${PAYLOAD}/${relative}`), "utf8"));
const text = (p, relative) => readFileSync(p.at(`${PAYLOAD}/${relative}`), "utf8");

test("refresh recomputes the Legion-shaped metadata from signed bytes, signs the wrapper last and seals the final bytes", () => {
  const p = prepare();
  try {
    const original = { release: read(p, "share/legion/release.json"), binding: read(p, "plugin/release-binding.json"), composition: read(p, "share/legion/composition.json") };
    const f = fakes(p);
    const result = run(p, f);
    assert.deepEqual(f.state.signed, [...NAMES, APP], "nested binaries first, the wrapper last");
    assert.deepEqual(result.files.map((file) => file.identifier), [...NAMES.map((name) => IDENTIFIERS[name]), WRAPPER_IDENTIFIER]);

    const signedLegion = sha(readFileSync(p.at(`${PAYLOAD}/bin/legion`)));
    assert.notEqual(signedLegion, p.unsigned, "signing changed the runtime bytes");
    const locator = `local-build://macos-aarch64/${signedLegion}`;
    const release = read(p, "share/legion/release.json");
    assert.equal(release.runtime.sha256, signedLegion);
    assert.equal(release.runtime.provenance, locator);
    assert.equal(release.portableCoreSha256, original.release.portableCoreSha256, "the portable core digest is verified and preserved");
    assert.equal(release.portableCoreSha256, sha(readFileSync(p.at(`${PAYLOAD}/plugin/rightax-portable-core.json`))));
    assert.equal(release.pluginSha256, treeDigest(p.at(`${PAYLOAD}/plugin`)), "the plugin tree digest covers the refreshed release-binding.json");
    assert.notEqual(release.pluginSha256, original.release.pluginSha256);
    const binding = read(p, "plugin/release-binding.json");
    assert.equal(binding.runtimeSha256, signedLegion);
    assert.equal(binding.runtimeProvenance, locator);
    assert.equal(read(p, "share/legion/composition.json").releaseBinding.runtimeProvenance, locator);

    // Key order and formatting are kept: only the refreshed values differ from the original text.
    const expected = structuredClone(original.release);
    Object.assign(expected.runtime, { sha256: signedLegion, provenance: locator });
    expected.pluginSha256 = release.pluginSha256;
    assert.equal(text(p, "share/legion/release.json"), jsonText(expected));
    assert.deepEqual(Object.keys(release), Object.keys(original.release));
    assert.deepEqual(Object.keys(binding), Object.keys(original.binding));

    // The wrapper was sealed over the final bytes: nothing it contains changed after it was signed.
    assert.deepEqual(f.state.sealed, { "share/legion/release.json": text(p, "share/legion/release.json"), "share/legion/composition.json": text(p, "share/legion/composition.json"), "plugin/release-binding.json": text(p, "plugin/release-binding.json") });

    // payload.json records the final bytes, including the refreshed files and the signed wrapper.
    const metadata = JSON.parse(readFileSync(p.metadata, "utf8"));
    const recorded = Object.fromEntries(metadata.files.map((item) => [item.path, item.sha256]));
    assert.equal(recorded[`${PAYLOAD}/share/legion/release.json`], sha(readFileSync(p.at(`${PAYLOAD}/share/legion/release.json`))));
    assert.equal(recorded[`${PAYLOAD}/plugin/release-binding.json`], sha(readFileSync(p.at(`${PAYLOAD}/plugin/release-binding.json`))));
    assert.equal(recorded[APP], fileEvidence(p.at(APP)).sha256);

    assert.equal(result.refresh.length, 7);
    assert.deepEqual(result.refresh.filter((entry) => entry.verified).map((entry) => entry.pointer), ["/portableCoreSha256"]);
    assert.deepEqual(JSON.parse(readFileSync(p.evidence, "utf8")), result);
  } finally { cleanup(p); }
});

test("sha256-tree-v1 matches the independent statement of the algorithm and is order independent", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "tree-v1-"));
  try {
    mkdirSync(path.join(root, "b"), { recursive: true });
    writeFileSync(path.join(root, "b", "z.txt"), "zed");
    writeFileSync(path.join(root, "a.txt"), "x");
    const expected = createHash("sha256");
    const length = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64BE(BigInt(n)); return b; };
    expected.update("a.txt").update(Buffer.from([0])).update(length(1)).update("x");
    expected.update("b/z.txt").update(Buffer.from([0])).update(length(3)).update("zed");
    assert.equal(sha256TreeV1(root), expected.digest("hex"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("a provenance that is not a recomputable locator (a signed attestation) stops signing before the wrapper", () => {
  const p = prepare({ provenance: "rightkit-release://attested/abc123" });
  try {
    const f = fakes(p);
    assert.throws(() => run(p, f), (error) => error.code === "SIGN_DEV_ARTIFACT_REFRESH_UNRECOMPUTABLE" && /rightkit-release:\/\/attested\/abc123/.test(error.message));
    assert.ok(!f.state.signed.includes(APP), "the wrapper is not signed");
    assert.equal(read(p, "share/legion/release.json").runtime.provenance, "rightkit-release://attested/abc123");
  } finally { cleanup(p); }
});

test("currentMatches requires the original locator prefix, and format is applied exactly as given", () => {
  const wrong = prepare({ provenance: "local-build://macos-arm64/abc" });
  try {
    const f = fakes(wrong);
    assert.throws(() => run(wrong, f), (error) => error.code === "SIGN_DEV_ARTIFACT_REFRESH_UNRECOMPUTABLE" && /macos-arm64/.test(error.message), "another architecture token is not rewritten");
    assert.ok(!f.state.signed.includes(APP));
  } finally { cleanup(wrong); }
  const p = prepare();
  try {
    const exact = "local-build://macos-aarch64/{value}";
    p.writeManifest({ refresh: p.ops().map((op) => (op.format ? { ...op, format: exact } : op)) });
    run(p);
    assert.equal(read(p, "share/legion/release.json").runtime.provenance, `local-build://macos-aarch64/${sha(readFileSync(p.at(`${PAYLOAD}/bin/legion`)))}`);
  } finally { cleanup(p); }
});

test("a portable core digest that does not match fails verification instead of being rewritten", () => {
  const p = prepare({ portableCoreSha256: "0".repeat(64) });
  try {
    const f = fakes(p);
    assert.throws(() => run(p, f), (error) => error.code === "SIGN_DEV_ARTIFACT_REFRESH_VERIFY_FAILED");
    assert.ok(!f.state.signed.includes(APP));
    assert.equal(read(p, "share/legion/release.json").portableCoreSha256, "0".repeat(64));
  } finally { cleanup(p); }
});

test("refresh ordering is enforced: a reader may not precede the writer, and an input signed after the refresh is refused", () => {
  const p = prepare();
  try {
    // The tree digest reads release-binding.json, so it must be listed after the operations that write it.
    const [first, second, ...rest] = p.ops();
    const tree = rest.find((op) => op.compute === "sha256-tree-v1");
    const reordered = [tree, first, second, ...rest.filter((op) => op !== tree)];
    p.writeManifest({ refresh: reordered });
    assert.throws(() => run(p), (error) => error.code === "SIGN_DEV_ARTIFACT_REFRESH_ORDER");
  } finally { cleanup(p); }
  const q = prepare();
  try {
    // A loose tool signed after the wrapper cannot be an input of the refresh: its final bytes do not exist yet.
    mkdirSync(q.at("extra"), { recursive: true });
    writeFileSync(q.at("extra/tool"), "tool");
    const record = JSON.parse(readFileSync(q.metadata, "utf8"));
    record.files.push({ path: "extra/tool", sha256: sha(Buffer.from("tool")), sizeBytes: 4 });
    writeFileSync(q.metadata, JSON.stringify(record));
    const entries = [...NAMES.map((name) => ({ file: `${PAYLOAD}/bin/${name}`, identifier: IDENTIFIERS[name] })), { file: APP, identifier: WRAPPER_IDENTIFIER }, { file: "extra/tool", identifier: "com.orthiclabs.legion.tool" }];
    const refresh = [{ file: "share/legion/release.json", pointer: "/runtime/sha256", compute: "sha256-file", of: "../../../../extra/tool" }];
    q.writeManifest({ entries, refresh, refreshRoot: PAYLOAD });
    assert.throws(() => run(q), (error) => error.code === "SIGN_DEV_ARTIFACT_PATH_INVALID", "`..` is refused outright");
    q.writeManifest({ entries, refreshRoot: undefined, refresh: [{ file: `${PAYLOAD}/share/legion/release.json`, pointer: "/runtime/sha256", compute: "sha256-file", of: "extra/tool" }] });
    assert.throws(() => run(q), (error) => error.code === "SIGN_DEV_ARTIFACT_REFRESH_ORDER");
  } finally { cleanup(q); }
});

test("payload metadata inside a signed bundle is refused: refreshing it after signing would break the bundle's signature", () => {
  const p = prepare({ metadataInsideWrapper: true });
  try {
    const f = fakes(p);
    assert.throws(() => run(p, f, { metadataPath: p.metadata }), (error) => error.code === "SIGN_DEV_ARTIFACT_METADATA_IN_SIGNED_ENTRY");
    assert.equal(f.state.signed.length, 0);
  } finally { cleanup(p); }
});

test("refresh paths are contained on disk: a symlink inside the artifact cannot redirect a read or a write outside it", (t) => {
  const p = prepare();
  try {
    const outside = path.join(p.root, "outside");
    mkdirSync(outside, { recursive: true });
    writeFileSync(path.join(outside, "release.json"), jsonText({ runtime: { sha256: "x" } }));
    writeFileSync(path.join(outside, "tool"), "outside bytes");
    try { symlinkSync(outside, p.at(`${PAYLOAD}/leak`), "junction"); } catch { t.skip("this host cannot create directory links"); return; }
    const [op] = p.ops();
    for (const refresh of [[{ ...op, file: "leak/release.json" }], [{ ...op, of: "leak/tool" }]]) {
      p.writeManifest({ refresh });
      const f = fakes(p);
      assert.throws(() => run(p, f), (error) => error.code === "SIGN_DEV_ARTIFACT_PATH_ESCAPE", JSON.stringify(refresh));
      assert.equal(f.state.signed.length, 0, "refused before anything is signed");
    }
    assert.equal(readFileSync(path.join(outside, "release.json"), "utf8"), jsonText({ runtime: { sha256: "x" } }), "the file outside the artifact is untouched");
    // refreshRoot cannot point through the link either, nor be the artifact root spelled out.
    p.writeManifest({ refreshRoot: `${PAYLOAD}/leak`, refresh: [{ ...op, file: "release.json", of: "tool" }] });
    assert.throws(() => run(p), (error) => error.code === "SIGN_DEV_ARTIFACT_PATH_ESCAPE");
    for (const refreshRoot of [".", "./", `${PAYLOAD}/..`]) {
      p.writeManifest({ refreshRoot, refresh: [op] });
      assert.throws(() => run(p), (error) => error.code === "SIGN_DEV_ARTIFACT_PATH_INVALID", JSON.stringify(refreshRoot));
    }
  } finally { cleanup(p); }
});

test("refresh refuses an input or file equal to the refresh root", () => {
  const p = prepare();
  try {
    const [op] = p.ops();
    for (const refresh of [[{ ...op, of: "." }], [{ ...op, file: "" }], [{ ...op, of: "bin/.", file: "." }]]) {
      p.writeManifest({ refresh });
      assert.throws(() => run(p), (error) => error.code === "SIGN_DEV_ARTIFACT_PATH_INVALID", JSON.stringify(refresh));
    }
  } finally { cleanup(p); }
});

test("refresh refuses malformed operations, missing pointers and JSON it would have to reformat", () => {
  const p = prepare();
  try {
    const bad = (refresh, code) => {
      p.writeManifest({ refresh });
      assert.throws(() => run(p), (error) => error.code === code, JSON.stringify(refresh));
    };
    const [op] = p.ops();
    bad([{ ...op, compute: "md5" }], "SIGN_DEV_ARTIFACT_REFRESH_INVALID");
    bad([{ ...op, extra: true }], "SIGN_DEV_ARTIFACT_REFRESH_INVALID");
    bad([{ ...op, pointer: "runtimeSha256" }], "SIGN_DEV_ARTIFACT_REFRESH_INVALID");
    bad([{ ...op, format: "no placeholder" }], "SIGN_DEV_ARTIFACT_REFRESH_INVALID");
    bad([{ ...op, file: "../outside.json" }], "SIGN_DEV_ARTIFACT_PATH_INVALID");
    bad([{ ...op, currentMatches: "(" }], "SIGN_DEV_ARTIFACT_REFRESH_INVALID");
    p.writeManifest({ refresh: [] });
    assert.throws(() => run(p), (error) => error.code === "SIGN_DEV_ARTIFACT_REFRESH_INVALID");
  } finally { cleanup(p); }
  // These fail while the refresh runs, after the nested binaries are signed, so each starts from a fresh artifact.
  for (const [pointer, code] of [["/doesNotExist", "SIGN_DEV_ARTIFACT_REFRESH_POINTER_MISSING"], ["/schemaVersion", "SIGN_DEV_ARTIFACT_REFRESH_POINTER_INVALID"]]) {
    const r = prepare();
    try {
      r.writeManifest({ refresh: [{ ...r.ops()[0], pointer }] });
      const f = fakes(r);
      assert.throws(() => run(r, f), (error) => error.code === code, pointer);
      assert.ok(!f.state.signed.includes(APP));
    } finally { cleanup(r); }
  }
  const q = prepare();
  try {
    // Compact JSON would be reformatted by this tool's writer, so the refresh refuses it.
    const file = q.at(`${PAYLOAD}/plugin/release-binding.json`);
    writeFileSync(file, JSON.stringify(JSON.parse(readFileSync(file, "utf8"))));
    const record = JSON.parse(readFileSync(q.metadata, "utf8"));
    // The wrapper tree hash covers the rewritten file too: re-record every entry so only the format check can refuse.
    for (const item of record.files) item.sha256 = fileEvidence(q.at(item.path)).sha256;
    writeFileSync(q.metadata, JSON.stringify(record));
    assert.throws(() => run(q), (error) => error.code === "SIGN_DEV_ARTIFACT_REFRESH_JSON_FORMAT");
  } finally { cleanup(q); }
});

test("dry run validates the refresh plan and ordering without signing or writing", () => {
  const p = prepare();
  try {
    const before = text(p, "share/legion/release.json");
    const f = fakes(p);
    const result = signDevArtifact({ artifact: p.artifact, manifestPath: p.manifest, dryRun: true, platform: "linux", ...f });
    assert.equal(result.dryRun, true);
    assert.equal(f.state.signed.length, 0);
    assert.equal(text(p, "share/legion/release.json"), before);
    const [first, second, ...rest] = p.ops();
    p.writeManifest({ refresh: [...rest, first, second] });
    assert.throws(() => signDevArtifact({ artifact: p.artifact, manifestPath: p.manifest, dryRun: true, platform: "linux", ...f }), (error) => error.code === "SIGN_DEV_ARTIFACT_REFRESH_ORDER");
  } finally { cleanup(p); }
});
