import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { execFileSync } from "./background-process.mjs";
import { canonicalAddonManifest, createAddonManifest } from "./addon-contract.mjs";
import { prepareGitHubAddonRelease, prepareGitHubDirectRelease, prepareGitHubRelease, publishGitHubRelease } from "./github-release.mjs";
import { readVerifiedArtifact } from "./publication-artifacts.mjs";
import { RIGHTRELEASE_MANIFEST_SIGNER, verifyManifestCatalogSignature } from "./sign-release-manifest.mjs";
import { registryEvidenceFixture } from "./test-support/admission.mjs";

const captured = (assets) => new Map(assets.map((file) => [file, readVerifiedArtifact(file, { sha256: sha256(readFileSync(file)) })]));
const sha256 = (value) => createHash("sha256").update(value).digest("hex");

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-github-"));
  const releaseId = "cutright-1.2.3-abcdef12";
  const sealedDir = path.join(root, ".right-release", "sealed", releaseId, "windows");
  mkdirSync(sealedDir, { recursive: true });
  writeFileSync(path.join(sealedDir, "CutRight.exe"), "signed-installer");
  writeFileSync(path.join(sealedDir, "release-manifest.json"), `${JSON.stringify({
    schema: 1, ...registryEvidenceFixture(), releaseId, app: "cutright", version: "1.2.3", commit: "abcdef1234567890", platform: "win",
    files: [{ role: "installer", name: "CutRight.exe", sha256: sha256("signed-installer"), sizeBytes: 16 }],
    checkpoints: ["sealed"],
  })}\n`);
  return { root, releaseId };
}

function addonFixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-github-addon-"));
  mkdirSync(path.join(root, "out"));
  for (const [name, bytes] of [["membrane", "command"], ["membrane-service", "service"], ["icon.png", "icon"], ["LICENSE", "license"], ["EULA.txt", "eula"], ["PRIVACY.md", "privacy"], ["THIRD-PARTY-NOTICES.txt", "notices"]]) writeFileSync(path.join(root, "out", name), bytes);
  execFileSync("git", ["init", "--initial-branch", "main"], { cwd: root });
  execFileSync("git", ["config", "user.email", "test@example.com"], { cwd: root });
  execFileSync("git", ["config", "user.name", "Test"], { cwd: root });
  writeFileSync(path.join(root, "tracked"), "source"); execFileSync("git", ["add", "."], { cwd: root }); execFileSync("git", ["commit", "-m", "source"], { cwd: root });
  const files = [["command", "membrane", true], ["service", "membrane-service", true], ["icon", "icon.png", false], ["license", "LICENSE", false], ["eula", "EULA.txt", false], ["privacy", "PRIVACY.md", false], ["third-party-notices", "THIRD-PARTY-NOTICES.txt", false]].map(([role, name, executable]) => ({ role, name, source: `out/${name}`, executable }));
  const config = { schema: 1, kind: "headless-addon", addon: "membrane", version: "0.1.0", packageManager: "pnpm", distribution: { provider: "github-releases", repository: "Orthic-Labs/Membrane" }, checks: [], buildInputs: { include: ["tracked"] }, consumer: { contract: "membrane-product-v1" }, targets: { win: { targetTriple: "x86_64-pc-windows-msvc", build: { cmd: "false", args: [] }, signing: { contract: "azure-artifact-signing-v1" }, files } } };
  const commit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim();
  const manifest = createAddonManifest({ config, root, platform: "win", commit, signing: { command: { contract: "test-fixture-v1", status: "verified" }, service: { contract: "test-fixture-v1", status: "verified" } } });
  const sealed = path.join(root, ".right-release", "addons", "membrane", "0.1.0", commit.slice(0, 8), "win"); mkdirSync(sealed, { recursive: true });
  for (const file of manifest.files) writeFileSync(path.join(sealed, file.name), readFileSync(path.join(root, "out", file.name)));
  writeFileSync(path.join(sealed, "addon-manifest.json"), canonicalAddonManifest(manifest));
  return { root, config };
}

test("GitHub plan derives tag, notes, installer, manifest & checksums only from sealed bytes", () => {
  const fx = fixture();
  const plan = prepareGitHubRelease({ repoRoot: fx.root, releaseId: fx.releaseId, platform: "win", repo: "Orthic-Labs/CutRight" });
  assert.equal(plan.tag, "v1.2.3");
  assert.equal(plan.assets.length, 3);
  assert.match(plan.notes, /abcdef1234567890/);
  assert.equal(readFileSync(plan.assets[2], "utf8"), `${sha256("signed-installer")}  CutRight.exe\n`);
});

