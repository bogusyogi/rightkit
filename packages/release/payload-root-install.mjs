// `development.targets.<platform>.install.kind: "payload-root"` (rightkit#816, #428): install a verified payload folder into exactly one root.
//
//   { kind: "payload-root", sourceDirectory: "<repo-relative folder>", installRoot: "<absolute folder>", entrypoint: "<file inside the payload>",
//     stopProcesses?: true }
//
// The folder `installRoot` is the install: no `current` sub folder, no versions, no setup program run. Like the production installer it owns only the
// files the payload ships (the same rule as install-app.mjs --installer on Windows, rightkit#348): every other file in the root (user data, a
// database, logs) is never read, copied, moved or removed. What the install owns is written to `<installRoot>/.rightkit-payload.json`
// ({ schema: 1, files: [{ path, sha256 }] }); a later install removes exactly the files that manifest listed and the new payload no longer has.
//
// Order: hash the source tree (no symlinks, no reparse points) -> stop what runs from the root (`stopProcesses: true`: only processes whose
// executable is inside `installRoot`, recipe-build killProcessesUnder) -> write each file beside its destination and rename it over -> read every
// installed file back and require its digest -> remove the old payload's stale files -> write the manifest. No old copy is kept (dev rule,
// rightkit#348); a failure leaves the files written so far, names them, and the next run of the same install repairs it.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import path from "node:path";
import { killProcessesUnder } from "./recipe-build.mjs";

export const PAYLOAD_ROOT_KIND = "payload-root";
export const PAYLOAD_ROOT_MANIFEST = ".rightkit-payload.json";
const KEYS = new Set(["kind", "sourceDirectory", "installRoot", "entrypoint", "stopProcesses"]);

