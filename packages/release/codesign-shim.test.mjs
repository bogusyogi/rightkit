import assert from "node:assert/strict";
import test from "node:test";
import * as fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { EventEmitter } from "node:events";
import { runInNewContext } from "node:vm";
import { spawnSync } from "./background-process.mjs";
import { parseCodesignArgv } from "./codesign-argv.mjs";
import { runCodesignShim } from "./codesign-shim.mjs";
import { codesignShimScript, macosCodesignEnvironment } from "./codesign-environment.mjs";
import { validateSigningRequest, executeSigningRequest, signingAgentPaths, defaultSigningRoots, MAX_FRAME } from "./rightkit-signing-agent.mjs";
import { installSigningAgent } from "./signing-agent-install.mjs";
import { runDevelopment } from "./development.mjs";
import { productionBuildEnv } from "./production-mode.mjs";

// Not run on Windows: the codesign shim runs on macOS.
const WINDOWS_SKIP = process.platform === "win32" ? "the codesign shim runs on macOS" : false;

function fixture(t) {
  // Match the signing agent's native canonical spelling, including Windows 8.3 aliases.
  const home = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "rk-codesign-")));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const cwd = path.join(home, "workspace"), managed = path.join(home, "managed"), outside = path.join(home, "outside");
  for (const root of [cwd, managed, outside]) fs.mkdirSync(root);
  for (const root of [cwd, outside]) {
    fs.writeFileSync(path.join(root, "app"), "fixture");
    fs.writeFileSync(path.join(root, "entitlements.plist"), "<plist/>");
  }
  fs.mkdirSync(path.join(cwd, "App with 'quotes'.app"));
  const paths = signingAgentPaths(home);
  fs.mkdirSync(paths.directory, { recursive: true, mode: 0o700 });
  fs.writeFileSync(paths.config, JSON.stringify({ roots: [cwd, managed] }), { mode: 0o600 });
  const request = (argv) => ({ schema: 1, type: "codesign-argv", allowedRoots: [cwd, managed], cwd,
    timeoutMs: 5000, argv, targets: parseCodesignArgv(argv, cwd).targets });
  return { home, cwd, managed, outside, paths, request, policy: { roots: [cwd, managed] } };
}

for (const argv of [
  ["--force", "-s", "Developer ID: Fixture (TEAM)", "--options", "runtime", "--entitlements", "entitlements.plist", "app"],
  ["--sign=Fixture", "--timestamp", "--deep", "--preserve-metadata=identifier,entitlements", "--identifier", "cc.fixture", "--verbose=4", "app"],
  ["-sFixture", "--force", "--timestamp=none", "app"],
  ["-s", "-", "app"],
  ["--verify", "--strict", "--deep", "--verbose", "app"],
  ["-vvv", "--strict", "app"],
  ["-dvv", "-r-", "--entitlements", ":-", "app"],
  ["--display", "-v", "--requirements=-", "--entitlements=-", "app"],
  ["-d", "app", "App with 'quotes'.app"],
]) test(`agent accepts codesign ${argv.join(" ")}`, (t) => {
  const f = fixture(t);
  assert.deepEqual(validateSigningRequest(f.request(argv), f.policy).argv, argv);
});

test("codesign argv rejects unknown operations, conflicting modes, keychains & file outputs", () => {
  for (const argv of [[], ["app"], ["--remove-signature", "app"], ["--sign", "id", "--keychain", "/tmp/key", "app"],
    ["--sign=id", "--keychain=/tmp/key", "app"], ["--verify", "--sign", "id", "app"], ["-dd", "app"],
    ["-s", "id", "--sign", "other", "app"], ["--sign"], ["--force", "app"],
    ["--verify", "--force", "app"], ["--display", "--deep", "app"], ["--display", "--entitlements", "output.plist", "app"],
    ["--display", "--entitlements=output.plist", "app"], ["--display", "--requirements", "output.txt", "app"],
    ["--sign=id", "--entitlements", "-", "app"], ["--sign=id", "--options", "--keychain", "app"],
    ["-s", "id", "--options=/tmp/flags", "app"], ["-s", "id", "--preserve-metadata=/tmp/file", "app"],
    ["--verify", "--detached", "output.sig", "app"], ["--display", "--extract-certificates", "output", "app"],
    ["--sign", "id\0oops", "app"], ["--verify", "app", null]]) {
    assert.throws(() => parseCodesignArgv(argv, "/workspace"), { code: "MAC_SIGNING_AGENT_ARGV_DENIED" }, JSON.stringify(argv));
  }
});

