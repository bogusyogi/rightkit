// Generates plugin.json / skills/<name>/SKILL.md / mcp.json from a repo's own
// package.json metadata. Never invents a capability the repo doesn't already
// have a working handler for (§5.1 invariant / static rule 12): mcp.json is
// only written when the caller supplies a real serve command, and SKILL.md
// only lists files that exist in the repo at scaffold time.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

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

export function scaffoldPlugin(repoPath, options = {}) {
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
