#!/usr/bin/env node
// Provision the catalog-pinned Windows ONNX Runtime for the real-runtime tests of rightkit-ort.
//
// The single source is crates/rightkit-media/catalog/native-libraries.json: the Windows x86_64 `onnxruntime`
// record names the archive (URL + sha256) and the shipped file (name + sha256). This script downloads that
// archive, refuses a digest mismatch, takes out the one member with the pinned name AND the pinned digest, and
// leaves it in `<root>/<record id>/`. A `directml` record for the platform, once the catalog has one, is
// provisioned into the same directory. Nothing is installed or run, and no secret is read. It is idempotent:
// a directory whose files already carry the pinned digests is reused. Other hosts provision nothing.
//
// The gate calls ortRuntimeEnv() and passes RIGHTKIT_ORT_TEST_DLL_DIR to the tests.
// Run by hand: RIGHTKIT_ORT_RUNTIME_ROOT=<dir> node scripts/ci/provision-ort.mjs
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const CATALOG = path.join(ROOT, "crates", "rightkit-media", "catalog", "native-libraries.json");
/** Statuses that carry a verified or donor-pinned digest. `planned` and `stale-unverified` never resolve. */
const RESOLVABLE = new Set(["approved", "pinned"]);
const HEX = /^[0-9a-f]{64}$/;

const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");

function usable(record, library) {
  if (!record.archive || !HEX.test(record.archive.sha256 ?? "") || !/^https:\/\//.test(record.archive.url ?? "")) {
    throw new Error(`catalog record ${record.id} has no https archive with a sha256`);
  }
  if (!HEX.test(record.sha256 ?? "") || !record.artifactFileName || /[\/]/.test(record.artifactFileName)) {
    throw new Error(`catalog record ${record.id} has no file name and sha256 for the ${library} file`);
  }
  return record;
}

/**
 * The records to provision for one platform: exactly one `onnxruntime` record, then any `directml` records.
 * Only approved or pinned records count. Throws when the runtime record is missing or ambiguous.
 */
export function recordsFor(catalog, { os: wantOs = "windows", arch = "x86_64" } = {}) {
  const of = (library) => (catalog.libraries ?? []).filter((r) => r.library === library && r.os === wantOs && r.arch === arch && RESOLVABLE.has(r.status));
  const runtime = of("onnxruntime");
  if (runtime.length !== 1) throw new Error(`the catalog has ${runtime.length} approved or pinned onnxruntime records for ${wantOs} ${arch}; exactly one is required`);
  return [usable(runtime[0], "onnxruntime"), ...of("directml").map((r) => usable(r, "directml"))];
}

/**
 * The archive member that is the record's file: its base name is the pinned name and its bytes hash to the
 * pinned digest. `members` is [{ path, sha256 }]. A package holds one build per architecture under the same
 * name, so the name alone does not choose. Throws when no member matches.
 */
export function pickMember(members, record) {
  const named = members.filter((m) => path.basename(m.path) === record.artifactFileName);
  const hit = named.find((m) => m.sha256 === record.sha256);
  if (!hit) throw new Error(`the archive of ${record.id} holds ${named.length} file(s) named ${record.artifactFileName}, none with the pinned sha256 ${record.sha256}`);
  return hit.path;
}

function walk(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = path.join(dir, entry.name);
    return entry.isDirectory() ? walk(full) : entry.isFile() ? [full] : [];
  });
}

function extractZip(zip, out) {
  // A .nupkg is a zip. bsdtar ships with Windows and reads the format from the content, not the extension.
  const tar = process.platform === "win32" ? path.join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe") : "tar";
  const run = spawnSync(tar, ["-xf", zip, "-C", out], { stdio: "inherit", windowsHide: true });
  if (run.status !== 0) throw new Error("extracting the archive failed");
}

async function download(url, fetchImpl) {
  const response = await fetchImpl(url, { signal: AbortSignal.timeout(600_000) });
  if (!response.ok) throw new Error(`download failed: HTTP ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

/**
 * Put every record's file in `<root>/<runtime record id>/` and return that directory. A file already there
 * with the pinned digest is kept; anything else is replaced from a verified download.
 */
export async function provision({ root, catalog, fetchImpl = fetch, extract = extractZip, platform = { os: "windows", arch: "x86_64" } }) {
  const records = recordsFor(catalog, platform);
  const dir = path.join(root, records[0].id);
  fs.mkdirSync(dir, { recursive: true });
  for (const record of records) {
    const target = path.join(dir, record.artifactFileName);
    if (fs.existsSync(target) && sha256(fs.readFileSync(target)) === record.sha256) continue;
    const staging = fs.mkdtempSync(path.join(root, ".staging-"));
    try {
      const bytes = await download(record.archive.url, fetchImpl);
      const digest = sha256(bytes);
      if (digest !== record.archive.sha256) throw new Error(`sha256 of the ${record.id} archive ${digest} is not the pinned ${record.archive.sha256}`);
      const zip = path.join(staging, "archive.zip");
      fs.writeFileSync(zip, bytes);
      const out = path.join(staging, "out");
      fs.mkdirSync(out);
      extract(zip, out);
      const members = walk(out).map((file) => ({ path: file, sha256: sha256(fs.readFileSync(file)) }));
      fs.copyFileSync(pickMember(members.filter((m) => path.basename(m.path) === record.artifactFileName), record), target);
    } finally { fs.rmSync(staging, { recursive: true, force: true }); }
  }
  return dir;
}

/** The environment the real-runtime tests need on this host: `{}` anywhere but Windows x64. */
export async function ortRuntimeEnv({ platform = process.platform, arch = process.arch, root = process.env.RIGHTKIT_ORT_RUNTIME_ROOT || path.join(process.env.RUNNER_TEMP || os.tmpdir(), "rightkit-ort-runtime") } = {}) {
  if (platform !== "win32" || arch !== "x64") return {};
  const catalog = JSON.parse(fs.readFileSync(CATALOG, "utf8"));
  fs.mkdirSync(root, { recursive: true });
  return { RIGHTKIT_ORT_TEST_DLL_DIR: await provision({ root, catalog }) };
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  ortRuntimeEnv().then((env) => console.log(env.RIGHTKIT_ORT_TEST_DLL_DIR ?? "nothing to provision on this host"), (error) => { console.error(`provision-ort: ${error.message}`); process.exit(1); });
}
