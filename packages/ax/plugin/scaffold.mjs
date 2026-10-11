// Generates plugin.json / skills/<name>/SKILL.md / mcp.json from a repo's own
// package.json metadata. Never invents a capability the repo doesn't already
// have a working handler for (§5.1 invariant / static rule 12): mcp.json is
// only written when the caller supplies a real serve command, and SKILL.md
// only lists files that exist in the repo at scaffold time.
//
// Two shapes:
//   * legacy (no `cli`): plugin.json + skills/<name>/SKILL.md, plus mcp.json
//     when a serve command is supplied.
//   * CLI-skill (`cli` set, the suite default): the plugin's core is ONE skill
//     teaching an agent the tool's command line. No mcp.json unless
//     `mcpCommand` is also given. Writes plugin.json plus the two host
//     manifests evidenced in pulse (.claude-plugin/plugin.json,
//     .codex-plugin/plugin.json). `hookArgs` adds hooks/hooks.json and its
//     shim, for tools that push into a chat. The skill description must pass
//     the description gate (plugin/skill-gate.mjs) or scaffolding throws.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { buildHooksFiles, HOOKS_FILE } from "./hooks.mjs";
import { checkSkillDescription } from "./skill-gate.mjs";

function sanitizeName(rawName) {
  const lowered = String(rawName ?? "plugin").toLowerCase().replace(/^@[^/]+\//, "").replace(/[^a-z0-9.-]+/g, "-");
  const trimmed = lowered.replace(/^[.-]+/, "").replace(/[.-]+$/, "").replace(/(--|\.\.)+/g, "-").slice(0, 64) || "plugin";
  return trimmed;
}

function readPackageJson(repoPath) {
  const path = join(repoPath, "package.json");
  if (!existsSync(path)) return {};
  try { return JSON.parse(readFileSync(path, "utf8")); } catch { return {}; }
}

export function buildPluginManifest(repoPath, options = {}) {
  const pkg = readPackageJson(repoPath);
  const manifest = {
    $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
    name: sanitizeName(options.name ?? pkg.name ?? "plugin"),
  };
  if (options.version ?? pkg.version) manifest.version = options.version ?? pkg.version;
  if (options.description ?? pkg.description) manifest.description = options.description ?? pkg.description;
  if (pkg.author) manifest.author = typeof pkg.author === "string" ? { name: pkg.author } : pkg.author;
  if (options.homepage ?? pkg.homepage) manifest.homepage = options.homepage ?? pkg.homepage;
  if (pkg.repository) manifest.repository = typeof pkg.repository === "string" ? { type: "git", url: pkg.repository } : pkg.repository;
  if (options.license ?? pkg.license) manifest.license = options.license ?? pkg.license;
  if (Array.isArray(pkg.keywords) && pkg.keywords.length) manifest.keywords = pkg.keywords;
  return manifest;
}

export function buildSkillMarkdown(repoPath, pluginManifest, options = {}) {
  const skillName = options.skillName ?? pluginManifest.name;
  const knownFiles = (options.referencedFiles ?? []).filter((path) => existsSync(join(repoPath, path)));
  const description = pluginManifest.description ?? `Agent-plugins.org packaging for ${pluginManifest.name}.`;
  const lines = [
    "---",
    `name: ${skillName}`,
    `description: ${description}`,
    "---",
    "",
    `# ${skillName}`,
    "",
    description,
    "",
  ];
  if (knownFiles.length) {
    lines.push("## Files shipped with this plugin", "");
    for (const path of knownFiles) lines.push(`- \`${path}\``);
    lines.push("");
  }
  return lines.join("\n");
}

export function buildMcpManifest(mcpCommand, options = {}) {
  if (!mcpCommand) return null;
  const [command, ...args] = mcpCommand.split(/\s+/).filter(Boolean);
  const server = { type: "stdio", command };
  if (args.length) server.args = args;
  if (options.env) server.env = options.env;
  if (options.cwd) server.cwd = options.cwd;
  return {
    $schema: "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
    mcpServers: { [options.serverName ?? "default"]: server },
  };
}

function cliBinary(cli) {
  const bin = String(cli ?? "").trim().split(/\s+/)[0];
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(bin)) throw new Error(`--cli needs a bare program name (for example "scraperight"), got ${JSON.stringify(cli)}`);
  return bin;
}

export function buildCliSkillMarkdown(cli, skillName, description, commands = []) {
  const bin = cliBinary(cli);
  const lines = [
    "---",
    `name: ${skillName}`,
    `description: ${description}`,
    "---",
    "",
    `# ${skillName}`,
    "",
    `Drive \`${bin}\` from the shell with the Bash tool. Read results from stdout and treat a non-zero exit as failure.`,
    "",
    "## Before you run it",
    "",
    `1. Check it is installed: \`command -v ${bin}\` (Windows: \`where ${bin}\`). If it is missing, tell the user the app is not installed; do not try to install it.`,
    `2. Learn the interface from the binary, not from memory: \`${bin} --help\`, then \`${bin} <command> --help\` for the command you need.`,
    "",
  ];
  const names = commands.map((name) => String(name).trim()).filter(Boolean);
  if (names.length) {
    lines.push("## Commands", "");
    for (const name of names) lines.push(`- \`${bin} ${name}\`: run \`${bin} ${name} --help\` for its flags.`);
    lines.push("");
  }
  lines.push(
    "## Rules",
    "",
    "- Prefer a machine-readable flag (such as `--json`) when the command's `--help` lists one.",
    "- Pass user content as arguments or on stdin; never build a shell string from untrusted text.",
    "- Report the command you ran and its exit status when something fails.",
    "",
  );
  return lines.join("\n");
}