test("agent contains cwd, every target & entitlement input with lexical plus real checks", (t) => {
  const f = fixture(t);
  const denied = (argv, changes = {}) => assert.throws(() => validateSigningRequest({ ...f.request(argv), ...changes }, f.policy), { code: "MAC_SIGNING_AGENT_PATH_DENIED" });
  denied(["--verify", "../outside/app"]);
  denied(["--verify", "app", path.join(f.outside, "app")]);
  denied(["--sign", "id", "--entitlements", path.join(f.outside, "entitlements.plist"), "app"]);
  denied(["--verify", path.join(f.cwd, "app")], { cwd: f.outside });
  denied(["--verify", "app"], { allowedRoots: [f.home] });
  const sibling = `${f.cwd}-sibling`; fs.mkdirSync(sibling); fs.writeFileSync(path.join(sibling, "app"), "fixture");
  denied(["--verify", path.join(sibling, "app")]);
  fs.symlinkSync(path.join(f.outside, "app"), path.join(f.cwd, "escape"));
  denied(["--verify", "escape"]);
  fs.symlinkSync(f.outside, path.join(f.cwd, "escape-dir"));
  denied(["--verify", "escape-dir/../outside/app"]);
  fs.symlinkSync(path.join(f.outside, "entitlements.plist"), path.join(f.cwd, "escape.plist"));
  denied(["--sign", "id", "--entitlements", "escape.plist", "app"]);
  assert.throws(() => validateSigningRequest({ ...f.request(["--verify", "app"]), targets: [] }, f.policy), { code: "MAC_SIGNING_AGENT_PROTOCOL" });
  fs.symlinkSync(path.join(f.cwd, "app"), path.join(f.cwd, "contained"));
  assert.doesNotThrow(() => validateSigningRequest(f.request(["--verify", "contained"]), f.policy));
});

test("read-only codesign (verify, display) runs the system codesign directly, outside the agent's signing roots", { skip: WINDOWS_SKIP }, async (t) => {
  const f = fixture(t), env = { SSH_CONNECTION: "fixture" };
  for (const argv of [["--verify", "--strict", "--deep", "/Applications/App.app"], ["--display", "-r-", "/Applications/App.app"], ["-dvv", "--entitlements", ":-", "App with 'quotes'.app"]]) {
    const result = await runCodesignShim(argv, { ...f, env, request: () => assert.fail("read-only codesign must not reach the agent"), run(command, args, options) {
      assert.equal(command, "/usr/bin/codesign"); assert.deepEqual(args, argv); assert.equal(options.cwd, f.cwd); assert.equal(options.env, env);
      assert.equal(options.shell, false); assert.equal(options.encoding, "utf8"); return { status: 3, stdout: "out", stderr: "err" };
    } });
    assert.deepEqual(result, { status: 3, stdout: "out", stderr: "err" });
  }
  await assert.rejects(runCodesignShim(["--verify", "app"], { ...f, env, request: () => assert.fail(), run: () => ({ error: new Error("spawn failed") }) }), /spawn failed/);
  assert.throws(() => parseCodesignArgv(["--verify", "--sign", "id", "app"], f.cwd), { code: "MAC_SIGNING_AGENT_ARGV_DENIED" });
});

test("codesign-argv relay preserves nonzero exit, cwd, argv & output beyond legacy truncation", { skip: WINDOWS_SKIP }, async (t) => {
  const f = fixture(t), argv = ["--sign", "Developer ID Application: Fixture (TEAM)", "--force", "--timestamp", "App with 'quotes'.app"];
  const expected = { status: 36, stdout: " leading\n" + "λ".repeat(9000) + "\n", stderr: "\n denied\r\ntrailing " };
  const result = await runCodesignShim(argv, { ...f, env: { SSH_CONNECTION: "fixture", PRIVATE_SECRET: "never forwarded" },
    run() { assert.fail("SSH detection must not probe keychain"); },
    request(request) {
      assert.equal(request.type, "codesign-argv"); assert.deepEqual(request.argv, argv); assert.equal(request.cwd, f.cwd);
      assert.doesNotMatch(JSON.stringify(request), /PRIVATE_SECRET|never forwarded/);
      return executeSigningRequest(validateSigningRequest(request, f.policy), { home: f.home, environmentPath: "/usr/bin", run(command, args, options) {
        assert.equal(command, "/usr/bin/codesign"); assert.deepEqual(args, argv); assert.equal(options.cwd, f.cwd);
        assert.equal(options.shell, false); assert.equal(options.windowsHide, true); assert.equal(options.timeout, 180000);
        assert.equal(options.env.PRIVATE_SECRET, undefined); return expected;
      } });
    } });
  assert.deepEqual(result, expected);
});

