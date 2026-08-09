import { createHash } from "node:crypto";
import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

function inside(root, candidate) { const rel = relative(root, candidate); return rel === "" || (!rel.startsWith(`..${sep}`) && rel !== ".." && !isAbsolute(rel)); }
function digestTree(root, current = root, entries = []) {
  for (const name of readdirSync(current).sort()) {
    const path = join(current, name); const stat = lstatSync(path); const rel = relative(root, path).replaceAll("\\", "/");
    if (stat.isSymbolicLink()) throw new Error(`artifact contains symlink: ${rel}`);
    if (stat.isDirectory()) digestTree(root, path, entries); else if (stat.isFile()) entries.push(`${rel}\0${createHash("sha256").update(readFileSync(path)).digest("hex")}`);
  }
  return entries;
}
export function resolveArtifact({ repoPath, artifactRoot }) {
  if (typeof artifactRoot !== "string" || !artifactRoot) throw new Error("artifact root is required");
  const repo = realpathSync(repoPath); const declared = isAbsolute(artifactRoot) ? resolve(artifactRoot) : resolve(repo, artifactRoot);
  // This check deliberately precedes existence/canonicalization: an artifact
  // declaration must never name the source repository, even if it is absent.
  if (inside(repo, declared)) throw new Error("artifact root must be outside source repository");
  if (!existsSync(declared)) throw new Error("artifact root does not exist");
  if (lstatSync(declared).isSymbolicLink()) throw new Error("artifact root cannot be symlink");
  const root = realpathSync(declared); if (inside(repo, root) || !lstatSync(root).isDirectory()) throw new Error("artifact root must be external directory");
  const digest = createHash("sha256").update(digestTree(root).join("\n")).digest("hex");
  return { root, digest, proof: { isolation: "external-clean-room", algorithm: "sha256-tree-v1", before: digest, after: null, immutable: null } };
}
export function verifyArtifactUnchanged(artifact) {
  const after = createHash("sha256").update(digestTree(artifact.root).join("\n")).digest("hex");
  const proof = { isolation: "external-clean-room", algorithm: "sha256-tree-v1", before: artifact.proof?.before ?? artifact.digest, after, immutable: after === (artifact.proof?.before ?? artifact.digest) };
  return { ...artifact, proof };
}
export function bindArtifactCommand({ artifact, command, args = [], cwd }) {
  const replace = (value) => { const text=String(value); if(text.includes("$ARTIFACT_ROOT")||text.includes("${artifactRoot}")) throw new Error("legacy artifact token forbidden"); return text.replaceAll("${ARTIFACT_ROOT}", artifact.root); };
  if (cwd !== "${ARTIFACT_ROOT}") throw new Error("MCP cwd must be exact ${ARTIFACT_ROOT} token");
  const bound = { command: replace(command), args: args.map(replace), cwd: replace(cwd) };
  const executableIsPath = bound.command.includes("/") || bound.command.includes("\\") || isAbsolute(bound.command);
  if (!existsSync(bound.cwd) || !lstatSync(bound.cwd).isDirectory() || !inside(artifact.root, realpathSync(bound.cwd))) throw new Error("MCP cwd escapes artifact root");
  const confined = (value, field) => { const lexical = isAbsolute(value) ? resolve(value) : resolve(bound.cwd, value); if (!inside(artifact.root, lexical)) throw new Error(`${field} escapes artifact root`); if (existsSync(lexical) && !inside(artifact.root, realpathSync(lexical))) throw new Error(`${field} escapes artifact root`); return lexical; };
  if (executableIsPath) { const executable = confined(bound.command, "MCP executable"); if (!existsSync(executable)) throw new Error("MCP executable does not exist"); }
  for (let index = 0; index < bound.args.length; index += 1) { const raw = bound.args[index]; if (raw.includes("/") || raw.includes("\\") || isAbsolute(raw)) { const path = confined(raw, "MCP argument"); if ((String(args[index]).includes("${ARTIFACT_ROOT}")) && !existsSync(path)) throw new Error("MCP token argument does not exist"); } }
  return bound;
}
