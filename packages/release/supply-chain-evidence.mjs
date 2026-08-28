import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { basename } from "node:path";

const SHA256 = /^[a-f0-9]{64}$/;
const VERSION = /^\d+\.\d+\.\d+$/;
const COMMIT = /^[a-f0-9]{40,64}$/;
const IN_TOTO_V1 = "https://in-toto.io/Statement/v1";
const SLSA_V1 = "https://slsa.dev/provenance/v1";
const CYCLONEDX_SPEC = "1.6";

function canonical(value) {
  if (Array.isArray(value)) return value.map(sortDeep);
  return sortDeep(value);
}

function sortDeep(value) {
  if (Array.isArray(value)) return value.map(sortDeep);
  if (value && typeof value === "object") return Object.fromEntries(Object.keys(value).sort().map((key) => [key, sortDeep(value[key])]));
  return value;
}

function stableSha256(value) {
  return createHash("sha256").update(JSON.stringify(canonical(value))).digest("hex");
}

function assertIdentity({ product, version, target, sourceCommit }) {
  if (!/^[a-z][a-z0-9-]+$/.test(product ?? "")) throw new Error("supply-chain product is invalid");
  if (!VERSION.test(version ?? "")) throw new Error("supply-chain version must be stable SemVer");
  if (!/^(?:windows|macos)-(?:x86_64|arm64)$/.test(target ?? "")) throw new Error("supply-chain target is invalid");
  if (!COMMIT.test(sourceCommit ?? "")) throw new Error("supply-chain source commit is invalid");
}

function normalizeFiles(files) {
  if (!Array.isArray(files) || !files.length) throw new Error("supply-chain evidence requires files");
  const seen = new Set();
  return files.map((file) => {
    const name = basename(String(file.name ?? ""));
    if (!name || name !== file.name || seen.has(name)) throw new Error("supply-chain file name must be one unique basename");
    seen.add(name);
    if (!SHA256.test(file.sha256 ?? "")) throw new Error(`supply-chain file digest is invalid: ${name}`);
    if (!Number.isSafeInteger(file.size) || file.size < 1) throw new Error(`supply-chain file size is invalid: ${name}`);
    return { name, sha256: file.sha256, size: file.size };
  }).sort((left, right) => left.name.localeCompare(right.name));
}

function assertIso(value, label) {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime()) || parsed.toISOString() !== value) throw new Error(label + " must be exact ISO-8601 UTC");
}

function deterministicUuid(seed) {
  const hex = stableSha256(seed).slice(0, 32).split("");
  hex[12] = "5";
  hex[16] = ((parseInt(hex[16], 16) & 3) | 8).toString(16);
  const value = hex.join("");
  return `${value.slice(0, 8)}-${value.slice(8, 12)}-${value.slice(12, 16)}-${value.slice(16, 20)}-${value.slice(20)}`;
}

export function materializeCycloneDxSbom({ outputPath, product, version, target, sourceCommit, files, createdAt = new Date().toISOString() }) {
  assertIdentity({ product, version, target, sourceCommit });
  assertIso(createdAt, "SBOM timestamp");
  if (!outputPath.endsWith(".cdx.json")) throw new Error("CycloneDX output must end in .cdx.json");
  const normalized = normalizeFiles(files);
  const identity = { product, version, target, sourceCommit, files: normalized };
  const sbom = {
    bomFormat: "CycloneDX",
    specVersion: CYCLONEDX_SPEC,
    serialNumber: `urn:uuid:${deterministicUuid(identity)}`,
    version: 1,
    metadata: {
      timestamp: createdAt,
      component: {
        type: "application",
        name: product,
        version,
        "bom-ref": `pkg:generic/${product}@${version}?target=${encodeURIComponent(target)}`,
        properties: [
          { name: "rightkit:target", value: target },
          { name: "rightkit:sourceCommit", value: sourceCommit },
        ],
      },
    },
    components: normalized.map((file) => ({
      type: "file",
      name: file.name,
      "bom-ref": `urn:sha256:${file.sha256}`,
      hashes: [{ alg: "SHA-256", content: file.sha256 }],
      properties: [{ name: "rightkit:sizeBytes", value: String(file.size) }],
    })),
  };
  writeFileSync(outputPath, JSON.stringify(sbom, null, 2) + "\n");
  validateCycloneDxSbom(outputPath);
  return sbom;
}

