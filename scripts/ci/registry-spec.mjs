// Source classification only: npm remains owner of semver range/tag resolution.
// No URLs, paths or hosted Git shortcuts may reach a shipped dependency field.
const registryName = /^(?:@[A-Za-z0-9][A-Za-z0-9._-]*\/)?[A-Za-z0-9][A-Za-z0-9._-]*$/;
// npm-package-arg's isPosixFile/isWindowsFile & isFileType classify these
// as local sources before registry range/tag resolution (lib/npa.js).
const archiveFile = /[.](?:tgz|tar\.gz|tar)$/i;
export const SHIPPED_DEPENDENCY_FIELDS = Object.freeze(["dependencies", "optionalDependencies", "peerDependencies"]);

function selector(value) {
  // Classify sources, never reimplement npm's range/tag grammar. These indicators
  // identify protocols, URLs, hosted Git shortcuts, filesystem paths or fragments.
  const normalized = value.trim();
  return normalized !== "" && !/[:/\\#@%\u0000-\u001f\u007f]/.test(value) && !normalized.startsWith(".") && !archiveFile.test(normalized);
}

/** Returns a diagnostic, or null. Workspace exceptions apply only to planner-known members. */
export function registrySpecProblem(spec, { workspaceMember = false } = {}) {
  if (typeof spec !== "string" || !spec.trim() || /[\u0000-\u001f\u007f]/.test(spec)) return "requires a nonempty registry specifier";
  const value = spec.trim();
  if (value.startsWith("workspace:")) {
    const requirement = value.slice("workspace:".length);
    return workspaceMember && (["*", "^", "~"].includes(requirement) || selector(requirement)) ? null : "workspace: requires an actual workspace member & supported version requirement";
  }
  if (value.startsWith("npm:")) {
    const alias = /^(?:npm:)(@[^/]+\/[^@]+|[^@]+)(?:@(.+))?$/.exec(value);
    if (!alias || alias[1].length > 214 || !registryName.test(alias[1]) || (!alias[1].startsWith("@") && archiveFile.test(alias[1])) || (alias[2] !== undefined && !selector(alias[2]))) return "requires an npm alias to a registry package & registry version/tag";
    return null;
  }
  return selector(value) ? null : "uses a non-registry specifier";
}

/** Packed manifests have no workspace exception; development dependencies are not shipped edges. */
export function shippedDependencyProblems(pkg) {
  const problems = [];
  for (const field of SHIPPED_DEPENDENCY_FIELDS) {
    const deps = pkg[field];
    if (deps === undefined) continue;
    if (!deps || typeof deps !== "object" || Array.isArray(deps)) { problems.push(`${pkg.name}: ${field} must be a dependency map`); continue; }
    for (const [name, spec] of Object.entries(deps)) {
      const problem = registrySpecProblem(spec);
      if (problem) problems.push(`${pkg.name}: ${field} ${name} ${problem}: ${JSON.stringify(spec)}`);
    }
  }
  return problems;
}
