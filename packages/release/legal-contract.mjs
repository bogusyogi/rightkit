import { createHash } from "node:crypto";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import path from "node:path";

const REQUIRED_ROLES = new Set([
  "license",
  "eula",
  "acceptable_use",
  "product_schedule",
  "privacy_notice",
  "third_party_notices",
]);
const AGREEMENT_ROLES = new Set(["license", "eula", "acceptable_use", "product_schedule"]);
const SHA256 = /^[a-f0-9]{64}$/i;
const APP_KEY = /^[a-z][a-z0-9-]{1,63}$/;
const PLACEHOLDER = /\[(?:COUNSEL|TODO|TBD)(?::|\])|\b(?:TODO|TBD|PLACEHOLDER|NOT READY|RELEASE BLOCKER)\b|⚠\s*verify|^- \[ \]/im;

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function fail(appName, message) {
  throw new Error(`${appName ?? "app"} legal contract: ${message}`);
}

function within(parent, candidate) {
  const relative = path.relative(parent, candidate);
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
}

function resolveContained(parent, relative, appName, label) {
  if (typeof relative !== "string" || !relative.trim() || path.isAbsolute(relative)) {
    fail(appName, `${label} must be a non-empty relative path`);
  }
  const candidate = path.resolve(parent, relative);
  if (!within(parent, candidate)) fail(appName, `${label} path escapes the legal snapshot (path traversal)`);
  return candidate;
}

function readJson(file, appName, label) {
  let value;
  try {
    value = JSON.parse(readFileSync(file, "utf8"));
  } catch (error) {
    fail(appName, `${label} is missing or invalid JSON: ${error.message}`);
  }
  return value;
}

