import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import { installAppWindows, InstallAppWindowsError, resolvePayloadAllowlist, windowsInstallArguments } from "./install-app-windows.mjs";
import { parseArgs } from "./install-app.mjs";

test("the Windows installer is pinned to the one fixed folder", () => {
  assert.deepEqual(windowsInstallArguments("C:\\Users\\a\\AppData\\Local\\Programs\\Pulse"), ["/S", "/D=C:\\Users\\a\\AppData\\Local\\Programs\\Pulse"]);
});

test("install-app --installer needs the fixed folder and the executable name, and excludes the macOS inputs", () => {
  const ok = parseArgs(["--installer", "C:\\x\\Setup.exe", "--dest", "C:\\Apps\\Pulse", "--exe", "Pulse.exe"]);
  assert.equal(ok.installer, "C:\\x\\Setup.exe");
  assert.equal(ok.dest, "C:\\Apps\\Pulse");
  assert.throws(() => parseArgs(["--installer", "C:\\x\\Setup.exe", "--exe", "Pulse.exe"]), /requires --dest/);
  assert.throws(() => parseArgs(["--installer", "C:\\x\\Setup.exe", "--dest", "C:\\Apps\\Pulse"]), /requires --dest/);
  assert.throws(() => parseArgs(["--installer", "C:\\x\\Setup.exe", "--dest", "C:\\A", "--exe", "P.exe", "--dev"]), /cannot be combined/);
});

test("the Windows installer mode refuses other platforms and unsafe folders", () => {
  assert.throws(() => installAppWindows({ installer: "C:\\x\\Setup.exe", dest: "C:\\Apps\\Pulse", exe: "Pulse.exe" }, { platform: "darwin" }), (error) => error instanceof InstallAppWindowsError && error.code === "unsupported");
  assert.throws(() => installAppWindows({ installer: "C:\\x\\Setup.exe", dest: "C:\\", exe: "Pulse.exe" }, { platform: "win32" }), /not a drive root/);
  assert.throws(() => installAppWindows({ installer: "C:\\x\\Setup.exe", dest: "C:\\Apps\\Pulse", exe: "..\\Pulse.exe" }, { platform: "win32" }), /bare executable name/);
});

// ---- data safety: payload-only backup and rollback (Orthic-Labs/rightkit#348) ----

const W = path.win32;
const DEST = "C:\\Users\\a\\AppData\\Local\\CodeRight";
const INSTALLER = "C:\\dl\\CodeRight_setup.exe";
const at = (...parts) => W.join(DEST, ...parts);
const DATA = { "coderight.db": "DB-75MB", "coderight.db-wal": "WAL", "state.json": "{}", "models\\x.bin": "MODEL" };
const PAYLOAD = { "CodeRight.exe": "old-exe", "app.dll": "old-dll", "resources\\ui.pak": "old-pak" };

