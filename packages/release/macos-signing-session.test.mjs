import assert from "node:assert/strict";
import test from "node:test";
import * as fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { detectMacSigningSession, codesignAgentItems, runMacCodesignCommands, runMacGuiSigning, signMacUpdaterFiles } from "./macos-signing-session.mjs";
import { SigningAgentError } from "./rightkit-signing-agent.mjs";

const codesign = ["--force", "--options", "runtime", "--timestamp", "--sign", "Developer ID Application: Fixture (TEAM)", "/Volumes/D/claude/Fixture.app"];
const result = { status: 0, stdout: "", stderr: "" };
function fixture() {
  const calls = [];
  const run = (command, args, options) => {
    calls.push({ command, args, options });
    if (command === "/bin/launchctl") return { status: 0, stdout: "Aqua" };
    if (command === "security") return { status: 0, stdout: "FAKE-PRIVATE-KEY" };
    return { status: 0, stdout: "direct output", stderr: "" };
  };
  const requests = [];
  const request = (plan) => { requests.push(plan); return { schema: 1, ok: true, results: (plan.items ?? plan.files).map(() => ({ ...result })) }; };
  const io = { existsSync: (file) => file === "/Volumes/D/claude" };
  return { calls, requests, run, request, io, env: { SSH_CONNECTION: "mock", UNRELATED_SECRET: "never sent" }, allowedRoots: ["/Volumes/D/claude"], cwd: "/Volumes/D/claude" };
}
test("SSH signals select agent without probing keychain", () => {
  for (const field of ["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"]) assert.equal(detectMacSigningSession({ env: { [field]: "mock" }, run() { assert.fail(); } }).guiRunner, true);
});
test("non-Aqua routes to agent; Aqua probes only keychain metadata", () => {
  for (const name of ["Background", "System", "LoginWindow"]) assert.equal(detectMacSigningSession({ env: {}, run: () => ({ status: 0, stdout: name }) }).guiRunner, true);
  const f = fixture();
  assert.equal(detectMacSigningSession({ env: {}, run: f.run }).guiRunner, false);
  assert.equal(f.calls[1].command, "/usr/bin/security");
  assert.equal(f.calls[1].args[0], "show-keychain-info");
});
test("exit-36 detection & unknown/probe errors stay typed", () => {
  for (const denied of [{ status: 36 }, { status: 1, stderr: "errSecInteractionNotAllowed" }, { status: 1, stderr: "User interaction is not allowed" }]) {
    assert.equal(detectMacSigningSession({ env: {}, run: (cmd) => cmd === "/bin/launchctl" ? { status: 0, stdout: "Aqua" } : denied }).guiRunner, true);
  }
  assert.throws(() => detectMacSigningSession({ env: {}, run: () => ({ status: 1 }) }), { code: "MAC_SIGNING_SESSION_UNKNOWN" });
  assert.throws(() => detectMacSigningSession({ env: {}, run: (cmd) => cmd === "/bin/launchctl" ? { status: 0, stdout: "Aqua" } : { status: 44 } }), { code: "MAC_SIGNING_KEYCHAIN_PROBE_FAILED" });
});
test("codesign adapter accepts closed options & preserves supplied inside-out ordering", () => {
  const f = fixture();
  runMacCodesignCommands([codesign, [...codesign.slice(0, -1), "/Volumes/D/claude/Parent.app"]], f);
  assert.equal(f.calls.length, 0);
  assert.deepEqual(f.requests[0].items[0], { force: true, runtime: true, timestamp: true, identity: codesign[5], path: codesign.at(-1) });
  assert.equal(f.requests[0].items[1].path, "/Volumes/D/claude/Parent.app");
  assert.doesNotMatch(JSON.stringify(f.requests), /UNRELATED_SECRET|never sent|commands|\.command/);
  assert.equal(codesignAgentItems([["--sign", "CERT-SHA1", "--preserve-metadata=entitlements", "/absolute/path"]])[0].preserveEntitlements, true);
  for (const args of [["--sign", "-", "/app"], ["--sign", "identity", "relative"], ["--sign", "identity", "--deep", "/app"], ["--sign", "identity", "--entitlements", "relative", "/app"], ["--sign", "identity", "--sign", "other", "/app"]]) assert.throws(() => codesignAgentItems([args]), TypeError);
});
test("missing agent installs once & retries once", () => {
  const f = fixture(); let attempts = 0, installs = 0;
  f.request = (plan) => { if (++attempts === 1) throw new SigningAgentError("MAC_SIGNING_AGENT_UNAVAILABLE", "missing"); return { schema: 1, ok: true, results: [result] }; };
  f.install = () => { installs++; };
  assert.deepEqual(runMacCodesignCommands([codesign], f), [result]);
  assert.equal(attempts, 2); assert.equal(installs, 1);
  assert.deepEqual(f.calls[0].args, ["print", `gui/${process.getuid()}`]);
  f.request = () => { throw new SigningAgentError("MAC_SIGNING_AGENT_UNAVAILABLE", "missing"); };
  assert.throws(() => runMacCodesignCommands([codesign], f), { code: "MAC_SIGNING_AGENT_UNAVAILABLE" });
  assert.equal(installs, 2);
});
test("no GUI session preserves MAC_SIGNING_NO_GUI_SESSION before install", () => {
  const f = fixture();
  f.request = () => { throw new SigningAgentError("MAC_SIGNING_AGENT_UNAVAILABLE", "missing"); };
  f.run = () => ({ status: 113 }); f.install = () => assert.fail("no GUI must not install");
  assert.throws(() => runMacCodesignCommands([codesign], f), { code: "MAC_SIGNING_NO_GUI_SESSION" });
});
test("auth, timeout & job failures never trigger reinstallation", () => {
  for (const code of ["MAC_SIGNING_AGENT_AUTH", "MAC_SIGNING_AGENT_TIMEOUT", "MAC_SIGNING_AGENT_JOB_FAILED"]) {
    const f = fixture(); f.request = () => { throw new SigningAgentError(code, "failed"); }; f.install = () => assert.fail();
    assert.throws(() => runMacCodesignCommands([codesign], f), { code });
  }
});
test("Aqua codesign uses original cwd/env/argv & stops on failure", () => {
  const f = fixture(); f.env = { EXISTING: "retained" };
  assert.equal(runMacCodesignCommands([codesign], f)[0].stdout, "direct output");
  const call = f.calls.find((c) => c.command === "codesign");
  assert.equal(call.options.env, f.env); assert.equal(call.options.cwd, f.cwd); assert.deepEqual(call.args, codesign);
  assert.equal(f.requests.length, 0);
  let signs = 0;
  f.run = (cmd) => cmd === "codesign" ? (signs++, { status: 1 }) : { status: 0, stdout: "Aqua" };
  assert.equal(runMacCodesignCommands([codesign, codesign], f).length, 1); assert.equal(signs, 1);
});
test("SSH updater stays inside agent even with supplied key; keys never serialized", () => {
  const f = fixture(); f.env.TAURI_SIGNING_PRIVATE_KEY = "SECRET"; f.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "SECRET-PASSWORD";
  assert.deepEqual(signMacUpdaterFiles(["/Volumes/D/claude/updater.tar.gz"], f), [result]);
  assert.equal(f.calls.length, 0); assert.equal(f.requests[0].type, "updater");
  assert.doesNotMatch(JSON.stringify(f.requests), /SECRET|PASSWORD|TAURI|username/);
});
test("Aqua updater retains local keychain & supplied-key paths without returning tool output", () => {
  const f = fixture(); f.env = { PATH: "/usr/bin" };
  assert.deepEqual(signMacUpdaterFiles(["/Volumes/D/claude/updater.tar.gz"], f), [result]);
  assert.equal(f.calls.find((c) => c.command === "pnpm").options.env.TAURI_SIGNING_PRIVATE_KEY, "FAKE-PRIVATE-KEY");
  const g = fixture(); g.env = { TAURI_SIGNING_PRIVATE_KEY: "provided", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "provided-password" };
  signMacUpdaterFiles(["/Volumes/D/claude/updater.tar.gz"], g);
  assert.equal(g.calls.length, 1); assert.equal(g.calls[0].options.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD, "provided-password");
});
test("malformed agent reports, invalid kind & invalid deadlines rejected", () => {
  const f = fixture();
  for (const report of [{}, { schema: 1, ok: true, results: [] }, { schema: 1, ok: true, results: [{ status: 1, stdout: "", stderr: "" }] }]) {
    f.request = () => report;
    assert.throws(() => runMacCodesignCommands([codesign], f), { code: "MAC_SIGNING_AGENT_PROTOCOL" });
  }
  for (const timeoutMs of [0, Infinity, 180001, 1.5]) assert.throws(() => runMacGuiSigning({ ...f, kind: "codesign", commands: [codesign], timeoutMs }), TypeError);
  assert.throws(() => runMacGuiSigning({ ...f, kind: "shell" }), TypeError);
});
test("external temporary entitlements use private staging & are removed after response", (t) => {
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sign-stage-")));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const workspace = path.join(home, "workspace"), external = path.join(home, "external.plist");
  fs.mkdirSync(workspace); fs.writeFileSync(external, "<plist/>");
  let staged;
  const f = fixture();
  runMacCodesignCommands([["--sign", "identity", "--entitlements", external, path.join(workspace, "app")]], { ...f, io: fs, home, cwd: workspace, allowedRoots: [workspace], request(plan) {
    staged = plan.items[0].entitlements;
    assert.equal(fs.readFileSync(staged, "utf8"), "<plist/>");
    assert.equal(fs.statSync(staged).mode & 0o777, 0o600);
    assert.equal(fs.statSync(path.dirname(staged)).mode & 0o777, 0o700);
    assert.deepEqual(plan.allowedRoots, [workspace, path.dirname(staged)]);
    return { schema: 1, ok: true, results: [result] };
  } });
  assert.ok(!fs.existsSync(staged));
});
test("transport source contains no Terminal launch, command scripts, unlocking, or password tooling", () => {
  for (const name of ["macos-signing-session.mjs", "rightkit-signing-agent.mjs", "signing-agent-install.mjs", "signing-agent-client.mjs"]) {
    const source = fs.readFileSync(new URL(name, import.meta.url), "utf8");
    assert.doesNotMatch(source, /\.command|\/usr\/bin\/open|Terminal|osascript|unlock-keychain|set-key-partition-list/);
  }
});
