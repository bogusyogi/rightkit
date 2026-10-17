import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import nodePath from "node:path";
import test from "node:test";
import { ToolchainError, diffInstallerEnvironment, installSwiftToolchain, parseToolchainArgs, resolveSwiftOptions, swiftInstallerUrl } from "./toolchain-swift.mjs";

const win = nodePath.win32;
const INSTALLER_BYTES = "swift-installer-bytes";
const PIN = createHash("sha256").update(INSTALLER_BYTES).digest("hex");
const TEMP = "C:\\runner\\temp";
const INSTALLER = win.join(TEMP, "swift-6.4.0-RELEASE-windows10.exe");
const LOCAL = "C:\\Users\\runner\\AppData\\Local";
const ROOT = win.join(LOCAL, "Programs", "Swift");
const SWIFT_BIN = win.join(ROOT, "Toolchains", "6.4.0+Asserts", "usr", "bin");
const RUNTIME_BIN = win.join(ROOT, "Runtimes", "6.4.0", "usr", "bin");

/** A string-keyed fake fs: a path exists when it is a file or the prefix of one. */
function fakeFs(initial = {}) {
  const files = new Map(Object.entries(initial));
  const prefix = (dir) => `${dir.replace(/\\+$/, "")}\\`;
  const isDir = (dir) => [...files.keys()].some((key) => key.startsWith(prefix(dir)));
  return {
    files,
    existsSync: (file) => files.has(file) || isDir(file),
    readdirSync: (dir) => [...new Set([...files.keys()].filter((key) => key.startsWith(prefix(dir))).map((key) => key.slice(prefix(dir).length).split("\\")[0]))],
    statSync: (file) => ({ isDirectory: () => !files.has(file) && isDir(file), isFile: () => files.has(file) }),
    readFileSync: (file) => { if (!files.has(file)) throw Object.assign(new Error(`ENOENT ${file}`), { code: "ENOENT" }); return Buffer.from(files.get(file)); },
    appendFileSync: (file, text) => files.set(file, `${files.get(file) ?? ""}${text}`),
    rmSync: (file) => { files.delete(file); },
  };
}

const before = { Machine: { Path: "C:\\Windows", TEMP: "C:\\Windows\\Temp" }, User: { Path: "C:\\Users\\runner\\bin" } };
const after = { Machine: { Path: "C:\\Windows", TEMP: "C:\\Windows\\Temp", SDKROOT: "C:\\Users\\runner\\AppData\\Local\\Programs\\Swift\\Platforms\\SDKs\\Windows.sdk" }, User: { Path: `C:\\Users\\runner\\bin;${SWIFT_BIN}` } };

function harness({ exitCode = 0, versionText = "Swift version 6.4.0 (swift-6.4-RELEASE)\nTarget: x86_64-unknown-windows-msvc\n", toolchain = true, runtime = true, downloaded = INSTALLER_BYTES, initial = {} } = {}) {
  const fs = fakeFs({
    ...(toolchain ? { [win.join(SWIFT_BIN, "swift.exe")]: "exe" } : {}),
    ...(runtime ? { [win.join(RUNTIME_BIN, "swiftCore.dll")]: "dll" } : {}),
    ...initial,
  });
  // The toolchain exists only after the installer ran.
  const installedPaths = [...fs.files.entries()].filter(([file]) => file.startsWith(ROOT));
  for (const [file] of installedPaths) fs.files.delete(file);
  const calls = { download: [], installer: [], environment: 0, swift: [] };
  const environments = [before, after];
  const env = { RUNNER_TEMP: TEMP, LOCALAPPDATA: LOCAL, ProgramFiles: "C:\\Program Files", Path: "C:\\Windows", GITHUB_ENV: "C:\\runner\\github.env", GITHUB_PATH: "C:\\runner\\github.path" };
  const run = (command, args, options) => {
    if (command === "powershell.exe") { calls.environment += 1; return { status: 0, stdout: JSON.stringify(environments[Math.min(calls.environment - 1, 1)]) }; }
    if (command === INSTALLER) {
      calls.installer.push(args);
      for (const [file, text] of installedPaths) fs.files.set(file, text);
      fs.files.set(win.join(TEMP, "swift-install.log"), "log line 1\nlog line 2\nboom\n");
      return { status: exitCode };
    }
    if (command.toLowerCase().endsWith("swift.exe")) { calls.swift.push({ command, args, path: options.env.Path }); return { status: 0, stdout: versionText, stderr: "" }; }
    return { status: 1, stdout: "", stderr: "unexpected" };
  };
  const download = async (url, file) => { calls.download.push(url); fs.files.set(file, downloaded); return createHash("sha256").update(downloaded).digest("hex"); };
  return { fs, env, run, download, calls, deps: { platform: "win32", fs, env, run, download } };
}

