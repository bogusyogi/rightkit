import assert from "node:assert/strict";
import test from "node:test";
import { dependentClosureEvidence } from "./gate-evidence.mjs";

const config = { rust: { "rightkit-dependent": { checkFeatureSets: [["--no-default-features"], ["--features", "native"]] } } };
const plan = dependentClosureEvidence({ selected: ["rightkit-dependent"], config });
const results = plan.expected.map((row, i) => ({ ...row, ok: true, code: 0, timedOut: false, seconds: i + 1 }));

test("dependent closure proof retains exact required commands and does not add test evidence", () => {
  const proof = dependentClosureEvidence({ selected: ["rightkit-dependent"], config, results });
  assert.equal(proof.ok, true);
  assert.deepEqual(proof.selection, ["rightkit-dependent"]);
  assert.deepEqual(proof.expected, plan.expected);
  assert.deepEqual(proof.results, results.map(({ unit, command, ok, code, timedOut, seconds }) => ({ unit, command, ok, code, timedOut, seconds })));
  assert.ok(proof.results.every((row) => row.command.startsWith("cargo check ")));
  assert.equal(proof.results.some((row) => row.command.startsWith("cargo test ")), false);
});

test("dependent closure proof fails when required commands are missing, failed, or duplicated", () => {
  const missing = dependentClosureEvidence({ selected: ["rightkit-dependent"], config, results: results.slice(1) });
  assert.equal(missing.ok, false);
  assert.match(missing.problems.join("\n"), /missing required dependent-check command/);

  const failed = dependentClosureEvidence({ selected: ["rightkit-dependent"], config, results: results.map((row, i) => i === 0 ? { ...row, ok: false, code: 1 } : row) });
  assert.equal(failed.ok, false);
  assert.match(failed.problems.join("\n"), /failed dependent-check command/);

  const contradictory = [
    { ...results[0], ok: true, code: 1 },
    { ...results[0], ok: true, timedOut: true },
    (({ code, ...row }) => row)(results[0]),
    (({ timedOut, ...row }) => row)(results[0]),
    { ...results[0], seconds: -1 },
    { ...results[0], seconds: Number.NaN },
  ];
  for (const invalid of contradictory) {
    const proof = dependentClosureEvidence({ selected: ["rightkit-dependent"], config, results: [invalid, ...results.slice(1)] });
    assert.equal(proof.ok, false, `rejected invalid result ${JSON.stringify(invalid)}`);
  }

  const duplicate = dependentClosureEvidence({ selected: ["rightkit-dependent"], config, results: [...results, results[0]] });
  assert.equal(duplicate.ok, false);
  assert.match(duplicate.problems.join("\n"), /duplicate dependent-check result/);
});

test("dependent closure proof rejects producer-substituted results and duplicate selections", () => {
  const substituted = dependentClosureEvidence({ selected: ["rightkit-dependent"], config, results: [{ ...results[0], command: "cargo check -p unrelated --all-targets" }] });
  assert.equal(substituted.ok, false);
  assert.match(substituted.problems.join("\n"), /missing required dependent-check command/);
  assert.match(substituted.problems.join("\n"), /unexpected dependent-check result/);

  const duplicateSelection = dependentClosureEvidence({ selected: ["rightkit-dependent", "rightkit-dependent"], config, results });
  assert.equal(duplicateSelection.ok, false);
  assert.match(duplicateSelection.problems.join("\n"), /duplicate crate in dependent-check selection/);

  const unexpectedUnit = dependentClosureEvidence({ selected: ["rightkit-dependent"], config, results: [...results, { unit: "rust:rightkit-unselected", command: "cargo check -p rightkit-unselected --all-targets", ok: true }] });
  assert.equal(unexpectedUnit.ok, false);
  assert.match(unexpectedUnit.problems.join("\n"), /unexpected dependent-check unit/);
});


test("dependent closure proof refuses missing, substituted or failed standalone consumer checks", () => {
  const manifestPath = "ci/consumers/native/Cargo.toml";
  const config = { rust: { dependent: { checkFeatureSets: [], consumerRows: [{ manifestPath }, { manifestPath, args: ["--features", "engine"] }] } } };
  const plan = dependentClosureEvidence({ selected: ["dependent"], config });
  assert.deepEqual(plan.expected.slice(-2).map((row) => row.command), [
    `cargo check --manifest-path ${manifestPath} --locked`,
    `cargo check --manifest-path ${manifestPath} --locked --features engine`,
  ]);
  const results = plan.expected.map((row) => ({ ...row, ok: true, code: 0, timedOut: false, seconds: 1 }));
  assert.equal(dependentClosureEvidence({ selected: ["dependent"], config, results }).ok, true);
  for (const damaged of [results.slice(0, -1), results.map((row, index) => index === 1 ? { ...row, command: row.command.replace("--locked", "--help") } : row), results.map((row, index) => index === 1 ? { ...row, ok: false, code: 1 } : row)]) {
    assert.equal(dependentClosureEvidence({ selected: ["dependent"], config, results: damaged }).ok, false);
  }
});
