import assert from "node:assert/strict";
import test from "node:test";
import * as fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { EventEmitter } from "node:events";
import { executeNotarizationRequest, validateSigningRequest, startSigningAgent, signingAgentPaths } from "./rightkit-signing-agent.mjs";
import { requestSigningAgent, requestSigningAgentSync } from "./signing-agent-client.mjs";
import { runMacGuiNotarization } from "./macos-signing-session.mjs";
import { notarizeMacosArtifact, stapleMacosArtifact, validateNotarizationReceipt } from "./macos-notarization.mjs";

// Not run on Windows: notarization runs on macOS (POSIX ownership and modes).
const WINDOWS_SKIP = process.platform === "win32" ? "notarization runs on macOS (POSIX ownership and modes)" : false;

const submissionId = "12345678-1234-1234-1234-123456789abc";
const jobId = "abcdef12-1234-1234-1234-123456789abc";
const secret = "FAKE-API-PRIVATE-SECRET";
function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-notary-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const file = path.join(root, "signed.dmg");
  fs.writeFileSync(file, "signed");
  return { root, file, request: { schema: 1, type: "notarize", path: file, profile: "apple-dev-notary", staple: true,
    cwd: root, allowedRoots: [root], timeoutMs: 1000 } };
}
function mockXcrun({ status = "Accepted", submitExit = 0, stapleExit = 0, validateExit = 0, logExit = 0, hang = false, delay = 0 } = {}) {
  const calls = [], children = [];
  const launch = (command, args, options) => {
    calls.push({ command, args, options });
    const child = new EventEmitter();
    child.stdout = new EventEmitter(); child.stderr = new EventEmitter();
    child.kill = (signal) => { child.killed = signal; queueMicrotask(() => child.emit("close", null)); };
    children.push(child);
    if (!hang) setTimeout(() => {
      const step = args[1];
      const output = step === "submit" ? { id: submissionId, status, password: secret }
        : step === "log" ? { statusCode: 4000, statusSummary: secret, issues: [{ severity: "error", code: 3087733, message: secret, path: secret }] } : {};
      child.stdout.emit("data", Buffer.from(JSON.stringify(output)));
      child.stderr.emit("data", Buffer.from(secret));
      child.emit("close", { submit: submitExit, log: logExit, staple: stapleExit, validate: validateExit }[step] ?? 0);
    }, delay);
    return child;
  };
  return { calls, children, launch };
}
function success(file, kind = "notarize") {
  return { schema: 1, ok: true, id: jobId, notarization: { id: kind === "notarize" ? submissionId : null,
    status: kind === "notarize" ? "Accepted" : "Unknown", logSummary: null },
    steps: [...(kind === "notarize" ? [{ step: "submit", exitCode: 0 }] : []), { step: "staple", file, exitCode: 0 }, { step: "validate", file, exitCode: 0 }],
    stapling: [{ file, alreadyStapled: false, validated: true }] };
}
const guiRun = () => ({ status: 0, stdout: "Aqua" });

test("notarize path policy rejects credentials, executable/env injection, traversal, symlinks & nonarchives", (t) => {
  const f = fixture(t), policy = { roots: [f.root] };
  const { profile, ...defaultProfile } = f.request;
  assert.equal(validateSigningRequest(defaultProfile, policy).profile, "apple-dev-notary");
  for (const patch of [{ apiKey: secret }, { env: { APPLE_API_KEY: secret } }, { argv: ["sh"] }, { profile: "--password" },
    { profile: "bad\nname" }, { staple: false }, { timeoutMs: 7200001 }, { path: "relative.dmg" }, { path: f.root }]) {
    assert.throws(() => validateSigningRequest({ ...f.request, ...patch }, policy));
  }
  const outside = fs.mkdtempSync(path.join(os.tmpdir(), "rk-outside-"));
  t.after(() => fs.rmSync(outside, { recursive: true, force: true }));
  fs.writeFileSync(path.join(outside, "other.dmg"), "signed");
  fs.symlinkSync(path.join(outside, "other.dmg"), path.join(f.root, "escape.dmg"));
  for (const file of [path.join(f.root, "escape.dmg"), path.join(f.root, "..", path.basename(outside), "other.dmg")]) {
    assert.throws(() => validateSigningRequest({ ...f.request, path: file }, policy), { code: "MAC_SIGNING_AGENT_PATH_DENIED" });
  }
  fs.writeFileSync(path.join(f.root, "app.zip"), "zip");
  assert.throws(() => validateSigningRequest({ ...f.request, path: path.join(f.root, "app.zip") }, policy), /requires app bundle/);
  fs.mkdirSync(path.join(f.root, "App.app"));
  assert.doesNotThrow(() => validateSigningRequest({ ...f.request, path: path.join(f.root, "app.zip"), app: path.join(f.root, "App.app") }, policy));
});

