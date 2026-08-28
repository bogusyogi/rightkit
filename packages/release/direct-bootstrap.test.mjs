import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import {
  buildChecksums,
  buildReleaseManifest,
  canonicalJson,
  collectReleaseAsset,
  planBootstrapPublication,
  publishBootstrapPlan,
  renderPowerShellBootstrap,
  runInstallTransaction,
  validatePowerShellBootstrap,
} from "./direct-bootstrap.mjs";
import { RIGHTRELEASE_MANIFEST_SIGNER, signReleaseManifestWithAzure } from "./sign-release-manifest.mjs";
import { materializeCycloneDxSbom, materializeInTotoSlsaProvenance } from "./supply-chain-evidence.mjs";

const digest = "a".repeat(64);
const asset = {
  target: "windows-x86_64",
  name: "legion-0.1.0-windows-x86_64.zip",
  url: "https://github.com/Orthic-Labs/legion/releases/download/v0.1.0/legion-0.1.0-windows-x86_64.zip",
  size: 10,
  sha256: digest,
  executablePath: "bin/legion.exe",
  nativeSignaturePolicy: "authenticode-valid",
  provenanceName: "legion-0.1.0-windows-x86_64.intoto.jsonl",
  provenanceUrl: "https://github.com/Orthic-Labs/legion/releases/download/v0.1.0/legion-0.1.0-windows-x86_64.intoto.jsonl",
  provenanceSha256: "b".repeat(64),
  sbomName: "legion-0.1.0-windows-x86_64.cdx.json",
  sbomUrl: "https://github.com/Orthic-Labs/legion/releases/download/v0.1.0/legion-0.1.0-windows-x86_64.cdx.json",
  sbomSha256: "c".repeat(64),
};
const manifestInput = {
  product: "legion",
  version: "0.1.0",
  sourceCommit: "d".repeat(40),
  minimumBootstrapVersion: "0.1.0",
  assets: [asset],
};
const acceptedSigner = { ...RIGHTRELEASE_MANIFEST_SIGNER, certificateSha256: "e".repeat(64) };

test("release asset binds archive, provenance, and SBOM as immutable GitHub assets", () => {
  const dir = mkdtempSync(join(tmpdir(), "rightrelease-assets-"));
  const archivePath = join(dir, "legion.zip");
  const provenancePath = join(dir, "legion.intoto.jsonl");
  const sbomPath = join(dir, "legion.cdx.json");
  writeFileSync(archivePath, "zip");
  const archiveFile = { name: "legion.zip", sha256: createHash("sha256").update("zip").digest("hex"), size: 3 };
  materializeCycloneDxSbom({ outputPath: sbomPath, product: "legion", version: "0.1.0", target: "windows-x86_64", sourceCommit: "d".repeat(40), files: [archiveFile], createdAt: "2026-08-28T08:01:00.000Z" });
  materializeInTotoSlsaProvenance({ outputPath: provenancePath, product: "legion", version: "0.1.0", target: "windows-x86_64", sourceCommit: "d".repeat(40), sourceRepository: "https://github.com/Orthic-Labs/legion", subjects: [archiveFile], startedAt: "2026-08-28T08:00:00.000Z", finishedAt: "2026-08-28T08:01:00.000Z" });
  const bound = collectReleaseAsset({
    target: "windows-x86_64",
    name: "legion.zip",
    url: "https://github.com/Orthic-Labs/legion/releases/download/v0.1.0/legion.zip",
    archivePath,
    executablePath: "bin/legion.exe",
    nativeSignaturePolicy: "authenticode-valid",
    provenancePath,
    sbomPath,
  });
  assert.equal(bound.provenanceName, "legion.intoto.jsonl");
  assert.equal(bound.sbomUrl, "https://github.com/Orthic-Labs/legion/releases/download/v0.1.0/legion.cdx.json");
  assert.equal(bound.provenanceSha256.length, 64);
});

