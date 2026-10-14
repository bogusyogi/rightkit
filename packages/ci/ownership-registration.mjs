import path from "node:path";

/** Normalize HTTPS, SSH, and SCP GitHub remotes to one immutable repository key. */
export function normalizeRepositoryIdentity(value) {
  if (typeof value !== "string" || !value.trim()) throw new Error("ownership registration requires a repository identity");
  let text = value.trim();
  const scp = text.match(/^(?:[^@/]+@)?([^:]+):([^/]+\/[^/]+)$/);
  if (scp) text = `https://${scp[1]}/${scp[2]}`;
  let url;
  try { url = new URL(text); } catch { throw new Error("ownership repository identity must be a Git remote URL"); }
  if (!["https:", "ssh:"].includes(url.protocol) || (url.username && !(url.protocol === "ssh:" && url.username === "git")) || url.password || url.search || url.hash) throw new Error("unsupported or credential-bearing ownership repository identity");
  const host = url.hostname.toLowerCase();
  const parts = url.pathname.replace(/\.git$/i, "").split("/").filter(Boolean);
  if (host !== "github.com" || parts.length !== 2 || parts.some((part) => !/^[A-Za-z0-9_.-]+$/.test(part))) throw new Error("ownership repository identity must be owner/repository on github.com");
  return `https://${host}/${parts[0].toLowerCase()}/${parts[1].toLowerCase()}`;
}

/** Resolve immutable OS-owned roots for one checkout; repo declarations can only narrow reporting, never authority. */
export function protectedOwnershipRoots(trust, { repositoryIdentity, scanRoot, repoRoot }) {
  if (!trust || !Array.isArray(trust.repositories)) throw new Error("protected ownership repository registry is missing");
  const wanted = normalizeRepositoryIdentity(repositoryIdentity);
  const matches = trust.repositories.filter((entry) => normalizeRepositoryIdentity(entry.repositoryIdentity) === wanted);
  if (matches.length !== 1) throw new Error(matches.length ? "protected ownership registry has duplicate repository identities" : `repository is not registered for ownership enforcement: ${wanted}`);
  const entry = matches[0];
  if (!["public", "private"].includes(entry.visibility) || !Array.isArray(entry.roots) || !entry.roots.length) throw new Error("protected ownership registration is malformed");
  const roots = entry.roots.map((root) => {
    if (typeof root !== "string" || !root || root.includes("\\") || root.startsWith("/") || (root !== "." && root.split("/").some((p) => !p || p === "." || p === "..")) || /[*?]/.test(root)) throw new Error("protected ownership roots must be exact relative paths");
    return root;
  });
  const relative = path.relative(repoRoot, scanRoot).replaceAll("\\", "/") || ".";
  if (!roots.includes(relative)) throw new Error(`scan root is not registered for ${wanted}: ${relative}`);
  return { repositoryIdentity: wanted, visibility: entry.visibility, roots };
}
