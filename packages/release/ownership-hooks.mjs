// right-release hooks install: writes the advisory pre-commit ownership gate into an app repository.
//
//   right-release hooks install [--app <dir>]
//
// The hook runs the app's pinned @rightkit/release (node_modules) via `right-release ownership --staged`.
// An existing foreign pre-commit is preserved as pre-commit.rightkit-foreign and chained first.
// The gate is advisory: `git commit --no-verify` bypasses it; broker admission and the release gate
// remain authoritative.
import { chmodSync, existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { repositoryRootFor } from "./ownership-staged.mjs";

export const HOOK_MARKER = "rightkit-ownership-precommit v1";
const FOREIGN = "pre-commit.rightkit-foreign";
const PINNED_CLI = path.join("node_modules", "@rightkit", "release", "cli", "right-release.mjs");

export function renderPreCommitHook() {
  return [
    "#!/bin/sh",
    `# ${HOOK_MARKER}: advisory ownership gate installed by \`right-release hooks install\`.`,
    "# Block-tier findings without a valid exception fail here; --no-verify bypasses this gate.",
    "# Authoritative enforcement remains in broker admission and the release gate.",
    "ROOT=\"$(git rev-parse --show-toplevel)\" || exit 1",
    "HOOKS_DIR=\"$(dirname \"$0\")\"",
    `if [ -x "$HOOKS_DIR/${FOREIGN}" ]; then`,
    `  "$HOOKS_DIR/${FOREIGN}" "$@" || exit $?`,
    "fi",
    `GATE="$ROOT/${PINNED_CLI.split(path.sep).join("/")}"`,
    "if [ ! -f \"$GATE\" ]; then",
    "  echo \"[rightkit] @rightkit/release is not installed in this repository; add it as a devDependency, then re-run: right-release hooks install\" >&2",
    "  exit 1",
    "fi",
    "exec node \"$GATE\" ownership --staged",
    "",
  ].join("\n");
}

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

/**
 * Idempotent install. Returns { status: "installed" | "updated" | "unchanged" | "chained", file }.
 */
export function installPreCommitHook(repoDir) {
  const root = repositoryRootFor(repoDir);
  const pinned = path.join(root, PINNED_CLI);
  if (!existsSync(pinned)) throw new Error(`@rightkit/release is not installed at ${pinned}; add it as a devDependency and run pnpm install first`);
  const hooksDir = hooksDirectoryFor(root);
  const file = path.join(hooksDir, "pre-commit");
  const foreign = path.join(hooksDir, FOREIGN);
  const body = renderPreCommitHook();
  mkdirSync(hooksDir, { recursive: true });
  let status = "installed";
  if (lstatSync(file, { throwIfNoEntry: false })) {
    const current = readFileSync(file, "utf8");
    if (current === body) return { status: "unchanged", file };
    if (current.includes(HOOK_MARKER)) status = "updated";
    else {
      if (existsSync(path.join(hooksDir, FOREIGN))) throw new Error(`${FOREIGN} already exists; resolve the chained hook manually before installing`);
      renameSync(file, foreign);
      status = "chained";
    }
  }
  const temporary = `${file}.${process.pid}.tmp`;
  writeFileSync(temporary, body, { mode: 0o755 });
  chmodSync(temporary, 0o755);
  renameSync(temporary, file);
  return { status, file };
}

export function main(args, { print = (line) => console.log(line) } = {}) {
  const flag = (name) => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };
  if (args[0] !== "install") {
    print("right-release hooks install [--app <dir>]");
    return args.includes("--help") ? 0 : 2;
  }
  try {
    const result = installPreCommitHook(path.resolve(flag("--app") ?? process.cwd()));
    print(`[right-release] pre-commit ownership gate ${result.status}: ${result.file}`);
    if (result.status === "chained") print(`[right-release] previous pre-commit preserved as ${FOREIGN} and runs first`);
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
