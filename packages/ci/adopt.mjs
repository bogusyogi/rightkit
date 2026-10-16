// Pull an existing repo's hand-written workflows into review-ready template
// candidates (DESIGN.md §4, "Cortex bootstrap"). Read-only against the
// source repo. Never writes into templates/ directly — adopt's output is
// mapped lane content for a human (or a follow-up Sage-authorized change)
// to review and fold in; silently overwriting the curated, parameterized
// template library from an arbitrary repo's raw workflow text is exactly
// the kind of unreviewed mutation this package must not perform on its own.

import fs from "node:fs";
import path from "node:path";
import { LANE_FILENAMES } from "./manifest.mjs";

// filename (in the source repo's .github/workflows/) -> lane, for the
// lanes that map 1:1 onto an existing file.
const FILENAME_TO_LANE = {
  "ci.yml": "ci",
  "qualification.yml": "qualification",
  "package.yml": "package-smoke",
  "package-smoke.yml": "package-smoke",
  "release-candidate.yml": "release-candidate",
  "publish-npm.yml": "publish-npm",
  "publish-package-managers.yml": "publish-package-managers",
};

/** Slice out one top-level job block from a workflow's `jobs:` map by name or predicate. */
export function extractJobBlock(yamlText, jobNameOrPredicate) {
  const lines = yamlText.split("\n");
  const jobsIdx = lines.findIndex((l) => /^jobs:\s*$/.test(l));
  if (jobsIdx === -1) return null;

  const jobStarts = [];
  for (let i = jobsIdx + 1; i < lines.length; i++) {
    if (/^ {2}\S.*:\s*$/.test(lines[i])) jobStarts.push(i);
  }

  for (let idx = 0; idx < jobStarts.length; idx++) {
    const start = jobStarts[idx];
    const end = idx + 1 < jobStarts.length ? jobStarts[idx + 1] : lines.length;
    const nameMatch = /^ {2}(\S+):/.exec(lines[start]);
    const name = nameMatch ? nameMatch[1] : null;
    const block = lines.slice(start, end).join("\n");
    const matches = typeof jobNameOrPredicate === "function" ? jobNameOrPredicate(name, block) : name === jobNameOrPredicate;
    if (matches) return { name, block };
  }
  return null;
}

/**
 * Inspect <repoPath>/.github/workflows/*.yml and map filenames onto known
 * right-git lanes. Never throws, never writes.
 * Returns { ok, mapped: [{lane, sourceFile, content}], unmapped: string[], errors }
 */
export function adoptCommand(repoPath) {
  const dir = path.join(repoPath, ".github", "workflows");
  let entries;
  try {
    entries = fs.readdirSync(dir).filter((n) => n.endsWith(".yml") || n.endsWith(".yaml"));
  } catch (error) {
    return { ok: false, mapped: [], unmapped: [], errors: [`cannot read ${dir}: ${error.message}`] };
  }

  const mapped = [];
  const unmapped = [];
  const foundLanes = new Set();

  for (const filename of entries) {
    const lane = FILENAME_TO_LANE[filename];
    if (lane) {
      const content = fs.readFileSync(path.join(dir, filename), "utf8");
      mapped.push({ lane, sourceFile: filename, content });
      foundLanes.add(lane);
      continue;
    }
    unmapped.push(filename);
  }

  // publish-npm has no 1:1 file in the Cortex-shaped source layout — it is a
  // single job inside a larger release workflow (DESIGN.md §6). Best-effort
  // extraction by scanning any remaining file for a job whose steps run
  // `npm publish`.
  if (!foundLanes.has("publish-npm")) {
    for (const filename of unmapped.slice()) {
      const content = fs.readFileSync(path.join(dir, filename), "utf8");
      const job = extractJobBlock(content, (_name, block) => /npm publish/.test(block));
      if (job) {
        mapped.push({ lane: "publish-npm", sourceFile: `${filename}#${job.name}`, content: job.block });
        foundLanes.add("publish-npm");
        break;
      }
    }
  }

  return { ok: true, mapped, unmapped, errors: [] };
}

export { LANE_FILENAMES };
