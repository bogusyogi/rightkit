// `right-ax adopt <repo-path>` — pull an existing product-local AX harness
// into this package (the Cortex bootstrap, DESIGN §5). It never copies
// Cortex's (or any product's) source files into RightKit (I3: engine here,
// evidence in the product repo); instead it inspects the product's existing
// evals/ax/-shaped layout and emits the .rightax.json that lets the shared
// engine run against it, plus a short adoption report of what it found and
// what it could not infer.

import { existsSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";

const CANDIDATE_SCENARIO_DIRS = ["evals/ax/scenarios", "ax/scenarios", "test/ax/scenarios"];
const CANDIDATE_SERVE_HINTS = [{ file: "scripts/cortex-mcp.mjs", serve: "node scripts/cortex-mcp.mjs" }];

function findScenariosDir(repoPath) {
  for (const relPath of CANDIDATE_SCENARIO_DIRS) {
    const path = join(repoPath, relPath);
    if (existsSync(path) && statSync(path).isDirectory()) return relPath;
  }
  return null;
}

function findServeCommand(repoPath, packageJson) {
  for (const candidate of CANDIDATE_SERVE_HINTS) if (existsSync(join(repoPath, candidate.file))) return candidate.serve;
  const binEntries = Object.entries(packageJson?.bin ?? {});
  if (binEntries.length) return `node ${binEntries[0][1]}`;
  return null;
}

function readPackageJsonSafe(repoPath) {
  const path = join(repoPath, "package.json");
  if (!existsSync(path)) return null;
  try { return JSON.parse(readFileSync(path, "utf8")); } catch { return null; }
}

export function adoptRepo(repoPath, options = {}) {
  const packageJson = readPackageJsonSafe(repoPath);
  const scenariosDir = options.scenariosDir ?? findScenariosDir(repoPath);
  const serve = options.serve ?? findServeCommand(repoPath, packageJson);
  const findings = [];
  if (!scenariosDir) findings.push({ code: "no_scenarios_dir", message: `no scenarios directory found under ${CANDIDATE_SCENARIO_DIRS.join(", ")}; pass --scenarios-dir explicitly` });
  if (!serve) findings.push({ code: "no_serve_command", message: "could not infer an MCP serve command; pass --serve explicitly" });

  const manifest = {
    schemaVersion: 1,
    surfaces: serve ? { mcp: { serve } } : {},
    plugin: packageJson?.name ? { name: packageJson.name.replace(/^@[^/]+\//, "") } : {},
    ax: {
      ...(scenariosDir ? { scenariosDir } : {}),
      gates: { static: "report-only", conformance: "report-only", behavioral: "report-only" },
      claimBoundaryRequired: false,
    },
  };

  const written = [];
  if (options.write !== false) {
    const manifestPath = join(repoPath, ".rightax.json");
    writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
    written.push(relative(repoPath, manifestPath));
  }

  return { manifest, findings, written, ok: findings.length === 0 };
}
