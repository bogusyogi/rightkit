// The file listers the signing policy's artifact classification reads a product package's integration tests with (rightkit#1303): the policy itself stays
// pure and is handed `listFiles(dir)`, which returns every file under an app-relative directory as an app-relative `/` path ([] when the directory
// does not exist, null when it cannot be read, so the policy treats it as unclassifiable and refuses).
import { readdirSync } from "node:fs";
import path from "node:path";

/** `listFiles` over a checkout or snapshot directory. */
export function fsListRustFiles(root) {
  return (dir) => {
    const out = [];
    const walk = (rel) => {
      let entries;
      try { entries = readdirSync(path.resolve(root, rel), { withFileTypes: true }); }
      catch (error) { if (error?.code === "ENOENT" || error?.code === "ENOTDIR") return true; return false; }
      for (const entry of entries) {
        const child = rel === "." ? entry.name : `${rel}/${entry.name}`;
        if (entry.isDirectory()) { if (entry.name === "target" || entry.name === "node_modules" || entry.name === ".git") continue; if (!walk(child)) return false; }
        else out.push(child);
      }
      return true;
    };
    return walk(String(dir).replaceAll("\\", "/")) ? out : null;
  };
}

/** `listFiles` over a commit's object database: `git(...args)` returns `{ status, stdout }`-shaped output as the gate's `git` helper does (via `list`). */
export function gitListRustFiles(list, commit, appDir = ".") {
  return (dir) => {
    const rel = path.posix.normalize(appDir === "." ? String(dir) : `${appDir}/${dir}`);
    const names = list(commit, rel);
    if (!Array.isArray(names)) return null;
    const prefix = appDir === "." ? "" : `${appDir}/`;
    return names.map((name) => (prefix && name.startsWith(prefix) ? name.slice(prefix.length) : name));
  };
}
