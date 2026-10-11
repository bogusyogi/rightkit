import assert from "node:assert/strict";
import test from "node:test";
import * as fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { EventEmitter } from "node:events";
import { AGENT_LABEL, authenticateSigningToken, validateSigningRequest, executeSigningRequest, privatePath,
  signingAgentPaths, startSigningAgent, managedSigningLog } from "./rightkit-signing-agent.mjs";
import { requestSigningAgent, requestSigningAgentSync } from "./signing-agent-client.mjs";
import { installSigningAgent, uninstallSigningAgent, signingAgentPlist, signingAgentStatus } from "./signing-agent-install.mjs";

// Not run on Windows: the signing agent runs on macOS (codesign, POSIX ownership and modes).
const WINDOWS_SKIP = process.platform === "win32" ? "the signing agent runs on macOS (codesign, POSIX ownership and modes)" : false;

function fixture(t) {
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-agent-test-")));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const workspace = path.join(home, "workspace"), managed = path.join(home, "managed"), outside = path.join(home, "outside");
  for (const directory of [workspace, managed, outside]) fs.mkdirSync(directory, { mode: 0o700 });
  const app = path.join(workspace, "app"), artifact = path.join(managed, "update.tar.gz"), entitlement = path.join(workspace, "entitlements.plist");
  for (const file of [app, artifact, entitlement, path.join(outside, "app")]) fs.writeFileSync(file, "fixture");
  const request = { schema: 1, token: "a".repeat(64), type: "codesign", allowedRoots: [workspace, managed], cwd: workspace, timeoutMs: 5000,
    items: [{ path: app, identity: "Developer ID Fixture", force: true, runtime: true, timestamp: true, entitlements: entitlement }] };
  return { home, workspace, managed, outside, app, artifact, entitlement, request, policy: { roots: [workspace, managed] } };
}
test("protocol rejects arbitrary execution, secrets, unknown fields & unsafe options", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t);
  assert.equal(validateSigningRequest(f.request, f.policy).items[0].path, f.app);
  for (const change of [{ type: "shell" }, { schema: 2 }, { command: "sh" }, { env: { SECRET: "secret" } }, { timeoutMs: Infinity }, { timeoutMs: 180001 },
    { items: [{ path: f.app, identity: "-" }] }, { items: [{ path: f.app, identity: "real", runtime: "yes" }] },
    { items: [{ path: f.app, identity: "real", entitlements: "relative" }] }, { items: [{ path: f.app, identity: "real", argv: ["--deep"] }] }]) {
    assert.throws(() => validateSigningRequest({ ...f.request, ...change }, f.policy));
  }
  assert.deepEqual(validateSigningRequest({ schema: 1, type: "health", token: "x" }, f.policy), { schema: 1, type: "health", token: "x" });
  assert.throws(() => validateSigningRequest({ schema: 1, type: "health", token: "x", command: "sh" }, f.policy), { code: "MAC_SIGNING_AGENT_PROTOCOL" });
});
test("caller allow-list narrows installed roots; traversal, prefix siblings & symlink escapes rejected", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t);
  for (const target of ["relative", path.join(f.workspace, "../outside/app"), path.join(f.outside, "app")]) {
    assert.throws(() => validateSigningRequest({ ...f.request, items: [{ path: target, identity: "real" }] }, f.policy), { code: "MAC_SIGNING_AGENT_PATH_DENIED" });
  }
  assert.throws(() => validateSigningRequest({ ...f.request, allowedRoots: [f.home] }, f.policy), { code: "MAC_SIGNING_AGENT_PATH_DENIED" });
  assert.throws(() => validateSigningRequest({ ...f.request, cwd: f.outside }, f.policy), { code: "MAC_SIGNING_AGENT_PATH_DENIED" });
  fs.symlinkSync(path.join(f.outside, "app"), path.join(f.workspace, "escape"));
  assert.throws(() => validateSigningRequest({ ...f.request, items: [{ path: path.join(f.workspace, "escape"), identity: "real" }] }, f.policy), { code: "MAC_SIGNING_AGENT_PATH_DENIED" });
  fs.symlinkSync(f.outside, path.join(f.workspace, "alias"));
  assert.throws(() => validateSigningRequest({ ...f.request, allowedRoots: [path.join(f.workspace, "alias")] }, f.policy), { code: "MAC_SIGNING_AGENT_PATH_DENIED" });
  const staging = path.join(f.home, "requests"); fs.mkdirSync(staging); fs.writeFileSync(path.join(staging, "e.plist"), "fixture");
  const staged = { ...f.request, allowedRoots: [...f.request.allowedRoots, staging], items: [{ path: f.app, identity: "real", entitlements: path.join(staging, "e.plist") }] };
  assert.doesNotThrow(() => validateSigningRequest(staged, { ...f.policy, stagingRoot: staging }));
  staged.items[0].path = path.join(staging, "e.plist");
  assert.throws(() => validateSigningRequest(staged, { ...f.policy, stagingRoot: staging }), { code: "MAC_SIGNING_AGENT_PATH_DENIED" });
});
test("updater signature output symlinks, including dangling links, denied", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t);
  const request = { ...f.request, type: "updater", files: [f.artifact] }; delete request.items;
  assert.equal(validateSigningRequest(request, f.policy).files[0], f.artifact);
  fs.symlinkSync(path.join(f.outside, "not-yet-created"), `${f.artifact}.sig`);
  assert.throws(() => validateSigningRequest(request, f.policy), { code: "MAC_SIGNING_AGENT_PATH_DENIED" });
});
test("token authentication requires exact random token & rejects missing/wrong/malformed tokens", () => {
  const token = "a".repeat(64);
  assert.doesNotThrow(() => authenticateSigningToken(token, token));
  for (const candidate of [undefined, "", token.slice(1), "b".repeat(64), "A".repeat(64), { token }]) assert.throws(() => authenticateSigningToken(candidate, token), { code: "MAC_SIGNING_AGENT_AUTH" });
});
test("client verifies private directory/token/socket uid, mode & symlinks", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t); const file = path.join(f.home, "token");
  fs.writeFileSync(file, "token", { mode: 0o600 }); privatePath(file, 0o600);
  assert.throws(() => privatePath(file, 0o600, { uid: process.getuid() + 1 }), { code: "MAC_SIGNING_AGENT_UNSAFE_PATH" });
  fs.chmodSync(file, 0o644); assert.throws(() => privatePath(file, 0o600), { code: "MAC_SIGNING_AGENT_UNSAFE_PATH" });
  fs.symlinkSync(file, path.join(f.home, "link")); assert.throws(() => privatePath(path.join(f.home, "link"), 0o600), { code: "MAC_SIGNING_AGENT_UNSAFE_PATH" });
});
test("codesign execution reconstructs fixed argv, preserves order & bounds duration", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), calls = [];
  const request = validateSigningRequest({ ...f.request, items: [...f.request.items, { path: f.artifact, identity: "SHA1", preserveEntitlements: true }] }, f.policy);
  const report = executeSigningRequest(request, { home: f.home, environmentPath: "/usr/bin", run(command, args, options) {
    calls.push({ command, args, options }); assert.equal(options.shell, false); assert.ok(options.timeout > 0 && options.timeout <= 5000);
    return { status: 0, stdout: "okay", stderr: "signed" };
  } });
  assert.equal(report.results.length, 2);
  assert.deepEqual(calls[0].args, ["--force", "--options", "runtime", "--timestamp", "--sign", "Developer ID Fixture", "--entitlements", f.entitlement, f.app]);
  assert.deepEqual(calls[1].args, ["--sign", "SHA1", "--preserve-metadata=entitlements", f.artifact]);
  assert.ok(calls.every((c) => c.command === "/usr/bin/codesign"));
  assert.throws(() => executeSigningRequest(request, { run: () => ({ status: 36, stderr: "denied" }) }), { code: "MAC_SIGNING_AGENT_JOB_FAILED" });
});
test("updater key is read & used only inside agent; output/errors & key read failures reveal no secrets", (t) => {
  const f = fixture(t), secret = "FAKE-PRIVATE-KEY\nsecret-line";
  const request = { schema: 1, type: "updater", files: [f.artifact], cwd: f.workspace, timeoutMs: 5000 };
  for (const failure of [null, "key", "signer", "throw"]) {
    const calls = [];
    const invoke = () => executeSigningRequest(request, { home: f.home, username: "fixture", pnpm: "/fixed/pnpm", environmentPath: "/fixed/bin:/usr/bin", run(command, args, options) {
      calls.push({ command, args, env: { ...options.env } });
      if (command === "/usr/bin/security") return { status: failure === "key" ? 36 : 0, stdout: secret, stderr: secret };
      assert.equal(command, "/fixed/pnpm"); assert.equal(options.env.TAURI_SIGNING_PRIVATE_KEY, secret); assert.equal(options.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD, "");
      assert.ok(!args.includes(secret));
      if (failure === "throw") throw new Error(secret);
      return { status: failure === "signer" ? 1 : 0, stdout: secret, stderr: secret };
    } });
    if (failure) {
      try { invoke(); assert.fail(); } catch (error) { if (failure !== "throw") assert.doesNotMatch(error.message, /FAKE-PRIVATE-KEY|secret-line/); }
    } else assert.deepEqual(invoke().results, [{ status: 0, stdout: "", stderr: "" }]);
    assert.deepEqual(calls[0].args, ["find-generic-password", "-a", "fixture", "-s", "rightsuite-updater-key", "-w"]);
    assert.equal(calls[0].env.TAURI_SIGNING_PRIVATE_KEY, undefined);
  }
});
function mockClient(report, { raw, error } = {}) {
  let frame;
  const socket = new EventEmitter(); socket.destroy = () => {}; socket.write = (value) => {
    frame = value.toString();
    queueMicrotask(() => { if (error) socket.emit("error", error); else socket.emit("data", Buffer.from(raw ?? `${JSON.stringify(report)}\n`)); });
  };
  const io = { lstatSync: (file) => ({ uid: process.getuid(), mode: file === "directory" ? 0o700 : 0o600, isSymbolicLink: () => false,
    isDirectory: () => file === "directory", isSocket: () => file === "socket", isFile: () => file === "token" || file === "plist" }), readFileSync: () => "a".repeat(64) };
  return { paths: { directory: "directory", socket: "socket", token: "token", plist: "plist" }, io, timeoutMs: 100,
    connect() { queueMicrotask(() => socket.emit("connect")); return socket; }, frame: () => frame };
}
test("socket client frames typed JSON with local token & rejects invalid/incomplete/secret-bearing reports", { skip: WINDOWS_SKIP }, async () => {
  const request = { schema: 1, type: "updater", files: ["/artifact"] }, success = { schema: 1, ok: true, results: [{ status: 0, stdout: "", stderr: "" }] };
  const f = mockClient(success);
  assert.deepEqual(await requestSigningAgent(request, f), success);
  assert.equal(JSON.parse(f.frame()).token, "a".repeat(64));
  for (const report of [{}, { schema: 1, ok: true, results: [] }, { schema: 1, ok: true, results: [{ status: 0, stdout: "SECRET", stderr: "" }] }]) {
    await assert.rejects(requestSigningAgent(request, mockClient(report)), { code: "MAC_SIGNING_AGENT_PROTOCOL" });
  }
  await assert.rejects(requestSigningAgent(request, mockClient(null, { error: { code: "ECONNREFUSED" } })), { code: "MAC_SIGNING_AGENT_UNAVAILABLE" });
  await assert.rejects(requestSigningAgent(request, mockClient({ schema: 1, ok: false, code: "MAC_SIGNING_AGENT_AUTH", message: "SECRET" })), (e) => e.code === "MAC_SIGNING_AGENT_AUTH" && !e.message.includes("SECRET"));
});
test("codesign-argv socket client accepts nonzero exits verbatim & rejects malformed results", { skip: WINDOWS_SKIP }, async () => {
  const request = { schema: 1, type: "codesign-argv", argv: ["-d", "/artifact"], targets: ["/artifact"] };
  const report = { schema: 1, ok: true, results: [{ status: 36, stdout: " untrimmed\n", stderr: "\nerror\r\n" }] };
  assert.deepEqual(await requestSigningAgent(request, mockClient(report)), report);
  for (const result of [{ status: -1, stdout: "", stderr: "" }, { status: 256, stdout: "", stderr: "" },
    { status: 1.5, stdout: "", stderr: "" }, { status: 0, stdout: null, stderr: "" }]) {
    await assert.rejects(requestSigningAgent(request, mockClient({ schema: 1, ok: true, results: [result] })), { code: "MAC_SIGNING_AGENT_PROTOCOL" });
  }
});
test("sync client sends no inherited secrets or token in environment/argv", () => {
  const report = { schema: 1, ok: true, results: [] };
  requestSigningAgentSync({ schema: 1, type: "health" }, { runClient(command, args, options) {
    assert.ok(args[0].endsWith("signing-agent-client.mjs"));
    assert.deepEqual(Object.keys(options.env).sort(), ["HOME", "PATH"]);
    assert.equal(JSON.parse(options.input).request.type, "health");
    return { status: 0, stdout: JSON.stringify(report) };
  } });
});
test("LaunchAgent install writes private standalone runtime & required invisible Aqua plist, bootstraps gui uid", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), calls = []; let attempts = 0, clock = 0;
  const run = (command, args) => { calls.push([command, args]); return { status: args[0] === "print" && args[1].includes(AGENT_LABEL) ? 113 : 0, stdout: command === "/usr/bin/which" ? "/fixed/pnpm\n" : "" }; };
  const installed = installSigningAgent({ home: f.home, uid: process.getuid(), username: "fixture", platform: "darwin", io: fs, run, managedRoot: f.managed,
    health() { if (++attempts === 1) throw Object.assign(new Error(), { code: "MAC_SIGNING_AGENT_UNAVAILABLE" }); return { pid: 123 }; }, now: () => clock, sleep: (ms) => { clock += ms; } });
  assert.equal(installed.running, true); assert.equal(attempts, 2);
  const paths = signingAgentPaths(f.home), plist = fs.readFileSync(paths.plist, "utf8");
  assert.match(plist, /LimitLoadToSessionType<\/key><string>Aqua/); assert.match(plist, /RunAtLoad<\/key><true\/>/); assert.match(plist, /KeepAlive<\/key><true\/>/);
  assert.match(plist, /ProcessType<\/key><string>Background/); assert.doesNotMatch(plist, /Terminal|\.command|\/usr\/bin\/open|SSH_CONNECTION|TAURI_SIGNING_PRIVATE_KEY/);
  assert.deepEqual(calls.at(-1), ["/bin/launchctl", ["bootstrap", `gui/${process.getuid()}`, paths.plist]]);
  assert.equal(fs.statSync(paths.directory).mode & 0o777, 0o700); assert.equal(fs.statSync(paths.config).mode & 0o777, 0o600);
  const config = JSON.parse(fs.readFileSync(paths.config)); assert.ok(config.roots.includes(f.managed)); assert.equal(config.pnpm, "/fixed/pnpm");
  assert.deepEqual(fs.readdirSync(paths.runtime).sort(), ["background-process.mjs", "codesign-argv.mjs", "managed-roots.mjs", "rightkit-signing-agent.mjs"]);
  assert.match(signingAgentPlist({ paths, home: "<&'", node: '/<&"node' }), /&lt;&amp;&quot;node/);
});
test("install rejects no-GUI session without writing anything; failed bootstrap remains typed", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t);
  assert.throws(() => installSigningAgent({ home: f.home, platform: "darwin", run: () => ({ status: 113 }) }), { code: "MAC_SIGNING_NO_GUI_SESSION" });
  assert.ok(!fs.existsSync(signingAgentPaths(f.home).directory));
  assert.throws(() => installSigningAgent({ home: f.home, platform: "darwin", managedRoot: f.managed,
    run(command, args) { return { status: args[0] === "bootstrap" || (args[0] === "print" && args[1].includes(AGENT_LABEL)) ? 1 : 0, stdout: command === "/usr/bin/which" ? "/fixed/pnpm" : "" }; } }), { code: "MAC_SIGNING_AGENT_INSTALL_FAILED" });
});
test("status reports GUI & health separately; uninstall bootouts only owned agent & preserves siblings", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), paths = signingAgentPaths(f.home);
  fs.mkdirSync(paths.directory, { recursive: true, mode: 0o700 }); fs.mkdirSync(path.dirname(paths.plist), { recursive: true }); fs.writeFileSync(paths.plist, "plist", { mode: 0o600 });
  const sibling = path.join(path.dirname(paths.directory), "other-service"); fs.mkdirSync(sibling);
  const status = signingAgentStatus({ home: f.home, run: () => ({ status: 0 }), health: () => ({ pid: 123 }) });
  assert.equal(status.running, true); assert.equal(status.guiSession, true);
  const unavailable = signingAgentStatus({ home: f.home, run: () => ({ status: 113 }), health() { throw Object.assign(new Error(), { code: "MAC_SIGNING_AGENT_UNAVAILABLE" }); } });
  assert.equal(unavailable.running, false); assert.equal(unavailable.guiSession, false);
  const calls = [];
  uninstallSigningAgent({ home: f.home, platform: "darwin", run(command, args) { calls.push(args); return { status: 0 }; } });
  assert.deepEqual(calls[1], ["bootout", `gui/${process.getuid()}/${AGENT_LABEL}`]);
  assert.ok(!fs.existsSync(paths.directory)); assert.ok(!fs.existsSync(paths.plist)); assert.ok(fs.existsSync(sibling));
});
function serverSocket() {
  const socket = new EventEmitter(); socket.destroyed = false; socket.setTimeout = () => {}; socket.destroy = () => { socket.destroyed = true; };
  socket.end = (frame) => { socket.report = JSON.parse(frame); socket.destroyed = true; };
  return socket;
}
test("agent rotates token, rejects unauthenticated requests, logs metadata only & runs jobs serially", { skip: WINDOWS_SKIP }, async (t) => {
  const f = fixture(t), paths = signingAgentPaths(f.home), logs = [], calls = [];
  fs.mkdirSync(paths.directory, { recursive: true, mode: 0o700 }); fs.writeFileSync(paths.config, "{}", { mode: 0o600 });
  fs.writeFileSync(paths.token, "stale-token", { mode: 0o600 });
  let connection;
  const server = new EventEmitter();
  server.listen = (file, ready) => { fs.writeFileSync(file, "mock socket", { mode: 0o600 }); queueMicrotask(ready); };
  await startSigningAgent({ paths, config: { roots: f.policy.roots, home: f.home, username: "fixture", pnpm: "/fixed/pnpm", environmentPath: "/usr/bin" }, log: (event) => logs.push(event),
    createServer(callback) { connection = callback; return server; }, run(command, args) {
      calls.push(args.at(-1)); return { status: 0, stdout: "", stderr: "" };
    } });
  const token = fs.readFileSync(paths.token, "utf8"); assert.match(token, /^[a-f0-9]{64}$/); assert.notEqual(token, "stale-token");
  assert.equal(fs.statSync(paths.token).mode & 0o777, 0o600); assert.equal(fs.statSync(paths.socket).mode & 0o777, 0o600);
  const bad = serverSocket(); connection(bad); bad.emit("data", Buffer.from(`${JSON.stringify(f.request)}\n`));
  assert.equal(bad.report.code, "MAC_SIGNING_AGENT_AUTH"); assert.equal(calls.length, 0);
  const first = serverSocket(), second = serverSocket(); connection(first); connection(second);
  first.emit("data", Buffer.from(`${JSON.stringify({ ...f.request, token })}\n`));
  second.emit("data", Buffer.from(`${JSON.stringify({ ...f.request, token, items: [{ path: f.artifact, identity: "Fixture" }] })}\n`));
  assert.equal(calls.length, 0);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls, [f.app, f.artifact]); assert.equal(first.report.ok, true); assert.equal(second.report.ok, true);
  assert.deepEqual(logs.filter((e) => /request\.(started|completed)/.test(e.event)).map((e) => e.event), ["request.started", "request.completed", "request.started", "request.completed"]);
  assert.doesNotMatch(JSON.stringify(logs), new RegExp(`${token}|stale-token|Developer ID|${f.entitlement}`));
});
test("agent rejects arbitrary requests before spawning & suppresses thrown updater diagnostics", { skip: WINDOWS_SKIP }, async (t) => {
  const f = fixture(t), paths = signingAgentPaths(f.home), logs = [];
  fs.mkdirSync(paths.directory, { recursive: true, mode: 0o700 }); fs.writeFileSync(paths.config, "{}", { mode: 0o600 });
  let connection, calls = 0;
  const server = new EventEmitter(); server.listen = (file, ready) => { fs.writeFileSync(file, "mock", { mode: 0o600 }); ready(); };
  await startSigningAgent({ paths, config: { roots: f.policy.roots, home: f.home, pnpm: "/fixed/pnpm", environmentPath: "/usr/bin" }, log: (e) => logs.push(e),
    createServer(callback) { connection = callback; return server; }, run() { calls++; throw new Error("FAKE-PRIVATE-KEY"); } });
  const token = fs.readFileSync(paths.token, "utf8");
  for (const bad of [{ schema: 1, type: "shell", command: "sh", token }, { ...f.request, token, allowedRoots: [f.home] }]) {
    const socket = serverSocket(); connection(socket); socket.emit("data", Buffer.from(`${JSON.stringify(bad)}\n`)); assert.equal(socket.report.ok, false);
  }
  assert.equal(calls, 0);
  const socket = serverSocket(); connection(socket);
  socket.emit("data", Buffer.from(`${JSON.stringify({ schema: 1, type: "updater", token, allowedRoots: f.request.allowedRoots, cwd: f.workspace, files: [f.artifact], timeoutMs: 5000 })}\n`));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(socket.report.ok, false); assert.equal(calls, 1); assert.doesNotMatch(JSON.stringify(socket.report) + JSON.stringify(logs), /FAKE-PRIVATE-KEY/);
});
test("codesign-argv jobs log unique ids plus exit status, never argv/output/secrets", { skip: WINDOWS_SKIP }, async (t) => {
  const f = fixture(t), paths = signingAgentPaths(f.home), logs = [], calls = [];
  fs.mkdirSync(paths.directory, { recursive: true, mode: 0o700 }); fs.writeFileSync(paths.config, "{}", { mode: 0o600 });
  let connection;
  const server = new EventEmitter(); server.listen = (file, ready) => { fs.writeFileSync(file, "mock", { mode: 0o600 }); ready(); };
  const result = { status: 36, stdout: "fixture-private-output\n", stderr: "fixture-private-error\n" };
  await startSigningAgent({ paths, config: { roots: f.policy.roots, home: f.home, environmentPath: "/usr/bin" }, log: (event) => logs.push(event),
    createServer(callback) { connection = callback; return server; }, run(command, args) { calls.push([command, args]); return result; } });
  const token = fs.readFileSync(paths.token, "utf8"), jobs = [];
  for (const file of [f.app, f.artifact]) {
    const socket = serverSocket(); connection(socket); jobs.push(socket);
    socket.emit("data", Buffer.from(`${JSON.stringify({ schema: 1, token, type: "codesign-argv", argv: ["--sign", "fixture-private-identity", file],
      targets: [file], allowedRoots: f.request.allowedRoots, cwd: f.workspace, timeoutMs: 5000 })}\n`));
  }
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(calls.length, 2); assert.ok(calls.every(([command]) => command === "/usr/bin/codesign"));
  for (const job of jobs) assert.deepEqual(job.report, { schema: 1, ok: true, results: [result] });
  const completed = logs.filter((event) => event.event === "request.completed");
  assert.equal(completed.length, 2); assert.notEqual(completed[0].id, completed[1].id);
  assert.ok(completed.every((event) => event.type === "codesign-argv" && event.status === 36));
  assert.doesNotMatch(JSON.stringify(logs), /fixture-private|--sign/); assert.ok(!JSON.stringify(logs).includes(token));
});
test("managed request log is private & rotates at bounded size", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t), file = path.join(f.home, "requests.jsonl");
  managedSigningLog(file, { event: "request.completed", id: "fixture", type: "codesign" });
  assert.equal(fs.statSync(file).mode & 0o777, 0o600);
  fs.appendFileSync(file, "x".repeat(5 * 1024 * 1024));
  managedSigningLog(file, { event: "service.started" });
  assert.ok(fs.existsSync(`${file}.1`)); assert.ok(fs.statSync(file).size < 1024);
});
