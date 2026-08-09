#!/usr/bin/env node
// Dispatcher for right-ax, mirroring right-release's CLI shape
// (tools/rightkit/packages/release/cli/right-release.mjs): a thin argv
// router into the package's library modules, in-process (no child spawn per
// command — the modules already spawn the target's own MCP server when a
// gate needs a live process).

import path from "node:path";
import { fileURLToPath } from "node:url";
import { adoptRepo } from "../adopt.mjs";
import { runConformance } from "../ax/conformance.mjs";
import { runScenarios } from "../ax/behavioral.mjs";
import { aggregateReport, gateExitCode, renderMarkdown, toSarif } from "../ax/report.mjs";
import { runStaticChecks } from "../ax/static.mjs";
import { loadManifest } from "../manifest.mjs";
import { scaffoldPlugin } from "../plugin/scaffold.mjs";
import { validateRepoLayout } from "../plugin/validate.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const [first, second, ...rest] = args;

function flag(name, fallback = undefined) {
  const index = rest.indexOf(`--${name}`);
  if (index === -1) return fallback;
  return rest[index + 1];
}

function resolveRepoPath(candidate) {
  return path.resolve(candidate ?? ".");
}

// F2: parses --agents into runScenarios' declared-roster array. Comma-
// separated, blank entries dropped; ax/behavioral.mjs normalizes and
// validates the roster itself (see scenarioMeetsRealMatrixBar).
function parseAgentsFlag(value) {
  return String(value ?? "").split(",").map((entry) => entry.trim()).filter(Boolean);
}

function printJson(value) {
  console.log(JSON.stringify(value, null, 2));
}

function requireManifest(repoPath) {
  const { manifest, errors, path: manifestPath } = loadManifest(repoPath);
  if (!manifest) {
    console.error(`right-ax: invalid or missing .rightax.json at ${manifestPath}`);
    for (const error of errors) console.error(`  - ${error}`);
    process.exit(2);
  }
  return manifest;
}

