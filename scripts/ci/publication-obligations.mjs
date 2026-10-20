// Trusted verifier policy over authenticated candidate source DATA. Never import candidate code.
import { parseLock, selectUnits } from "./select.mjs";
import { requiredConsumerLockCommands, requiredShardCommands } from "./shards.mjs";
import { judgeUnit, TEST_COMMAND } from "./test-results.mjs";

const signature = (row) => JSON.stringify([row?.unit, row?.command]);
const count = (value) => Number.isSafeInteger(value) && value >= 0;
const equal = (left, right) => JSON.stringify(left) === JSON.stringify(right);
const list = (value, label) => {
  if (!Array.isArray(value) || value.some((name) => typeof name !== "string" || !name || /\s/.test(name)) || new Set(value).size !== value.length) throw new Error(`invalid ${label}`);
  return [...value];
};

/**
 * Inputs must describe the COMPLETE candidate workspace inventory (not a producer plan).
 * Caller authenticates source bytes, requested units & waivers independently. This adapter
 * neither executes source nor asserts archive/Actions provenance or runtime qualification.
 */
export function derivePublicationObligations({ registry, units, host, config = {}, lockText = "", memberManifests = [], packageManifests = [], assetGated = {}, allow = {} }) {
  if (!["macos", "windows"].includes(host)) throw new Error("publication host must be macos or windows");
  units = list(units, "units");
  const allowHostExclusion = list(allow.allowHostExclusion ?? [], "allowHostExclusion");
  const allowNoTest = list(allow.allowNoTest ?? [], "allowNoTest");
  const allowIgnored = list(allow.allowIgnored ?? [], "allowIgnored");
  const allowMissingAssets = list(allow.allowMissingAssets ?? [], "allowMissingAssets");
  const members = memberManifests.map(({ dir, text }) => {
    if (typeof dir !== "string" || !/^crates\/[^/]+$/.test(dir) || typeof text !== "string") throw new Error("invalid member manifest data");
    // Same package-name parser as gate.mjs rustMembers; no TOML evaluation or code loading.
    const name = /^\[package\][^[]*?^name\s*=\s*"([^"]+)"/ms.exec(text)?.[1];
    if (!name) throw new Error(`missing package name in ${dir}/Cargo.toml`);
    return { name, dir, extraPaths: config.rust?.[name]?.paths ?? [] };
  });
  const names = new Set(packageManifests.map(({ json }) => json?.name));
  const packages = packageManifests.map(({ dir, json }) => {
    if (typeof dir !== "string" || !/^packages\/[^/]+$/.test(dir) || typeof json?.name !== "string" || !json.name) throw new Error("invalid package manifest data");
    return { name: json.name, dir, scripts: json.scripts ?? {}, deps: Object.keys({ ...json.dependencies, ...json.devDependencies, ...json.peerDependencies }).filter((name) => names.has(name)) };
  });
  for (const inventory of [members, packages]) {
    if (new Set(inventory.map((entry) => entry.name)).size !== inventory.length || new Set(inventory.map((entry) => entry.dir)).size !== inventory.length) throw new Error("duplicate source inventory entry");
    list(inventory.map((entry) => entry.name), "inventory names");
  }
  const selection = selectUnits({ registry, units, host, config, members, packages, lock: parseLock(lockText), assetGated, allowHostExclusion, allowNoTest, allowIgnored, allowMissingAssets });
  const tokens = [...selection.rust.test.map((name) => `rust:${name}`), ...selection.rust.check.map((name) => `rust-check:${name}`), ...selection.npm.test.map((dir) => `npm:${dir}`)].join(" ");
  const commands = [];
  if (selection.rust.test.length || selection.rust.check.length) commands.push({ unit: "lock", command: "cargo metadata --locked --format-version 1" });
  commands.push(...requiredConsumerLockCommands([...selection.rust.test, ...selection.rust.check].flatMap((name) => config.rust?.[name]?.consumerRows ?? [])).map(({ unit, command }) => ({ unit, command })));
  for (let index = 0; index < selection.rust.test.length; index += 15) commands.push({ unit: "fmt", command: `cargo fmt ${selection.rust.test.slice(index, index + 15).map((name) => `-p ${name}`).join(" ")}` });
  if (tokens) commands.push(...requiredShardCommands(tokens, { config, packages }));
  if (new Set(commands.map(signature)).size !== commands.length) throw new Error("duplicate required publication command");
  const unitPolicies = units.map((name) => ({
    name,
    id: registry === "crates" ? `rust:${name}` : `npm:${packages.find((pkg) => pkg.name === name).dir}`,
    ...(selection.excluded.includes(name) ? { status: "allowed-exclusion" } : selection.noTest.includes(name) ? { status: "allowed-no-test" } : {}),
    allowIgnored: allowIgnored.includes(name), allowMissingAssets: allowMissingAssets.includes(name),
  }));
  return { schema: 1, registry, units, host, commands, unitPolicies, selection, tokens, steps: "lock,fmt,test,closure", separateObligations: ["authenticated-source-and-Actions-provenance", "archive-consumer-and-runtime-qualification"] };
}