/** A fake file system: a map of path -> contents plus an operation log. cpSync is forbidden (a whole-folder copy is the bug). */
function fakeFs(initial) {
  const files = new Map(Object.entries(initial).map(([file, body]) => [W.normalize(file), Buffer.from(body)]));
  const log = [];
  const links = new Set(); // paths that lstat reports as symlinks or junctions
  const failCopyTo = new Set(); // destinations whose copyFileSync throws
  const underDir = (dir) => [...files.keys()].filter((file) => file.startsWith(`${W.normalize(dir)}\\`));
  const fs = {
    existsSync: (file) => files.has(W.normalize(file)) || underDir(file).length > 0,
    lstatSync: (file) => {
      const key = W.normalize(file);
      if (links.has(key)) return { isFile: () => false, isDirectory: () => false, isSymbolicLink: () => true, size: 0 };
      if (files.has(key)) return { isFile: () => true, isDirectory: () => false, isSymbolicLink: () => false, size: files.get(key).length };
      if (underDir(file).length > 0) return { isFile: () => false, isDirectory: () => true, isSymbolicLink: () => false, size: 0 };
      throw Object.assign(new Error(`ENOENT ${file}`), { code: "ENOENT" });
    },
    statSync: (file) => fs.lstatSync(file),
    readFileSync: (file) => { const key = W.normalize(file); if (!files.has(key)) throw Object.assign(new Error(`ENOENT ${file}`), { code: "ENOENT" }); return files.get(key); },
    mkdirSync: (dir) => { log.push(["mkdir", W.normalize(dir)]); },
    copyFileSync: (from, to) => { if (failCopyTo.has(W.normalize(to))) throw new Error(`EPERM ${to}`); log.push(["copy", W.normalize(from), W.normalize(to)]); files.set(W.normalize(to), Buffer.from(files.get(W.normalize(from)))); },
    rmSync: (target, { recursive = false } = {}) => {
      log.push(["rm", W.normalize(target), recursive]);
      files.delete(W.normalize(target));
      if (recursive) for (const file of underDir(target)) files.delete(file);
    },
    cpSync: () => { throw new Error("cpSync of a folder is forbidden: the install folder holds user data"); },
  };
  return { fs, files, log, links, failCopyTo };
}
const bodyOf = (files, file) => files.get(W.normalize(file))?.toString();
const dataSnapshot = (files) => Object.keys(DATA).map((name) => [name, bodyOf(files, at(name))]);
const seed = () => fakeFs({ [INSTALLER]: "setup", ...Object.fromEntries(Object.entries({ ...PAYLOAD, ...DATA }).map(([name, body]) => [at(name), body])) });
// The fake NSIS installer: success rewrites the payload (and adds one new file); failure half-writes it and exits non-zero.
const installerRun = (files, { fail = false, empty = false } = {}) => () => {
  files.set(W.normalize(at("CodeRight.exe")), Buffer.from(empty ? "" : fail ? "half-written" : "new-exe"));
  files.set(W.normalize(at("app.dll")), Buffer.from(fail ? "garbage" : "new-dll"));
  files.set(W.normalize(at("resources", "new.dat")), Buffer.from("new-file"));
  return { status: fail ? 1 : 0 };
};
const PAYLOAD_OPTS = { installer: INSTALLER, dest: DEST, exe: "CodeRight.exe", payload: ["app.dll", "resources/ui.pak", "resources/new.dat"] };
const touchesData = (entry) => entry.slice(1).some((arg) => typeof arg === "string" && /coderight\.db|state\.json|models/i.test(arg));

test("windows: data files beside the payload are byte-identical and never copied on a successful install", () => {
  const { fs, files, log } = seed();
  const before = dataSnapshot(files);
  const result = installAppWindows(PAYLOAD_OPTS, { platform: "win32", fs, run: installerRun(files) });
  assert.deepEqual(dataSnapshot(files), before);
  assert.deepEqual(log.filter(touchesData), [], "no copy, remove or mkdir ever named a data file");
  assert.equal(bodyOf(files, at("CodeRight.exe")), "new-exe");
  // The rollback copy holds the payload that existed before, and NOTHING else.
  const previous = `${DEST}.previous`;
  assert.equal(result.previous, previous);
  const kept = [...files.keys()].filter((file) => file.startsWith(`${W.normalize(previous)}\\`)).map((file) => file.slice(previous.length + 1)).sort();
  assert.deepEqual(kept, ["CodeRight.exe", "app.dll", "resources\\ui.pak"]);
  assert.equal(bodyOf(files, W.join(previous, "CodeRight.exe")), "old-exe");
});

test("windows: a failed installer restores only payload files and never overwrites or removes data", () => {
  const { fs, files, log } = seed();
  const before = dataSnapshot(files);
  assert.throws(() => installAppWindows(PAYLOAD_OPTS, { platform: "win32", fs, run: installerRun(files, { fail: true }) }), (error) => error.code === "installer-failed" && /no other file was touched/.test(error.message));
  assert.deepEqual(dataSnapshot(files), before, "the live database, WAL, state and models are byte-identical");
  assert.deepEqual(log.filter(touchesData), []);
  assert.equal(bodyOf(files, at("CodeRight.exe")), "old-exe");
  assert.equal(bodyOf(files, at("app.dll")), "old-dll");
  assert.equal(files.has(W.normalize(at("resources", "new.dat"))), false, "an allowlisted file the failed run newly wrote is removed");
  // Every restore copy goes from the payload-only backup to an allowlisted path.
  const allowed = new Set(["CodeRight.exe", "app.dll", "resources\\ui.pak"].map((name) => W.normalize(at(name))));
  for (const [, from, to] of log.filter((entry) => entry[0] === "copy")) {
    assert.ok(from.startsWith(`${DEST}.previous\\`) ? allowed.has(to) : allowed.has(from), `unexpected copy ${from} -> ${to}`);
  }
});

