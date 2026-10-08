import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { gunzipSync } from "node:zlib";
import { parse } from "smol-toml";
import { managedRuntimeClient } from "./managed-cargo.mjs";
import { PublicationError, assertArtifactUnchanged, readVerifiedArtifact } from "./publication-artifacts.mjs";

// Registry wire format: https://doc.rust-lang.org/cargo/reference/registry-web-api.html#publish
// Metadata comes from normalized Cargo.toml INSIDE the hashed archive, never mutable source.
export function crateUploadMetadata(bytes, name, version) {
  const tar = gunzipSync(bytes, { maxOutputLength: 512 * 1024 * 1024 });
  const files = new Map();
  for (let offset = 0; offset + 512 <= tar.length;) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const text = (start, length) => header.subarray(start, start + length).toString().split("\0")[0];
    const filename = [text(345, 155), text(0, 100)].filter(Boolean).join("/");
    const size = Number.parseInt(text(124, 12).trim(), 8);
    if (!Number.isSafeInteger(size) || size < 0 || offset + 512 + size > tar.length) throw new PublicationError("PUBLICATION_CRATE_INVALID", "invalid crate tar entry");
    if (header[156] === 0 || header[156] === 48) {
      if (files.has(filename)) throw new PublicationError("PUBLICATION_CRATE_INVALID", "duplicate crate tar entry");
      files.set(filename, tar.subarray(offset + 512, offset + 512 + size));
    }
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  const prefix = `${name}-${version}/`;
  const manifestBytes = files.get(`${prefix}Cargo.toml`);
  if (!manifestBytes) throw new PublicationError("PUBLICATION_CRATE_INVALID", "normalized Cargo.toml missing from crate");
  const manifest = parse(manifestBytes.toString("utf8"));
  const pkg = manifest.package;
  if (pkg?.name !== name || pkg.version !== version || pkg.publish === false) throw new PublicationError("PUBLICATION_CRATE_IDENTITY_MISMATCH", "packaged crate identity differs from evidence");
  const deps = [];
  const dependencies = (table, kind, target = null) => {
    for (const [alias, value] of Object.entries(table ?? {})) {
      const dep = typeof value === "string" ? { version: value } : value;
      if (!dep.version || dep.git || dep.path || dep.workspace || dep.registry) throw new PublicationError("PUBLICATION_CRATE_INVALID", `unsupported packaged dependency: ${alias}`);
      deps.push({ name: dep.package ?? alias, version_req: dep.version, features: dep.features ?? [], optional: dep.optional ?? false, default_features: dep["default-features"] ?? true, target, kind, registry: dep["registry-index"] ?? null, explicit_name_in_toml: dep.package ? alias : null });
    }
  };
  const tables = (table, target) => {
    dependencies(table.dependencies, "normal", target);
    dependencies(table["dev-dependencies"], "dev", target);
    dependencies(table["build-dependencies"], "build", target);
  };
  tables(manifest, null);
  for (const [target, table] of Object.entries(manifest.target ?? {})) tables(table, target);
  const readmeFile = typeof pkg.readme === "string" ? pkg.readme : null;
  if (readmeFile && !files.has(prefix + readmeFile)) throw new PublicationError("PUBLICATION_CRATE_INVALID", "packaged README missing");
  return {
    name, vers: version, deps, features: manifest.features ?? {}, authors: pkg.authors ?? [],
    description: pkg.description ?? null, documentation: pkg.documentation ?? null, homepage: pkg.homepage ?? null,
    readme: readmeFile ? files.get(prefix + readmeFile).toString("utf8") : null, readme_file: readmeFile,
    keywords: pkg.keywords ?? [], categories: pkg.categories ?? [], license: pkg.license ?? null,
    license_file: pkg["license-file"] ?? null, repository: pkg.repository ?? null, badges: manifest.badges ?? {},
    links: pkg.links ?? null, rust_version: pkg["rust-version"] ?? null,
  };
}

export function buildCrateUploadBody(metadata, bytes) {
  const json = Buffer.from(JSON.stringify(metadata));
  const jsonLength = Buffer.alloc(4); jsonLength.writeUInt32LE(json.length);
  const crateLength = Buffer.alloc(4); crateLength.writeUInt32LE(bytes.length);
  return Buffer.concat([jsonLength, json, crateLength, bytes]);
}

function readCargoToken() {
  const managed = Boolean(managedRuntimeClient() || process.env.RIGHTKIT_BUILD_BROKER_SOCKET || /(?:rightkit-managed|managedagent|rightkit[/\\]build-control)[/\\]agent-bin(?:[;:]|$)/i.test(String(process.env.PATH ?? "")));
  if (!managed && process.env.CARGO_REGISTRY_TOKEN?.trim()) return process.env.CARGO_REGISTRY_TOKEN.trim();
  const home = process.env.CARGO_HOME || path.join(os.homedir(), ".cargo");
  for (const name of ["credentials.toml", "credentials"]) {
    const file = path.join(home, name);
    if (!fs.existsSync(file)) continue;
    const token = parse(fs.readFileSync(file, "utf8")).registry?.token;
    if (typeof token === "string" && token.trim()) return token.trim();
  }
  throw new PublicationError("PUBLICATION_CARGO_CREDENTIAL_UNSUPPORTED", "exact-byte Cargo upload requires provisioned crates.io token credentials");
}

export async function publishGatedCrate({ name, version, crateFile, evidence }, deps = {}) {
  const artifact = readVerifiedArtifact(crateFile, { sha256: evidence?.artifactSha256 });
  const metadata = crateUploadMetadata(artifact.bytes, name, version);
  const token = (deps.readToken ?? readCargoToken)();
  const body = buildCrateUploadBody(metadata, assertArtifactUnchanged(artifact));
  const fetchImpl = deps.fetch ?? fetch;
  const response = await fetchImpl("https://crates.io/api/v1/crates/new", {
    method: "PUT", headers: { Authorization: token, "Content-Type": "application/octet-stream", Accept: "application/json", "User-Agent": "rightkit-release" },
    body, signal: AbortSignal.timeout(300_000), redirect: "error",
  });
  const result = await response.json();
  if (!response.ok || result.errors?.length) throw new PublicationError("PUBLICATION_UPLOAD_FAILED", `crates.io upload rejected for ${name}@${version} (HTTP ${response.status})`);
  return { name, version, sha256: artifact.sha256 };
}
