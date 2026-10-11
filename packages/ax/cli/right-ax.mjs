#!/usr/bin/env node
// Dispatcher for right-ax, mirroring right-release's CLI shape
// (rightkit/packages/release/cli/right-release.mjs): a thin argv
// router into the package's library modules, in-process (no child spawn per
// command — the modules already spawn the target's own MCP server when a
// gate needs a live process).
//
// Argument parsing is table-driven (COMMANDS). Every leaf subcommand declares
// its positionals, value options and boolean options. Before any command runs:
//   * --help / -h anywhere after the command prints that command's usage and exits 0;
//   * an undeclared flag, a stray positional, or a target starting with "-" exits 2;
//   * nothing touches the filesystem on a parse failure.

import path from "node:path";
import { existsSync, mkdirSync, rmdirSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { adoptRepo } from "../adopt.mjs";
import { runConformance } from "../ax/conformance.mjs";
import { runScenarios, runMatrix } from "../ax/behavioral.mjs";
import { runAdversarial } from "../ax/adversarial.mjs";
import { assertOwnershipReviewPacket, buildOwnershipReviewPacket, readOwnershipReviewJson, validateOwnershipReviewReceipt, writeOwnershipReviewPacket } from "../ax/ownership-review.mjs";
import { resolveArtifact } from "../ax/artifact.mjs";
import { aggregateReport, gateExitCode, renderMarkdown, toSarif } from "../ax/report.mjs";
import { runStaticChecks } from "../ax/static.mjs";
import { loadManifest } from "../manifest.mjs";
import { PACKAGE_VERSION } from "../lib/version.mjs";
import { buildMarketplace, validateMarketplace } from "../plugin/marketplace.mjs";
import { scaffoldPlugin } from "../plugin/scaffold.mjs";
import { validateRepoLayout } from "../plugin/validate.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const [first, second] = args;

// positionals: "required" (exactly one target), "optional" (zero or one, default "."), or "none".
// values: long options that take the next token as their value.
// bools: long options that take no value.
// Group entries (plugin, marketplace, ax, ownership-review) carry usage only.
const COMMANDS = {
  "plugin": {
    usage: "right-ax plugin init <repo-path> [options] | right-ax plugin validate <repo-path>",
  },
  "plugin init": {
    usage: [
      "right-ax plugin init <repo-path> [--name] [--version] [--description] [--license] [--homepage] [--mcp-command] [--skill-name] [--force]",
      "              [--cli <program> --skill-description <text> [--commands a,b] [--hook-args \"<subcommand>\" [--hook-events E1,E2]]]",
      "  <repo-path> is required and is created when missing. An existing plugin.json is refused unless --force is given.",
    ].join("\n"),
    positionals: "required",
    values: ["name", "version", "description", "license", "homepage", "mcp-command", "skill-name", "cli", "skill-description", "commands", "hook-args", "hook-events"],
    bools: ["force"],
  },
  "plugin validate": {
    usage: "right-ax plugin validate <repo-path>",
    positionals: "optional",
    values: [],
    bools: [],
  },
  "marketplace": {
    usage: "right-ax marketplace build [--sources ...] [--out <dir>] [--check] | right-ax marketplace validate [--sources ...] [--out <dir>] (--workspace <dir of checkouts, or the marketplace repository root> | --fetch) [--no-drift]",
  },
  "marketplace build": {
    usage: "right-ax marketplace build [--sources marketplace/plugins.json] [--out .] [--check]",
    positionals: "none",
    values: ["sources", "out"],
    bools: ["check"],
  },
  "marketplace validate": {
    usage: "right-ax marketplace validate [--sources marketplace/plugins.json] [--out .] (--workspace <dir of checkouts, or the marketplace repository root> | --fetch) [--no-drift]",
    positionals: "none",
    values: ["sources", "out", "workspace"],
    bools: ["fetch", "no-drift"],
  },
  "ax": {
    usage: "right-ax ax <static|conformance|behavioral|adversarial|report> <repo-path> [--artifact-root <dir>]",
  },
  "ax static": {
    usage: "right-ax ax static <repo-path> [--artifact-root <dir>]",
    positionals: "optional",
    values: ["artifact-root"],
    bools: [],
  },
  "ax conformance": {
    usage: "right-ax ax conformance <repo-path> --artifact-root <dir>",
    positionals: "optional",
    values: ["artifact-root"],
    bools: [],
  },
  "ax behavioral": {
    usage: "right-ax ax behavioral <repo-path> [--matrix --artifact-root <dir>] [--k <n>]",
    positionals: "optional",
    // driver and agents are removed flags; they stay declared so the existing
    // "uses manifest adapters" refusal still fires instead of a generic unknown-option error.
    values: ["artifact-root", "k", "driver", "agents"],
    bools: ["matrix"],
  },
  "ax adversarial": {
    usage: "right-ax ax adversarial <repo-path> --artifact-root <dir>",
    positionals: "optional",
    values: ["artifact-root"],
    bools: [],
  },
  "ax report": {
    usage: "right-ax ax report <repo-path> [--artifact-root <dir>] [--format json|sarif|markdown] [--k <n>]",
    positionals: "optional",
    values: ["artifact-root", "format", "k"],
    bools: [],
  },
  "adopt": {
    usage: "right-ax adopt <repo-path> [--scenarios-dir <dir>] [--serve <program> [--serve-arg <argv>]...] [--dry-run]",
    positionals: "optional",
    values: ["scenarios-dir", "serve", "serve-arg"],
    bools: ["dry-run"],
  },
  "ownership-review": {
    usage: "right-ax ownership-review prepare <repo-path> --base <commit> --head <commit> [--out <file>] | right-ax ownership-review verify --packet <file> --receipt <file>",
  },
  "ownership-review prepare": {
    usage: "right-ax ownership-review prepare <repo-path> --base <commit> --head <commit> [--out <file>]",
    positionals: "optional",
    values: ["base", "head", "out"],
    bools: [],
  },
  "ownership-review verify": {
    usage: "right-ax ownership-review verify --packet <file> --receipt <file>",
    positionals: "none",
    values: ["packet", "receipt"],
    bools: [],
  },
};

class UsageError extends Error {
  constructor(key, message) {
    super(message);
    this.key = key;
  }
}

function spec(key) {
  return Object.hasOwn(COMMANDS, key) ? COMMANDS[key] : undefined;
}

function commandKey(command, subcommand) {
  const pair = `${command} ${subcommand}`;
  if (spec(pair)) return pair;
  return command;
}

function parseOptions(key, tokens) {
  const commandSpec = spec(key);
  const values = new Map();
  const bools = new Set();
  const positionals = [];
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (!token.startsWith("-")) {
      positionals.push(token);
      continue;
    }
    const name = token.startsWith("--") ? token.slice(2) : null;
    if (name !== null && commandSpec.values.includes(name)) {
      if (index + 1 >= tokens.length) throw new UsageError(key, `${key}: --${name} requires a value`);
      index += 1;
      values.set(name, tokens[index]);
      continue;
    }
    if (name !== null && commandSpec.bools.includes(name)) {
      bools.add(name);
      continue;
    }
    const hint = positionals.length === 0 && commandSpec.positionals !== "none" ? " (a target path that starts with - is refused)" : "";
    throw new UsageError(key, `${key}: unknown option ${token}${hint}`);
  }
  if (commandSpec.positionals === "none" && positionals.length) throw new UsageError(key, `${key}: unexpected argument ${positionals[0]}`);
  if (positionals.length > 1) throw new UsageError(key, `${key}: expected one target path, got ${positionals.length}`);
  if (commandSpec.positionals === "required" && positionals.length === 0) throw new UsageError(key, `${key}: requires an explicit target directory, for example: right-ax ${key} ./my-app`);
  if (positionals[0] === "") throw new UsageError(key, `${key}: target directory must not be empty`);
  return { positional: positionals[0], values, bools };
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

  const key = commandKey(first, second);
  // adopt takes its target as the second token; every other command takes it after the subcommand.
  const tokens = first === "adopt" ? args.slice(1) : args.slice(2);

  if (args.slice(1).some((token) => token === "--help" || token === "-h")) return printUsage(key);

  const parsed = spec(key)?.values
    ? parseOptions(key, tokens)
    : { positional: undefined, values: new Map(), bools: new Set() };
  const opt = (name, fallback = undefined) => (parsed.values.has(name) ? parsed.values.get(name) : fallback);
  const has = (name) => parsed.bools.has(name);

  if (first === "ownership-review") {
    if (second === "prepare") {
      const repoPath = resolveRepoPath(parsed.positional);
      const base = opt("base");
      const head = opt("head");
      if (!base || !head) throw new UsageError(key, "ownership-review prepare requires --base <commit> and --head <commit>");
      const packet = buildOwnershipReviewPacket({ repoPath, base, head });
      const output = opt("out");
      if (output) writeOwnershipReviewPacket(path.resolve(output), packet);
      else printJson(packet);
      return;
    }
    if (second === "verify") {
      const packetPath = opt("packet");
      const receiptPath = opt("receipt");
      if (!packetPath || !receiptPath) throw new UsageError(key, "ownership-review verify requires --packet <file> and --receipt <file>");
      const packet = assertOwnershipReviewPacket(readOwnershipReviewJson(path.resolve(packetPath)));
      const receipt = readOwnershipReviewJson(path.resolve(receiptPath));
      const report = validateOwnershipReviewReceipt(packet, receipt);
      printJson(report);
      if (!report.valid) process.exitCode = 2;
      return;
    }
    console.error("right-ax ownership-review: expected prepare or verify");
    process.exitCode = 2;
    return;
  }

  if (first === "plugin") {
    if (second === "init") {
      const repoPath = resolveRepoPath(parsed.positional);
      const force = has("force");
      let createdDir = false;
      if (!existsSync(repoPath)) {
        try {
          mkdirSync(repoPath, { recursive: true });
          createdDir = true;
        } catch (error) {
          console.error(`right-ax plugin init: cannot create target directory ${repoPath}: ${error?.message ?? error}`);
          process.exitCode = 2;
          return;
        }
      } else if (!statSync(repoPath).isDirectory()) {
        console.error(`right-ax plugin init: target is not a directory: ${repoPath}`);
        process.exitCode = 2;
        return;
      }
      const manifestPath = path.join(repoPath, "plugin.json");
      if (!force && existsSync(manifestPath)) {
        console.error(`right-ax plugin init: ${manifestPath} already exists; pass --force to overwrite it`);
        process.exitCode = 2;
        return;
      }
      const list = (name) => (opt(name) ?? "").split(",").map((item) => item.trim()).filter(Boolean);
      let result;
      try {
        result = scaffoldPlugin(repoPath, {
          name: opt("name"),
          version: opt("version"),
          description: opt("description"),
          license: opt("license"),
          homepage: opt("homepage"),
          mcpCommand: opt("mcp-command"),
          skillName: opt("skill-name"),
          cli: opt("cli"),
          skillDescription: opt("skill-description"),
          commands: list("commands"),
          hookArgs: opt("hook-args"),
          hookEvents: opt("hook-events") ? list("hook-events") : undefined,
          referencedFiles: [],
        });
      } catch (error) {
        if (createdDir) {
          try { rmdirSync(repoPath); } catch { /* not empty or already gone: leave it */ }
        }
        console.error(`right-ax plugin init: ${error?.message ?? error}`);
        process.exitCode = 2;
        return;
      }
      console.log(`right-ax plugin init: wrote ${result.written.length} file(s)`);
      for (const file of result.written) console.log(`  - ${file}`);
      return;
    }
    if (second === "validate") {
      const repoPath = resolveRepoPath(parsed.positional);
      const { valid, layout, findings } = validateRepoLayout(repoPath);
      printJson({ valid, layout, findings });
      process.exitCode = valid ? 0 : 1;
      return;
    }
    console.error("right-ax plugin: expected init or validate");
    process.exitCode = 2;
    return;
  }

  if (first === "marketplace") {
    const sourcesPath = path.resolve(opt("sources", "marketplace/plugins.json"));
    const outDir = path.resolve(opt("out", "."));
    if (second === "build") {
      const result = buildMarketplace({ sourcesPath, outDir, check: has("check") });
      printJson(result);
      process.exitCode = result.ok ? 0 : 1;
      return;
    }
    if (second === "validate") {
      const result = validateMarketplace({ sourcesPath, outDir, workspace: opt("workspace"), fetch: has("fetch"), skipDrift: has("no-drift") });
      printJson(result);
      process.exitCode = result.valid ? 0 : 1;
      return;
    }
    console.error("right-ax marketplace: expected build or validate");
    process.exitCode = 2;
    return;
  }

  if (first === "ax") {
    const repoPath = resolveRepoPath(parsed.positional);
    const manifest = requireManifest(repoPath);
    const artifactRoot = opt("artifact-root");
    const matrix = has("matrix");

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
      if (parsed.values.has("driver") || parsed.values.has("agents")) usage("ax behavioral uses manifest adapters; pass --matrix");
      if (matrix && !manifest.ax.matrix) usage("ax behavioral --matrix requires manifest matrix");
      if (matrix) requireArtifact(artifactRoot, "ax behavioral --matrix");
      const result = matrix
        ? await runMatrix({ repoPath, artifactRoot, scenariosDir, matrix: manifest.ax.matrix })
        : await runScenarios({ scenariosDir, k: Number(opt("k", "3")), driver: "stub" });
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
          : await runScenarios({ scenariosDir, k: Number(opt("k", "3")), driver: "stub" });
      }
      const adversarialResult = configured(manifest, "adversarial") && manifest.ax?.adversarial
        ? await runAdversarial({ repoPath, manifest, artifactRoot, casesDir: path.join(repoPath, manifest.ax.adversarial.casesDir) })
        : null;
      const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult, adversarialResult });
      // CLAIM-FIDELITY: pass all three raw gate results, not just static —
      // see ax/report.mjs's toSarif for why a static-only SARIF can read
      // clean while gateResult is "fail".
      const sarif = toSarif({ staticResult, conformanceResult, behavioralResult, adversarialResult });
      const format = opt("format", "json");
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
    const repoPath = resolveRepoPath(parsed.positional);
    const result = adoptRepo(repoPath, { scenariosDir: opt("scenarios-dir"), serve: parseAdoptServe(tokens), write: has("dry-run") ? false : undefined });
    printJson(result);
    process.exitCode = result.ok ? 0 : 1;
    return;
  }

  console.error(`right-ax: unknown command: ${first}\n`);
  printHelp();
  process.exitCode = 2;
}