test("codesign-argv timeout, spawn errors & oversized output fail explicitly", (t) => {
  const f = fixture(t), request = validateSigningRequest(f.request(["--verify", "app"]), f.policy);
  assert.throws(() => executeSigningRequest(request, { run: () => ({ error: { code: "ETIMEDOUT" }, status: null }) }), { code: "MAC_SIGNING_AGENT_TIMEOUT" });
  assert.throws(() => executeSigningRequest(request, { run: () => ({ error: { code: "ENOENT" }, status: null }) }), { code: "MAC_SIGNING_AGENT_JOB_FAILED" });
  assert.throws(() => executeSigningRequest(request, { run: () => ({ status: 0, stdout: "x".repeat(MAX_FRAME), stderr: "" }) }), { code: "MAC_SIGNING_AGENT_PROTOCOL" });
});

test("GUI API uses original argv/cwd/env & absolute real codesign without validation or agent", async () => {
  const env = { PATH: "/managed/codesign:/usr/bin" }, argv = ["--help"];
  const result = await runCodesignShim(argv, { cwd: "/workspace", env, detect: () => ({ guiRunner: false }), request: () => assert.fail(), run(command, args, options) {
    assert.equal(command, "/usr/bin/codesign"); assert.equal(args, argv); assert.equal(options.cwd, "/workspace"); assert.equal(options.env, env);
    assert.equal(options.stdio, "inherit"); return { status: 23 };
  } });
  assert.equal(result.status, 23);
});

test("managed PATH injection preserves Cargo precedence, deduplicates wrapper & isolates GUI/SSH", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), base = { PATH: "/cargo-shims:/usr/bin", RIGHTKIT_MANAGED_ROOT: f.managed, SSH_CONNECTION: "fixture", KEEP: "yes" };
  const injected = macosCodesignEnvironment(base, { platform: "darwin" });
  const bin = injected.PATH.split(path.delimiter)[0];
  assert.ok(bin.startsWith(f.managed + path.sep)); assert.equal(injected.PATH, `${bin}:/cargo-shims:/usr/bin`);
  assert.equal(injected.KEEP, "yes"); assert.equal(base.PATH, "/cargo-shims:/usr/bin");
  assert.equal(fs.statSync(path.join(bin, "codesign")).mode & 0o777, 0o700);
  assert.equal(macosCodesignEnvironment(injected, { platform: "darwin" }).PATH, injected.PATH);
  const direct = macosCodesignEnvironment({ ...base, SSH_CONNECTION: "" }, { platform: "darwin", detect: () => ({ guiRunner: false }) });
  assert.notEqual(direct.PATH.split(":")[0], bin);
  assert.equal(macosCodesignEnvironment(base, { platform: "win32", detect: () => assert.fail() }), base);
});

test("GUI shell wrapper execs real codesign with exact args/output/exit & no Node shim", { skip: process.platform === "win32" }, (t) => {
  const f = fixture(t), tool = path.join(f.home, "real 'codesign'"), entrypoint = path.join(f.home, "agent.mjs"), shim = path.join(f.home, "codesign");
  fs.writeFileSync(tool, `#!${process.execPath}\nprocess.stdout.write(JSON.stringify({pid:process.pid,cwd:process.cwd(),args:process.argv.slice(2)}));process.stderr.write(' unchanged\\n');process.exitCode=23;`, { mode: 0o700 });
  fs.writeFileSync(shim, codesignShimScript({ route: "direct", node: "/does/not/exist", entrypoint, realCodesign: tool }), { mode: 0o700 });
  const argv = ["--help", "with 'quotes' & spaces", "literal $HOME `pwd`"];
  const env = { ...process.env, SSH_CONNECTION: "", SSH_CLIENT: "", SSH_TTY: "" };
  const result = spawnSync(shim, argv, { cwd: f.cwd, env, encoding: "utf8" });
  assert.equal(result.status, 23, result.stderr); assert.equal(result.stderr, " unchanged\n");
  assert.deepEqual(JSON.parse(result.stdout), { pid: result.pid, cwd: f.cwd, args: argv });
  fs.writeFileSync(entrypoint, "process.stdout.write(JSON.stringify(process.argv.slice(2)));process.stderr.write(' agent\\n');process.exitCode=36;");
  fs.writeFileSync(shim, codesignShimScript({ route: "direct", entrypoint, realCodesign: "/does/not/exist" }), { mode: 0o700 });
  const ssh = spawnSync(shim, argv, { cwd: f.cwd, env: { ...env, SSH_CONNECTION: "fixture" }, encoding: "utf8" });
  assert.equal(ssh.status, 36, ssh.stderr); assert.deepEqual(JSON.parse(ssh.stdout), argv); assert.equal(ssh.stderr, " agent\n");
});