export class PayloadRootError extends Error {
  constructor(message) { super(message); this.name = "PayloadRootError"; this.code = "RK_PAYLOAD_ROOT"; }
}
const fail = (message) => { throw new PayloadRootError(message); };
const sha256 = (fs, file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const containedRelative = (value, label) => {
  if (typeof value !== "string" || !value || path.isAbsolute(value) || path.win32.isAbsolute(value) || /^[A-Za-z]:/.test(value) || value.split(/[\\/]/).includes("..")) fail(`${label} must be a contained relative path`);
  return value.replaceAll("\\", "/");
};

/** Validate the descriptor's shape (no file system access). `platform` is the dev target platform (win | mac | linux). */
export function validatePayloadRootDescriptor(descriptor, platform = "win") {
  if (descriptor === null || typeof descriptor !== "object" || Array.isArray(descriptor) || descriptor.kind !== PAYLOAD_ROOT_KIND) fail(`install.kind must be "${PAYLOAD_ROOT_KIND}"`);
  const unknown = Object.keys(descriptor).filter((key) => !KEYS.has(key));
  if (unknown.length) fail(`install has unknown key(s): ${unknown.join(", ")} (allowed: ${[...KEYS].join(", ")})`);
  const sourceDirectory = containedRelative(descriptor.sourceDirectory, "install.sourceDirectory");
  const entrypoint = containedRelative(descriptor.entrypoint, "install.entrypoint");
  const absolute = platform === "win" ? path.win32.isAbsolute(descriptor.installRoot ?? "") && /^[A-Za-z]:[\\/]/.test(descriptor.installRoot) : path.posix.isAbsolute(descriptor.installRoot ?? "");
  if (typeof descriptor.installRoot !== "string" || !absolute) fail("install.installRoot must be an absolute path");
  if (descriptor.installRoot.replaceAll("\\", "/").split("/").includes("..")) fail("install.installRoot must not contain ..");
  const segments = descriptor.installRoot.replaceAll("\\", "/").split("/").filter(Boolean);
  if (segments.length < 2) fail("install.installRoot must not be a drive or filesystem root (one dedicated folder for the app)");
  if (descriptor.stopProcesses !== undefined && typeof descriptor.stopProcesses !== "boolean") fail("install.stopProcesses must be a boolean");
  return { ...descriptor, sourceDirectory, entrypoint };
}

/** Every regular file under `dir` as sorted forward-slash relative paths; a symlink or special file refuses. */
export function listPayloadFiles(dir, fs = nodeFs, prefix = "") {
  const out = [];
  for (const name of [...fs.readdirSync(path.join(dir, prefix))].sort()) {
    const relative = prefix ? `${prefix}/${name}` : name;
    const info = fs.lstatSync(path.join(dir, relative));
    if (info.isSymbolicLink()) fail(`${relative} is a symbolic link or reparse point; a payload holds regular files only`);
    if (info.isDirectory()) out.push(...listPayloadFiles(dir, fs, relative));
    else if (info.isFile()) out.push(relative);
    else fail(`${relative} is neither a file nor a directory`);
  }
  return out;
}

function previousFiles(fs, root) {
  let parsed;
  try { parsed = JSON.parse(String(fs.readFileSync(path.join(root, PAYLOAD_ROOT_MANIFEST), "utf8"))); } catch { return []; }
  if (parsed?.schema !== 1 || !Array.isArray(parsed.files)) return [];
  return parsed.files.map((row) => row?.path).filter((file) => { try { return containedRelative(file, "manifest path") === file; } catch { return false; } });
}

/**
 * Install `sourceDirectory` (already resolved, absolute) into `installRoot`. Returns { installRoot, files, removed, entrypoint, entrypointDigest }.
 * Injectable: fs, log, stop(dirs) (default recipe-build killProcessesUnder).
 */
export function installPayloadRoot(descriptor, { source, fs = nodeFs, log = () => {}, platform = process.platform, stop = (dirs) => killProcessesUnder(dirs, { platform, log }) } = {}) {
  const installRoot = path.resolve(descriptor.installRoot);
  if (!fs.existsSync(source) || !fs.lstatSync(source).isDirectory()) fail(`payload source ${source} is not a directory`);
  const files = listPayloadFiles(source, fs);
  if (!files.length) fail(`payload source ${source} contains no files`);
  if (!files.includes(descriptor.entrypoint)) fail(`the entrypoint ${descriptor.entrypoint} is not in the payload ${source}`);
  if (files.includes(PAYLOAD_ROOT_MANIFEST)) fail(`the payload must not contain ${PAYLOAD_ROOT_MANIFEST}; it is the install's own record`);
  const rows = files.map((file) => ({ path: file, sha256: sha256(fs, path.join(source, ...file.split("/"))) }));
  const old = previousFiles(fs, installRoot);
  fs.mkdirSync(installRoot, { recursive: true });
  const rootInfo = fs.lstatSync(installRoot);
  if (rootInfo.isSymbolicLink() || !rootInfo.isDirectory()) fail(`${installRoot} is not a real directory`);
  if (descriptor.stopProcesses === true) stop([installRoot]);
  for (const row of rows) {
    const to = path.join(installRoot, ...row.path.split("/"));
    // Nothing between the root and a payload file may be a link: a write through one would land outside the install.
    let walk = installRoot;
    for (const segment of row.path.split("/").slice(0, -1)) {
      walk = path.join(walk, segment);
      const info = fs.lstatSync(walk, { throwIfNoEntry: false });
      if (info && (info.isSymbolicLink() || !info.isDirectory())) fail(`${walk} is not a real directory; refusing to write ${row.path} through it`);
    }
    fs.mkdirSync(path.dirname(to), { recursive: true });
    const existing = fs.lstatSync(to, { throwIfNoEntry: false });
    if (existing && (existing.isSymbolicLink() || !existing.isFile())) fail(`${to} is not a regular file; refusing to replace it`);
    if (existing && sha256(fs, to) === row.sha256) continue; // already this payload's bytes
    const temporary = `${to}.rightkit-tmp-${process.pid}`;
    fs.copyFileSync(path.join(source, ...row.path.split("/")), temporary);
    fs.renameSync(temporary, to);
  }
  for (const row of rows) {
    const got = sha256(fs, path.join(installRoot, ...row.path.split("/")));
    if (got !== row.sha256) fail(`installed ${row.path} does not match the payload (sha256 ${got}, expected ${row.sha256}); the install is incomplete, run it again`);
  }
  const wanted = new Set(rows.map((row) => row.path));
  const removed = [];
  for (const file of old) {
    if (wanted.has(file)) continue;
    const target = path.join(installRoot, ...file.split("/"));
    const info = fs.lstatSync(target, { throwIfNoEntry: false });
    if (info?.isFile()) { fs.rmSync(target, { force: true }); removed.push(file); }
  }
  const manifest = path.join(installRoot, PAYLOAD_ROOT_MANIFEST);
  fs.writeFileSync(`${manifest}.tmp-${process.pid}`, `${JSON.stringify({ schema: 1, files: rows }, null, 2)}\n`);
  fs.renameSync(`${manifest}.tmp-${process.pid}`, manifest);
  const entry = rows.find((row) => row.path === descriptor.entrypoint);
  log(`right-release dev: payload-root installed ${rows.length} file(s) into ${installRoot}${removed.length ? `, removed ${removed.length} stale` : ""}`);
  return { installRoot, files: rows.length, removed, entrypoint: path.join(installRoot, ...descriptor.entrypoint.split("/")), entrypointDigest: entry.sha256 };
}