const options = { version: "6.4.0", sha256: PIN.toUpperCase() };

test("options: version and a 64-hex pin are required, the URL is derived and must be https", () => {
  assert.equal(swiftInstallerUrl("6.4.0"), "https://download.swift.org/swift-6.4.0-release/windows10/swift-6.4.0-RELEASE/swift-6.4.0-RELEASE-windows10.exe");
  assert.deepEqual(resolveSwiftOptions({ version: "6.4.0", sha256: PIN.toUpperCase() }), { version: "6.4.0", sha256: PIN, url: swiftInstallerUrl("6.4.0"), githubEnv: false, json: false });
  for (const bad of [{ version: "six", sha256: PIN }, { version: "6.4.0" }, { version: "6.4.0", sha256: "abc" }, { version: "6.4.0", sha256: PIN, url: "http://x/y.exe" }]) {
    assert.throws(() => resolveSwiftOptions(bad), { code: "TOOLCHAIN_OPTIONS_INVALID" });
  }
  assert.deepEqual(parseToolchainArgs(["swift", "--version", "6.4.0", "--sha256", PIN, "--github-env", "--json"]), { version: "6.4.0", sha256: PIN, githubEnv: true, json: true });
  assert.throws(() => parseToolchainArgs(["gcc"]), /expected: toolchain swift/);
  assert.throws(() => parseToolchainArgs(["swift", "--version"]), /needs a value/);
  assert.throws(() => parseToolchainArgs(["swift", "--wat"]), /unknown toolchain argument/);
});

test("only Windows installs; every other host is refused before any effect", async () => {
  const h = harness();
  await assert.rejects(installSwiftToolchain(options, { ...h.deps, platform: "darwin" }), { code: "TOOLCHAIN_HOST_UNSUPPORTED" });
  assert.deepEqual(h.calls.download, []);
});

test("diffInstallerEnvironment keeps what the installer added or changed and the new PATH entries, never PATH, denied or multi-line values", () => {
  const diff = diffInstallerEnvironment(before, {
    Machine: { ...after.Machine, GITHUB_TOKEN: "x", NODE_OPTIONS: "--require x", MULTI: "a\nb", "BAD-NAME": "x" },
    User: { Path: `C:\\Users\\runner\\bin;${SWIFT_BIN};${RUNTIME_BIN}`, SWIFT_HOME: "C:\\swift" },
  });
  assert.deepEqual(diff.changed, { SDKROOT: after.Machine.SDKROOT, SWIFT_HOME: "C:\\swift" });
  assert.deepEqual(diff.addedPath, [SWIFT_BIN, RUNTIME_BIN]);
});

test("a fresh install verifies the pin first, runs the installer quietly, finds swift and the runtime, and exports only what changed", async () => {
  const h = harness();
  const result = await installSwiftToolchain({ ...options, githubEnv: true }, h.deps);
  assert.deepEqual(h.calls.download, [swiftInstallerUrl("6.4.0")]);
  assert.deepEqual(h.calls.installer, [["/quiet", "/norestart", "/log", win.join(TEMP, "swift-install.log")]]);
  assert.equal(result.installed, true);
  assert.equal(result.swift, win.join(SWIFT_BIN, "swift.exe"));
  assert.equal(result.runtimeBin, RUNTIME_BIN);
  assert.deepEqual(result.exported.variables, ["SDKROOT"]);
  assert.equal(h.fs.files.get(h.env.GITHUB_ENV), `SDKROOT=${after.Machine.SDKROOT}\n`);
  assert.deepEqual(h.fs.files.get(h.env.GITHUB_PATH).trim().split("\n"), [SWIFT_BIN, RUNTIME_BIN]);
  assert.ok(h.calls.swift.every((call) => call.path.includes(SWIFT_BIN) || call.path === "C:\\Windows"), "swift --version runs with the refreshed PATH");
});

