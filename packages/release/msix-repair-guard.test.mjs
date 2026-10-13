import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { MSIX_REPAIR_GUARD_PATH, msixRepairGuard, msixRepairGuardSha256 } from "./msix-repair-guard.mjs";

test("msixRepairGuard returns the script file and the sha256 of its bytes", () => {
  const guard = msixRepairGuard();
  const bytes = readFileSync(MSIX_REPAIR_GUARD_PATH);
  assert.equal(guard.text, bytes.toString("utf8").replace(/\r\n/g, "\n"));
  assert.match(guard.sha256, /^[0-9a-f]{64}$/);
  assert.equal(guard.sha256, msixRepairGuardSha256(guard.text));
  assert.equal(guard.sha256, msixRepairGuardSha256(guard.text.replace(/\n/g, "\r\n")), "line endings do not change the hash");
  assert.ok(Number.isInteger(guard.guardVersion) && guard.guardVersion >= 2);
});

test("the script is ASCII only, BOM free and PowerShell 5.1 safe in the ways the guard relies on", () => {
  const { text } = msixRepairGuard();
  assert.doesNotMatch(text, /[^\x00-\x7F]/, "ASCII only: Windows PowerShell 5.1 reads a BOM-less file as ANSI");
  assert.ok(!text.startsWith("﻿"));
  assert.match(text, /\[Threading\.Mutex\]::new\(/, "New-Object Threading.Mutex takes the 4-argument form and throws");
  assert.doesNotMatch(text, /New-Object\s+(System\.)?Threading\.Mutex/);
});

test("the script keeps its safety contract", () => {
  const { text } = msixRepairGuard();
  for (const needle of [
    "Test-AllowedUri",          // https, or http to loopback only
    "Test-Identity",            // feed / msix / installed identity must match the recorded Name and Publisher
    "last-result.json",         // machine-readable result next to repair.log
    "retry.json",               // retry state
    "<TimeTrigger>",            // one-shot retry trigger
    "-AppInstallerFile",
    "Add-AppxPackage -Path",
  ]) assert.ok(text.includes(needle), `script mentions ${needle}`);
  assert.deepEqual([...text.matchAll(/^\$RetryDelaysMinutes = @\(([^)]*)\)/gm)].map((m) => m[1].replace(/\s/g, "")), ["2,5,15,60"]);
  assert.match(text, /\$RetryWindowHours = 24/);
  assert.doesNotMatch(text, /Remove-Item[^\n]*-Recurse[^\n]*\$env:LOCALAPPDATA/, "never removes the app data parent");
});