test("manifest authority identity and bound checksums are fixed", () => {
  const result = buildReleaseManifest({ ...manifestInput, checksumsDigest: "f".repeat(64) });
  assert.equal(result.signingKeyId, RIGHTRELEASE_MANIFEST_SIGNER.id);
  assert.equal(result.signatureAlgorithm, "cms-sha256");
  assert.equal(result.checksumsSha256.length, 64);
  assert.deepEqual(buildChecksums([asset]).assets, {
    [asset.name]: digest,
    [asset.provenanceName]: "b".repeat(64),
    [asset.sbomName]: "c".repeat(64),
  });
});

test("generated PowerShell owns signed-manifest install, health, and rollback contract", () => {
  const script = renderPowerShellBootstrap({
    product: "legion",
    repository: "Orthic-Labs/legion",
    bootstrapVersion: "0.1.0",
    acceptedManifestSigners: [acceptedSigner],
    installRootSubdir: "Orthic Labs\\Legion",
    executablePath: "bin/legion.exe",
    activationArgs: ["setup", "repair", "--confirm"],
    statusArgs: ["--json", "setup", "status"],
    expectedStatusKind: "legion-setup-status",
  });
  assert.deepEqual(validatePowerShellBootstrap(script, { product: "legion", acceptedSignerIds: [RIGHTRELEASE_MANIFEST_SIGNER.id] }), { valid: true, errors: [] });
  assert.match(script, /Get-AuthenticodeSignature/);
  assert.match(script, /SignerFingerprint/);
  assert.match(script, /\.current-previous/);
  assert.match(script, /Expand-BootstrapArgs/);
  const scriptPath = join(mkdtempSync(join(tmpdir(), "rightrelease-ps-")), "install.ps1");
  writeFileSync(scriptPath, script);
  const escapedScriptPath = scriptPath.replaceAll("'", "''");
  const parsed = spawnSync("pwsh", ["-NoProfile", "-Command", `$e=@();$t=@();[System.Management.Automation.Language.Parser]::ParseFile('${escapedScriptPath}',[ref]$t,[ref]$e)|Out-Null;if($e.Count){$e|% Message;exit 1}`], { encoding: "utf8", windowsHide: true });
  assert.equal(parsed.status, 0, parsed.stderr || parsed.stdout);
});

test("generated bootstrap supports current-path activation and product-specific health schema", () => {
  const script = renderPowerShellBootstrap({
    product: "membrane",
    repository: "Orthic-Labs/membrane",
    bootstrapVersion: "0.1.0",
    acceptedManifestSigners: [acceptedSigner],
    installRootSubdir: "Orthic Labs\\Membrane",
    executablePath: "bin/membrane.exe",
    activationArgs: ["activate", "--install-root", "{current}"],
    statusArgs: ["activate", "--install-root", "{current}", "--dry-run"],
    healthAssertions: [
      { path: "schemaVersion", equals: 1 },
      { path: "dryRun", equals: true },
      { path: "service.serviceId", equals: "membrane-hub" },
      { path: "service.releaseGeneration", nonempty: true },
      { path: "clients", minCount: 2 },
    ],
  });
  assert.deepEqual(validatePowerShellBootstrap(script, { product: "membrane", acceptedSignerIds: [RIGHTRELEASE_MANIFEST_SIGNER.id] }), { valid: true, errors: [] });
  const encoded = script.match(/FromBase64String\('([^']+)'\)/)[1];
  const contract = JSON.parse(Buffer.from(encoded, "base64").toString());
  assert.equal(contract.healthAssertions[2].path, "service.serviceId");
  assert.match(script, /Replace\('\{current\}', \$Current\)/);
});

