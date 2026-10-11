import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { buildHooksFiles, validateHooks } from "./plugin/hooks.mjs";
import { validateRepoLayout } from "./plugin/validate.mjs";

const CLAUDE_COMMAND = 'sh "${CLAUDE_PLUGIN_ROOT}/hooks/run.sh" claude-hook';

function plugin() {
  return mkdtempSync(path.join(os.tmpdir(), "right-ax-hooks-"));
}

function writeHooks(root, hooks, extra = {}) {
  mkdirSync(path.join(root, "hooks"), { recursive: true });
  writeFileSync(path.join(root, "hooks", "hooks.json"), JSON.stringify({ hooks, ...extra }, null, 2));
}

function writeScript(root, name, text) {
  mkdirSync(path.join(root, "hooks"), { recursive: true });
  writeFileSync(path.join(root, "hooks", name), text);
}

function handler(command, extra = {}) {
  return [{ hooks: [{ type: "command", command, timeout: 10, ...extra }] }];
}

function errorRules(findings) {
  return findings.filter((item) => item.severity === "error").map((item) => item.ruleId);
}

test("no hooks/hooks.json means no hook findings", () => {
  assert.deepEqual(validateHooks(plugin()), []);
});

test("a Claude-style hook with an existing shim script validates clean", () => {
  const root = plugin();
  writeHooks(root, { UserPromptSubmit: handler(CLAUDE_COMMAND), Stop: handler(CLAUDE_COMMAND) });
  writeScript(root, "run.sh", "#!/bin/sh\ncommand -v tool >/dev/null 2>&1 || exit 0\nexec tool chat claude-hook\n");
  assert.deepEqual(validateHooks(root), []);
});

test("an agent-plugins/Codex-style hook with a Windows command and both scripts validates clean", () => {
  const root = plugin();
  const windows = 'powershell -NoProfile -File "${PLUGIN_ROOT}\\hooks\\run.ps1" codex-hook';
  writeHooks(root, { Stop: handler('sh "${PLUGIN_ROOT}/hooks/run.sh" codex-hook', { commandWindows: windows }) });
  writeScript(root, "run.sh", "#!/bin/sh\nexit 0\n");
  writeScript(root, "run.ps1", "exit 0\n");
  assert.deepEqual(validateHooks(root), []);
});

test("a referenced script that does not exist is an error", () => {
  const root = plugin();
  writeHooks(root, { Stop: handler(CLAUDE_COMMAND) });
  assert.deepEqual(errorRules(validateHooks(root)), ["hooks-script-missing"]);
});

test("a missing Windows script is an error too", () => {
  const root = plugin();
  writeHooks(root, { Stop: handler(CLAUDE_COMMAND, { commandWindows: 'powershell -File "${CLAUDE_PLUGIN_ROOT}\\hooks\\gone.ps1"' }) });
  writeScript(root, "run.sh", "exit 0\n");
  const findings = validateHooks(root);
  assert.ok(findings.some((item) => item.ruleId === "hooks-script-missing" && /gone\.ps1/.test(item.evidence)), JSON.stringify(findings));
});

test("a script path that climbs out of the plugin root is an error", () => {
  const root = plugin();
  writeHooks(root, { Stop: handler('sh "${CLAUDE_PLUGIN_ROOT}/../evil.sh"') });
  assert.ok(errorRules(validateHooks(root)).includes("hooks-script-containment"));
});

test("absolute machine paths in a hook command are errors", () => {
  for (const command of ["/Users/someone/bin/tool hook", "C:\\Tools\\tool.exe hook", "\\\\host\\share\\tool.exe hook", 'sh "/opt/tool/run.sh"']) {
    const root = plugin();
    writeHooks(root, { Stop: handler(command) });
    assert.ok(errorRules(validateHooks(root)).includes("hooks-absolute-path"), command);
  }
});

test("a user-home path inside a referenced script is an error", () => {
  const root = plugin();
  writeHooks(root, { Stop: handler(CLAUDE_COMMAND) });
  writeScript(root, "run.sh", "#!/bin/sh\nexec /Users/adrian/bin/tool\n");
  const findings = validateHooks(root);
  assert.ok(findings.some((item) => item.ruleId === "hooks-absolute-path" && /run\.sh/.test(item.evidence)), JSON.stringify(findings));
});

test("a standard install location probed by a shim script is allowed", () => {
  const root = plugin();
  writeHooks(root, { Stop: handler(CLAUDE_COMMAND) });
  writeScript(root, "run.sh", '#!/bin/sh\n[ -x /Applications/Tool.app/Contents/Helpers/tool ] && exec /Applications/Tool.app/Contents/Helpers/tool hook\nexit 0\n');
  assert.deepEqual(validateHooks(root), []);
});

test("a secret-shaped string in hooks.json is an error", () => {
  const root = plugin();
  writeHooks(root, { Stop: handler(`${CLAUDE_COMMAND} --token=abcdefghijkl12345`) });
  writeScript(root, "run.sh", "exit 0\n");
  assert.ok(errorRules(validateHooks(root)).includes("hooks-secret"));
});

