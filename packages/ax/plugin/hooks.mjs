// Optional hooks for a plugin. Only tools that push into a chat (Pulse, Legion)
// justify hooks; every other tool ships a skill alone.
//
// Format evidenced in pulse/plugins/claude/pulse/hooks/hooks.json and
// legion/hooks/hooks.json: `hooks/hooks.json` holding
//   { "description"?: string,
//     "hooks": { "<Event>": [ { "matcher"?: string,
//                               "hooks": [ { "type": "command", "command": string,
//                                            "timeout"?: number, "shell"?: string,
//                                            "commandWindows"?: string } ] } ] } }
// Claude Code anchors scripts with ${CLAUDE_PLUGIN_ROOT}; the agent-plugins /
// Codex projection uses ${PLUGIN_ROOT} (pulse/plugins/codex/pulse/hooks/hooks.json).
//
// validateHooks checks: structure, that every script a command names under a
// plugin-root placeholder exists inside the root, that no command embeds an
// absolute machine path, and that neither hooks.json nor the scripts it
// references carry secret-shaped text or a user-home path.

import { existsSync, lstatSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { containsSecretShapedString } from "../lib/redact.mjs";
import { resolvesOutsideRoot } from "../lib/path-safety.mjs";

export const HOOKS_FILE = "hooks/hooks.json";
export const KNOWN_HOOK_EVENTS = Object.freeze([
  "SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure",
  "Stop", "SubagentStart", "SubagentStop", "Notification", "PreCompact", "PermissionRequest",
]);
export const DEFAULT_PUSH_EVENTS = Object.freeze(["UserPromptSubmit", "PostToolUse", "Stop"]);

const ROOT_REF = /\$\{(?:CLAUDE_PLUGIN_ROOT|PLUGIN_ROOT)\}([\\/][^\s"'`;|&)]+)/g;
// A machine-specific absolute path: user-home directories, mounted volumes, or
// drive-letter paths. System install locations (/Applications, /usr/bin) are
// legitimate probe targets for a shim and are only flagged inside hooks.json
// commands, where an absolute program path is never portable.
const USER_HOME_PATH = /(?:\/Users\/[^/\s"']+|\/home\/[^/\s"']+|\/Volumes\/[^/\s"']+|[A-Za-z]:[\\/]+Users[\\/]+[^\\/\s"']+)/;
const ABSOLUTE_IN_COMMAND = /(?:^|[\s"'=(])(\/(?!dev\/null)[^\s"']+|[A-Za-z]:[\\/][^\s"']*|\\\\[^\s"']+)/;
const SECRET_KEY = /^(?:password|passwd|secret|secrets|token|api[_-]?key|access[_-]?key|private[_-]?key|credential|credentials)$/i;

function finding(ruleId, severity, claim, evidence, remediation) {
  return { ruleId, severity, claim, evidence, remediation };
}

function commandStrings(handler) {
  const out = [];
  for (const field of ["command", "commandWindows"]) if (typeof handler?.[field] === "string") out.push([field, handler[field]]);
  return out;
}

function scanSecrets(text, label, findings) {
  if (containsSecretShapedString(text)) {
    findings.push(finding("hooks-secret", "error", "hook file carries a secret-shaped string", label, "remove the credential; read it from the environment or the product's own store at run time"));
  }
}

function walkForSecretKeys(value, location, findings) {
  if (Array.isArray(value)) { value.forEach((item, index) => walkForSecretKeys(item, `${location}[${index}]`, findings)); return; }
  if (!value || typeof value !== "object") return;
  for (const [key, item] of Object.entries(value)) {
    if (SECRET_KEY.test(key) && typeof item === "string" && item && !/^\$\{?[A-Za-z_]/.test(item)) {
      findings.push(finding("hooks-secret", "error", "hooks.json holds a literal value under a secret-bearing key", `${location}.${key}`, "reference an environment variable instead of a literal"));
    }
    walkForSecretKeys(item, `${location}.${key}`, findings);
  }
}

export function validateHooks(pluginRoot) {
  const findings = [];
  const hooksPath = join(pluginRoot, ...HOOKS_FILE.split("/"));
  if (!existsSync(hooksPath)) return findings;
  let text;
  let manifest;
  try {
    text = readFileSync(hooksPath, "utf8");
    manifest = JSON.parse(text);
  } catch (error) {
    findings.push(finding("hooks-json-parse", "error", "hooks/hooks.json is not valid JSON", String(error?.message ?? error), "fix the JSON syntax error"));
    return findings;
  }
  if (!manifest || typeof manifest !== "object" || Array.isArray(manifest) || !manifest.hooks || typeof manifest.hooks !== "object" || Array.isArray(manifest.hooks)) {
    findings.push(finding("hooks-json-schema", "error", "hooks/hooks.json needs a top-level hooks object keyed by event name", HOOKS_FILE, "shape it as { \"hooks\": { \"<Event>\": [ { \"hooks\": [ { \"type\": \"command\", \"command\": ... } ] } ] } }"));
    return findings;
  }
  if (manifest.description !== undefined && typeof manifest.description !== "string") {
    findings.push(finding("hooks-json-schema", "error", "hooks.description must be a string", HOOKS_FILE, "make it a string or remove it"));
  }
  scanSecrets(text, HOOKS_FILE, findings);
  walkForSecretKeys(manifest, "hooks.json", findings);

  const scripts = new Map(); // relative path -> first location that references it
  let handlerCount = 0;
  for (const [event, groups] of Object.entries(manifest.hooks)) {
    if (!KNOWN_HOOK_EVENTS.includes(event)) {
      findings.push(finding("hooks-event", "info", "unknown hook event name (reported, not fatal)", `${HOOKS_FILE}: ${event}`, "check the host's event list; ignore if intentional forward-compat"));
    }
    if (!Array.isArray(groups)) {
      findings.push(finding("hooks-json-schema", "error", `hooks.${event} must be an array of matcher groups`, HOOKS_FILE, "wrap the handlers: [ { \"hooks\": [ ... ] } ]"));
      continue;
    }
    groups.forEach((group, groupIndex) => {
      const where = `hooks.${event}[${groupIndex}]`;
      if (!group || typeof group !== "object" || !Array.isArray(group.hooks) || group.hooks.length === 0) {
        findings.push(finding("hooks-json-schema", "error", `${where} needs a non-empty hooks array`, HOOKS_FILE, "list at least one handler"));
        return;
      }
      if (group.matcher !== undefined && typeof group.matcher !== "string") {
        findings.push(finding("hooks-json-schema", "error", `${where}.matcher must be a string`, HOOKS_FILE, "use a regex string or remove the matcher"));
      }
      group.hooks.forEach((handler, handlerIndex) => {
        const at = `${where}.hooks[${handlerIndex}]`;
        handlerCount += 1;
        if (!handler || typeof handler !== "object" || handler.type !== "command" || typeof handler.command !== "string" || !handler.command.trim()) {
          findings.push(finding("hooks-json-schema", "error", `${at} must be { type: "command", command: <non-empty string> }`, HOOKS_FILE, "only command hooks are scaffolded and validated here"));
          return;
        }
        if (handler.timeout !== undefined && !(typeof handler.timeout === "number" && handler.timeout > 0)) {
          findings.push(finding("hooks-json-schema", "error", `${at}.timeout must be a positive number of seconds`, HOOKS_FILE, "set a timeout so a stuck hook cannot hang the chat"));
        }
        for (const [field, command] of commandStrings(handler)) {
          const hasRootRef = new RegExp(ROOT_REF.source).test(command);
          const withoutRefs = command.replace(/\$\{(?:CLAUDE_PLUGIN_ROOT|PLUGIN_ROOT)\}/g, "PLUGINROOT");
          const absolute = ABSOLUTE_IN_COMMAND.exec(withoutRefs);
          if (absolute) {
            findings.push(finding("hooks-absolute-path", "error", "hook command embeds an absolute machine path", `${at}.${field}: ${absolute[1]}`, "anchor scripts at ${CLAUDE_PLUGIN_ROOT} (Claude) or ${PLUGIN_ROOT} (agent-plugins), or call a bare PATH binary"));
          }
          for (const match of command.matchAll(ROOT_REF)) {
            const relative = match[1].replace(/^[\\/]+/, "").replaceAll("\\", "/");
            if (!scripts.has(relative)) scripts.set(relative, `${at}.${field}`);
          }
          if (!hasRootRef && !absolute) {
            const program = command.trim().split(/\s+/)[0];
            findings.push(finding("hooks-path-binary", "info", "hook runs a PATH-resolved program that this check cannot verify is installed", `${at}.${field}: ${program}`, "document the install prerequisite and make the program exit 0 silently when the product is absent"));
          }
        }
      });
    });
  }
  if (handlerCount === 0) {
    findings.push(finding("hooks-json-schema", "error", "hooks/hooks.json declares no handlers", HOOKS_FILE, "remove the file or add at least one handler"));
  }

  for (const [relative, location] of scripts) {
    const absolute = join(pluginRoot, ...relative.split("/"));
    if (relative.split("/").includes("..") || resolvesOutsideRoot(relative, pluginRoot)) {
      findings.push(finding("hooks-script-containment", "error", "hook script escapes the plugin root", `${location} -> ${relative}`, "keep scripts, and every symlinked directory above them, inside the plugin root"));
      continue;
    }
    if (!existsSync(absolute) || !statSync(absolute).isFile()) {
      findings.push(finding("hooks-script-missing", "error", "hook references a script that does not exist in the plugin", `${location} -> ${relative}`, "ship the script under the plugin root or fix the path"));
      continue;
    }
    if (lstatSync(absolute).size > 262144) continue;
    let scriptText;
    try { scriptText = readFileSync(absolute, "utf8"); } catch { continue; }
    if (scriptText.includes("\u0000")) continue;
    scanSecrets(scriptText, relative, findings);
    const home = USER_HOME_PATH.exec(scriptText);
    if (home) {
      findings.push(finding("hooks-absolute-path", "error", "hook script embeds a user-specific machine path", `${relative}: ${home[0]}`, "resolve locations at run time from PATH or the environment"));
    }
  }
  return findings;
}

// Scaffold: a Claude-format hooks.json plus a shim script that stays silent
// and successful when the product is not installed (same contract as
// pulse/plugins/claude/pulse/hooks/run-pulse.sh).
export function buildHooksFiles(cliCommand, { hookArgs, events = DEFAULT_PUSH_EVENTS, description, timeout = 10 } = {}) {
  const bin = String(cliCommand ?? "").trim().split(/\s+/)[0];
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(bin)) throw new Error(`hooks need a bare program name for --cli, got ${JSON.stringify(cliCommand)}`);
  const args = String(hookArgs ?? "").trim();
  if (!args) throw new Error("hooks need --hook-args, the subcommand the hook runs (for example \"chat claude-hook\")");
  if (/[`$;&|<>\\"']/.test(args)) throw new Error("--hook-args must be plain words (no quotes, redirects or shell operators)");
  for (const event of events) if (!KNOWN_HOOK_EVENTS.includes(event)) throw new Error(`unknown hook event: ${event}`);
  const shim = `run-${bin}.sh`;
  const handler = { type: "command", shell: "bash", command: `sh "\${CLAUDE_PLUGIN_ROOT}/hooks/${shim}" ${args}`, timeout };
  const manifest = {
    description: description ?? `${bin}: hand waiting messages to this chat.`,
    hooks: Object.fromEntries(events.map((event) => [event, [{ hooks: [{ ...handler }] }]])),
  };
  const script = [
    "#!/bin/sh",
    `# Run \`${bin} ${args}\` with the hook's stdin. Silent and successful when ${bin} is absent,`,
    `# so a computer without ${bin} never sees a hook error.`,
    `if ! command -v ${bin} >/dev/null 2>&1; then`,
    "  exit 0",
    "fi",
    `exec ${bin} ${args}`,
    "",
  ].join("\n");
  return { manifest, shimName: shim, script };
}
