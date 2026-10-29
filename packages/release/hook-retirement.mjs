// Retirement of the git hooks RightKit used to write (docs/GATE-CONTRACT.md: "Commit and push run no gate and no local hook
// checks"). `rightkit-gate install-hook` and `right-release hooks install` no longer write a pre-push gate, the forwarders
// that chained .git/hooks beside it, or the advisory ownership pre-commit step; they remove the ones an earlier release wrote.
//
// A file is ours only by its own text, never by its name or its place:
//   - the gate hook      : line 2 is the header every published hook carries ("# Mandatory local [check and] gate before every
//                          push (rightkit-gate install-hook). Never bypass with --no-verify.");
//   - a forwarder        : line 2 is the forwarder header naming the hook it forwards, and that hook is the file's own name
//                          (pre-commit.rightkit-foreign may forward `pre-commit` or itself);
//   - the ownership step : the HOOK_MARKER sits in the first three lines.
// Anything else is a foreign hook and is never read for change, moved, rewritten or deleted. A symbolic link is never ours.
// One exception to "never moved": the foreign pre-commit the ownership step preserved as pre-commit.rightkit-foreign goes back to
// pre-commit when the ownership step that displaced it is removed.
import path from "node:path";

export const HOOK_MARKER = "rightkit-ownership-precommit v1";
export const FOREIGN_PRE_COMMIT = "pre-commit.rightkit-foreign";

const GATE_HEADER = /^# Mandatory local (?:check and )?gate before every push \(rightkit-gate install-hook\)\. Never bypass with --no-verify\.$/;
const FORWARDER_HEADER = /^# rightkit-gate install-hook: core\.hooksPath=\.githooks hides \.git\/hooks, so run this repository's own (\S+) hook from there\.$/;

const lines = (text) => String(text).replace(/\r\n/g, "\n").split("\n");

/** What a hook file is, by its text and its name: "gate", "forwarder", "ownership" or null (foreign). `scripts` is false for a directory the gate never wrote into. */
export function hookKind(name, text, { scripts = true } = {}) {
  const [shebang, second = "", third = ""] = lines(text);
  if (shebang !== "#!/bin/sh") return null;
  if ([second, third].some((line) => line.startsWith(`# ${HOOK_MARKER}`))) return "ownership";
  if (!scripts) return null;
  if (name === "pre-push" && GATE_HEADER.test(second)) return "gate";
  const forwarded = FORWARDER_HEADER.exec(second)?.[1];
  if (forwarded && (forwarded === name || (name === FOREIGN_PRE_COMMIT && forwarded === "pre-commit"))) return "forwarder";
  return null;
}

/**
 * Remove the hooks RightKit wrote from `dir`. `ops` = { readdir, readFile, lstat, unlink, rename, rmdir } (fs-shaped). Returns
 * { found, removed: [name], restored: [name], remaining: [name] }: `found` is false when `dir` cannot be listed, `remaining` is every
 * file left in it (foreign hooks), and an emptied directory is removed (`removedDir`).
 */
export function retireHooksIn(dir, ops, { scripts = true, removeEmptyDir = scripts } = {}) {
  let names;
  try { names = [...ops.readdir(dir)].map(String).sort(); } catch { return { found: false, removed: [], restored: [], remaining: [], removedDir: false }; }
  const removed = []; const restored = []; const kinds = new Map();
  for (const name of names) {
    const file = path.join(dir, name);
    let kind = null;
    try {
      const stat = ops.lstat(file);
      if (stat.isFile() && !stat.isSymbolicLink()) kind = hookKind(name, ops.readFile(file), { scripts });
    } catch { kind = null; }
    kinds.set(name, kind);
    if (!kind) continue;
    try { ops.unlink(file); removed.push(name); } catch { kinds.set(name, null); }
  }
  // The ownership step displaced a foreign pre-commit into pre-commit.rightkit-foreign: it goes back where git runs it.
  if (kinds.get("pre-commit") === "ownership" && removed.includes("pre-commit") && names.includes(FOREIGN_PRE_COMMIT) && kinds.get(FOREIGN_PRE_COMMIT) === null) {
    try { ops.rename(path.join(dir, FOREIGN_PRE_COMMIT), path.join(dir, "pre-commit")); restored.push("pre-commit"); } catch { /* left where it is, still foreign */ }
  }
  let left = names.filter((name) => !removed.includes(name));
  if (restored.length) left = [...left.filter((name) => name !== FOREIGN_PRE_COMMIT), "pre-commit"].sort();
  let removedDir = false;
  if (removeEmptyDir && removed.length && !left.length) { try { ops.rmdir(dir); removedDir = true; } catch { /* not empty after all */ } }
  return { found: true, removed, restored, remaining: left, removedDir };
}
