#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { installCommand } from "../install.mjs";
import { syncCommand } from "../sync.mjs";
import { driftCommand, driftAll } from "../drift.mjs";
import { adoptCommand } from "../adopt.mjs";
import { uninstallCommand } from "../uninstall.mjs";
import { artifactsCommand } from "../artifacts.mjs";

const commands = new Map([
  ["install", runInstall],
  ["sync", runSync],
  ["drift", runDrift],
  ["adopt", runAdopt],
  ["uninstall", runUninstall],
  ["artifacts", (argv) => artifactsCommand(argv)],
]);

// F7 fix: flags that are always boolean and must never consume the next
// argv token as a value. Without this, `drift --all <root>` (the documented
// syntax — see printHelp below) fed `<root>` to parseFlags' generic
// "--flag value" branch as the *value* of --all, leaving `positional` empty;
// runDrift then fell back to `.` and silently scanned the current working
// directory instead of the supplied workspace root. `--all=true` happened to
// work because the `=` form never consumes a following token. Declared here
// (above the dispatch below, which calls parseFlags synchronously at module
// load) rather than next to parseFlags itself, to avoid a temporal-dead-zone
// ReferenceError on a const declared after its first use point at runtime.
const BOOLEAN_ONLY_FLAGS = new Set(["all"]);

const args = process.argv.slice(2);
const first = args[0];

if (!first || first === "-h" || first === "--help") {
  printHelp();
  process.exit(first ? 0 : 1);
} else if (first === "--version" || first === "-v") {
  const pkg = JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8"));
  console.log(pkg.version);
  process.exit(0);
} else if (commands.has(first)) {
  const exitCode = await commands.get(first)(args.slice(1));
  process.exit(exitCode);
} else {
  console.error(`right-git: unknown command: ${first}\n`);
  printHelp();
  process.exit(2);
}

/** Split argv into positional args and --flag / --flag=value pairs. */
function parseFlags(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg.startsWith("--")) {
      const eq = arg.indexOf("=");
      if (eq !== -1) {
        flags[arg.slice(2, eq)] = arg.slice(eq + 1);
      } else {
        const key = arg.slice(2);
        if (!BOOLEAN_ONLY_FLAGS.has(key) && i + 1 < argv.length && !argv[i + 1].startsWith("--")) {
          flags[key] = argv[++i];
        } else {
          flags[key] = true;
        }
      }
    } else {
      positional.push(arg);
    }
  }
  return { positional, flags };
}

function printErrors(errors) {
  for (const error of errors || []) console.error(`  ${error}`);
}

// Advisory lines never change drift's exit code: warnings are settings to fix,
// notes are checks that could not run here.
function printAdvisories(result) {
  for (const warning of result.warnings || []) console.log(`  warn: ${warning}`);
  for (const note of result.notes || []) console.log(`  note: ${note}`);
}

async function runInstall(argv) {
  const { positional, flags } = parseFlags(argv);
  const repoPath = positional[0];
  if (!repoPath) {
    console.error("right-git install <repo-path> --lanes ci,qualification,... [--profile node|rust-hybrid --gate-path scripts/gate.sh]");
    return 2;
  }
  const opts = {
    lanes: typeof flags.lanes === "string" ? flags.lanes.split(",").map((s) => s.trim()).filter(Boolean) : undefined,
    homebrewTap: flags["homebrew-tap"],
    homebrewFormula: flags["homebrew-formula"],
    wingetId: flags["winget-id"],
    provenance: flags["no-provenance"] ? false : undefined,
    profile: flags.profile,
    gatePath: flags["gate-path"],
  };
  const result = await installCommand(path.resolve(repoPath), opts);
  if (!result.ok) {
    console.error(`right-git install: failed at stage '${result.stage}'`);
    printErrors(result.errors);
    return 1;
  }
  console.log(`right-git install: wrote ${result.manifestPath} and synced ${result.plan.changes.length} workflow file(s).`);
  return 0;
}

async function runSync(argv) {
  const { positional, flags } = parseFlags(argv);
  const repoPath = positional[0];
  if (!repoPath) {
    console.error("right-git sync <repo-path> [--dry-run]");
    return 2;
  }
  const result = await syncCommand(path.resolve(repoPath), { dryRun: Boolean(flags["dry-run"]) });
  if (!result.ok) {
    console.error(`right-git sync: failed at stage '${result.stage}'`);
    printErrors(result.errors);
    return 1;
  }
  const changed = result.changes.filter((c) => c.action !== "unchanged");
  console.log(changed.length === 0 ? "right-git sync: already up to date." : `right-git sync: ${changed.length} file(s) ${flags["dry-run"] ? "would change" : "changed"}.`);
  return 0;
}