test("windows: a failure after the installer ran (empty executable) rolls back payload only", () => {
  const { fs, files, log } = seed();
  const before = dataSnapshot(files);
  assert.throws(() => installAppWindows(PAYLOAD_OPTS, { platform: "win32", fs, run: installerRun(files, { empty: true }) }), (error) => error.code === "path-changed");
  assert.deepEqual(dataSnapshot(files), before);
  assert.deepEqual(log.filter(touchesData), []);
  assert.equal(bodyOf(files, at("CodeRight.exe")), "old-exe");
});

test("windows: with no declared payload --exe alone is backed up and restored, and data is untouched", () => {
  const { fs, files, log } = seed();
  const before = dataSnapshot(files);
  const opts = { installer: INSTALLER, dest: DEST, exe: "CodeRight.exe" };
  assert.throws(() => installAppWindows(opts, { platform: "win32", fs, run: installerRun(files, { fail: true }) }), (error) => error.code === "installer-failed");
  assert.equal(bodyOf(files, at("CodeRight.exe")), "old-exe", "the exe is restored from the backup");
  assert.equal(bodyOf(files, at("app.dll")), "garbage", "a file outside the allowlist is never touched");
  assert.deepEqual(dataSnapshot(files), before);
  assert.deepEqual(log.filter(touchesData), []);
  const ok = installAppWindows(opts, { platform: "win32", fs, run: installerRun(files) });
  assert.equal(ok.previous, `${DEST}.previous`);
  assert.equal(bodyOf(files, `${DEST}.previous\\CodeRight.exe`), "old-exe", "the second install backs up the restored exe");
  assert.equal(files.has(W.normalize(`${DEST}.previous\\app.dll`)), false, "only the exe is in the backup");
  assert.deepEqual(dataSnapshot(files), before);
});

test("windows: restore copies each file independently, creates the parent folder, and reports every failure with the installer error", () => {
  const { fs, files, log, failCopyTo } = seed();
  const opts = { ...PAYLOAD_OPTS, payload: ["app.dll", "resources/ui.pak"] };
  // The installer failure deletes the resources folder content and one restore target will refuse the copy.
  const run = () => { files.delete(W.normalize(at("resources", "ui.pak"))); failCopyTo.add(W.normalize(at("app.dll"))); return { status: 7 }; };
  let error;
  try { installAppWindows(opts, { platform: "win32", fs, run }); } catch (caught) { error = caught; }
  assert.equal(error.code, "installer-failed");
  assert.match(error.message, /installer exited 7/);
  assert.match(error.message, /ROLLBACK INCOMPLETE, 1 file\(s\) failed: app\.dll: EPERM/);
  assert.equal(bodyOf(files, at("resources", "ui.pak")), "old-pak", "a later file is still restored after an earlier one failed");
  assert.ok(log.some((entry) => entry[0] === "mkdir" && entry[1] === W.normalize(at("resources"))), "parent folder is created before the copy");
});

test("windows: a symlink or junction anywhere in the chain from dest to a payload file is refused before backup and before restore", () => {
  const before = seed();
  before.links.add(W.normalize(at("resources")));
  assert.throws(() => installAppWindows(PAYLOAD_OPTS, { platform: "win32", fs: before.fs, run: installerRun(before.files) }), (error) => error.code === "usage" && /symlink or junction/.test(error.message));
  assert.deepEqual(before.log, [], "nothing was copied or removed");
  const root = seed();
  root.links.add(W.normalize(DEST));
  assert.throws(() => installAppWindows(PAYLOAD_OPTS, { platform: "win32", fs: root.fs, run: installerRun(root.files) }), (error) => /symlink or junction/.test(error.message));
  // A link that appears while the installer runs is refused at restore time and reported with the installer failure.
  const during = seed();
  const run = () => { during.links.add(W.normalize(at("resources"))); return { status: 1 }; };
  assert.throws(() => installAppWindows(PAYLOAD_OPTS, { platform: "win32", fs: during.fs, run }), (error) => error.code === "installer-failed" && /ROLLBACK INCOMPLETE/.test(error.message) && /symlink or junction/.test(error.message));
  assert.equal(bodyOf(during.files, at("resources", "ui.pak")), "old-pak");
  assert.deepEqual(during.log.filter((entry) => entry[0] === "rm" && /resources/.test(entry[1]) && !/previous/.test(entry[1])), [], "nothing is removed through the link");
});