test("a literal value under a secret-bearing key is an error, an env reference is not", () => {
  const literal = plugin();
  writeHooks(literal, { Stop: handler(CLAUDE_COMMAND) }, { apiKey: "hunter2hunter2" });
  writeScript(literal, "run.sh", "exit 0\n");
  assert.ok(errorRules(validateHooks(literal)).includes("hooks-secret"));
  const reference = plugin();
  writeHooks(reference, { Stop: handler(CLAUDE_COMMAND) }, { apiKey: "${TOOL_API_KEY}" });
  writeScript(reference, "run.sh", "exit 0\n");
  assert.deepEqual(errorRules(validateHooks(reference)), []);
});

test("a secret-shaped string in a referenced script is an error", () => {
  const root = plugin();
  writeHooks(root, { Stop: handler(CLAUDE_COMMAND) });
  writeScript(root, "run.sh", "#!/bin/sh\nexport API_KEY=abcdefgh12345678\nexec tool\n");
  assert.ok(errorRules(validateHooks(root)).includes("hooks-secret"));
});

test("a bare PATH program is info, not an error (the Legion shape)", () => {
  const root = plugin();
  writeHooks(root, { SessionStart: [{ matcher: "startup", hooks: [{ type: "command", command: "legion-hook", timeout: 10 }] }] });
  const findings = validateHooks(root);
  assert.deepEqual(errorRules(findings), []);
  assert.ok(findings.some((item) => item.ruleId === "hooks-path-binary" && item.severity === "info"));
});

test("structure errors: missing hooks object, bad groups, bad handlers, bad timeout, invalid JSON", () => {
  const root = plugin();
  mkdirSync(path.join(root, "hooks"));
  writeFileSync(path.join(root, "hooks", "hooks.json"), "{ not json");
  assert.deepEqual(errorRules(validateHooks(root)), ["hooks-json-parse"]);

  writeFileSync(path.join(root, "hooks", "hooks.json"), JSON.stringify({ description: "x" }));
  assert.deepEqual(errorRules(validateHooks(root)), ["hooks-json-schema"]);

  writeHooks(root, { Stop: { not: "an array" } });
  assert.ok(errorRules(validateHooks(root)).includes("hooks-json-schema"));

  writeHooks(root, { Stop: [{ hooks: [{ type: "prompt", command: "x" }] }] });
  assert.ok(errorRules(validateHooks(root)).includes("hooks-json-schema"));

  writeHooks(root, { Stop: handler("tool hook", { timeout: -1 }) });
  assert.ok(errorRules(validateHooks(root)).includes("hooks-json-schema"));

  writeHooks(root, {});
  assert.ok(errorRules(validateHooks(root)).includes("hooks-json-schema"));
});

test("an unknown event name is info only", () => {
  const root = plugin();
  writeHooks(root, { FutureEvent: handler("tool hook") });
  const findings = validateHooks(root);
  assert.deepEqual(errorRules(findings), []);
  assert.ok(findings.some((item) => item.ruleId === "hooks-event" && item.severity === "info"));
});

test("plugin validate includes hook findings", () => {
  const root = plugin();
  writeFileSync(path.join(root, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  writeHooks(root, { Stop: handler(CLAUDE_COMMAND) });
  const { valid, findings } = validateRepoLayout(root);
  assert.equal(valid, false);
  assert.ok(findings.some((item) => item.ruleId === "hooks-script-missing"));
});

test("buildHooksFiles produces the Pulse-shaped hooks.json and a silent shim", () => {
  const { manifest, shimName, script } = buildHooksFiles("pulse", { hookArgs: "chat claude-hook" });
  assert.equal(shimName, "run-pulse.sh");
  assert.deepEqual(Object.keys(manifest.hooks), ["UserPromptSubmit", "PostToolUse", "Stop"]);
  const only = manifest.hooks.Stop[0].hooks[0];
  assert.equal(only.type, "command");
  assert.equal(only.command, 'sh "${CLAUDE_PLUGIN_ROOT}/hooks/run-pulse.sh" chat claude-hook');
  assert.equal(only.timeout, 10);
  assert.match(script, /command -v pulse/);
  assert.match(script, /exit 0/);
  assert.match(script, /exec pulse chat claude-hook/);
});

test("buildHooksFiles refuses unsafe input", () => {
  assert.throws(() => buildHooksFiles("../tool", { hookArgs: "hook" }), /bare program name/);
  assert.throws(() => buildHooksFiles("tool", {}), /hook-args/);
  assert.throws(() => buildHooksFiles("tool", { hookArgs: "hook; rm -rf ." }), /plain words/);
  assert.throws(() => buildHooksFiles("tool", { hookArgs: "hook", events: ["NotAnEvent"] }), /unknown hook event/);
});