test("release worker injects PATH after command env overrides before spawning prePackage/package children", { skip: WINDOWS_SKIP }, async (t) => {
  const f = fixture(t), source = fs.readFileSync(new URL("./release.mjs", import.meta.url), "utf8");
  const start = source.indexOf("function run(cmd, runArgs, cwd"), end = source.indexOf("function ", start + 9);
  const calls = [];
  const context = { process: { platform: "darwin", env: { PATH: "/cargo-shims:/usr/bin", SSH_CONNECTION: "fixture", RIGHTKIT_MANAGED_ROOT: f.managed } },
    opts: { dryRun: false }, Date, console: { log() {} }, macosCodesignEnvironment, productionBuildEnv,
    spawn(command, argv, options) { calls.push({ command, argv, options }); const child = new EventEmitter(); queueMicrotask(() => child.emit("exit", 0)); return child; } };
  runInNewContext(`${source.slice(start, end)};globalThis.launch = run;`, context);
  for (const phase of ["build", "prePackage", "package"]) await context.launch("tool", [phase], f.cwd, { PATH: "/command/bin:/cargo-shims:/usr/bin", CI: "false", TAURI_BUNDLER_DMG_IGNORE_CI: "true" });
  for (const call of calls) {
    const entries = call.options.env.PATH.split(":");
    assert.ok(fs.existsSync(path.join(entries[0], "codesign")));
    assert.ok(fs.existsSync(path.join(entries[0], "osascript")));
    assert.equal(call.options.env.CI, "true");
    assert.equal(call.options.env.TAURI_BUNDLER_DMG_IGNORE_CI, "false");
    assert.deepEqual(entries.slice(1), ["/command/bin", "/cargo-shims", "/usr/bin"]);
  }
});

test("build runner injects shim into environment inherited by release worker", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), source = fs.readFileSync(new URL("./build-release.mjs", import.meta.url), "utf8");
  const start = source.indexOf("  const pathPrefix = buildPathPrefix"), end = source.indexOf("  if (managedCargoTarget)", start);
  const context = { process: { env: { PATH: "/cargo-shims:/usr/bin", SSH_CONNECTION: "fixture", RIGHTKIT_MANAGED_ROOT: f.managed } },
    swiftPlan: null, buildPathPrefix: () => "", platform: "mac", macosCodesignEnvironment, heavyCommandEnvironment: (env) => env,
    releaseEnvironment: () => ({}), vaultRoot: f.home, sharedCacheRoot: f.home, cacheIdentity: { architecture: "arm64" },
    config: { app: "fixture", version: "1" }, cacheKey: "fixture", appRoot: f.cwd, cacheMode: "legacy", layout: { repoRoot: f.cwd, appRoot: f.cwd },
    resolveManagedRoot: () => f.managed, resolveArtifactDir: () => f.managed, path };
  runInNewContext(`${source.slice(start, end)};globalThis.childEnv = env;`, context);
  assert.ok(fs.existsSync(path.join(context.childEnv.PATH.split(":")[0], "codesign")));
  assert.equal(context.childEnv.CI, "true");
  assert.ok(context.childEnv.PATH.endsWith(":/cargo-shims:/usr/bin"));
});

test("right-release dev passes installed shim to every build & install child", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t);
  for (const args of [["init", "--initial-branch", "main"]]) assert.equal(spawnSync("git", args, { cwd: f.cwd }).status, 0);
  const platform = process.platform === "win32" ? "win" : "mac", seen = [];
  runDevelopment({ door: (command, args) => ({ command, args }), root: f.cwd, platform, config: { development: { targets: { [platform]: {
    build: [{ cmd: "build" }, { cmd: "build2" }], install: { kind: "command", cmd: "install" },
  } } } }, env: { PATH: "/cargo-shims:/usr/bin", RIGHTKIT_MANAGED_ROOT: f.managed, SSH_CONNECTION: "fixture" },
    codesignEnvironment: (env) => macosCodesignEnvironment(env, { platform: "darwin" }),
    run(command, argv, options) { seen.push(command); assert.ok(fs.existsSync(path.join(options.env.PATH.split(path.delimiter)[0], "codesign"))); assert.equal(options.env.CI, "true"); assert.equal(options.env.TAURI_BUNDLER_DMG_IGNORE_CI, "false"); return { status: 0 }; } });
  assert.deepEqual(seen, ["build", "build2", "install"]);
});