test("a download that does not match the pin is deleted and nothing is installed", async () => {
  const h = harness({ downloaded: "tampered installer" });
  await assert.rejects(installSwiftToolchain(options, h.deps), (error) => error instanceof ToolchainError && error.code === "TOOLCHAIN_HASH_MISMATCH" && error.details.expected === PIN);
  assert.equal(h.fs.existsSync(INSTALLER), false);
  assert.deepEqual(h.calls.installer, []);
  // A file already on disk with the wrong bytes is not trusted either: it is downloaded again.
  const stale = harness({ initial: { [INSTALLER]: "stale" } });
  await installSwiftToolchain(options, stale.deps);
  assert.equal(stale.calls.download.length, 1);
  // A file already on disk that matches the pin is reused without a download.
  const reuse = harness({ initial: { [INSTALLER]: INSTALLER_BYTES } });
  await installSwiftToolchain(options, reuse.deps);
  assert.deepEqual(reuse.calls.download, []);
});

test("installer exit 3010 passes, any other failure prints the log tail, and a wrong or missing toolchain afterwards is refused", async () => {
  assert.equal((await installSwiftToolchain(options, harness({ exitCode: 3010 }).deps)).installed, true);
  await assert.rejects(installSwiftToolchain(options, harness({ exitCode: 1603 }).deps), (error) => error.code === "TOOLCHAIN_INSTALL_FAILED" && /1603/.test(error.message) && /boom/.test(error.message));
  await assert.rejects(installSwiftToolchain(options, harness({ versionText: "Swift version 6.3.1 (swift-6.3.1-RELEASE)" }).deps), { code: "TOOLCHAIN_VERSION_MISMATCH" });
  await assert.rejects(installSwiftToolchain(options, harness({ versionText: "Swift version 6.4.01" }).deps), { code: "TOOLCHAIN_VERSION_MISMATCH" });
  await assert.rejects(installSwiftToolchain(options, harness({ toolchain: false }).deps), { code: "TOOLCHAIN_SWIFT_NOT_FOUND" });
  await assert.rejects(installSwiftToolchain(options, harness({ runtime: false }).deps), { code: "TOOLCHAIN_RUNTIME_NOT_FOUND" });
});

test("the pinned version already on PATH is not reinstalled; --github-env then needs GITHUB_ENV and GITHUB_PATH", async () => {
  const present = fakeFs({ [win.join(SWIFT_BIN, "swift.exe")]: "exe", [win.join(RUNTIME_BIN, "swiftCore.dll")]: "dll" });
  const calls = [];
  const run = (command, args) => { calls.push(command); return { status: 0, stdout: "Swift version 6.4.0 (swift-6.4-RELEASE)", stderr: "" }; };
  const env = { RUNNER_TEMP: TEMP, Path: `C:\\Windows;${SWIFT_BIN}`, GITHUB_ENV: "C:\\g.env", GITHUB_PATH: "C:\\g.path" };
  const result = await installSwiftToolchain({ ...options, githubEnv: true }, { platform: "win32", fs: present, env, run, download: async () => assert.fail("no download") });
  assert.equal(result.installed, false);
  assert.deepEqual(calls, [win.join(SWIFT_BIN, "swift.exe")]);
  assert.deepEqual(present.files.get("C:\\g.path").trim().split("\n"), [SWIFT_BIN, RUNTIME_BIN]);
  assert.equal(present.files.has("C:\\g.env"), false, "nothing changed, so no variable is exported");
  await assert.rejects(installSwiftToolchain({ ...options, githubEnv: true }, { platform: "win32", fs: present, env: { ...env, GITHUB_ENV: undefined }, run }), { code: "TOOLCHAIN_GITHUB_ENV_MISSING" });
});