test("windows: a whole-folder .previous left by an older RightKit (with a stale database copy) is replaced by a payload-only one", () => {
  const { fs, files } = seed();
  files.set(W.normalize(`${DEST}.previous\\coderight.db`), Buffer.from("STALE-DB-COPY"));
  files.set(W.normalize(`${DEST}.previous\\models\\x.bin`), Buffer.from("STALE-MODEL"));
  installAppWindows(PAYLOAD_OPTS, { platform: "win32", fs, run: installerRun(files) });
  assert.equal(files.has(W.normalize(`${DEST}.previous\\coderight.db`)), false);
  assert.equal(files.has(W.normalize(`${DEST}.previous\\models\\x.bin`)), false);
  assert.equal(bodyOf(files, `${DEST}.previous\\CodeRight.exe`), "old-exe");
});

test("windows: the payload allowlist is explicit relative regular files (manifest or flags), never directories or escapes", () => {
  const bad = (entry) => assert.throws(() => resolvePayloadAllowlist({ exe: "A.exe", payload: [entry] }), (error) => error.code === "usage", entry);
  for (const entry of ["..\\x.dll", "a/../../b", "C:\\Windows\\x.dll", "\\\\server\\share\\x", "/etc/x", "dir/", "models\\*.bin", "", "a//b"]) bad(entry);
  assert.deepEqual(resolvePayloadAllowlist({ exe: "A.exe", payload: [] }), ["A.exe"], "nothing declared means --exe alone");
  assert.deepEqual(resolvePayloadAllowlist({ exe: "A.exe", payload: ["lib\\b.dll", "lib/c.dll"] }), ["A.exe", "lib/b.dll", "lib/c.dll"]);
  assert.throws(() => resolvePayloadAllowlist({ exe: "A.exe", payload: ["lib\\b.dll", "LIB/B.dll"] }), (error) => error.code === "usage");
  const { fs } = fakeFs({ "C:\\m\\payload.json": JSON.stringify({ files: ["x.dll", "bin\\y.dll"] }) });
  assert.deepEqual(resolvePayloadAllowlist({ exe: "A.exe", payloadManifest: "C:\\m\\payload.json" }, { fs }), ["A.exe", "bin/y.dll", "x.dll"]);
  assert.throws(() => resolvePayloadAllowlist({ exe: "A.exe", payloadManifest: "C:\\m\\missing.json" }, { fs }), /cannot be read/);
  // A directory named as payload (for example the models folder) is refused before anything is copied.
  const dirFs = seed();
  assert.throws(() => installAppWindows({ ...PAYLOAD_OPTS, payload: ["models"] }, { platform: "win32", fs: dirFs.fs, run: installerRun(dirFs.files) }), (error) => error.code === "usage" && /not a regular file/.test(error.message));
  assert.deepEqual(dirFs.log, []);
});

test("windows: --payload and --payload-manifest parse for --installer only", () => {
  const ok = parseArgs(["--installer", "C:\\x\\Setup.exe", "--dest", "C:\\Apps\\P", "--exe", "P.exe", "--payload", "a.dll", "--payload", "b\\c.dll", "--payload-manifest", "manifest.json"]);
  assert.deepEqual(ok.payload, ["a.dll", "b\\c.dll"]);
  assert.ok(path.isAbsolute(ok.payloadManifest));
  assert.throws(() => parseArgs(["--dmg", "x.dmg", "--payload", "a.dll"]), /only apply to --installer/);
});
