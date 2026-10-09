import assert from "node:assert/strict";
import { existsSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import test from "node:test";
import { macosCodesignEnvironment } from "./codesign-environment.mjs";
import { automationDenied } from "./osascript-guard.mjs";

test("Automation guard names caller with typed error without running AppleScript", () => {
  const error = automationDenied({ parentPid: 42, run(command, args, options) {
    assert.equal(command, "/bin/ps"); assert.deepEqual(args, ["-p", "42", "-o", "command="]);
    assert.equal(options.timeout, 1000); return { status: 0, stdout: "bash /tmp/bundle_dmg.sh\n" };
  } });
  assert.equal(error.code, "MAC_AUTOMATION_DENIED"); assert.equal(error.caller, "bash /tmp/bundle_dmg.sh");
});

test("legacy bundle_dmg.sh is denied before absolute osascript; Tauri skip flag & other bash pass", { skip: process.platform === "win32" }, (t) => {
  const managed = mkdtempSync(path.join(realpathSync(os.tmpdir()), "rk-bash-guard-"));
  t.after(() => rmSync(managed, { recursive: true, force: true }));
  const script = path.join(managed, "bundle_dmg.sh"), marker = path.join(managed, "executed");
  writeFileSync(script, '#!/usr/bin/env bash\nprintf "%s" "$1" > "$2"\n', { mode: 0o700 });
  const env = macosCodesignEnvironment({ ...process.env, RIGHTKIT_MANAGED_ROOT: managed }, { platform: "darwin", detect: () => ({ guiRunner: false }) });
  for (const [cmd, args] of [["bash", [script, "unsafe", marker]], [script, ["unsafe", marker]]]) {
    const denied = spawnSync(cmd, args, { env, encoding: "utf8", timeout: 3000 });
    assert.equal(denied.status, 125, denied.stderr); assert.equal(JSON.parse(denied.stderr).caller, script);
    assert.equal(existsSync(marker), false);
  }
  const skipped = spawnSync(script, ["--skip-jenkins", marker], { env, encoding: "utf8", timeout: 3000 });
  assert.equal(skipped.status, 0, skipped.stderr); assert.equal(existsSync(marker), true);
  assert.equal(spawnSync("bash", ["-c", "exit 23"], { env }).status, 23);
});

test("PATH osascript fails fast for SSH & GUI with structured denial", { skip: process.platform === "win32" }, (t) => {
  const managed = mkdtempSync(path.join(realpathSync(os.tmpdir()), "rk-automation-"));
  t.after(() => rmSync(managed, { recursive: true, force: true }));
  for (const ssh of ["", "fixture"]) {
    const env = macosCodesignEnvironment({ ...process.env, RIGHTKIT_MANAGED_ROOT: managed, SSH_CONNECTION: ssh }, {
      platform: "darwin", detect: () => ({ guiRunner: !!ssh }),
    });
    const result = spawnSync("osascript", ["-e", 'tell application "Finder" to activate'], { env, encoding: "utf8", timeout: 3000 });
    assert.equal(result.status, 125, result.stderr);
    const receipt = JSON.parse(result.stderr);
    assert.equal(receipt.code, "MAC_AUTOMATION_DENIED"); assert.ok(receipt.caller); assert.ok(receipt.parentPid > 0);
    assert.equal(result.stdout, ""); assert.equal(result.error, undefined);
  }
});