function printUsage(key) {
  const entry = spec(key);
  if (!entry) return printHelp();
  console.log(`usage: ${entry.usage}\n\nRun \`right-ax --help\` for every command.`);
}

function printHelp() {
  console.log(`right-ax <command> [options]

Commands:
  plugin init <repo-path> [--name] [--version] [--description] [--license] [--homepage] [--mcp-command] [--skill-name] [--force]
              [--cli <program> --skill-description <text> [--commands a,b] [--hook-args "<subcommand>" [--hook-events E1,E2]]]
                                              Scaffold plugin.json / skills/<name>/SKILL.md (/ mcp.json).
                                              The target directory is required and is created when missing.
                                              An existing plugin.json is refused unless --force is given.
                                              --cli scaffolds a CLI-skill plugin (no MCP) with the
                                              Claude and Codex manifests; --hook-args adds hooks/.
  plugin validate <repo-path>                Validate layout, path safety, hooks, and the skill description gate
  marketplace build [--sources marketplace/plugins.json] [--out .] [--check]
                                              Generate the suite marketplace manifest(s) from the source list
  marketplace validate [--sources ...] [--out .] (--workspace <dir of checkouts, or the marketplace repository root> | --fetch) [--no-drift]
                                              Source list valid, manifests in sync, every plugin resolves and validates
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
  ownership-review prepare <repo-path> --base <commit> --head <commit> [--out <file>]
                                              Prepare bounded, content/catalog-deduped advisory packet
  ownership-review verify --packet <file> --receipt <file>
                                              Validate supervised receipt; findings remain advisory

Artifact roots are external clean-room directories, absolute or repo-relative paths resolving outside source repo.

Package root: ${packageRoot}`);
}

main().catch((error) => {
  if (error instanceof UsageError) {
    console.error(`right-ax: ${error.message}`);
    const entry = spec(error.key);
    if (entry) console.error(`usage: ${entry.usage}`);
    process.exitCode = 2;
    return;
  }
  console.error(`right-ax: crashed: ${error?.stack ?? error}`);
  process.exitCode = 1;
});
