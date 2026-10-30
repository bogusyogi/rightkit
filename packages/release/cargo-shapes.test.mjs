// GB-01 / GB-02: the one command-shape table, exercised against RustRight's production vector (test-support/vectors/cargo-shapes.fixtures.json,
// docs/contracts of rustright, pinned in vectors-source.json). Every row is judged by classifyCargoArgv; the wrapper, the carried call and the
// evidence rule derive from it, so an accepted row reaches the broker's parser and a refused row launches zero processes.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { COMMAND_SHAPES_VECTOR_SCHEMA, carriedShapeDecision, classifyCargoArgv, isCompileCommand } from "./cargo-shapes.mjs";
import { WRAPPER_COMMANDS, decideCargoInvocation } from "./cargo-target-shim.mjs";
import { carryBrokerCargo } from "./snapshot-broker.mjs";
import { isCompileClass, lockedArgv } from "./snapshot-request.mjs";
import { loadVector } from "./test-support/vectors.mjs";

const vector = loadVector("cargo-shapes.fixtures.json");

test("[GB-01] the table agrees with every row of RustRight's cargo-shapes vector (normalizedArgv, command, compile, carried, grant, carriedRefusal, typed refusals)", () => {
  assert.equal(vector.schema, COMMAND_SHAPES_VECTOR_SCHEMA);
  assert.ok(vector.rows.length >= 30, "the corpus keeps covering the audit's shapes");
  for (const row of vector.rows) {
    const got = classifyCargoArgv(row.argv, { registered: [] });
    if (row.refused) { assert.equal(got.refused?.code, row.refused, `${row.name}: typed refusal`); continue; }
    assert.equal(got.refused, undefined, `${row.name}: ${JSON.stringify(got.refused)}`);
    assert.deepEqual(got.normalizedArgv, row.normalizedArgv, `${row.name}: normalizedArgv`);
    assert.equal(got.command, row.command, `${row.name}: command`);
    assert.equal(got.compile, row.compile, `${row.name}: compile`);
    assert.equal(got.carriedReadOnly, row.carriedReadOnly, `${row.name}: carriedReadOnly`);
    assert.equal(got.grant, row.grant, `${row.name}: grant`);
    assert.equal(got.carriedRefusal, row.carriedRefusal, `${row.name}: carriedRefusal`);
  }
});

test("[GB-01] aliases are normalized before locking and hashing: what is sent, digested and classified is the canonical command, and only the subcommand token changes", () => {
  assert.deepEqual(lockedArgv(["b", "-p", "x"]), ["build", "--locked", "-p", "x"]);
  assert.deepEqual(lockedArgv(["t"]), ["test", "--locked"]);
  assert.deepEqual(lockedArgv(["--color", "always", "r", "--", "t"]), ["--color", "always", "run", "--locked", "--", "t"], "an alias after -- is the program's, not cargo's");
  assert.deepEqual(lockedArgv(["c", "--locked"]), ["check", "--locked"]);
  assert.deepEqual(lockedArgv(["d"]), ["doc", "--locked"]);
  assert.deepEqual(lockedArgv(["fmt", "--all", "--check"]), ["fmt", "--all", "--check"], "fmt arguments are never rewritten");
  assert.deepEqual(lockedArgv(["tree", "--locked"]), ["tree", "--locked"]);
});

test("[GB-01] every exported WRAPPER_COMMANDS member has a row: the broker's decision is typed, never unknown to the gate", () => {
  for (const command of [...WRAPPER_COMMANDS.bound, ...WRAPPER_COMMANDS.passthrough]) {
    const row = classifyCargoArgv([command, "--locked"], { registered: ["deny", "audit"] });
    assert.ok(row.refused ? typeof row.refused.code === "string" : typeof row.command === "string", `cargo ${command}`);
    if (command.length === 1) assert.notEqual(row.normalizedArgv?.[0], command, `the alias ${command} is expanded`);
  }
});

