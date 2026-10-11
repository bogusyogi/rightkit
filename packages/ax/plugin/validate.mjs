// Validates a repo's agent-plugins.org layout: plugin.json / mcp.json against
// the pinned 1.0.0 schemas (plugin/schemas/), plus path-safety rules the
// schemas cannot express (plugin-root containment, ${PLUGIN_ROOT}/${PLUGIN_DATA}
// expansion). Per the contract: unknown top-level fields are reported and
// ignored (a finding, not a failure); every other schema violation is fatal.

import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { validateAgainstSchema } from "../lib/json-schema-lite.mjs";
import { describePathHazard, resolvesOutsideRoot } from "../lib/path-safety.mjs";
import { validateHooks } from "./hooks.mjs";
import { checkSkillDescription } from "./skill-gate.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const SCHEMA_DIR = join(HERE, "schemas");
const PLACEHOLDER_PATTERN = /\$\{PLUGIN_(ROOT|DATA)\}/g;
const HAS_PLACEHOLDER = /\$\{PLUGIN_(ROOT|DATA)\}/;

export function loadPinnedSchemas() {
  const pins = JSON.parse(readFileSync(join(SCHEMA_DIR, "pins.json"), "utf8"));
  const load = (name) => {
    const path = join(SCHEMA_DIR, name);
    const raw = readFileSync(path, "utf8");
    const digest = `sha256:${createHash("sha256").update(raw).digest("hex")}`;
    const pinned = pins.files[name];
    if (digest !== pinned) {
      throw new Error(`plugin/schemas/${name} does not match pinned digest (invariant I4): expected ${pinned}, got ${digest}`);
    }
    return JSON.parse(raw);
  };
  return { pluginSchema: load("plugin.schema.json"), mcpSchema: load("mcp.schema.json"), pins };
}

function finding(ruleId, severity, claim, evidence, remediation) {
  return { ruleId, severity, claim, evidence, remediation };
}

function classifySchemaErrors(ruleId, errors) {
  const findings = [];
  for (const message of errors) {
    const isTopLevelUnknown = /^\$: unexpected property /.test(message);
    findings.push(finding(
      ruleId,
      isTopLevelUnknown ? "info" : "error",
      isTopLevelUnknown ? "unknown top-level field (reported, not fatal)" : "schema violation",
      message,
      isTopLevelUnknown ? "remove or rename the field, or ignore if intentional forward-compat" : "fix the manifest to satisfy the pinned agent-plugins.org 1.0.0 schema",
    ));
  }
  return findings;
}

function expandPlaceholders(value, pluginRoot) {
  return String(value).replace(PLACEHOLDER_PATTERN, (_, kind) => (kind === "ROOT" ? pluginRoot : join(pluginRoot, ".data")));
}

// F8 fix: every path-bearing field is examined — command, every arg, cwd,
// and every env value — not just args/env entries that happen to contain a
// ${PLUGIN_ROOT}/${PLUGIN_DATA} placeholder (the original gap: command was
// never checked at all, and args/env without a placeholder were skipped
// entirely, so a plain parent-relative, absolute, UNC, or drive-relative
// path sailed through unexamined).
//
// F8 (round 3): a bare token (e.g. "node", "python3", or a same-directory
// filename with no "./" prefix like "file-link.mjs") is NEVER lexically
// hazardous — describePathHazard() already returns null for it, since it has
// no separators to be absolute/UNC/drive-relative/parent-traversing. But a
// bare token still resolves to a real filesystem entry relative to
// pluginRoot, and that entry can itself be a symlink pointing outside the
// root — the earlier fix special-cased bare tokens and skipped
// resolvesOutsideRoot() for them entirely, which is exactly the gap a bare
// symlinked filename walks through. Every candidate — bare or not — is now
// routed through the same resolvesOutsideRoot() canonicalization; a bare
// token that genuinely has nothing on disk at pluginRoot/<token> (e.g. the
// PATH-resolved "node") degrades gracefully there (see path-safety.mjs) and
// is still never flagged.
function collectPathCandidates(server) {
  const candidates = [];
  if (typeof server.command === "string") candidates.push(["command", server.command]);
  if (server.cwd) candidates.push(["cwd", server.cwd]);
  for (const [index, arg] of (server.args ?? []).entries()) if (typeof arg === "string") candidates.push([`args[${index}]`, arg]);
  for (const [key, value] of Object.entries(server.env ?? {})) if (typeof value === "string") candidates.push([`env.${key}`, value]);
  return candidates;
}

