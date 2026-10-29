// right-release hooks: the advisory pre-commit ownership gate is retired (docs/GATE-CONTRACT.md: "Commit and push run no gate and no
// local hook checks"). Nothing here writes a hook any more.
//
//   right-release hooks install [--app <dir>]   (`right-release hooks install` is `rightkit-gate install-hook`: it retires the hook)
//
// An earlier release wrote `.git/hooks/pre-commit` (or the one in core.hooksPath) running `right-release ownership --staged`, and
// kept a foreign pre-commit as pre-commit.rightkit-foreign. Retirement removes the hook by its own marker only and puts the foreign
// pre-commit back. `right-release ownership --staged` stays a manual command; broker admission and the release gate remain
// authoritative.
import { lstatSync, readFileSync, readdirSync, realpathSync, renameSync, rmdirSync, unlinkSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { HOOK_MARKER, retireHooksIn } from "./hook-retirement.mjs";
import { repositoryRootFor } from "./ownership-staged.mjs";

export { HOOK_MARKER };

const FS_OPS = Object.freeze({ readdir: readdirSync, readFile: (file) => readFileSync(file, "utf8"), lstat: lstatSync, unlink: unlinkSync, rename: renameSync, rmdir: rmdirSync });

function git(cwd, args) {
  const result = spawnSync("git", ["-C", cwd, ...args], { encoding: "utf8", windowsHide: true, env: Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^GIT_/i.test(k))) });
  if (result.status !== 0) return null;
  return String(result.stdout).trim();
}

/** The directory git runs hooks from: core.hooksPath when configured, else the common hooks directory. */
export function hooksDirectoryFor(root) {
  const configured = git(root, ["config", "--get", "core.hooksPath"]);
  if (configured) return path.resolve(root, configured);
  const common = git(root, ["rev-parse", "--git-path", "hooks"]);
  if (!common) throw new Error("could not resolve the repository hooks directory");
  return path.resolve(root, common);
}

/** Remove the ownership pre-commit hook an earlier release wrote, restoring the foreign pre-commit it displaced. { status: "retired" | "restored" | "absent", file } */
export function retirePreCommitHook(repoDir) {
  const root = repositoryRootFor(repoDir);
  const hooksDir = hooksDirectoryFor(root);
  const result = retireHooksIn(hooksDir, FS_OPS, { scripts: false });
  const file = path.join(hooksDir, "pre-commit");
  return { status: result.restored.length ? "restored" : result.removed.includes("pre-commit") ? "retired" : "absent", file };
}

export function main(args, { print = (line) => console.log(line) } = {}) {
  const flag = (name) => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };
  if (args[0] !== "install") {
    print("right-release hooks install [--app <dir>]   (retires the ownership pre-commit hook; no hook is written)");
    return args.includes("--help") ? 0 : 2;
  }
  try {
    const result = retirePreCommitHook(path.resolve(flag("--app") ?? process.cwd()));
    print(`[right-release] pre-commit ownership gate retired (${result.status}): ${result.file}`);
    if (result.status === "restored") print("[right-release] the previous pre-commit hook is back in place");
    print("[right-release] commit runs no local hook check; `right-release ownership --staged` is still available by hand");
    return 0;
  } catch (error) {
    print(`[right-release] hooks install FAILED: ${error.message}`);
    return 1;
  }
}

const isEntry = (() => {
  if (!process.argv[1]) return false;
  try { return realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url)); }
  catch { return false; }
})();
if (isEntry) process.exitCode = main(process.argv.slice(2));