async function runDrift(argv) {
  const { positional, flags } = parseFlags(argv);
  if (flags.all) {
    const root = path.resolve(positional[0] || ".");
    const results = await driftAll(root);
    let anyDrift = false;
    for (const { repoPath, result } of results) {
      if (!result.ok) {
        console.error(`${repoPath}: error`);
        printErrors(result.errors);
        anyDrift = true;
        continue;
      }
      console.log(`${repoPath}: visibility=${result.visibility.visibility} drift=${result.driftDetected}`);
      for (const finding of result.findings) {
        if (finding.status !== "match") console.log(`  ${finding.status}: ${finding.filename}`);
      }
      printAdvisories(result);
      if (result.driftDetected) anyDrift = true;
    }
    return anyDrift ? 1 : 0;
  }

  const repoPath = positional[0];
  if (!repoPath) {
    console.error("right-git drift <repo-path> | right-git drift --all [workspace-root]");
    return 2;
  }
  const result = await driftCommand(path.resolve(repoPath));
  if (!result.ok) {
    console.error("right-git drift: failed");
    printErrors(result.errors);
    return 1;
  }
  console.log(`visibility=${result.visibility.visibility} drift=${result.driftDetected}`);
  for (const finding of result.findings) console.log(`  ${finding.status}: ${finding.filename}`);
  printAdvisories(result);
  return result.driftDetected ? 1 : 0;
}

async function runAdopt(argv) {
  const { positional, flags } = parseFlags(argv);
  const repoPath = positional[0];
  if (!repoPath) {
    console.error("right-git adopt <repo-path> [--out <dir>]");
    return 2;
  }
  const result = adoptCommand(path.resolve(repoPath));
  if (!result.ok) {
    console.error("right-git adopt: failed");
    printErrors(result.errors);
    return 1;
  }
  for (const { lane, sourceFile } of result.mapped) console.log(`mapped ${lane} <- ${sourceFile}`);
  for (const filename of result.unmapped) console.log(`unmapped: ${filename}`);
  if (typeof flags.out === "string") {
    fs.mkdirSync(flags.out, { recursive: true });
    for (const { lane, content } of result.mapped) {
      fs.writeFileSync(path.join(flags.out, `${lane}.yml`), content, "utf8");
    }
    console.log(`right-git adopt: wrote ${result.mapped.length} candidate template(s) to ${flags.out} for review.`);
  }
  return 0;
}

async function runUninstall(argv) {
  const { positional } = parseFlags(argv);
  const repoPath = positional[0];
  if (!repoPath) {
    console.error("right-git uninstall <repo-path>");
    return 2;
  }
  const result = uninstallCommand(path.resolve(repoPath));
  console.log(result.removed.length === 0 ? "right-git uninstall: no managed files found." : `right-git uninstall: removed ${result.removed.length} file(s): ${result.removed.join(", ")}`);
  return 0;
}

function printHelp() {
  console.log(`right-git <command> [options]

Commands:
  install <repo-path> --lanes ci,... [--profile rust-hybrid --gate-path scripts/gate.sh]  Write .rightgit.json and render workflows; refuses private repos
  sync <repo-path> [--dry-run]                        Re-render templates from .rightgit.json, show diff, apply
  drift <repo-path>                                   CI-safe check: installed workflows == rendered templates? Also warns when the regenerate lane cannot open pull requests (gh)
  drift --all [workspace-root]                        Drift-check + re-check visibility for every managed repo found
  adopt <repo-path> [--out <dir>]                      Map an existing repo's workflows onto right-git lanes for review
  uninstall <repo-path>                                Remove every right-git-managed workflow file
  artifacts fetch --run <id> --out <dir> [--name <glob>] [--repo owner/repo] [--expect-sha256 <hex> [--file <path>]] [--receipt <file>] [--json]
                                                      Download a run's artifacts via gh, merge them, verify the sha256 pin, write a JSON receipt

Public-repo only: install/sync refuse private, internal, or unconfirmed-visibility repos. No override flag (invariant I1).`);
}