export function validateCycloneDxSbom(path, { expectedFile } = {}) {
  if (!path.endsWith(".cdx.json")) throw new Error("CycloneDX evidence must end in .cdx.json");
  const value = JSON.parse(readFileSync(path, "utf8"));
  if (value.bomFormat !== "CycloneDX" || value.specVersion !== CYCLONEDX_SPEC || value.version !== 1) throw new Error("invalid CycloneDX 1.6 document");
  if (!/^urn:uuid:[0-9a-f-]{36}$/.test(value.serialNumber ?? "")) throw new Error("CycloneDX serialNumber is invalid");
  assertIso(value.metadata?.timestamp, "SBOM timestamp");
  if (value.metadata?.component?.type !== "application" || !value.metadata.component.name || !VERSION.test(value.metadata.component.version ?? "")) throw new Error("CycloneDX root component is invalid");
  if (!Array.isArray(value.components) || !value.components.length) throw new Error("CycloneDX components are required");
  for (const component of value.components) {
    if (component.type !== "file" || !component.name || !Array.isArray(component.hashes)) throw new Error("CycloneDX file component is invalid");
    const digest = component.hashes.find((hash) => hash.alg === "SHA-256")?.content;
    if (!SHA256.test(digest ?? "")) throw new Error("CycloneDX file component lacks SHA-256");
  }
  if (expectedFile) {
    const match = value.components.find((component) => component.name === expectedFile.name && component.hashes.some((hash) => hash.alg === "SHA-256" && hash.content === expectedFile.sha256));
    if (!match) throw new Error("CycloneDX document does not bind expected release file");
  }
  return value;
}

export function materializeInTotoSlsaProvenance({
  outputPath,
  product,
  version,
  target,
  sourceCommit,
  sourceRepository,
  subjects,
  startedAt,
  finishedAt = new Date().toISOString(),
}) {
  assertIdentity({ product, version, target, sourceCommit });
  if (!outputPath.endsWith(".intoto.jsonl")) throw new Error("in-toto output must end in .intoto.jsonl");
  const repository = new URL(sourceRepository);
  if (repository.protocol !== "https:" || repository.hostname !== "github.com") throw new Error("SLSA source repository must be GitHub HTTPS");
  assertIso(startedAt, "SLSA startedAt");
  assertIso(finishedAt, "SLSA finishedAt");
  const files = normalizeFiles(subjects);
  const statementSubjects = files.map((file) => ({ name: file.name, digest: { sha256: file.sha256 } }));
  const invocationId = `urn:sha256:${stableSha256({ product, version, target, sourceCommit, statementSubjects, startedAt, finishedAt })}`;
  const statement = {
    _type: IN_TOTO_V1,
    subject: statementSubjects,
    predicateType: SLSA_V1,
    predicate: {
      buildDefinition: {
        buildType: "https://orthiclabs.com/rightkit/release/direct-bootstrap/v1",
        externalParameters: { product, version, target },
        internalParameters: {},
        resolvedDependencies: [{ uri: `git+${repository.href.replace(/\/$/, "")}@${sourceCommit}`, digest: { gitCommit: sourceCommit } }],
      },
      runDetails: {
        builder: { id: "https://orthiclabs.com/rightkit/release" },
        metadata: { invocationId, startedOn: startedAt, finishedOn: finishedAt },
      },
    },
  };
  writeFileSync(outputPath, JSON.stringify(statement) + "\n");
  validateInTotoSlsaProvenance(outputPath);
  return statement;
}

export function validateInTotoSlsaProvenance(path, { expectedSubject } = {}) {
  if (!path.endsWith(".intoto.jsonl")) throw new Error("in-toto evidence must end in .intoto.jsonl");
  const lines = readFileSync(path, "utf8").trimEnd().split(/\r?\n/);
  if (lines.length !== 1) throw new Error("in-toto evidence must contain exactly one JSONL statement");
  const value = JSON.parse(lines[0]);
  if (value._type !== IN_TOTO_V1 || value.predicateType !== SLSA_V1) throw new Error("invalid in-toto/SLSA v1 statement");
  if (!Array.isArray(value.subject) || !value.subject.length || value.subject.some((subject) => !subject.name || !SHA256.test(subject.digest?.sha256 ?? ""))) throw new Error("in-toto subjects are invalid");
  if (value.predicate?.buildDefinition?.buildType !== "https://orthiclabs.com/rightkit/release/direct-bootstrap/v1") throw new Error("SLSA build type is invalid");
  if (!Array.isArray(value.predicate.buildDefinition.resolvedDependencies) || value.predicate.buildDefinition.resolvedDependencies.length !== 1) throw new Error("SLSA source dependency is invalid");
  if (!/^urn:sha256:[a-f0-9]{64}$/.test(value.predicate?.runDetails?.metadata?.invocationId ?? "")) throw new Error("SLSA invocation identity is invalid");
  assertIso(value.predicate.runDetails.metadata.startedOn, "SLSA startedOn");
  assertIso(value.predicate.runDetails.metadata.finishedOn, "SLSA finishedOn");
  if (expectedSubject && !value.subject.some((subject) => subject.name === expectedSubject.name && subject.digest.sha256 === expectedSubject.sha256)) {
    throw new Error("in-toto statement does not bind expected release subject");
  }
  return value;
}