test("shared Azure signer owns detached CMS invocation without product private keys", () => {
  const dir = mkdtempSync(join(tmpdir(), "rightrelease-cms-"));
  const manifestPath = join(dir, "release-manifest.json");
  const signaturePath = join(dir, "release-manifest.sig");
  writeFileSync(manifestPath, "{}\n");
  const calls = [];
  const runner = (command, args) => {
    calls.push({ command, args });
    if (command === "signtool.exe") {
      const out = args[args.indexOf("/p7") + 1];
      writeFileSync(join(out, "release-manifest.json.p7"), "cms");
      return { status: 0, stdout: "", stderr: "" };
    }
    return { status: 0, stdout: JSON.stringify({ subject: RIGHTRELEASE_MANIFEST_SIGNER.subject, certificateSha256: acceptedSigner.certificateSha256 }), stderr: "" };
  };
  const signed = signReleaseManifestWithAzure({
    manifestPath,
    signaturePath,
    commandRunner: runner,
    environment: {},
    signtoolPath: "signtool.exe",
    dlibPath: "azure.dlib",
    metadataPath: "metadata.json",
  });
  assert.equal(readFileSync(signaturePath, "utf8"), "cms");
  assert.equal(signed.signer.id, RIGHTRELEASE_MANIFEST_SIGNER.id);
  assert.equal(signed.signer.certificateSha256, acceptedSigner.certificateSha256);
  assert.ok(calls[0].args.includes("DetachedSignedData"));
});

test("manifest rejects R2 payload URLs", () => {
  assert.throws(() => buildReleaseManifest({ ...manifestInput, checksumsDigest: digest, assets: [{ ...asset, url: "https://x.r2.dev/a.zip" }] }), /GitHub Release/);
});

test("direct distribution rejects prerelease versions", () => {
  assert.throws(() => buildReleaseManifest({ ...manifestInput, version: "0.1.0-dev.1", tag: "v0.1.0-dev.1", checksumsDigest: digest }), /version\/tag/);
});

test("direct distribution rejects leading-zero SemVer", () => {
  assert.throws(() => buildReleaseManifest({ ...manifestInput, version: "01.2.3", tag: "v01.2.3", checksumsDigest: digest }), /version\/tag/);
});

test("bootstrap publishes immutable version before stable key and verifies both", () => {
  const dir = mkdtempSync(join(tmpdir(), "rightrelease-bootstrap-"));
  const script = join(dir, "install.ps1");
  const verify = join(dir, "verify.ps1");
  writeFileSync(script, renderPowerShellBootstrap({
    product: "legion",
    repository: "Orthic-Labs/legion",
    bootstrapVersion: "0.1.0",
    acceptedManifestSigners: [acceptedSigner],
    installRootSubdir: "Orthic Labs\\Legion",
    executablePath: "bin/legion.exe",
    activationArgs: ["setup", "repair", "--confirm"],
    statusArgs: ["--json", "setup", "status"],
    expectedStatusKind: "legion-setup-status",
  }));
  const plan = planBootstrapPublication({ product: "legion", bootstrapVersion: "0.1.0", scriptPath: script });
  const objects = new Map();
  const events = [];
  const r2 = {
    put(bucket, key, path) { events.push(`put:${key}`); objects.set(`${bucket}/${key}`, readFileSync(path)); },
    get(bucket, key, path) { events.push(`get:${key}`); writeFileSync(path, objects.get(`${bucket}/${key}`)); },
  };
  assert.equal(publishBootstrapPlan(plan, r2, { verificationPath: verify }).verified, true);
  assert.deepEqual(events, [
    "put:legion/versions/0.1.0/install.ps1",
    "get:legion/versions/0.1.0/install.ps1",
    "put:legion/install.ps1",
    "get:legion/install.ps1",
  ]);
});

test("bootstrap publication rejects arbitrary or oversized scripts", () => {
  const dir = mkdtempSync(join(tmpdir(), "rightrelease-bootstrap-"));
  const script = join(dir, "install.ps1");
  writeFileSync(script, "Write-Output ok\n");
  assert.throws(() => planBootstrapPublication({ product: "legion", bootstrapVersion: "0.1.0", scriptPath: script }), /generated RightRelease contract/);
  writeFileSync(script, "# rightkit-direct-bootstrap:v1\n" + "x".repeat(256 * 1024));
  assert.throws(() => planBootstrapPublication({ product: "legion", bootstrapVersion: "0.1.0", scriptPath: script }), /small install\.ps1/);
});