function validateMcpPathSafety(mcpManifest, pluginRoot) {
  const findings = [];
  for (const [serverName, server] of Object.entries(mcpManifest.mcpServers ?? {})) {
    for (const [field, value] of collectPathCandidates(server)) {
      const hasPlaceholder = HAS_PLACEHOLDER.test(value);
      const expanded = hasPlaceholder ? expandPlaceholders(value, pluginRoot) : value;
      // Lexical hazard shapes (absolute/UNC/drive-relative/'..') get a
      // precise, actionable message first. A value with none of those
      // shapes (including every ${PLACEHOLDER} value, which never itself
      // looks hazardous before expansion) still goes through the F8
      // resolvesOutsideRoot() check below: a plain-looking relative
      // reference can pass THROUGH a directory symlink that resolves
      // outside pluginRoot, which no lexical shape check can see.
      const hazard = hasPlaceholder ? null : describePathHazard(value);
      if (hazard) {
        findings.push(finding(
          "plugin-path-containment",
          "error",
          "mcp.json path escapes the plugin root",
          `mcpServers.${serverName}.${field} = ${JSON.stringify(value)} (${hazard}, not anchored to \${PLUGIN_ROOT}/\${PLUGIN_DATA})`,
          "reference paths via ${PLUGIN_ROOT}/${PLUGIN_DATA}, or a plain relative path with no '..' segments, instead of an absolute, UNC, drive-relative, or parent-escaping path",
        ));
        continue;
      }
      if (resolvesOutsideRoot(expanded, pluginRoot)) {
        findings.push(finding(
          "plugin-path-containment",
          "error",
          "mcp.json path escapes the plugin root",
          `mcpServers.${serverName}.${field} = ${JSON.stringify(value)} (resolves outside the plugin root, directly or via a symlinked directory)`,
          "keep this path, and every symlinked directory component in it, inside the plugin root",
        ));
      }
    }
  }
  return findings;
}

function listSkillDirs(repoPath) {
  const skillsDir = join(repoPath, "skills");
  if (!existsSync(skillsDir) || !statSync(skillsDir).isDirectory()) return [];
  // Directories starting with "." or "_" (for example _shared, .audit) are the
  // conventional home of shared assets, not skills.
  return readdirSync(skillsDir).filter((name) => !/^[._]/.test(name) && statSync(join(skillsDir, name)).isDirectory());
}

const NAME_PATTERN = /^(?!.*(--|\.\.))[a-z0-9]([a-z0-9.-]*[a-z0-9])?$/;
const HOST_MANIFESTS = [
  ["claude", ".claude-plugin/plugin.json"],
  ["codex", ".codex-plugin/plugin.json"],
];

function readJsonFile(path) {
  try { return { value: JSON.parse(readFileSync(path, "utf8")) }; } catch (error) { return { error: String(error?.message ?? error) }; }
}

// Which layout a plugin root uses: the portable agent-plugins.org layout
// (plugin.json at the root), or a host-native one (only
// .claude-plugin/plugin.json, as in pulse/plugins/claude/pulse, or only
// .codex-plugin/plugin.json, as in pulse/plugins/codex/pulse).
export function detectLayout(repoPath) {
  if (existsSync(join(repoPath, "plugin.json"))) return "portable";
  for (const [host, rel] of HOST_MANIFESTS) if (existsSync(join(repoPath, ...rel.split("/")))) return `${host}-native`;
  return "none";
}

