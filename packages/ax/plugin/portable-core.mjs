import { cpSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve } from "node:path";

export const PORTABLE_CORE_SCHEMA_VERSION = 1;
export const PORTABLE_CORE_KIND = "rightax-portable-core";
export const ANTIGRAVITY_PLUGIN_SCHEMA = "https://antigravity.google/schemas/v1/plugin.json";
export const CLIENT_PROJECTION_KINDS = Object.freeze({
  claude: Object.freeze({ portableCore: false, projection: "claude-native-plugin+standalone-skills", fidelity: "full+skills-only", executableRegistration: true }),
  codex: Object.freeze({ portableCore: true, projection: "agent-plugins+codex-sidecar", fidelity: "full+sidecar", executableRegistration: true }),
  cursor: Object.freeze({ portableCore: true, projection: "agent-plugins+optional-cursor-sidecar", fidelity: "full+optional-sidecar", executableRegistration: true }),
  pi: Object.freeze({ portableCore: false, projection: "agents-skills", fidelity: "skills-only", executableRegistration: false }),
  antigravity: Object.freeze({ portableCore: false, projection: "antigravity-native-plugin", fidelity: "native", schema: ANTIGRAVITY_PLUGIN_SCHEMA, executableRegistration: true }),
});
export const PORTABLE_CORE_SCHEMA = Object.freeze({
  $schema: "https://json-schema.org/draft/2020-12/schema", type: "object", additionalProperties: false,
  required: ["schemaVersion", "kind", "plugin", "publicSkills", "publicFiles", "privateWorkspaceContent", "clientProjections"],
  properties: {
    schemaVersion: { const: PORTABLE_CORE_SCHEMA_VERSION }, kind: { const: PORTABLE_CORE_KIND }, plugin: { type: "string", minLength: 1 },
    publicSkills: { type: "array", minItems: 1, items: { type: "string", pattern: "^[a-z0-9]+(?:-[a-z0-9]+)*$" } },
    publicAgents: { type: "array", items: { type: "string", pattern: "^[a-z0-9]+(?:-[a-z0-9]+)*$" } },
    publicFiles: { type: "array", minItems: 2, items: { type: "string", minLength: 1 } }, privateWorkspaceContent: { const: false },
    clientProjections: { type: "object", additionalProperties: { type: "object" } },
  },
});

const PRIVATE_PATH = /(?:^|[\\/])(?:\.env(?:\..*)?|private|personal|secrets?|credentials?|workspace-private)(?:$|[.\\/])/i;
const PRIVATE_EXTENSION = /\.(?:pem|p12|pfx|key|kdbx|sqlite3?)$/i;
const PERSONAL_MARKER = /(?:C:\\Users\\adrds|D:\\Claude|\/Users\/adrds|<private-overlay>|workspace-private)/i;
const SENSITIVE_KEY = /^(?:password|passwd|secret|secrets|token|api[_-]?key|access[_-]?key|private[_-]?key|credential|credentials)$/i;
const URL_SCHEME = /^(?:[a-z][a-z\d+.-]*:|\/\/)/i;
const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));
const finding = (message) => ({ ruleId: "portable-core", severity: "error", message });

