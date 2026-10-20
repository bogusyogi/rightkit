import { requiredShardCommands } from "./shards.mjs";

/**
 * Validate compile-only proof for explicit crate-selection dependents. Expected commands come from
 * the same frozen rust-check command policy as sharded CI; actual runner results are retained as
 * data and never define what was required.
 */
export function dependentClosureEvidence({ selected, requested = [], config = {}, results = [] }) {
  const names = [...(selected ?? [])];
  const problems = [];
  if (new Set(names).size !== names.length) problems.push("duplicate crate in dependent-check selection");

  let expected = [];
  try {
    expected = names.length ? requiredShardCommands(names.map((name) => `rust-check:${name}`).join(" "), { config }) : [];
  } catch (error) {
    problems.push(`invalid dependent-check plan: ${error.message}`);
  }

  const units = new Set(names.map((name) => `rust:${name}`));
  const requestedUnits = new Set(requested.map((name) => `rust:${name}`));
  const rustResults = results.filter((result) => typeof result.unit === "string" && result.unit.startsWith("rust:") && !requestedUnits.has(result.unit));
  const actual = rustResults.map((result) => ({
    unit: result.unit,
    command: result.command,
    ok: result.ok === true,
    ...(result.code !== undefined ? { code: result.code } : {}),
    ...(result.timedOut !== undefined ? { timedOut: result.timedOut } : {}),
    ...(result.seconds !== undefined ? { seconds: result.seconds } : {}),
  }));
  for (const result of actual) {
    if (!units.has(result.unit)) problems.push(`unexpected dependent-check unit: ${result.unit}`);
  }

  const expectedKeys = new Set();
  for (const command of expected) {
    const key = `${command.unit}\n${command.command}`;
    if (expectedKeys.has(key)) problems.push(`duplicate required dependent-check command: ${command.command}`);
    expectedKeys.add(key);
    const matches = actual.filter((result) => result.unit === command.unit && result.command === command.command);
    if (matches.length === 0) problems.push(`missing required dependent-check command: ${command.command}`);
    else if (matches.length > 1) problems.push(`duplicate dependent-check result: ${command.command}`);
    else {
      const result = matches[0];
      if (!Number.isSafeInteger(result.code) || !Number.isSafeInteger(result.seconds) || result.seconds < 0 || typeof result.timedOut !== "boolean") {
        problems.push(`invalid dependent-check result fields: ${command.command}`);
      }
      if (result.ok !== true || result.code !== 0 || result.timedOut !== false) problems.push(`failed dependent-check command: ${command.command}`);
    }
  }
  for (const result of actual) {
    if (!expectedKeys.has(`${result.unit}\n${result.command}`)) problems.push(`unexpected dependent-check result: ${result.command}`);
  }

  return { selection: names, expected, results: actual, ok: problems.length === 0, problems };
}