async function main() {
  if (!first || first === "-h" || first === "--help") return printHelp();
  if (first === "--version" || first === "-v") { console.log("right-ax 0.1.0"); return; }

  if (first === "plugin") {
    const repoPath = resolveRepoPath(rest[0]?.startsWith("--") ? undefined : rest[0]);
    if (second === "init") {
      const result = scaffoldPlugin(repoPath, {
        name: flag("name"),
        version: flag("version"),
        description: flag("description"),
        license: flag("license"),
        homepage: flag("homepage"),
        mcpCommand: flag("mcp-command"),
        skillName: flag("skill-name"),
        referencedFiles: [],
      });
      console.log(`right-ax plugin init: wrote ${result.written.length} file(s)`);
      for (const file of result.written) console.log(`  - ${file}`);
      return;
    }
    if (second === "validate") {
      const { valid, findings } = validateRepoLayout(repoPath);
      printJson({ valid, findings });
      process.exitCode = valid ? 0 : 1;
      return;
    }
    console.error("right-ax plugin: expected init or validate");
    process.exitCode = 2;
    return;
  }

  if (first === "ax") {
    const repoPath = resolveRepoPath(rest[0]?.startsWith("--") ? undefined : rest[0]);
    const manifest = requireManifest(repoPath);

    if (second === "static") {
      const result = await runStaticChecks({ repoPath, manifest });
      printJson(result);
      process.exitCode = result.verdict === "pass" ? 0 : 1;
      return;
    }
    if (second === "conformance") {
      const result = await runConformance({ repoPath, manifest });
      printJson(result);
      process.exitCode = result.verdict === "pass" ? 0 : 1;
      return;
    }
    if (second === "behavioral") {
      const scenariosDir = path.join(repoPath, manifest.ax?.scenariosDir ?? "evals/ax/scenarios");
      const driver = flag("driver", "stub");
      const k = Number(flag("k", "3"));
      // F2: --agents is the caller's explicit declared roster (comma-
      // separated distinct agent identifiers); runScenarios only credits
      // real-matrix status to trials whose agentId is a declared member —
      // see ax/behavioral.mjs's scenarioMeetsRealMatrixBar for why this is a
      // caller contract, not something this CLI can verify on its own.
      const agents = parseAgentsFlag(flag("agents"));
      // F11 — SCOPED, not fixed: --driver accepts any name, but only "stub"
      // actually executes anything here. There is no --run-trial-command (or
      // equivalent) that spawns/talks to a real external agent process per
      // trial, so passing --driver claude-opus (for example) still calls
      // runTrial, which is undefined, and every trial fails with "driver
      // produced no trial". Building that bridge means choosing which
      // agent CLI(s) to shell out to, the trial invocation/prompt shape, and
      // a timeout/cost policy — DESIGN.md OD-2 ("behavioral matrix
      // composition") is exactly that open decision, and it is not decided
      // here; fabricating a driver integration would be inventing that
      // decision, not fixing a bug.
      const result = await runScenarios({ scenariosDir, k, driver, agents });
      printJson(result);
      // F11 fix: this was previously `result.fatal ? 1 : 0`, which ignored
      // result.verdict entirely — a total-failure run (every trial failed,
      // fatal still null) exited 0. Exit code now matches static/conformance:
      // it reflects the command's own verdict. Per-repo severity (whether a
      // failing behavioral gate blocks `ax report`'s aggregate exit code) is
      // aggregateReport's job, not this subcommand's.
      process.exitCode = result.verdict === "pass" ? 0 : 1;
      return;
    }
    if (second === "report") {
      const [staticResult, conformanceResult] = await Promise.all([
        runStaticChecks({ repoPath, manifest }),
        runConformance({ repoPath, manifest }),
      ]);
      let behavioralResult = null;
      if (manifest.ax?.scenariosDir) {
        const scenariosDir = path.join(repoPath, manifest.ax.scenariosDir);
        behavioralResult = await runScenarios({ scenariosDir, k: Number(flag("k", "3")), driver: flag("driver", "stub"), agents: parseAgentsFlag(flag("agents")) });
      }
      const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult });
      // CLAIM-FIDELITY: pass all three raw gate results, not just static —
      // see ax/report.mjs's toSarif for why a static-only SARIF can read
      // clean while gateResult is "fail".
      const sarif = toSarif({ staticResult, conformanceResult, behavioralResult });
      const format = flag("format", "json");
      if (format === "sarif") printJson(sarif);
      else if (format === "markdown" || format === "md") console.log(renderMarkdown(report));
      else printJson(report);
      process.exitCode = gateExitCode(report);
      return;
    }
    console.error("right-ax ax: expected static, conformance, behavioral, or report");
    process.exitCode = 2;
    return;
  }

  if (first === "adopt") {
    const repoPath = resolveRepoPath(rest[0]?.startsWith("--") ? undefined : rest[0]);
    const result = adoptRepo(repoPath, { scenariosDir: flag("scenarios-dir"), serve: flag("serve"), write: flag("dry-run") !== undefined ? false : undefined });
    printJson(result);
    process.exitCode = result.ok ? 0 : 1;
    return;
  }

  console.error(`right-ax: unknown command: ${first}\n`);
  printHelp();
  process.exitCode = 2;
}

function printHelp() {
  console.log(`right-ax <command> [options]

Commands:
  plugin init <repo-path> [--name] [--version] [--description] [--license] [--homepage] [--mcp-command] [--skill-name]
                                              Scaffold plugin.json / skills/<name>/SKILL.md / mcp.json
  plugin validate <repo-path>                Validate agent-plugins.org layout + path-safety rules
  ax static <repo-path>                      Phase B contract checklist (14 rules, DESIGN §6)
  ax conformance <repo-path>                 Phase C live stdio probes against .rightax.json's MCP surface
  ax behavioral <repo-path> [--driver stub|<name>] [--k <n>] [--agents a,b]
                                              Phase D scenario runner; UNPROVEN until a real matrix runs.
                                              --agents declares the distinct-agent roster (required for
                                              real-matrix; see DESIGN.md OD-2 — no bundled real driver ships)
  ax report <repo-path> [--format json|sarif|markdown] [--driver] [--k] [--agents a,b]
                                              Aggregate scorecard; gates and reports, never signs off
  adopt <repo-path> [--scenarios-dir] [--serve] [--dry-run]
                                              Infer and write .rightax.json from an existing harness

Package root: ${packageRoot}`);
}

main().catch((error) => {
  console.error(`right-ax: crashed: ${error?.stack ?? error}`);
  process.exitCode = 1;
});