/**
 * summary is actual unit-mode summary.json, unitEvidence is actual unit-evidence.json.
 * No shard identity or historical metadata is invented. Producer expected lists are ignored.
 */
export function publicationObligationsVerdict(obligations, { summary, unitEvidence } = {}, sha) {
  const failures = [];
  const fail = (reason) => failures.push(reason);
  if (obligations?.schema !== 1 || !Array.isArray(obligations.commands) || !Array.isArray(obligations.unitPolicies)) return { ok: false, failures: ["invalid trusted publication obligations"] };
  if (typeof sha !== "string" || !/^[a-f0-9]{40}$/.test(sha)) fail("invalid authenticated commit");
  if (!summary || summary.sha !== sha || summary.host !== obligations.host || summary.selection !== null || summary.steps !== obligations.steps) fail("summary commit, host, selection or steps mismatch");
  const results = summary?.results;
  const rows = summary?.rows;
  if (!Array.isArray(results) || !Array.isArray(rows)) return { ok: false, failures: [...failures, "missing summary command proof"] };
  const expectedKeys = obligations.commands.map(signature).sort();
  if (new Set(expectedKeys).size !== expectedKeys.length || !equal(expectedKeys, results.map(signature).sort())) fail("command coverage differs from source-derived obligations (missing, duplicate or substituted command)");
  // Mirrors shards.evidenceVerdict command/aggregate validation, without imposing fake shard
  // metadata on a historical unit-mode report. Tests additionally require positive counts below.
  for (const result of results) {
    if (!result || result.ok !== true || result.code !== 0 || result.timedOut !== false || !count(result.seconds) || result.isTest !== TEST_COMMAND.test(result.command ?? "") || ["executed", "failed", "ignored", "skipped", "binaries"].some((key) => !count(result[key])) || result.failed !== 0) fail(`failed or incomplete command: ${result?.command ?? "missing"}`);
    if (result && !TEST_COMMAND.test(result.command ?? "") && ["executed", "ignored", "skipped", "binaries"].some((key) => result[key] !== 0)) fail(`non-test command carries test counts: ${result.command}`);
  }
  const unitIds = new Set(obligations.commands.map((command) => command.unit));
  const seen = new Set();
  for (const row of rows) {
    if (!row || seen.has(row.id) || !unitIds.has(row.id)) { fail("unexpected or duplicate summary unit row"); continue; }
    seen.add(row.id);
    const commands = results.filter((result) => result?.unit === row.id);
    if (row.ok !== true || row.known !== false || !Array.isArray(row.failed) || row.failed.length || !count(row.seconds) || !count(row.skipped) || row.seconds !== commands.reduce((sum, command) => sum + command.seconds, 0) || row.skipped !== commands.reduce((sum, command) => sum + command.skipped, 0)) fail(`failed or inconsistent summary row: ${row.id}`);
  }
  if (seen.size !== unitIds.size) fail("missing summary unit row");
  if (!unitEvidence || unitEvidence.schema !== 2 || unitEvidence.sha !== sha || unitEvidence.host !== obligations.host || unitEvidence.registry !== obligations.registry || unitEvidence.ok !== true || !Array.isArray(unitEvidence.units)) return { ok: false, failures: [...failures, "missing or mismatched unit evidence"] };
  const evidence = unitEvidence.units;
  if (!equal(evidence.map((row) => row?.name).sort(), obligations.units.slice().sort())) fail("unit evidence coverage differs from request");
  for (const policy of obligations.unitPolicies) {
    const matches = evidence.filter((row) => row?.name === policy.name);
    if (matches.length !== 1) { fail(`missing or duplicate unit evidence: ${policy.name}`); continue; }
    const actual = matches[0];
    const commands = results.filter((result) => result?.unit === policy.id);
    const expectedSteps = commands.map((result) => ({ command: result.command, ok: result.ok, seconds: result.seconds, ...(result.isTest ? Object.fromEntries(["executed", "failed", "ignored", "skipped", "binaries"].map((key) => [key, result[key]])) : {}) }));
    if (actual.id !== policy.id || !Array.isArray(actual.problems) || actual.problems.length || !equal(actual.steps, expectedSteps)) fail(`unit evidence disagrees with summary: ${policy.name}`);
    if (policy.status) {
      if (commands.length || actual.status !== policy.status || actual.executed !== 0 || actual.ignored !== 0 || actual.skipped !== 0) fail(`invalid explicit waiver: ${policy.name}`);
      continue;
    }
    const verdict = judgeUnit({ name: policy.name, steps: commands, allowIgnored: policy.allowIgnored ? [policy.name] : [], allowMissingAssets: policy.allowMissingAssets ? [policy.name] : [], requiredCommands: obligations.commands.filter((command) => command.unit === policy.id && TEST_COMMAND.test(command.command)).length });
    if (verdict.status === "fail" || actual.status !== verdict.status || actual.executed !== verdict.executed || actual.ignored !== verdict.ignored || actual.skipped !== verdict.skipped) fail(`unqualified test evidence: ${policy.name}`);
  }
  return { ok: failures.length === 0, failures };
}