test("Accepted notarization runs fixed xcrun submit, staple & validate; env/output contain no credentials", async (t) => {
  const f = fixture(t), mock = mockXcrun(), progress = [];
  const report = await executeNotarizationRequest(f.request, { launch: mock.launch, home: f.root, id: jobId, progress: (frame) => progress.push(frame) });
  assert.equal(report.ok, true); assert.equal(report.id, jobId); assert.equal(report.notarization.id, submissionId);
  assert.deepEqual(report.steps.map((step) => [step.step, step.exitCode]), [["submit", 0], ["staple", 0], ["validate", 0]]);
  assert.deepEqual(mock.calls[0].args, ["notarytool", "submit", f.file, "--keychain-profile", "apple-dev-notary", "--wait", "--output-format", "json"]);
  assert.ok(mock.calls.every((call) => call.command === "/usr/bin/xcrun" && call.options.shell === false));
  assert.deepEqual(Object.keys(mock.calls[0].options.env).sort(), ["HOME", "PATH", "TMPDIR"]);
  assert.doesNotMatch(JSON.stringify(report) + JSON.stringify(progress) + JSON.stringify(mock.calls), new RegExp(secret));
});

test("Invalid submission fetches log summary & exits without stapling or exposing raw diagnostics", async (t) => {
  const f = fixture(t), mock = mockXcrun({ status: "Invalid", submitExit: 65 });
  const report = await executeNotarizationRequest(f.request, { launch: mock.launch });
  assert.equal(report.ok, false); assert.equal(report.code, "MAC_SIGNING_NOTARIZATION_FAILED");
  assert.deepEqual(report.notarization, { id: submissionId, status: "Invalid", logSummary: {
    statusCode: 4000, errors: 1, warnings: 0, issues: [{ severity: "error", code: 3087733 }] } });
  assert.deepEqual(report.steps.map((step) => [step.step, step.exitCode]), [["submit", 65], ["log", 0]]);
  assert.doesNotMatch(JSON.stringify(report), new RegExp(secret));
});

test("deadline kills stalled xcrun & reports timeout with submit exit", async (t) => {
  const f = fixture(t), mock = mockXcrun({ hang: true });
  const report = await executeNotarizationRequest({ ...f.request, timeoutMs: 10 }, { launch: mock.launch });
  assert.equal(report.code, "MAC_SIGNING_AGENT_TIMEOUT"); assert.equal(report.ok, false);
  assert.deepEqual(report.steps, [{ step: "submit", exitCode: null }]);
  assert.equal(mock.children[0].killed, "SIGKILL");
});

test("Accepted ZIP staples app bundle; PKG staples container", { skip: WINDOWS_SKIP }, async (t) => {
  const f = fixture(t), app = path.join(f.root, "App.app"); fs.mkdirSync(app);
  for (const extension of ["zip", "pkg"]) {
    const file = path.join(f.root, `signed.${extension}`); fs.writeFileSync(file, "signed");
    const mock = mockXcrun();
    const request = validateSigningRequest({ ...f.request, path: file, ...(extension === "zip" ? { app } : {}) }, { roots: [f.root] });
    const report = await executeNotarizationRequest(request, { launch: mock.launch });
    assert.equal(report.ok, true);
    assert.deepEqual(report.stapling.map((item) => item.file), [extension === "zip" ? app : file]);
    assert.ok(mock.calls.filter((call) => call.args[0] === "stapler").every((call) => call.args[2] !== (extension === "zip" ? file : app)));
  }
});

for (const [options, steps] of [[{ stapleExit: 65 }, ["submit", "staple"]], [{ validateExit: 65 }, ["submit", "staple", "validate"]]]) {
  test(`stapling failure preserves Accepted submission & failed step (${JSON.stringify(options)})`, async (t) => {
    const f = fixture(t), mock = mockXcrun(options);
    const report = await executeNotarizationRequest(f.request, { launch: mock.launch });
    assert.equal(report.code, "MAC_SIGNING_STAPLE_FAILED"); assert.equal(report.notarization.status, "Accepted");
    assert.deepEqual(report.steps.map((step) => step.step), steps); assert.equal(report.steps.at(-1).exitCode, 65);
    assert.deepEqual(report.stapling, []);
  });
}