test("dry-run checks public visibility without creating or uploading a release", () => {
  const fx = fixture();
  const plan = prepareGitHubRelease({ repoRoot: fx.root, releaseId: fx.releaseId, platform: "win", repo: "Orthic-Labs/CutRight" });
  const calls = [];
  const run = (_command, args, options = {}) => {
    calls.push(args);
    if (args[0] === "repo") return JSON.stringify({ visibility: "PUBLIC" });
    if (args[0] === "release" && args[1] === "view") return options.allowFailure ? { ok: false } : "";
    throw new Error(`unexpected mutation: ${args.join(" ")}`);
  };
  assert.equal(publishGitHubRelease(plan, { repo: "Orthic-Labs/CutRight", dryRun: true, run }).status, "would-create");
  assert.equal(calls.length, 2);
});

test("GitHub add-on plan uses a content-addressed platform tag and manifest URL", () => {
  const fx = addonFixture();
  const plan = prepareGitHubAddonRelease({ repoRoot: fx.root, config: fx.config, platform: "win", repo: "Orthic-Labs/Membrane" });
  assert.match(plan.tag, /^addon-membrane-v0\.1\.0-win-sha256-[0-9a-f]{64}$/);
  assert.equal(plan.assets.at(-1), plan.manifestPath);
  assert.equal(plan.manifestUrl, `https://github.com/Orthic-Labs/Membrane/releases/download/${plan.tag}/addon-manifest.json`);
});

test("existing GitHub add-on release is verified without upload mutation", () => {
  const asset = path.join(mkdtempSync(path.join(os.tmpdir(), "right-release-existing-")), "addon-manifest.json"); writeFileSync(asset, "sealed");
  const calls = [];
  const run = (_command, args, options = {}) => {
    calls.push(args);
    if (args[0] === "repo") return JSON.stringify({ visibility: "PUBLIC" });
    if (args[0] === "release" && args[1] === "view") return options.allowFailure ? { ok: true } : "";
    if (args[0] === "release" && args[1] === "download") { mkdirSync(args[args.indexOf("--dir") + 1], { recursive: true }); writeFileSync(path.join(args[args.indexOf("--dir") + 1], path.basename(asset)), "sealed"); return options.allowFailure ? { ok: true, output: "" } : ""; }
    throw new Error(`unexpected mutation: ${args.join(" ")}`);
  };
  const result = publishGitHubRelease({ kind: "addon", publicationArtifacts: captured([asset]), manifest: { files: [] }, sealedDir: path.dirname(asset), tag: "immutable", assets: [asset], manifestUrl: "https://example.test/manifest" }, { repo: "Orthic-Labs/Membrane", run });
  assert.equal(result.status, "already-verified");
  assert.equal(calls.some((args) => args[0] === "release" && args[1] === "upload"), false);
});

test("partial GitHub add-on release uploads only missing assets then verifies all", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-resume-")); const first = path.join(root, "first"); const second = path.join(root, "second"); writeFileSync(first, "one"); writeFileSync(second, "two");
  const remote = new Map([["first", "one"]]); const uploads = [];
  const run = (_command, args, options = {}) => {
    if (args[0] === "repo") return JSON.stringify({ visibility: "PUBLIC" });
    if (args[0] === "release" && args[1] === "view") return { ok: true };
    if (args[0] === "release" && args[1] === "download") { const name = args[args.indexOf("--pattern") + 1]; if (!remote.has(name)) return { ok: false, error: "no assets match" }; const dir = args[args.indexOf("--dir") + 1]; mkdirSync(dir, { recursive: true }); writeFileSync(path.join(dir, name), remote.get(name)); return { ok: true, output: "" }; }
    if (args[0] === "release" && args[1] === "upload") { for (const value of args.slice(3, args.indexOf("--repo"))) { uploads.push(path.basename(value)); remote.set(path.basename(value), readFileSync(value, "utf8")); } return ""; }
    throw new Error(`unexpected call: ${args.join(" ")}`);
  };
  const result = publishGitHubRelease({ kind: "addon", publicationArtifacts: captured([first, second]), manifest: { files: [] }, sealedDir: root, tag: "immutable", assets: [first, second], manifestUrl: "https://example.test/manifest" }, { repo: "Orthic-Labs/Membrane", run });
  assert.equal(result.status, "resumed-verified"); assert.deepEqual(uploads, ["second"]);
});

function directFixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-direct-github-"));
  const archive = path.join(root, "cutright-windows-x86_64.zip");
  const provenance = path.join(root, "cutright-windows-x86_64.intoto.jsonl");
  const sbom = path.join(root, "cutright-windows-x86_64.cdx.json");
  const manifest = path.join(root, "release-manifest.json");
  const signature = path.join(root, "release-manifest.cat");
  const checksums = path.join(root, "checksums.json");
  writeFileSync(archive, "archive");
  writeFileSync(provenance, "provenance");
  writeFileSync(sbom, "sbom");
  const archiveDigest = sha256("archive");
  const provenanceDigest = sha256("provenance");
  const sbomDigest = sha256("sbom");
  const checksumsText = `${JSON.stringify({ schemaVersion: 1, algorithm: "sha256", assets: { [path.basename(archive)]: archiveDigest, [path.basename(provenance)]: provenanceDigest, [path.basename(sbom)]: sbomDigest } })}\n`;
  writeFileSync(checksums, checksumsText);
  writeFileSync(manifest, JSON.stringify({
    schemaVersion: 1,
    kind: "rightkit-direct-release-manifest",
    product: "cutright",
    version: "1.2.3",
    tag: "v1.2.3",
    sourceCommit: "a".repeat(40),
    signingKeyId: RIGHTRELEASE_MANIFEST_SIGNER.id,
    signatureAlgorithm: RIGHTRELEASE_MANIFEST_SIGNER.algorithm,
    signatureProvider: RIGHTRELEASE_MANIFEST_SIGNER.provider,
    signatureProviderVersion: RIGHTRELEASE_MANIFEST_SIGNER.providerVersion,
    checksumsSha256: sha256(checksumsText),
    assets: [{ name: path.basename(archive), size: statSync(archive).size, sha256: archiveDigest, provenanceName: path.basename(provenance), provenanceSha256: provenanceDigest, sbomName: path.basename(sbom), sbomSha256: sbomDigest }],
  }));
  writeFileSync(signature, "catalog");
  const signer = { ...RIGHTRELEASE_MANIFEST_SIGNER, certificateSha256: "f".repeat(64) };
  const signatureCommandRunner = (_command, args) => {
    assert.equal(args[0], "-NoProfile");
    return { status: 0, stdout: JSON.stringify({ verified: true, catalogStatus: "Valid", signatureStatus: "Valid", subject: signer.subject, certificateSha256: signer.certificateSha256 }), stderr: "" };
  };
  const plan = prepareGitHubDirectRelease({ repoRoot: root, repo: "Orthic-Labs/CutRight", manifestPath: manifest, signaturePath: signature, checksumsPath: checksums, archivePaths: [archive], provenancePaths: [provenance], sbomPaths: [sbom], signing: { signer }, signatureCommandRunner, signaturePlatform: "win32" });
  return { plan, root, archive, manifest, signature, checksums, provenance, sbom, signer, signatureCommandRunner };
}

test("direct GitHub plan publishes archive plus manifest authority and evidence without installer", () => {
  const { plan, root, archive, manifest, signature, checksums, provenance, sbom, signer, signatureCommandRunner } = directFixture();
  assert.equal(plan.kind, "direct-release");
  assert.equal(plan.assets.length, 6);
  assert.ok(plan.assets.some((file) => file.endsWith(".zip")));
  assert.equal(plan.assets.some((file) => /setup|installer/i.test(path.basename(file))), false);
  writeFileSync(archive, "tampered");
  assert.throws(() => prepareGitHubDirectRelease({ repoRoot: root, repo: "Orthic-Labs/CutRight", manifestPath: manifest, signaturePath: signature, checksumsPath: checksums, archivePaths: [archive], provenancePaths: [provenance], sbomPaths: [sbom], signing: { signer }, signatureCommandRunner, signaturePlatform: "win32" }), /archive size mismatch/);
});