test("installed agent policy includes system temp & configured cargo targets without caller widening", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), target = path.join(f.home, "cargo-target"); fs.mkdirSync(target);
  fs.writeFileSync(path.join(target, "app"), "fixture");
  const temp = path.join(f.home, "system-temp"), alias = path.join(f.home, "temp-alias");
  fs.mkdirSync(temp); fs.symlinkSync(temp, alias);
  const result = installSigningAgent({ home: f.home, platform: "darwin", managedRoot: f.managed, cargoTargetDirs: [target],
    run(command, args) { return { status: args[0] === "print" && args[1].includes("signing-agent") ? 113 : 0,
      stdout: command === "/usr/bin/which" ? "/fixed/pnpm" : command === "/usr/bin/getconf" ? alias : "" }; }, health: () => ({ pid: 42 }) });
  assert.equal(result.running, true);
  const config = JSON.parse(fs.readFileSync(f.paths.config));
  assert.ok(config.roots.includes(target)); assert.ok(defaultSigningRoots(f.home, f.managed).includes(os.tmpdir()));
  assert.ok(config.roots.includes("/private/tmp"));
  assert.ok(config.roots.includes(temp)); assert.ok(config.roots.includes(alias));
  const request = { ...f.request(["--verify", path.join(target, "app")]), allowedRoots: [target], cwd: target };
  assert.doesNotThrow(() => validateSigningRequest(request, { roots: config.roots }));
});

// Typed codesign `identifier` (request schema 2): strict charset, never free-form argv, old schema refuses it.
const codesignRequest = (f, items, schema = 1) => ({ schema, type: "codesign", allowedRoots: [f.cwd, f.managed], cwd: f.cwd, timeoutMs: 5000, items });
const signItem = (f, extra = {}) => ({ path: path.join(f.cwd, "app"), identity: "Developer ID Application: Fixture (TEAM)", runtime: true, timestamp: true, force: true, ...extra });
const runCodesign = (f, request) => {
  const calls = [];
  executeSigningRequest(validateSigningRequest(request, f.policy), { home: f.home, environmentPath: "/usr/bin", run(command, args) { calls.push({ command, args }); return { status: 0, stdout: "", stderr: "" }; } });
  return calls;
};
test("agent codesign without identifier keeps schema 1 and the historical argv", (t) => {
  const f = fixture(t);
  const [call] = runCodesign(f, codesignRequest(f, [signItem(f)]));
  assert.deepEqual(call.args, ["--force", "--options", "runtime", "--timestamp", "--sign", "Developer ID Application: Fixture (TEAM)", fs.realpathSync.native(path.join(f.cwd, "app"))]);
  assert.doesNotThrow(() => validateSigningRequest(codesignRequest(f, [signItem(f)], 2), f.policy)); // schema 2 without identifier is harmless
});
test("agent codesign identifier travels as typed --identifier before --sign, with per-item entitlements", (t) => {
  const f = fixture(t);
  const [call] = runCodesign(f, codesignRequest(f, [signItem(f, { identifier: "com.example.helper-1", entitlements: path.join(f.cwd, "entitlements.plist") })], 2));
  assert.deepEqual(call.args, ["--force", "--options", "runtime", "--timestamp", "--identifier", "com.example.helper-1", "--sign", "Developer ID Application: Fixture (TEAM)", "--entitlements", fs.realpathSync.native(path.join(f.cwd, "entitlements.plist")), fs.realpathSync.native(path.join(f.cwd, "app"))]);
});
test("agent refuses identifier under schema 1, bad identifiers, unknown fields and schema 3", (t) => {
  const f = fixture(t);
  const rejected = (request) => assert.throws(() => validateSigningRequest(request, f.policy), { code: "MAC_SIGNING_AGENT_PROTOCOL" });
  rejected(codesignRequest(f, [signItem(f, { identifier: "com.example.ok" })], 1));
  for (const identifier of ["-bad", "has space", "a_b", "", ".x", "x".repeat(256), 5, null, "a\nb", "--sign"]) rejected(codesignRequest(f, [signItem(f, { identifier })], 2));
  rejected(codesignRequest(f, [signItem(f, { identifier: "com.example.ok", argv: ["--deep"] })], 2));
  rejected(codesignRequest(f, [signItem(f, { identifier: "com.example.ok" })], 3));
  rejected({ ...codesignRequest(f, [signItem(f)], 2), type: "updater", files: [path.join(f.cwd, "app")], items: undefined });
});