test("SSH/non-Aqua routing preserves seal receipt schema & final-byte digest; GUI retains API auth", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), receiptPath = path.join(f.root, "macos-notarization.json"), calls = [];
  const env = { SSH_CONNECTION: "ssh", APPLE_API_KEY_PATH: secret, APPLE_API_KEY: secret, APPLE_API_ISSUER: secret };
  const request = (plan) => {
    calls.push(plan); fs.appendFileSync(f.file, "ticket"); return success(f.file);
  };
  const receipt = notarizeMacosArtifact({ archive: f.file, receiptPath, env, cwd: f.root, allowedRoots: [f.root], run: guiRun, request });
  assert.equal(calls[0].type, "notarize"); assert.equal(calls[0].profile, "apple-dev-notary");
  assert.doesNotMatch(JSON.stringify(calls), new RegExp(secret));
  assert.deepEqual(Object.keys(receipt), ["schema", "signingContract", "archive", "notarization", "stapling"]);
  assert.equal(receipt.archive.sizeBytes, fs.statSync(f.file).size);
  assert.doesNotThrow(() => validateNotarizationReceipt({ archive: f.file, receiptPath, run: () => ({ status: 0 }) }));
  const localCalls = [];
  const run = (command, args) => {
    if (command === "/bin/launchctl") return { status: 0, stdout: "Aqua" };
    if (command === "/usr/bin/security") return { status: 0 };
    localCalls.push(args);
    return { status: 0, stdout: args[0] === "notarytool" ? JSON.stringify({ id: submissionId, status: "Accepted" }) : "" };
  };
  notarizeMacosArtifact({ archive: f.file, receiptPath, env: { ...env, SSH_CONNECTION: undefined }, run,
    request: () => assert.fail("GUI must keep local notarization") });
  assert.ok(localCalls[0].includes("--key")); assert.ok(localCalls[0].includes(secret));
  notarizeMacosArtifact({ archive: f.file, receiptPath, env: {}, cwd: f.root, allowedRoots: [f.root], request,
    run: (_command, args) => ({ status: 0, stdout: args[0] === "managername" ? "Background" : "" }) });
  assert.equal(calls.length, 2);
});

test("missing or old agent installs once & retries; no GUI remains typed; timeout never installs", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t);
  for (const old of [false, true]) {
    let attempts = 0, installs = 0;
    const report = runMacGuiNotarization({ file: f.file, cwd: f.root, allowedRoots: [f.root], run: guiRun,
      request(plan) {
        if (plan.type === "health") return { schema: 1, ok: true, capabilities: ["codesign"] };
        if (++attempts === 1) throw Object.assign(new Error(), { code: old ? "MAC_SIGNING_AGENT_PROTOCOL" : "MAC_SIGNING_AGENT_UNAVAILABLE" });
        return success(f.file);
      }, install: () => { installs++; } });
    assert.equal(report.ok, true); assert.equal(attempts, 2); assert.equal(installs, 1);
  }
  assert.throws(() => runMacGuiNotarization({ file: f.file, run: () => ({ status: 113 }), install: () => assert.fail() }), { code: "MAC_SIGNING_NO_GUI_SESSION" });
  assert.throws(() => runMacGuiNotarization({ file: f.file, cwd: f.root, allowedRoots: [f.root], run: guiRun,
    request: () => { throw Object.assign(new Error(), { code: "MAC_SIGNING_AGENT_TIMEOUT" }); }, install: () => assert.fail() }), { code: "MAC_SIGNING_AGENT_TIMEOUT" });
});

test("SSH standalone stapling goes through agent & failed jobs never write sealable receipts", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), receiptPath = path.join(f.root, "receipt.json");
  const options = { env: { SSH_TTY: "ssh" }, cwd: f.root, allowedRoots: [f.root], run: guiRun };
  const stapled = stapleMacosArtifact(f.file, { ...options, request(plan) {
    assert.equal(plan.type, "staple"); assert.equal(plan.profile, undefined); return success(f.file, "staple");
  } });
  assert.equal(stapled.validated, true);
  assert.throws(() => notarizeMacosArtifact({ ...options, archive: f.file, receiptPath,
    request: () => { throw Object.assign(new Error("Invalid"), { code: "MAC_SIGNING_NOTARIZATION_FAILED" }); } }), { code: "MAC_SIGNING_NOTARIZATION_FAILED" });
  assert.equal(fs.existsSync(receiptPath), false);
});

test("SSH receipt preserves caller path spelling across symlinked roots", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), alias = path.join(f.root, "alias"); fs.symlinkSync(f.root, alias);
  const archive = path.join(alias, "signed.dmg"), receiptPath = path.join(f.root, "receipt.json");
  const receipt = notarizeMacosArtifact({ archive, receiptPath, env: { SSH_CONNECTION: "ssh" }, cwd: f.root,
    allowedRoots: [f.root], run: guiRun, request: () => success(f.file) });
  assert.equal(receipt.archive.file, archive); assert.equal(receipt.stapling[0].file, archive);
  assert.doesNotThrow(() => validateNotarizationReceipt({ archive, receiptPath, run: () => ({ status: 0 }) }));
});

