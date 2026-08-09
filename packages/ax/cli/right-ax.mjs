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
import { runScenarios, runMatrix } from "../ax/behavioral.mjs";
import { runAdversarial } from "../ax/adversarial.mjs";
import { resolveArtifact } from "../ax/artifact.mjs";
import { aggregateReport, gateExitCode, renderMarkdown, toSarif } from "../ax/report.mjs";
import { runStaticChecks } from "../ax/static.mjs";
import { loadManifest } from "../manifest.mjs";
import { PACKAGE_VERSION } from "../lib/version.mjs";
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

function hasMcp(manifest) {
  return Boolean(manifest?.surfaces?.mcp?.command);
}

function configured(manifest, gate) {
  return Object.hasOwn(manifest?.ax?.gates ?? {}, gate);
}

function usage(message) {
  console.error(`right-ax: ${message}`);
  process.exit(2);
}

function requireArtifact(artifactRoot, message) {
  if (!artifactRoot) usage(`${message}; --artifact-root <external clean-room directory> is required`);
}

function parseAdoptServe(argv) {
  const serveIndex = argv.indexOf("--serve");
  const argIndexes = argv.reduce((indexes, value, index) => value === "--serve-arg" ? [...indexes, index] : indexes, []);
  if (serveIndex === -1) {
    if (argIndexes.length) usage("--serve-arg requires --serve <program>");
    return undefined;
  }
  const command = argv[serveIndex + 1];
  if (!command || command.startsWith("--")) usage("--serve requires one program token");
  const args = argIndexes.map((index) => {
    const value = argv[index + 1];
    if (!value) usage("--serve-arg requires one argv token");
    return value;
  });
  return { command, args };
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
  if (first === "--version" || first === "-v") { console.log(`right-ax ${PACKAGE_VERSION}`); return; }

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
    const artifactRoot = flag("artifact-root");
    const matrix = rest.includes("--matrix");

    if (second === "static") {
      if (hasMcp(manifest)) requireArtifact(artifactRoot, "ax static with an MCP surface");
      const result = await runStaticChecks({ repoPath, manifest, artifactRoot });
      printJson(result);
      process.exitCode = result.verdict === "pass" ? 0 : 1;
      return;
    }
    if (second === "conformance") {
      if (!hasMcp(manifest)) usage("ax conformance requires a manifest MCP surface");
      requireArtifact(artifactRoot, "ax conformance");
      const result = await runConformance({ repoPath, manifest, artifactRoot });
      printJson(result);
      process.exitCode = result.verdict === "pass" ? 0 : 1;
      return;
    }
    if (second === "behavioral") {
      const scenariosDir = path.join(repoPath, manifest.ax.scenariosDir);
      if (rest.includes("--driver") || rest.includes("--agents")) usage("ax behavioral uses manifest adapters; pass --matrix");
      if (matrix && !manifest.ax.matrix) usage("ax behavioral --matrix requires manifest matrix");
      if (matrix) requireArtifact(artifactRoot, "ax behavioral --matrix");
      const result = matrix
        ? await runMatrix({ repoPath, artifactRoot, scenariosDir, matrix: manifest.ax.matrix })
        : await runScenarios({ scenariosDir, k: Number(flag("k", "3")), driver: "stub" });
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
    if (second === "adversarial") {
      if (!manifest.ax?.adversarial) usage("ax adversarial requires manifest ax.adversarial configuration");
      if (!hasMcp(manifest)) usage("ax adversarial requires a manifest MCP surface");
      requireArtifact(artifactRoot, "ax adversarial");
      const result = await runAdversarial({ repoPath, manifest, artifactRoot, casesDir: path.join(repoPath, manifest.ax.adversarial.casesDir) });
      printJson(result);
      process.exitCode = result.verdict === "pass" ? 0 : 1;
      return;
    }
    if (second === "report") {
      const gates = manifest.ax?.gates ?? {};
      const needsArtifact = (hasMcp(manifest) && (configured(manifest, "static") || configured(manifest, "conformance")))
        || (configured(manifest, "adversarial") && Boolean(manifest.ax?.adversarial))
        || (configured(manifest, "behavioral") && Boolean(manifest.ax?.matrix));
      if (needsArtifact) requireArtifact(artifactRoot, "ax report");
      const staticResult = configured(manifest, "static") ? await runStaticChecks({ repoPath, manifest, artifactRoot }) : null;
      const conformanceResult = configured(manifest, "conformance") ? await runConformance({ repoPath, manifest, artifactRoot }) : null;
      let behavioralResult = null;
      if (configured(manifest, "behavioral")) {
        const scenariosDir = path.join(repoPath, manifest.ax.scenariosDir);
        behavioralResult = manifest.ax.matrix
          ? await runMatrix({ repoPath, artifactRoot, scenariosDir, matrix: manifest.ax.matrix })
          : await runScenarios({ scenariosDir, k: Number(flag("k", "3")), driver: "stub" });
      }
      const adversarialResult = configured(manifest, "adversarial") && manifest.ax?.adversarial
        ? await runAdversarial({ repoPath, manifest, artifactRoot, casesDir: path.join(repoPath, manifest.ax.adversarial.casesDir) })
        : null;
      const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult, adversarialResult });
      // CLAIM-FIDELITY: pass all three raw gate results, not just static —
      // see ax/report.mjs's toSarif for why a static-only SARIF can read
      // clean while gateResult is "fail".
      const sarif = toSarif({ staticResult, conformanceResult, behavioralResult, adversarialResult });
      const format = flag("format", "json");
      if (format === "sarif") printJson(sarif);
      else if (format === "markdown" || format === "md") console.log(renderMarkdown(report));
      else printJson(report);
      process.exitCode = gateExitCode(report);
      return;
    }
    console.error("right-ax ax: expected static, conformance, behavioral, adversarial, or report");
    process.exitCode = 2;
    return;
  }

  if (first === "adopt") {
    const repoPath = resolveRepoPath(rest[0]?.startsWith("--") ? undefined : rest[0]);
    const result = adoptRepo(repoPath, { scenariosDir: flag("scenarios-dir"), serve: parseAdoptServe(rest), write: flag("dry-run") !== undefined ? false : undefined });
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
  ax static <repo-path> [--artifact-root <dir>]
                                              Phase B contract checklist; artifact required for MCP surfaces
  ax conformance <repo-path> --artifact-root <dir>
                                              Phase C live stdio probes against packaged MCP surface
  ax behavioral <repo-path> [--matrix --artifact-root <dir>] [--k <n>]
                                              Phase D stub or configured real-adapter matrix
  ax adversarial <repo-path> --artifact-root <dir>
                                              Phase E packaged MCP security probes
  ax report <repo-path> [--artifact-root <dir>] [--format json|sarif|markdown] [--k]
                                              Aggregate scorecard; gates and reports, never signs off
  adopt <repo-path> [--scenarios-dir] [--serve <program> [--serve-arg <argv>]...] [--dry-run]
                                              Infer and write .rightax.json from an existing harness

Artifact roots are external clean-room directories, absolute or repo-relative paths resolving outside source repo.

Package root: ${packageRoot}`);
}

main().catch((error) => {
  console.error(`right-ax: crashed: ${error?.stack ?? error}`);
  process.exitCode = 1;
});
