import { cpSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { basename, isAbsolute, join, relative, resolve } from "node:path";

export const CLIENT_PROJECTION_KINDS = Object.freeze({
  claude: Object.freeze({ portableCore: false, projection: "claude-native-plugin+standalone-skills", executableRegistration: true }),
  codex: Object.freeze({ portableCore: true, projection: "agent-plugins+codex-sidecar", executableRegistration: true }),
  cursor: Object.freeze({ portableCore: true, projection: "agent-plugins+optional-cursor-sidecar", executableRegistration: true }),
  pi: Object.freeze({ portableCore: false, projection: "agents-skills", executableRegistration: false }),
  antigravity: Object.freeze({ portableCore: false, projection: "antigravity-native-plugin", executableRegistration: true }),
});
const PRIVATE_PATH = /(?:^|[\\/])(?:\.env(?:\..*)?|private|personal|secrets?|credentials?|workspace-private)(?:$|[.\\/])/i;
const PRIVATE_EXTENSION = /\.(?:pem|p12|pfx|key|kdbx)$/i;
const PERSONAL_MARKER = /(?:C:\\Users\\adrds|D:\\Claude|\/Users\/adrds)/i;

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function assertContained(root, path, label) {
  const base = realpathSync(root);
  const candidate = realpathSync(path);
  const rel = relative(base, candidate);
  if (rel === ".." || rel.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`) || isAbsolute(rel)) throw new Error(`${label} escapes source root`);
  return candidate;
}

function assertSkillId(id) {
  if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(id)) throw new Error(`invalid public skill ID: ${id}`);
}

function copyTreeNoLinks(source, destination) {
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    const from = join(source, entry.name);
    const to = join(destination, entry.name);
    if (PRIVATE_PATH.test(from) || PRIVATE_EXTENSION.test(entry.name)) throw new Error(`portable core rejects private/personal path: ${from}`);
    if (entry.isSymbolicLink() || lstatSync(from).isSymbolicLink()) throw new Error(`portable core cannot contain symlink: ${from}`);
    if (entry.isDirectory()) {
      mkdirSync(to, { recursive: true });
      copyTreeNoLinks(from, to);
    } else if (entry.isFile()) {
      const bytes = readFileSync(from);
      if (bytes.includes(0) === false && PERSONAL_MARKER.test(bytes.toString("utf8"))) throw new Error(`portable core rejects personal workspace marker: ${from}`);
      cpSync(from, to);
    } else {
      throw new Error(`portable core cannot contain special file: ${from}`);
    }
  }
}

export function normalizeClientProjections(projections) {
  const result = {};
  const supplied = projections ?? {};
  for (const client of Object.keys(CLIENT_PROJECTION_KINDS)) if (!Object.hasOwn(supplied, client)) throw new Error(`missing required client projection: ${client}`);
  for (const [client, declaration] of Object.entries(supplied)) {
    const expected = CLIENT_PROJECTION_KINDS[client];
    if (!expected) throw new Error(`unsupported client projection: ${client}`);
    if (declaration.portableCore !== expected.portableCore || declaration.projection !== expected.projection || declaration.executableRegistration !== expected.executableRegistration || Object.keys(declaration).some((key) => !Object.hasOwn(expected, key))) {
      throw new Error(`invalid ${client} client projection declaration`);
    }
    result[client] = { ...expected };
  }
  return result;
}

export function assemblePortableCore({
  outputDir,
  pluginManifestPath,
  mcpManifestPath = null,
  skills,
  clientProjections,
}) {
  const plugin = readJson(pluginManifestPath);
  if (typeof plugin.name !== "string" || !plugin.name.trim()) throw new Error("plugin.json requires name");
  if (!Array.isArray(skills) || skills.length === 0) throw new Error("portable core requires public skills");
  const seen = new Set();
  const normalizedSkills = skills.map((skill) => {
    assertSkillId(skill.id);
    if (seen.has(skill.id)) throw new Error(`duplicate public skill ID: ${skill.id}`);
    seen.add(skill.id);
    if (skill.visibility !== "public") throw new Error(`portable core rejects non-public skill: ${skill.id}`);
    if (skill.personal === true || skill.private === true) throw new Error(`portable core rejects private/personal skill: ${skill.id}`);
    const source = assertContained(skill.sourceRoot, skill.sourceDir, `skill ${skill.id}`);
    if (!existsSync(join(source, "SKILL.md"))) throw new Error(`skill ${skill.id} lacks SKILL.md`);
    return { id: skill.id, source };
  }).sort((left, right) => left.id.localeCompare(right.id));
  const projections = normalizeClientProjections(clientProjections);
  rmSync(outputDir, { recursive: true, force: true });
  mkdirSync(join(outputDir, "skills"), { recursive: true });
  cpSync(pluginManifestPath, join(outputDir, "plugin.json"));
  if (mcpManifestPath) cpSync(mcpManifestPath, join(outputDir, "mcp.json"));
  for (const skill of normalizedSkills) {
    const destination = join(outputDir, "skills", skill.id);
    mkdirSync(destination, { recursive: true });
    copyTreeNoLinks(skill.source, destination);
  }
  const contract = {
    schemaVersion: 1,
    kind: "rightax-portable-core",
    plugin: plugin.name,
    publicSkills: normalizedSkills.map(({ id }) => id),
    privateWorkspaceContent: false,
    clientProjections: projections,
  };
  writeFileSync(join(outputDir, "rightax-portable-core.json"), `${JSON.stringify(contract, null, 2)}\n`);
  return contract;
}

export function validatePortableCore(outputDir) {
  const contractPath = join(outputDir, "rightax-portable-core.json");
  if (!existsSync(join(outputDir, "plugin.json")) || !existsSync(contractPath)) return { valid: false, errors: ["portable core metadata missing"] };
  const contract = readJson(contractPath);
  const errors = [];
  if (contract.schemaVersion !== 1 || contract.kind !== "rightax-portable-core") errors.push("invalid portable core contract");
  if (contract.privateWorkspaceContent !== false) errors.push("portable core must exclude private workspace content");
  try { normalizeClientProjections(contract.clientProjections); } catch (error) { errors.push(String(error.message)); }
  const diskSkills = existsSync(join(outputDir, "skills")) ? readdirSync(join(outputDir, "skills"), { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort() : [];
  if (JSON.stringify(diskSkills) !== JSON.stringify([...(contract.publicSkills ?? [])].sort())) errors.push("public skill set differs from portable core contract");
  for (const id of diskSkills) if (!existsSync(join(outputDir, "skills", id, "SKILL.md"))) errors.push(`skill ${id} lacks SKILL.md`);
  return { valid: errors.length === 0, errors, contract };
}