function assertContained(root, path, label) {
  const base = realpathSync(root); const candidate = realpathSync(path); const rel = relative(base, candidate);
  if (rel === ".." || rel.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`) || isAbsolute(rel)) throw new Error(`${label} escapes source root`);
  return candidate;
}
function assertSkillId(id) { if (typeof id !== "string" || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(id)) throw new Error(`invalid public skill ID: ${id}`); }
function assertRegularPublicFile(path, label) {
  // Judge the file's own name, not host ancestors: macOS temp dirs resolve
  // under /private/var, which is not a private path inside the portable core.
  if (PRIVATE_PATH.test(`/${basename(path)}`) || PRIVATE_EXTENSION.test(basename(path))) throw new Error(`portable core rejects private/personal path: ${path}`);
  const stat = lstatSync(path); if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`portable core requires regular non-symlink ${label}: ${path}`);
}
function scanPublicValue(value, location, findings = []) {
  if (typeof value === "string") { if (PERSONAL_MARKER.test(value)) findings.push(finding(`${location} contains private/personal workspace content`)); return findings; }
  if (Array.isArray(value)) { value.forEach((item, index) => scanPublicValue(item, `${location}[${index}]`, findings)); return findings; }
  if (value && typeof value === "object") for (const [key, item] of Object.entries(value)) {
    if (SENSITIVE_KEY.test(key)) findings.push(finding(`${location}.${key} is a secret-bearing manifest field`));
    if (/^(?:private|personal)$/i.test(key) && item !== false) findings.push(finding(`${location}.${key} must be false or absent`));
    scanPublicValue(item, `${location}.${key}`, findings);
  }
  return findings;
}
function copyTreeNoLinks(source, destination, prefix = "", files = []) {
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    const from = join(source, entry.name); const to = join(destination, entry.name); const outputPath = prefix ? `${prefix}/${entry.name}` : entry.name;
    // Judge the path relative to the copied root; host ancestors (e.g. /private/var on macOS) are not part of the core.
    if (PRIVATE_PATH.test(`/${outputPath}`) || PRIVATE_EXTENSION.test(entry.name)) throw new Error(`portable core rejects private/personal path: ${from}`);
    if (entry.isSymbolicLink() || lstatSync(from).isSymbolicLink()) throw new Error(`portable core cannot contain symlink: ${from}`);
    if (entry.isDirectory()) { mkdirSync(to, { recursive: true }); copyTreeNoLinks(from, to, outputPath, files); }
    else if (entry.isFile()) { const bytes = readFileSync(from); if (!bytes.includes(0) && PERSONAL_MARKER.test(bytes.toString("utf8"))) throw new Error(`portable core rejects personal workspace marker: ${from}`); cpSync(from, to); files.push(outputPath.replaceAll("\\", "/")); }
    else throw new Error(`portable core cannot contain special file: ${from}`);
  }
  return files;
}
function enumerateFiles(root, prefix = "", files = [], findings = []) {
  if (!existsSync(root)) return { files, findings };
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const absolute = join(root, entry.name); const outputPath = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (PRIVATE_PATH.test(outputPath) || PRIVATE_EXTENSION.test(entry.name)) findings.push(finding(`private/personal output file: ${outputPath}`));
    if (entry.isSymbolicLink()) findings.push(finding(`output contains symlink: ${outputPath}`));
    else if (entry.isDirectory()) enumerateFiles(absolute, outputPath, files, findings);
    else if (entry.isFile()) { files.push(outputPath.replaceAll("\\", "/")); const bytes = readFileSync(absolute); if (!bytes.includes(0) && PERSONAL_MARKER.test(bytes.toString("utf8"))) findings.push(finding(`personal workspace marker in output file: ${outputPath}`)); }
    else findings.push(finding(`output contains special file: ${outputPath}`));
  }
  return { files, findings };
}
function normalizeTarget(target) { const value = target.trim().replace(/^<|>$/g, "").split(/[?#]/, 1)[0]; if (!value || value.startsWith("#") || URL_SCHEME.test(value)) return null; return decodeURIComponent(value); }
function markdownTargets(text) { const targets = []; const markdown = /!?\[[^\]]*\]\(\s*(<[^>]+>|[^\s)]+)(?:\s+['"][^'"]*['"])?\s*\)/g; for (const match of text.matchAll(markdown)) { const target = normalizeTarget(match[1]); if (target) targets.push(target); } return targets; }

export function validateResourceClosure(outputDir, publicFiles = null) {
  const findings = []; const enumerated = enumerateFiles(outputDir); const allowed = new Set(publicFiles ?? enumerated.files); const root = resolve(outputDir);
  for (const file of enumerated.files.filter((name) => /\.md$/i.test(name))) for (const target of markdownTargets(readFileSync(join(outputDir, ...file.split("/")), "utf8"))) {
    const absolute = resolve(dirname(join(outputDir, ...file.split("/"))), target); const rel = relative(root, absolute).replaceAll("\\", "/");
    if (rel === ".." || rel.startsWith("../") || isAbsolute(relative(root, absolute))) findings.push(finding(`${file} references resource outside portable core: ${target}`));
    else if (!existsSync(absolute)) findings.push(finding(`${file} references missing resource: ${target}`));
    else if (!allowed.has(rel)) findings.push(finding(`${file} references undeclared resource: ${rel}`));
  }
  return findings;
}
function validateMcpClosure(mcp, outputDir, publicFiles, findings) {
  for (const [name, server] of Object.entries(mcp?.mcpServers ?? {})) {
    const values = []; if (typeof server.command === "string") values.push(["command", server.command]); if (typeof server.cwd === "string") values.push(["cwd", server.cwd]);
    for (const [index, value] of (server.args ?? []).entries()) if (typeof value === "string") values.push([`args[${index}]`, value]); for (const [key, value] of Object.entries(server.env ?? {})) if (typeof value === "string") values.push([`env.${key}`, value]);
    for (const [field, value] of values) { const match = value.match(/^(?:\.\/|\$\{PLUGIN_ROOT\}\/)(.+)$/); if (!match) continue; const rel = match[1].replaceAll("\\", "/"); const candidate = resolve(outputDir, rel); const rootRel = relative(resolve(outputDir), candidate).replaceAll("\\", "/");
      if (rootRel === ".." || rootRel.startsWith("../") || isAbsolute(relative(resolve(outputDir), candidate))) findings.push(finding(`mcpServers.${name}.${field} escapes portable core`)); else if (!existsSync(candidate)) findings.push(finding(`mcpServers.${name}.${field} references missing resource: ${rel}`)); else if (!publicFiles.has(rootRel)) findings.push(finding(`mcpServers.${name}.${field} references undeclared resource: ${rootRel}`));
    }
  }
}

export function normalizeClientProjections(projections) {
  const result = {}; const supplied = projections ?? {};
  for (const client of Object.keys(CLIENT_PROJECTION_KINDS)) if (!Object.hasOwn(supplied, client)) throw new Error(`missing required client projection: ${client}`);
  for (const [client, declaration] of Object.entries(supplied)) { const expected = CLIENT_PROJECTION_KINDS[client]; if (!expected) throw new Error(`unsupported client projection: ${client}`); if (!declaration || typeof declaration !== "object" || Object.keys(declaration).some((key) => !Object.hasOwn(expected, key)) || Object.keys(expected).some((key) => declaration[key] !== expected[key])) throw new Error(`invalid ${client} client projection declaration`); if (client === "pi" && (declaration.executableRegistration || declaration.fidelity !== "skills-only")) throw new Error("Pi projection cannot register executables or expose non-skill content"); result[client] = { ...expected }; }
  return result;
}
export function validatePortableCoreContract(contract) {
  const findings = []; if (!contract || typeof contract !== "object") findings.push(finding("portable core contract must be an object")); else {
    if (contract.schemaVersion !== PORTABLE_CORE_SCHEMA_VERSION || contract.kind !== PORTABLE_CORE_KIND) findings.push(finding("invalid portable core contract identity")); if (typeof contract.plugin !== "string" || !contract.plugin) findings.push(finding("portable core contract requires plugin name")); if (!Array.isArray(contract.publicSkills) || contract.publicSkills.length === 0) findings.push(finding("portable core contract requires publicSkills")); if (!Array.isArray(contract.publicFiles) || contract.publicFiles.length < 2) findings.push(finding("portable core contract requires publicFiles")); if (contract.privateWorkspaceContent !== false) findings.push(finding("portable core must exclude private workspace content")); try { normalizeClientProjections(contract.clientProjections); } catch (cause) { findings.push(finding(cause.message)); }
  } return { valid: findings.length === 0, findings, errors: findings.map((item) => item.message), contract };
}


// One agent per public markdown file, named by its own file stem so a host can
// address it. Sources are validated the same way skills are: real regular
// files, contained under a declared root, no links.
function normalizeAgents(agents) {
  if (agents === null || agents === undefined) return [];
  if (!Array.isArray(agents)) throw new Error("portable core agents must be an array");
  const seen = new Set();
  return agents.map((agent) => {
    const name = String(agent?.name ?? "").trim();
    if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(name)) throw new Error(`invalid public agent name: ${agent?.name}`);
    if (seen.has(name)) throw new Error(`duplicate public agent: ${name}`);
    seen.add(name);
    const source = assertContained(agent.sourceRoot, agent.sourceFile, `agent ${name}`);
    assertRegularPublicFile(source, `agent ${name}`);
    return { name, source };
  }).sort((left, right) => left.name.localeCompare(right.name));
}

export function assemblePortableCore({ outputDir, pluginManifestPath, mcpManifestPath = null, hooksManifestPath = null, skills, agents = [], clientProjections }) {
  if (!outputDir || !pluginManifestPath) throw new Error("outputDir & pluginManifestPath are required"); assertRegularPublicFile(pluginManifestPath, "plugin manifest"); const plugin = readJson(pluginManifestPath); const manifestFindings = scanPublicValue(plugin, "plugin.json"); if (manifestFindings.length) throw new Error(manifestFindings[0].message);
  let mcp = null; if (mcpManifestPath) { assertRegularPublicFile(mcpManifestPath, "MCP manifest"); mcp = readJson(mcpManifestPath); const findings = scanPublicValue(mcp, "mcp.json"); if (findings.length) throw new Error(findings[0].message); }
  if (typeof plugin.name !== "string" || !plugin.name.trim()) throw new Error("plugin.json requires name"); if (!Array.isArray(skills) || skills.length === 0) throw new Error("portable core requires explicit public skills"); const seen = new Set();
  const normalizedSkills = skills.map((skill) => { assertSkillId(skill.id); if (seen.has(skill.id)) throw new Error(`duplicate public skill ID: ${skill.id}`); seen.add(skill.id); if (skill.visibility !== "public") throw new Error(`portable core rejects non-public skill: ${skill.id}`); if (skill.personal === true || skill.private === true) throw new Error(`portable core rejects private/personal skill: ${skill.id}`); const source = assertContained(skill.sourceRoot, skill.sourceDir, `skill ${skill.id}`); if (!existsSync(join(source, "SKILL.md"))) throw new Error(`skill ${skill.id} lacks SKILL.md`); return { id: skill.id, source }; }).sort((left, right) => left.id.localeCompare(right.id));
  const projections = normalizeClientProjections(clientProjections); rmSync(outputDir, { recursive: true, force: true }); mkdirSync(join(outputDir, "skills"), { recursive: true }); cpSync(pluginManifestPath, join(outputDir, "plugin.json")); const publicFiles = ["plugin.json"]; if (mcpManifestPath) {
    cpSync(mcpManifestPath, join(outputDir, "mcp.json"));
    publicFiles.push("mcp.json");
    // Client descriptor aliases. Claude Code reads `.claude-plugin/plugin.json`
    // and `.mcp.json`; Antigravity reads `mcp_config.json`. Carrying them here
    // makes every client's plugin root byte-identical to this core, which is
    // what lets a root be a link to one installed tree instead of a private
    // copy per harness that upgrades never reach.
    mkdirSync(join(outputDir, ".claude-plugin"), { recursive: true });
    cpSync(pluginManifestPath, join(outputDir, ".claude-plugin", "plugin.json"));
    cpSync(mcpManifestPath, join(outputDir, ".mcp.json"));
    cpSync(mcpManifestPath, join(outputDir, "mcp_config.json"));
    publicFiles.push(".claude-plugin/plugin.json", ".mcp.json", "mcp_config.json");
  }
  // Hooks are part of the surface a host loads. A core that omits them
  // leaves every declared hook event unregistered, which reads to the user
  // as the guard being absent rather than degraded.
  if (hooksManifestPath) { assertRegularPublicFile(hooksManifestPath, "hooks manifest"); const hooks = readJson(hooksManifestPath); const findings = scanPublicValue(hooks, "hooks/hooks.json"); if (findings.length) throw new Error(findings[0].message); mkdirSync(join(outputDir, "hooks"), { recursive: true }); cpSync(hooksManifestPath, join(outputDir, "hooks", "hooks.json")); publicFiles.push("hooks/hooks.json"); }
  for (const skill of normalizedSkills) { const destination = join(outputDir, "skills", skill.id); mkdirSync(destination, { recursive: true }); publicFiles.push(...copyTreeNoLinks(skill.source, destination, `skills/${skill.id}`)); }
  // Agents are part of the plugin surface a host loads, not an optional extra:
  // a core that ships only skills leaves every agent-only role unreachable,
  // which is exactly how Legion's sage, alchemist and covenant-seat never
  // reached any client while oracle worked because it is also a skill.
  const normalizedAgents = normalizeAgents(agents);
  if (normalizedAgents.length) {
    mkdirSync(join(outputDir, "agents"), { recursive: true });
    for (const agent of normalizedAgents) {
      const relative = `agents/${agent.name}.md`;
      cpSync(agent.source, join(outputDir, relative));
      publicFiles.push(relative);
    }
  }
  const publicFileSet = new Set(publicFiles); const contract = { schemaVersion: PORTABLE_CORE_SCHEMA_VERSION, kind: PORTABLE_CORE_KIND, plugin: plugin.name, publicSkills: normalizedSkills.map(({ id }) => id), publicAgents: normalizedAgents.map(({ name }) => name), publicFiles, privateWorkspaceContent: false, clientProjections: projections }; writeFileSync(join(outputDir, "rightax-portable-core.json"), `${JSON.stringify(contract, null, 2)}\n`);
  const closure = validateResourceClosure(outputDir, publicFileSet); if (mcp) validateMcpClosure(mcp, outputDir, publicFileSet, closure); const outputCheck = enumerateFiles(outputDir); const unexpected = outputCheck.files.filter((path) => path !== "rightax-portable-core.json" && !publicFileSet.has(path)); if (closure.length || outputCheck.findings.length || unexpected.length) throw new Error([...closure, ...outputCheck.findings, ...unexpected.map((path) => finding(`undeclared output file: ${path}`))][0].message); return contract;
}

export function validatePortableCore(outputDir) {
  if (outputDir && typeof outputDir === "object") return validatePortableCoreContract(outputDir); const contractPath = join(outputDir, "rightax-portable-core.json"); const findings = []; if (!existsSync(join(outputDir, "plugin.json")) || !existsSync(contractPath)) findings.push(finding("portable core metadata or plugin.json missing")); if (findings.length) return { valid: false, findings, errors: findings.map((item) => item.message) };
  let contract; let plugin; try { contract = readJson(contractPath); } catch (cause) { findings.push(finding(`portable core metadata parse failure: ${cause.message}`)); } try { plugin = readJson(join(outputDir, "plugin.json")); } catch (cause) { findings.push(finding(`plugin.json parse failure: ${cause.message}`)); }
  findings.push(...validatePortableCoreContract(contract).findings); findings.push(...scanPublicValue(plugin, "plugin.json")); if (existsSync(join(outputDir, "mcp.json"))) try { const mcp = readJson(join(outputDir, "mcp.json")); findings.push(...scanPublicValue(mcp, "mcp.json")); validateMcpClosure(mcp, outputDir, new Set(contract?.publicFiles ?? []), findings); } catch (cause) { findings.push(finding(`mcp.json parse failure: ${cause.message}`)); }
  const disk = enumerateFiles(outputDir); findings.push(...disk.findings); const publicFiles = new Set(contract?.publicFiles ?? []); findings.push(...disk.files.filter((path) => path !== "rightax-portable-core.json" && !publicFiles.has(path)).map((path) => finding(`undeclared output file: ${path}`))); const diskSkills = [...new Set(disk.files.filter((path) => path.startsWith("skills/") && path.endsWith("/SKILL.md")).map((path) => path.split("/")[1]))].sort(); if (JSON.stringify(diskSkills) !== JSON.stringify([...(contract?.publicSkills ?? [])].sort())) findings.push(finding("public skill set differs from portable core contract")); findings.push(...validateResourceClosure(outputDir, publicFiles));
  return { valid: findings.length === 0, findings, errors: findings.map((item) => item.message), contract };
}
export const validatePortableCoreDirectory = validatePortableCore;
export const assemble = assemblePortableCore;