test("shared catalog verifier rejects invalid catalog signatures", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-catalog-invalid-"));
  const manifest = path.join(root, "release-manifest.json");
  const signature = path.join(root, "release-manifest.cat");
  writeFileSync(manifest, "manifest"); writeFileSync(signature, "not-catalog");
  const signer = { ...RIGHTRELEASE_MANIFEST_SIGNER, certificateSha256: "f".repeat(64) };
  assert.throws(() => verifyManifestCatalogSignature({ manifestPath: manifest, signaturePath: signature, expectedSigner: signer, platform: "win32", commandRunner: () => ({ status: 1, stderr: "catalog parse failed" }) }), /manifest catalog verification failed/);
});

test("shared catalog verifier binds exact Azure subject and fingerprint through system runner", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-catalog-valid-"));
  const manifest = path.join(root, "release-manifest.json");
  const signature = path.join(root, "release-manifest.cat");
  writeFileSync(manifest, "manifest"); writeFileSync(signature, "catalog-bytes");
  const signer = { ...RIGHTRELEASE_MANIFEST_SIGNER, certificateSha256: "f".repeat(64) };
  let verifierSource = "";
  const result = verifyManifestCatalogSignature({
    manifestPath: manifest,
    signaturePath: signature,
    expectedSigner: signer,
    platform: "win32",
    commandRunner: (_command, args) => {
      verifierSource = readFileSync(args[2], "utf8");
      return { status: 0, stdout: JSON.stringify({ verified: true, catalogStatus: "Valid", signatureStatus: "Valid", subject: signer.subject, certificateSha256: signer.certificateSha256 }) };
    },
  });
  assert.equal(result.signer.certificateSha256, signer.certificateSha256);
  assert.match(verifierSource, /Test-FileCatalog/);
  assert.match(verifierSource, /Get-AuthenticodeSignature/);
});

test("shared catalog verifier rejects Azure subject or fingerprint mismatch", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-catalog-mismatch-"));
  const manifest = path.join(root, "release-manifest.json");
  const signature = path.join(root, "release-manifest.cat");
  writeFileSync(manifest, "manifest"); writeFileSync(signature, "catalog-bytes");
  const signer = { ...RIGHTRELEASE_MANIFEST_SIGNER, certificateSha256: "f".repeat(64) };
  const run = (_command, _args) => ({ status: 0, stdout: JSON.stringify({ verified: true, catalogStatus: "Valid", signatureStatus: "Valid", subject: "CN=Unexpected", certificateSha256: "0".repeat(64) }) });
  assert.throws(() => verifyManifestCatalogSignature({ manifestPath: manifest, signaturePath: signature, expectedSigner: signer, platform: "win32", commandRunner: run }), /catalog signer subject mismatch/);
});

test("direct GitHub publication accepts only strict stable SemVer", () => {
  for (const version of ["1.2.3-beta.1", "01.2.3"]) {
    const root = mkdtempSync(path.join(os.tmpdir(), "right-release-semver-"));
    const manifest = path.join(root, "release-manifest.json");
    const signature = path.join(root, "release-manifest.cat");
    const checksums = path.join(root, "checksums.json");
    writeFileSync(manifest, JSON.stringify({ schemaVersion: 1, kind: "rightkit-direct-release-manifest", product: "cutright", version, tag: `v${version}` }));
    writeFileSync(signature, "catalog"); writeFileSync(checksums, "{}");
    assert.throws(() => prepareGitHubDirectRelease({ repoRoot: root, repo: "Orthic-Labs/CutRight", manifestPath: manifest, signaturePath: signature, checksumsPath: checksums }), /stable SemVer/);
  }
});