export function assertLegalReleaseContract(root, legal, appName, platform) {
  const appRoot = realpathSync(root);
  if (!legal || typeof legal !== "object" || typeof legal.manifest !== "string") {
    fail(appName, "release config must declare legal.manifest");
  }
  const manifestPath = path.resolve(appRoot, legal.manifest);
  if (!within(appRoot, manifestPath) || !existsSync(manifestPath)) {
    fail(appName, `legal.manifest must exist inside the app root: ${legal.manifest}`);
  }
  const manifestBytes = readFileSync(manifestPath);
  const manifest = readJson(manifestPath, appName, "legal manifest");
  const snapshotRoot = realpathSync(path.dirname(manifestPath));

  if (manifest.schema !== 2 || manifest.suite !== "right-suite-desktop") {
    fail(appName, "manifest must use Right Suite legal schema 2");
  }
  if (!APP_KEY.test(manifest.appKey ?? "") || (appName && manifest.appKey !== appName)) {
    fail(appName, `manifest appKey must match release app (${manifest.appKey ?? "missing"})`);
  }
  if (typeof manifest.acceptanceVersion !== "string" || !manifest.acceptanceVersion.trim()) {
    fail(appName, "manifest acceptanceVersion is required");
  }
  if (!Array.isArray(manifest.documents)) fail(appName, "manifest documents must be an array");

  const seen = new Set();
  const resolvedDocuments = [];
  for (const document of manifest.documents) {
    if (!REQUIRED_ROLES.has(document.role)) fail(appName, `unsupported document role: ${document.role ?? "missing"}`);
    if (seen.has(document.role)) fail(appName, `duplicate document role: ${document.role}`);
    seen.add(document.role);
    if (!SHA256.test(document.sha256 ?? "")) fail(appName, `${document.role} sha256 must be 64 hexadecimal characters`);
    if (AGREEMENT_ROLES.has(document.role) && (document.treatment !== "agree" || document.material !== true)) {
      fail(appName, `${document.role} must be a material agreement`);
    }
    if (document.role === "privacy_notice" && document.treatment !== "acknowledge") {
      fail(appName, "privacy_notice must be acknowledged, not treated as consent/agreement");
    }
    if (document.role === "third_party_notices" && document.treatment !== "notice") {
      fail(appName, "third_party_notices must use notice treatment");
    }
    const file = resolveContained(snapshotRoot, document.path, appName, `${document.role} document`);
    if (!existsSync(file)) fail(appName, `missing ${document.role} document: ${document.path}`);
    const realFile = realpathSync(file);
    if (!within(snapshotRoot, realFile) || !within(appRoot, realFile)) {
      fail(appName, `${document.role} document resolves outside the legal snapshot`);
    }
    const bytes = readFileSync(realFile);
    if (sha256(bytes) !== document.sha256.toLowerCase()) {
      fail(appName, `${document.path} sha256 does not match legal-manifest.json`);
    }
    const text = bytes.toString("utf8");
    if (PLACEHOLDER.test(text)) {
      fail(appName, `${document.path} contains a placeholder or unchecked release blocker`);
    }
    if (document.role === "third_party_notices") {
      if (!/build provenance\s*:/i.test(text) || !/inputs\s*:/i.test(text) || !/generated-by\s*=/i.test(text)) {
        fail(appName, "third-party notices must contain Build provenance, inputs, and generated-by evidence");
      }
    }
    resolvedDocuments.push({ ...document, absolutePath: realFile });
  }
  for (const role of REQUIRED_ROLES) {
    if (!seen.has(role)) fail(appName, `missing required document role: ${role}`);
  }

  const appendixIds = new Set();
  const appendices = [];
  if (manifest.appendices != null && !Array.isArray(manifest.appendices)) {
    fail(appName, "manifest appendices must be an array");
  }
  for (const appendix of manifest.appendices ?? []) {
    if (typeof appendix.id !== "string" || !appendix.id.trim()) fail(appName, "appendix id is required");
    if (appendixIds.has(appendix.id)) fail(appName, `duplicate appendix id: ${appendix.id}`);
    appendixIds.add(appendix.id);
    if (typeof appendix.title !== "string" || !appendix.title.trim()) fail(appName, `${appendix.id} appendix title is required`);
    if (!SHA256.test(appendix.sha256 ?? "")) fail(appName, `${appendix.id} appendix sha256 is invalid`);
    const file = resolveContained(snapshotRoot, appendix.path, appName, `${appendix.id} appendix`);
    if (!existsSync(file)) fail(appName, `${appendix.id} appendix is missing: ${appendix.path}`);
    const realFile = realpathSync(file);
    if (!within(snapshotRoot, realFile) || !within(appRoot, realFile)) {
      fail(appName, `${appendix.id} appendix resolves outside the legal snapshot`);
    }
    const bytes = readFileSync(realFile);
    if (sha256(bytes) !== appendix.sha256.toLowerCase()) fail(appName, `${appendix.id} appendix sha256 does not match`);
    if (PLACEHOLDER.test(bytes.toString("utf8"))) fail(appName, `${appendix.id} appendix contains a release blocker`);
    appendices.push({ ...appendix, absolutePath: realFile });
  }

  if (platform === "win") {
    if (typeof legal.tauriConfig !== "string") fail(appName, "Windows legal gate requires legal.tauriConfig");
    const tauriPath = path.resolve(appRoot, legal.tauriConfig);
    if (!within(appRoot, tauriPath) || !existsSync(tauriPath)) fail(appName, `missing Tauri config: ${legal.tauriConfig}`);
    const tauri = readJson(tauriPath, appName, "Tauri config");
    const configured = tauri.bundle?.licenseFile;
    if (typeof configured !== "string" || !configured.trim()) {
      fail(appName, "Windows Tauri bundle.licenseFile must point to the snapshot EULA");
    }
    const configuredPath = path.resolve(path.dirname(tauriPath), configured);
    const eula = resolvedDocuments.find((document) => document.role === "eula");
    if (!eula || path.normalize(configuredPath).toLowerCase() !== path.normalize(eula.absolutePath).toLowerCase()) {
      fail(appName, "Windows Tauri bundle.licenseFile must point to the exact EULA snapshot");
    }
  }

  return {
    appKey: manifest.appKey,
    acceptanceVersion: manifest.acceptanceVersion,
    manifestPath,
    manifestSha256: sha256(manifestBytes),
    documents: resolvedDocuments,
    appendices,
  };
}