test("[GB-01] a carried call the broker refuses is refused before submission with a typed reason, and no process is launched", () => {
  for (const argv of [["tree", "--locked"], ["pkgid"], ["verify-project"], ["metadata"], ["fmt", "--all"], ["fmt", "--check", "--", "--edition", "2021"], ["help"], ["build", "-Zfoo"], ["clean"], ["install", "x"], ["frobnicate"]]) {
    const decision = carriedShapeDecision(argv);
    assert.equal(decision.ok, false, `cargo ${argv.join(" ")}`);
    assert.match(decision.code, /^(RK_|RR_)/, `cargo ${argv.join(" ")}`);
  }
  for (const argv of [["build", "--locked"], ["b"], ["package", "-p", "u", "--list", "--locked"], ["metadata", "--no-deps"], ["metadata", "--locked"], ["fetch", "--locked"], ["fmt", "--all", "--check"], ["--version"], ["locate-project"], ["deny", "--frozen", "check"], ["audit", "--db", "/d", "--no-fetch"]]) {
    assert.equal(carriedShapeDecision(argv).ok, true, `cargo ${argv.join(" ")}`);
  }
  // carryBrokerCargo: the shape decision comes before the gate check, the carrier and the ownership mark.
  for (const words of [["tree", "--locked"], ["pkgid"], ["verify-project"], ["metadata"]]) {
    const records = []; const calls = []; const marks = [];
    const out = carryBrokerCargo({ words, cwd: "/nowhere", env: {}, section: { markSend: (h) => marks.push(h) }, workspace: "/nowhere", checkoutWorkspace: "/nowhere", record: (e) => records.push(e), spawn: (...a) => { calls.push(a); return { status: 0 }; }, err: () => {} });
    assert.equal(out.outcome.status, "refused", words.join(" "));
    assert.match(out.outcome.code, /^RK_CARGO_BROKER_READ_ONLY_SHAPE$/);
    assert.deepEqual(calls, [], "zero processes"); assert.deepEqual(marks, [], "nothing marked");
    assert.equal(records.length, 1);
  }
});

test("[GB-01] the broker-mode wrapper refuses tree, pkgid and verify-project even locked (RustRight has no non-writing policy for them), and expands an alias before it decides", () => {
  const map = { mode: "broker", shim: "/gate/bin/cargo", workspaces: { "/snap": "/checkout" }, broker: { snapshot: { path: "/nonexistent-snapshot" }, checkout: null } };
  const decide = (args) => decideCargoInvocation({ args, cwd: "/snap", env: {}, map, locate: () => "/snap/Cargo.toml", realpath: (p) => p, exists: () => true });
  for (const args of [["tree", "--locked"], ["pkgid", "--locked"], ["verify-project", "--locked"]]) assert.equal(decide(args).refuse?.code, "RK_CARGO_BROKER_READ_ONLY_SHAPE", args.join(" "));
  assert.equal(decide(["b", "--locked"]).exec?.args.join(" "), "build --locked", "the alias is expanded before it is classified and sent");
});

test("[GB-02] the compile rule is the vector's `compile` column: every compile form owes freshness, attribution and toolchain; a read-only form owes none", () => {
  for (const row of vector.rows.filter((r) => !r.refused)) {
    assert.equal(isCompileCommand(row.normalizedArgv), row.compile, `${row.name}: gate compile rule`);
    assert.equal(isCompileClass(row.argv), row.compile, `${row.name}: gate compile rule on the raw argv`);
  }
  for (const argv of [["bench"], ["run"], ["rustdoc"], ["package", "-p", "u", "--locked"]]) assert.equal(isCompileClass(argv), true, argv.join(" "));
  for (const argv of [["package", "-p", "u", "--list", "--locked"], ["metadata", "--no-deps"], ["fmt", "--all", "--check"]]) assert.equal(isCompileClass(argv), false, argv.join(" "));
});