// Host manifests are looser than the portable schema (Claude's `repository` is a
// string, Codex adds `interface`), so they get the checks every host shares:
// parseable, an object, a valid name, string version/description.
function validateHostManifest(repoPath, host, rel, portableName) {
  const findings = [];
  const parsed = readJsonFile(join(repoPath, ...rel.split("/")));
  if (parsed.error) return [finding("plugin-host-manifest", "error", `${rel} is not valid JSON`, parsed.error, "fix the JSON syntax error")];
  const manifest = parsed.value;
  if (!manifest || typeof manifest !== "object" || Array.isArray(manifest)) return [finding("plugin-host-manifest", "error", `${rel} must be a JSON object`, rel, "write a manifest object")];
  if (typeof manifest.name !== "string" || !NAME_PATTERN.test(manifest.name) || manifest.name.length > 64) {
    findings.push(finding("plugin-host-manifest", "error", `${rel} needs a lowercase kebab-case name of at most 64 characters`, JSON.stringify(manifest.name), "set name to match the plugin directory/skill naming"));
  } else if (portableName && manifest.name !== portableName) {
    findings.push(finding("plugin-host-name-mismatch", "error", `${rel} name differs from plugin.json`, `${manifest.name} vs ${portableName}`, "keep one plugin name across host manifests"));
  }
  for (const field of ["version", "description"]) {
    if (manifest[field] !== undefined && (typeof manifest[field] !== "string" || !manifest[field].trim())) {
      findings.push(finding("plugin-host-manifest", "error", `${rel} ${field} must be a non-empty string`, field, `set ${field} to a string or remove it`));
    }
  }
  return findings;
}

export function validateRepoLayout(repoPath, options = {}) {
  const descriptionGate = options.descriptionGate !== false;
  const findings = [];
  const { pluginSchema, mcpSchema } = loadPinnedSchemas();
  const layout = detectLayout(repoPath);
  let portableName = null;

  const pluginJsonPath = join(repoPath, "plugin.json");
  if (layout === "none") {
    findings.push(finding("plugin-json-present", "error", "plugin.json is required for agent-plugins.org packaging", pluginJsonPath, "run `right-ax plugin init <repo>` to scaffold plugin.json"));
  } else if (layout === "portable") {
    let manifest;
    try { manifest = JSON.parse(readFileSync(pluginJsonPath, "utf8")); } catch (error) {
      findings.push(finding("plugin-json-parse", "error", "plugin.json is not valid JSON", String(error?.message ?? error), "fix the JSON syntax error"));
      manifest = null;
    }
    if (manifest) {
      const { errors } = validateAgainstSchema(manifest, pluginSchema);
      findings.push(...classifySchemaErrors("plugin-json-schema", errors));
      if (typeof manifest.name === "string") portableName = manifest.name;
    }
  }
  if (layout !== "none") {
    for (const [host, rel] of HOST_MANIFESTS) {
      if (layout === `${host}-native` || (layout === "portable" && existsSync(join(repoPath, ...rel.split("/"))))) {
        findings.push(...validateHostManifest(repoPath, host, rel, portableName));
      }
    }
  }

  const skillDirs = listSkillDirs(repoPath);
  if (skillDirs.length === 0) {
    findings.push(finding("plugin-skills-present", "info", "no skills/<name>/SKILL.md discovered", join(repoPath, "skills"), "add at least one skills/<name>/SKILL.md if this plugin exposes a skill"));
  }
  for (const name of skillDirs) {
    const skillPath = join(repoPath, "skills", name, "SKILL.md");
    if (!existsSync(skillPath)) {
      findings.push(finding("plugin-skill-file", "error", `skills/${name}/ has no SKILL.md`, skillPath, `add skills/${name}/SKILL.md`));
    } else if (descriptionGate) {
      findings.push(...checkSkillDescription(readFileSync(skillPath, "utf8"), { skillDirName: name, skillPath: `skills/${name}/SKILL.md` }));
    }
  }

  findings.push(...validateHooks(repoPath));

  const mcpJsonPath = join(repoPath, "mcp.json");
  if (existsSync(mcpJsonPath)) {
    let mcpManifest;
    try { mcpManifest = JSON.parse(readFileSync(mcpJsonPath, "utf8")); } catch (error) {
      findings.push(finding("mcp-json-parse", "error", "mcp.json is not valid JSON", String(error?.message ?? error), "fix the JSON syntax error"));
      mcpManifest = null;
    }
    if (mcpManifest) {
      const { errors } = validateAgainstSchema(mcpManifest, mcpSchema);
      findings.push(...classifySchemaErrors("mcp-json-schema", errors));
      findings.push(...validateMcpPathSafety(mcpManifest, repoPath));
    }
  }

  const fatal = findings.some((item) => item.severity === "error");
  return { valid: !fatal, layout, findings };
}
