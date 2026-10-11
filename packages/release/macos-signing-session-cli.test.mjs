import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";

// Not run on Windows: the macOS signing session runs on macOS.
const WINDOWS_SKIP = process.platform === "win32" ? "the macOS signing session runs on macOS" : false;

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
function fixture(t) {
  const root = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rightkit-sign-cli-test-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const callsPath = path.join(root, "calls.json");
  const preload = path.join(root, "mock.mjs");
  writeFileSync(preload, `
${readFileSync(new URL("./background-process.mjs", import.meta.url), "utf8")}
import * as fs from 'node:fs';
import os from 'node:os';
import { executeSigningRequest } from '${fileURLToPath(new URL('./rightkit-signing-agent.mjs', import.meta.url))}';
Object.defineProperty(process, 'platform', { value: 'darwin' });
const calls = [];
let gui = false;
childProcess.spawnSync = (command, args, options) => {
  calls.push({ command, args, gui, hasKey: Boolean(options?.env?.TAURI_SIGNING_PRIVATE_KEY) });
  fs.writeFileSync(${JSON.stringify(callsPath)}, JSON.stringify(calls));
  if (command === '/bin/launchctl') return { status: 0, stdout: 'Aqua' };
  if (command === '/usr/bin/stat') return { status: 0, stdout: os.userInfo().username };
  if (command === process.execPath && args[0].endsWith('signing-agent-client.mjs')) {
    const { request } = JSON.parse(options.input);
    gui = true;
    try {
      const report = executeSigningRequest(request, { home: os.userInfo().homedir, username: os.userInfo().username, pnpm: 'pnpm', environmentPath: '/usr/bin' });
      return { status: 0, stdout: JSON.stringify(report) };
    } finally { gui = false; }
  }
  if (command.endsWith('security') && args[0] === 'find-generic-password') return { status: 0, stdout: 'FAKE-CLI-SECRET', stderr: '' };
  if (command.endsWith('codesign') && args[0] === '--verify') return { status: 0, stderr: 'valid on disk\\nsatisfies its designated requirement' };
  if (command.endsWith('codesign') && args[0] === '-dvv') return { status: 0, stderr: 'Authority=Developer ID Application: Fixture (TEAM)\\nTeamIdentifier=TEAM\\nTimestamp=fixture' };
  if (command === 'pnpm') {
    fs.writeFileSync(args.at(-1) + '.sig', 'fixture signature');
    return { status: 0, stdout: 'FAKE-CLI-SECRET', stderr: 'FAKE-CLI-SECRET' };
  }
  return { status: 0, stdout: '', stderr: '' };
};
`);
  const app = path.join(root, "Fixture.app");
  mkdirSync(path.join(app, "Contents"), { recursive: true });
  writeFileSync(path.join(app, "Contents/Info.plist"), "fixture");
  const artifact = path.join(root, "Fixture.app.tar.gz");
  writeFileSync(artifact, "fixture artifact");
  const invoke = (helper, args, ssh = true) => {
    const env = { ...process.env, APPLE_DEVELOPER_ID: "Developer ID Application: Fixture (TEAM)", TAURI_SIGNING_PRIVATE_KEY: "", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" };
    for (const name of ["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"]) delete env[name];
    if (ssh) env.SSH_CONNECTION = "mock ssh";
    const result = spawnSync(process.execPath, ["--import", preload, path.join(packageRoot, helper), ...args], { cwd: root, env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stdout + result.stderr, /FAKE-CLI-SECRET/);
    return JSON.parse(readFileSync(callsPath, "utf8"));
  };
  return { root, app, artifact, invoke };
}

test("release CLI routes timestamped Developer ID signing through agent, verifies from SSH & writes receipt", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t);
  const receipt = path.join(f.root, "receipt.json");
  const calls = f.invoke("sign-macos.mjs", ["--receipt", receipt, f.app]);
  const sign = calls.find((c) => c.args.includes("--sign"));
  assert.equal(sign.command, "/usr/bin/codesign");
  assert.equal(sign.gui, true);
  assert.ok(sign.args.includes("--timestamp"));
  for (const call of calls.filter((c) => c.args.includes("--verify") || c.args.includes("-dvv"))) assert.equal(call.gui, false);
  assert.equal(JSON.parse(readFileSync(receipt, "utf8")).files[0].timestampPresent, true);
});

test("release CLI in Aqua keeps codesign direct", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t);
  const calls = f.invoke("sign-macos.mjs", ["--receipt", path.join(f.root, "receipt.json"), f.app], false);
  assert.equal(calls.find((c) => c.args.includes("--sign")).command, "codesign");
  assert.ok(!calls.some((c) => c.command === "/usr/bin/open"));
});

for (const helper of ["sign-updater.mjs", "create-mac-updater.mjs"]) {
  test(`${helper} keeps SSH updater key read & signing inside agent`, { skip: WINDOWS_SKIP }, (t) => {
    const f = fixture(t);
    const args = helper === "sign-updater.mjs" ? ["--file", f.artifact] : ["--app", f.app, "--output", f.artifact, "--cwd", f.root];
    const calls = f.invoke(helper, args);
    const keyRead = calls.find((c) => c.args.includes("find-generic-password"));
    const signer = calls.find((c) => c.command === "pnpm");
    assert.equal(keyRead.gui, true);
    assert.equal(signer.gui, true);
    assert.equal(signer.hasKey, true);
    assert.equal(readFileSync(`${f.artifact}.sig`, "utf8"), "fixture signature");
    const tar = calls.find((c) => c.command === "tar");
    if (helper === "create-mac-updater.mjs") {
      assert.equal(tar.gui, false);
      assert.ok(calls.indexOf(tar) < calls.indexOf(signer));
    }
  });
}