function githubMock(plan, { swap, corrupt = false } = {}) {
  const remote = new Map(), calls = [];
  const run = (_command, args) => {
    calls.push(args);
    if (args[0] === "repo") return JSON.stringify({ visibility: "PUBLIC" });
    if (args[1] === "view") return { ok: false, error: "not found" };
    if (args[1] === "create") return "";
    if (args[1] === "upload") {
      if (swap) swap();
      for (const file of args.slice(3, args.indexOf("--repo"))) remote.set(path.basename(file), corrupt ? Buffer.from("wrong remote bytes") : readFileSync(file));
      return "";
    }
    if (args[1] === "download") {
      const name = args[args.indexOf("--pattern") + 1], dir = args[args.indexOf("--dir") + 1];
      writeFileSync(path.join(dir, name), remote.get(name));
      return { ok: true };
    }
    throw new Error(`unexpected call ${args}`);
  };
  return { run, calls, remote };
}
for (const kind of ["sealed release", "add-on", "direct SDK"]) {
  const makePlan = () => {
    if (kind === "direct SDK") return directFixture().plan;
    if (kind === "add-on") {
      const fx = addonFixture();
      return prepareGitHubAddonRelease({ repoRoot: fx.root, config: fx.config, platform: "win", repo: "Orthic-Labs/Membrane" });
    }
    const fx = fixture();
    return prepareGitHubRelease({ repoRoot: fx.root, releaseId: fx.releaseId, platform: "win", repo: "Orthic-Labs/CutRight" });
  };
  test(`${kind}: unsealed publication refused before transport`, () => {
    const plan = makePlan();
    delete plan.publicationArtifacts;
    if (kind === "direct SDK") { writeFileSync(plan.signaturePath, ""); }
    const mock = githubMock(plan);
    assert.throws(() => publishGitHubRelease(plan, { repo: "Orthic-Labs/CutRight", run: mock.run, trustCommandRunner: () => ({ status: 0, stdout: "TeamIdentifier=6KLGD3LLKF", stderr: "" }) }), /sealed|signed|signature/);
    assert.equal(mock.calls.length, 0);
  });
  test(`${kind}: changed local payload refused before transport`, () => {
    const plan = makePlan(); writeFileSync(plan.assets[0], "tampered");
    const mock = githubMock(plan);
    assert.throws(() => publishGitHubRelease(plan, { repo: "Orthic-Labs/CutRight", run: mock.run, trustCommandRunner: () => ({ status: 0, stdout: "TeamIdentifier=6KLGD3LLKF", stderr: "" }) }), /changed|replaced|mismatch/);
    assert.equal(mock.calls.length, 0);
  });
  test(`${kind}: transport swap caught by readback against captured evidence`, () => {
    const plan = makePlan();
    const mock = githubMock(plan, { swap: () => writeFileSync(plan.assets[0], "swapped during upload") });
    assert.throws(() => publishGitHubRelease(plan, { repo: "Orthic-Labs/CutRight", run: mock.run, trustCommandRunner: () => ({ status: 0, stdout: "TeamIdentifier=6KLGD3LLKF", stderr: "" }) }), /remove remote asset/);
    assert.equal(mock.calls.some((args) => args[1] === "delete"), false);
  });
  test(`${kind}: happy path verifies every remote payload`, () => {
    const plan = makePlan(), mock = githubMock(plan);
    const result = publishGitHubRelease(plan, { repo: "Orthic-Labs/CutRight", run: mock.run, trustCommandRunner: () => ({ status: 0, stdout: "TeamIdentifier=6KLGD3LLKF", stderr: "" }) });
    assert.equal(result.status, "verified");
    assert.equal(mock.calls.filter((args) => args[1] === "download").length, plan.assets.length);
  });
}
test("direct SDK refuses payload absent from signed manifest evidence", () => {
  const fx = directFixture(), extra = path.join(fx.root, "unbound.txt"); writeFileSync(extra, "unsealed");
  fx.plan.assets.push(extra);
  assert.throws(() => publishGitHubRelease(fx.plan, { repo: "Orthic-Labs/CutRight", run: () => assert.fail("remote called") }), /lacks signed evidence/);
});

test("direct SDK refuses authority replacement during signature verification", () => {
  const fx = directFixture();
  const runner = (command, args, options) => {
    writeFileSync(fx.signature, "swapped authority");
    return fx.signatureCommandRunner(command, args, options);
  };
  assert.throws(() => publishGitHubRelease(fx.plan, { repo: "Orthic-Labs/CutRight", signatureCommandRunner: runner, run: () => assert.fail("unexpected remote mutation") }), /SOURCE_CHANGED/);
});
test("upload, legacy publish, github --release and github --addon share verified publisher", () => {
  const cli = readFileSync(new URL("./cli/right-release.mjs", import.meta.url), "utf8");
  const upload = readFileSync(new URL("./upload-release.mjs", import.meta.url), "utf8");
  const github = readFileSync(new URL("./github-release.mjs", import.meta.url), "utf8");
  assert.match(cli, /\["upload", "upload-release.mjs"\]/);
  assert.match(cli, /first === "publish"[\s\S]*run\("upload-release.mjs", rest\)/);
  assert.match(upload, /publishGitHubRelease\(plan/);
  assert.match(github, /prepareGitHubAddonRelease\([\s\S]*prepareGitHubRelease\([\s\S]*publishGitHubRelease\(plan, options\)/);
});