test("bootstrap validation rejects executable substitution even when both digests are recomputed", () => {
  const script = renderPowerShellBootstrap({
    product: "legion",
    repository: "Orthic-Labs/legion",
    bootstrapVersion: "0.1.0",
    acceptedManifestSigners: [acceptedSigner],
    installRootSubdir: "Orthic Labs\\Legion",
    executablePath: "bin/legion.exe",
    activationArgs: ["setup"],
    statusArgs: ["status"],
    expectedStatusKind: "legion-setup-status",
  });
  const encodedMatch = script.match(/FromBase64String\('([^']+)'\)/);
  const contract = JSON.parse(Buffer.from(encodedMatch[1], "base64").toString());
  contract.unrecognizedForgeField = "caller-authored";
  const { contractSha256: ignored, ...unsignedContract } = contract;
  contract.contractSha256 = createHash("sha256").update(canonicalJson(unsignedContract)).digest("hex");
  let forged = script.replace(encodedMatch[1], Buffer.from(JSON.stringify(contract)).toString("base64"));
  const markerOffset = forged.lastIndexOf("# source-sha256:");
  forged = `${forged.slice(0, markerOffset)}Write-Output 'caller-authored executable statement'\r\n`;
  forged += `# source-sha256:${createHash("sha256").update(forged).digest("hex")}\r\n`;
  const validation = validatePowerShellBootstrap(forged, { product: "legion" });
  assert.equal(validation.valid, false);
  assert.ok(validation.errors.includes("bootstrap source does not match canonical generated output"));
});

test("install transaction restores pointer and integrations on failed health", async () => {
  const events = [];
  await assert.rejects(() => runInstallTransaction({
    targetVersion: "0.1.0",
    currentVersion: null,
    stage: async () => "staged",
    verify: async () => events.push("verify"),
    snapshotIntegrations: async () => "journal",
    switchCurrent: async () => events.push("switch"),
    activate: async () => events.push("activate"),
    verifyHealth: async () => ({ status: "incomplete" }),
    restoreCurrent: async () => events.push("restore-current"),
    restoreIntegrations: async (journal) => events.push(`restore-${journal}`),
  }), /health/);
  assert.deepEqual(events, ["verify", "switch", "activate", "restore-current", "restore-journal"]);
});

test("install transaction restores after switchCurrent partially mutates then throws", async () => {
  const events = [];
  await assert.rejects(() => runInstallTransaction({
    targetVersion: "0.1.1",
    currentVersion: "0.1.0",
    stage: async () => "staged",
    verify: async () => {},
    snapshotIntegrations: async () => "journal",
    switchCurrent: async () => { events.push("switch-partial"); throw new Error("switch interrupted"); },
    activate: async () => {},
    verifyHealth: async () => ({ status: "complete" }),
    restoreCurrent: async () => events.push("restore-current"),
    restoreIntegrations: async () => events.push("restore-integrations"),
  }), /switch interrupted/);
  assert.deepEqual(events, ["switch-partial", "restore-current", "restore-integrations"]);
});

test("bootstrap plan revalidates product-scoped R2 keys before mutation", () => {
  const dir = mkdtempSync(join(tmpdir(), "rightrelease-bootstrap-boundary-"));
  const script = join(dir, "install.ps1");
  writeFileSync(script, renderPowerShellBootstrap({
    product: "legion",
    repository: "Orthic-Labs/legion",
    bootstrapVersion: "0.1.0",
    acceptedManifestSigners: [acceptedSigner],
    installRootSubdir: "Orthic Labs\\Legion",
    executablePath: "bin/legion.exe",
    activationArgs: ["setup"],
    statusArgs: ["status"],
    expectedStatusKind: "legion-setup-status",
  }));
  const plan = planBootstrapPublication({ product: "legion", bootstrapVersion: "0.1.0", scriptPath: script });
  const calls = [];
  assert.throws(() => publishBootstrapPlan({ ...plan, stableKey: "other/install.ps1" }, { put: () => calls.push("put"), get: () => {} }, { verificationPath: join(dir, "verify.ps1") }), /product boundary/);
  assert.deepEqual(calls, []);
});