function claudeManifest(pluginManifest) {
  const out = { name: pluginManifest.name };
  for (const key of ["version", "description"]) if (pluginManifest[key]) out[key] = pluginManifest[key];
  if (pluginManifest.author) out.author = pluginManifest.author;
  if (pluginManifest.homepage) out.homepage = pluginManifest.homepage;
  if (pluginManifest.repository?.url) out.repository = pluginManifest.repository.url;
  if (pluginManifest.license) out.license = pluginManifest.license;
  if (pluginManifest.keywords) out.keywords = pluginManifest.keywords;
  return out;
}

function codexManifest(pluginManifest) {
  const out = { name: pluginManifest.name };
  for (const key of ["version", "description"]) if (pluginManifest[key]) out[key] = pluginManifest[key];
  out.skills = "./skills/";
  return out;
}

function writeJson(path, value, written) {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, "utf8");
  written.push(path);
}

function scaffoldCliPlugin(repoPath, options) {
  const written = [];
  if (!options.skillDescription) throw new Error("CLI-skill scaffolding needs a skill description (--skill-description): what the tool does, then \"use when the user asks to ...\" with real requests");
  const baseName = options.name ?? options.cli;
  const pluginManifest = buildPluginManifest(repoPath, { ...options, name: baseName, description: options.description ?? options.skillDescription });
  const skillName = options.skillName ?? pluginManifest.name;
  if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(skillName)) throw new Error(`skill name must be lowercase kebab-case, got ${JSON.stringify(skillName)}`);
  const gate = checkSkillDescription(buildCliSkillMarkdown(options.cli, skillName, options.skillDescription, options.commands), { skillDirName: skillName });
  const rejected = gate.filter((item) => item.severity === "error");
  if (rejected.length) throw new Error(`skill description rejected by the description gate: ${rejected.map((item) => item.claim).join("; ")}`);

  // Built before anything is written so a bad hook argument leaves the tree untouched.
  const hooks = options.hookArgs ? buildHooksFiles(options.cli, { hookArgs: options.hookArgs, events: options.hookEvents }) : null;

  writeJson(join(repoPath, "plugin.json"), pluginManifest, written);
  writeJson(join(repoPath, ".claude-plugin", "plugin.json"), claudeManifest(pluginManifest), written);
  writeJson(join(repoPath, ".codex-plugin", "plugin.json"), codexManifest(pluginManifest), written);

  const skillDir = join(repoPath, "skills", skillName);
  mkdirSync(skillDir, { recursive: true });
  const skillPath = join(skillDir, "SKILL.md");
  writeFileSync(skillPath, buildCliSkillMarkdown(options.cli, skillName, options.skillDescription, options.commands), "utf8");
  written.push(skillPath);

  if (hooks) {
    writeJson(join(repoPath, ...HOOKS_FILE.split("/")), hooks.manifest, written);
    const shimPath = join(repoPath, "hooks", hooks.shimName);
    writeFileSync(shimPath, hooks.script, "utf8");
    written.push(shimPath);
  }

  const mcpManifest = buildMcpManifest(options.mcpCommand, options);
  if (mcpManifest) writeJson(join(repoPath, "mcp.json"), mcpManifest, written);
  return { pluginManifest, mcpManifest, hooksManifest: hooks?.manifest ?? null, written };
}

export function scaffoldPlugin(repoPath, options = {}) {
  if (options.cli) return scaffoldCliPlugin(repoPath, options);
  if (options.hookArgs) throw new Error("hooks need --cli: a hook runs the tool's own command");
  const written = [];
  const pluginManifest = buildPluginManifest(repoPath, options);
  const pluginJsonPath = join(repoPath, "plugin.json");
  writeFileSync(pluginJsonPath, `${JSON.stringify(pluginManifest, null, 2)}\n`, "utf8");
  written.push(pluginJsonPath);

  const skillDir = join(repoPath, "skills", options.skillName ?? pluginManifest.name);
  mkdirSync(skillDir, { recursive: true });
  const skillPath = join(skillDir, "SKILL.md");
  writeFileSync(skillPath, buildSkillMarkdown(repoPath, pluginManifest, options), "utf8");
  written.push(skillPath);

  const mcpManifest = buildMcpManifest(options.mcpCommand, options);
  if (mcpManifest) {
    const mcpJsonPath = join(repoPath, "mcp.json");
    writeFileSync(mcpJsonPath, `${JSON.stringify(mcpManifest, null, 2)}\n`, "utf8");
    written.push(mcpJsonPath);
  }

  return { pluginManifest, mcpManifest, written };
}
