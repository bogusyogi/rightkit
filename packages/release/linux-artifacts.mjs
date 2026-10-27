// Linux build artifacts and their digest manifest (rightkit#1230): a native Linux dev/build run packs the declared files into one
// tarball per architecture and records it in linux-artifacts.json beside it. A Pulse guest link carries the sha256 and size from
// this manifest and its bootstrap refuses any download that does not match (ADR-0004 I1: pinned digest, no signature scheme).
// Nothing here uploads: publication is a separate, explicitly approved step.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

export const LINUX_ARTIFACTS_SCHEMA = "rightkit.linux-artifacts/1";
export const LINUX_ARCHES = Object.freeze(["x86_64", "aarch64"]);
const NAME = /^[a-z0-9][a-z0-9._-]{0,60}$/;

/** `development.targets.linux.archive`: `{ name, include: [relative paths] }`. */
export function validateLinuxArchive(archive, label = "development.targets.linux.archive") {
  if (archive === undefined) return null;
  if (!archive || typeof archive !== "object" || Array.isArray(archive)) throw new Error(`${label} must be an object { name, include }`);
  const unknown = Object.keys(archive).filter((key) => !["name", "include"].includes(key));
  if (unknown.length) throw new Error(`${label} has unknown key(s): ${unknown.join(", ")} (allowed: name, include)`);
  if (typeof archive.name !== "string" || !NAME.test(archive.name)) throw new Error(`${label}.name must be a lower-case file name stem`);
  if (!Array.isArray(archive.include) || !archive.include.length) throw new Error(`${label}.include must list the files or folders to pack`);
  for (const item of archive.include) {
    if (typeof item !== "string" || !item || path.isAbsolute(item) || item.split(/[\\/]/).includes("..")) throw new Error(`${label}.include entries must be relative paths inside the app root: ${JSON.stringify(item)}`);
  }
  return { name: archive.name, include: [...archive.include] };
}

/** The host architecture as `uname -m` names it; only the two native targets are accepted. */
export function linuxArch(run = spawnSync) {
  const r = run("uname", ["-m"], { encoding: "utf8" });
  const arch = String(r.stdout ?? "").trim();
  if (r.status !== 0 || !LINUX_ARCHES.includes(arch)) throw new Error(`linux artifacts: unsupported or unknown architecture ${JSON.stringify(arch)} (supported: ${LINUX_ARCHES.join(", ")})`);
  return arch;
}

const versionKey = (v) => v.split(".").map((n) => Number(n));
const newer = (a, b) => { const x = versionKey(a), y = versionKey(b); for (let i = 0; i < Math.max(x.length, y.length); i++) { if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) > (y[i] ?? 0); } return false; };

/** The highest GLIBC_x.y symbol version any listed ELF file needs; fails closed when a file cannot be read by objdump. */
export function glibcFloor(files, { run = spawnSync, fs = nodeFs } = {}) {
  let floor = null;
  for (const file of files) {
    const head = Buffer.alloc(4);
    const fd = fs.openSync(file, "r"); try { fs.readSync(fd, head, 0, 4, 0); } finally { fs.closeSync(fd); }
    if (!head.equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46]))) continue; // not ELF
    const r = run("objdump", ["-T", file], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
    if (r.status !== 0) throw new Error(`linux artifacts: objdump -T ${file} failed (${r.status}); the glibc floor cannot be stated`);
    for (const m of String(r.stdout).matchAll(/GLIBC_(\d+(?:\.\d+)+)/g)) if (!floor || newer(m[1], floor)) floor = m[1];
  }
  return floor;
}

function listFiles(fs, root, rel) {
  const abs = path.join(root, rel);
  const st = fs.statSync(abs);
  if (!st.isDirectory()) return [abs];
  return fs.readdirSync(abs).flatMap((entry) => listFiles(fs, root, path.join(rel, entry)));
}

/**
 * Pack `archive.include` (relative to `root`) into `<outDir>/<name>-<version>-linux-<arch>.tar.gz`, then write or merge
 * `<outDir>/linux-artifacts.json`. A manifest of another version or commit is replaced, never mixed.
 */
export function packLinuxArtifacts({ root, archive, app, version, commit, outDir = path.join(root, ".right-release", "linux"), run = spawnSync, fs = nodeFs, log = console.log }) {
  if (!/^\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$/.test(String(version ?? ""))) throw new Error(`linux artifacts: version ${JSON.stringify(version)} is not semver`);
  if (!/^[0-9a-f]{40}$/.test(String(commit ?? ""))) throw new Error("linux artifacts: commit must be the 40-hex source commit");
  const arch = linuxArch(run);
  for (const item of archive.include) if (!fs.existsSync(path.join(root, item))) throw new Error(`linux artifacts: ${item} does not exist; build it before packing`);
  const glibc = glibcFloor(archive.include.flatMap((item) => listFiles(fs, root, item)), { run, fs });
  fs.mkdirSync(outDir, { recursive: true });
  const file = `${archive.name}-${version}-linux-${arch}.tar.gz`;
  const target = path.join(outDir, file);
  fs.rmSync(target, { force: true });
  // Flat archive: each listed path is stored under its base name (dist/stage/pulse -> pulse), which is what a guest bootstrap
  // extracts and runs; two entries with the same base name are refused rather than one silently overwriting the other.
  const names = archive.include.map((item) => path.basename(item));
  const dup = names.find((n, i) => names.indexOf(n) !== i);
  if (dup) throw new Error(`linux artifacts: two include entries share the base name ${dup}; the archive is flat`);
  const tar = run("tar", ["-czf", target, ...archive.include.flatMap((item) => ["-C", path.join(root, path.dirname(item)), path.basename(item)])], { stdio: "inherit" });
  if (tar.status !== 0) throw new Error(`linux artifacts: tar failed (${tar.status})`);
  const bytes = fs.readFileSync(target);
  const entry = { file, sha256: createHash("sha256").update(bytes).digest("hex"), size: bytes.length, glibc };
  const manifestPath = path.join(outDir, "linux-artifacts.json");
  let manifest = null;
  try { manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8")); } catch { manifest = null; }
  if (!manifest || manifest.schema !== LINUX_ARTIFACTS_SCHEMA || manifest.app !== app || manifest.version !== version || manifest.commit !== commit) {
    manifest = { schema: LINUX_ARTIFACTS_SCHEMA, app, version, commit, artifacts: {} };
  }
  manifest.artifacts[arch] = entry;
  fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  log(`right-release: linux artifact ${file} sha256 ${entry.sha256} size ${entry.size} glibc ${glibc ?? "none"}; manifest ${manifestPath}`);
  return { manifestPath, arch, ...entry };
}