function socketClient(frames, onProgress = () => {}) {
  const socket = new EventEmitter(); socket.destroy = () => {};
  socket.write = () => { queueMicrotask(() => {
    const bytes = Buffer.from(frames.map((frame) => JSON.stringify(frame)).join("\n") + "\n");
    socket.emit("data", bytes.subarray(0, 15)); socket.emit("data", bytes.subarray(15));
  }); };
  return { paths: { directory: "directory", plist: "plist", token: "token", socket: "socket" }, timeoutMs: 100, onProgress,
    io: { lstatSync: (file) => ({ uid: process.getuid(), mode: file === "directory" ? 0o700 : 0o600, isSymbolicLink: () => false,
      isDirectory: () => file === "directory", isSocket: () => file === "socket", isFile: () => ["plist", "token"].includes(file) }), readFileSync: () => "a".repeat(64) },
    connect() { queueMicrotask(() => socket.emit("connect")); return socket; } };
}
test("client handles split/coalesced progress frames & failure reports without secret diagnostics", { skip: WINDOWS_SKIP }, async () => {
  const frames = [{ schema: 1, event: "status", step: "submit", id: jobId }, { schema: 1, event: "heartbeat", id: jobId }];
  const progress = [], report = success("/tmp/test.dmg");
  assert.deepEqual(await requestSigningAgent({ type: "notarize" }, socketClient([...frames, report], (frame) => progress.push(frame))), report);
  assert.deepEqual(progress, frames);
  const failed = { ...report, ok: false, code: "MAC_SIGNING_STAPLE_FAILED", message: "Notarization or stapling failed inside signing agent" };
  await assert.rejects(requestSigningAgent({ type: "notarize" }, socketClient([failed])), (error) => error.report?.notarization.id === submissionId);
  await assert.rejects(requestSigningAgent({ type: "notarize" }, socketClient([{ ...report, credentials: secret }])), { code: "MAC_SIGNING_AGENT_PROTOCOL" });
  await assert.rejects(requestSigningAgent({ type: "notarize" }, socketClient([{ ...failed, message: secret }])), { code: "MAC_SIGNING_AGENT_PROTOCOL" });
  const sync = () => requestSigningAgentSync({ type: "notarize", timeoutMs: 1800000 }, { runClient(_command, _args, options) {
    assert.equal(options.timeout, 1806000); assert.deepEqual(options.stdio, ["pipe", "pipe", "inherit"]);
    return { status: 1, stdout: JSON.stringify(failed) };
  } });
  assert.throws(sync, (error) => error.report?.id === jobId);
});

test("agent emits live heartbeats with per-job ids during silent xcrun & queued jobs; logs only metadata", { skip: WINDOWS_SKIP }, async (t) => {
  const f = fixture(t), paths = signingAgentPaths(f.root), logs = [], mock = mockXcrun({ delay: 15 });
  fs.mkdirSync(paths.directory, { recursive: true, mode: 0o700 }); fs.writeFileSync(paths.config, "{}", { mode: 0o600 });
  let connection;
  const server = new EventEmitter(); server.listen = (file, ready) => { fs.writeFileSync(file, "socket", { mode: 0o600 }); ready(); };
  await startSigningAgent({ paths, config: { roots: [f.root], home: f.root }, launch: mock.launch, heartbeatMs: 5,
    log: (event) => logs.push(event), createServer(callback) { connection = callback; return server; } });
  const token = fs.readFileSync(paths.token, "utf8"), sockets = [];
  for (let index = 0; index < 2; index++) {
    const socket = new EventEmitter(); socket.frames = []; socket.destroyed = false; socket.setTimeout = () => {};
    socket.write = (frame) => socket.frames.push(JSON.parse(frame));
    socket.end = (frame) => { socket.write(frame); socket.destroyed = true; socket.emit("done"); };
    socket.destroy = () => { socket.destroyed = true; };
    connection(socket); sockets.push(socket);
    socket.emit("data", Buffer.from(`${JSON.stringify({ ...f.request, token })}\n`));
  }
  await Promise.all(sockets.map((socket) => new Promise((resolve) => socket.once("done", resolve))));
  for (const socket of sockets) {
    const report = socket.frames.at(-1); assert.equal(report.ok, true);
    assert.ok(socket.frames.some((frame) => frame.event === "heartbeat"));
    assert.ok(socket.frames.every((frame) => frame.id === report.id));
  }
  assert.notEqual(sockets[0].frames.at(-1).id, sockets[1].frames.at(-1).id);
  assert.deepEqual(mock.calls.map((call) => call.args[1]), ["submit", "staple", "validate", "submit", "staple", "validate"]);
  assert.doesNotMatch(JSON.stringify(logs) + JSON.stringify(sockets.map((socket) => socket.frames)), new RegExp(`${secret}|${token}`));
});
