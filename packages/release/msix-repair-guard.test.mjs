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

test("after the first 24 h the guard keeps one attempt per 24 h window instead of giving up", () => {
  const { text } = msixRepairGuard();
  assert.doesNotMatch(text, /gaveUp/, "no terminal give-up state");
  assert.match(text, /\$DailyWindowHours = 24/);
  for (const needle of ["dailyMode", "windowStartEpoch", "windowAttempts"]) assert.ok(text.includes(needle), `retry.json records ${needle}`);
  assert.match(text, /\$delay = \$DailyWindowHours \* 60/, "daily mode schedules the next one-shot a full window out");
  assert.match(text, /if \(\$daily\) \{[^}]*\$next = \$nowEpoch \+ \(\$delay \* 60\)/s, "never a tight loop: the next trigger is always in the future");
  assert.doesNotMatch(text, /Register-GuardTask \$family \$null[^\n]*\n[^\n]*retry window/, "exhausting the window no longer clears the trigger");
});

test("the event 751 trigger is filtered to this package family in the script", () => {
  const { text } = msixRepairGuard();
  assert.match(text, /<ValueQueries><Value name="EventRecordID">Event\/System\/EventRecordID<\/Value><\/ValueQueries>/);
  assert.match(text, /-EventRecordID \$\(EventRecordID\)/, "the task passes the triggering record id");
  assert.match(text, /\[string\]\$EventRecordID = ''/);
  assert.match(text, /function Test-ForeignEvent/);
  assert.match(text, /EventID=751 and EventRecordID=/);
  assert.match(text, /\$family\.Substring\(0, \$family\.LastIndexOf\('_'\)\)/, "matches the family Name and publisher id");
  assert.match(text, /Test-ForeignEvent \$family \$EventRecordID\nif \(\$foreign\) \{ Log \('ignored: ' \+ \$foreign\); exit 0 \}/, "a foreign 751 exits before the mutex and leaves last-result.json alone");
  assert.match(text, /event .* unreadable, proceeding/, "an unreadable event fails open");
});

test("every receipt carries the guard script's own sha256 computed at run time", () => {
  const { text } = msixRepairGuard();
  assert.match(text, /\$ScriptHashes = Get-ScriptHashes/);
  assert.match(text, /ReadAllBytes\(\$PSCommandPath\)/, "hashes the script file itself, not an embedded constant");
  assert.match(text, /guardVersion = \$GuardVersion; guardSha256 = \$ScriptHashes\.guardSha256; guardSha256Lf = \$ScriptHashes\.guardSha256Lf;/, "in the one function that writes last-result.json and install-result.json");
  assert.doesNotMatch(text, /guardSha256 = '[0-9a-f]{64}'/);
});

test("only a started installer counts as a launched attempt; a failed launch is recorded, not counted", () => {
  const { text } = msixRepairGuard();
  assert.equal([...text.matchAll(/\$script:Launched = \$true/g)].length, 2, "set only immediately before Add-AppxPackage (feed and msix)");
  for (const call of ["Add-AppxPackage -AppInstallerFile $feedUri", "Add-AppxPackage -Path $tmpMsix"]) {
    const at = text.indexOf(`try { ${call}`);
    assert.ok(at > 0, call);
    assert.match(text.slice(Math.max(0, at - 60), at), /\$script:Launched = \$true\s*$/, `${call} is preceded by the launched flag`);
  }
  assert.match(text, /if \(\$script:Launched\) \{ \$lastLaunch = \$nowEpoch \}/, "lastLaunchEpoch moves only for a launched attempt");
  assert.match(text, /\$r\.attemptLaunched = \[bool\]\$script:Launched; \$r\.launchFailure = \[string\]\$script:LaunchFailure/);
  assert.match(text, /no installer started: /);
  assert.match(text, /\$AttemptPath/, "install mode still clears install-attempt.json (the app owns writing it)");
  assert.doesNotMatch(text, /Write-Json \$AttemptPath|WriteAllText\(\$AttemptPath/, "the guard never writes the app's attempt marker");
});
