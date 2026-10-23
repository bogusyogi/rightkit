import assert from "node:assert/strict";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
import test from "node:test";
import { consumerRecipePolicy, planCommandsFromWorkflow, PLAN_CONTRACT_STEP, countIgnoredTests, verifyAttestation, brokerRequestIds, checkTools, countDocWarnings, crateRows, parseArgs, receiptRefusal, SCHEMA } from "./rk-preflight.mjs";

const sha = "a".repeat(40), unit = "rightkit-example", version = "1.0.0", config = {};
const expected = { sha, registry: "crates", unit, version, config };
export function greenReceipt() {
  return { schema: SCHEMA, mode: "local-compile", sha, registry: "crates", unit, version, route: "local", ok: true, finishedAt: new Date().toISOString(), scans: { ok: true, exit: 0, timedOut: false }, planChecks: { ok: true, exit: 0, timedOut: false }, rows: crateRows(unit, config).map((r) => ({ cwd: r.cwd ?? ".", attestation: { verified: true, signatureVerified: true, nonce: "f".repeat(64), requestId: "11111111-1111-4111-8111-111111111111" }, id: r.id, command: `rustright cargo ${r.args.join(" ")}`, exit: 0, timedOut: false, warnings: 0, brokerRequests: ["11111111-1111-4111-8111-111111111111"], ok: true })) };
}

test("publication preflight refuses scan/compile shortcuts & unbrokered npm", () => {
  for (const flag of ["--skip-tests", "--rows-only"]) assert.throws(() => parseArgs(["--registry", "crates", "--unit", unit, flag]), /partial/);
  assert.throws(() => parseArgs(["--registry", "npm", "--unit", "@rightkit/legal"]), /broker-admitted/);
  assert.throws(() => parseArgs(["--registry", "crates", "--unit", unit, "--budget-min", "0"]), /positive/);
});

test("compile plan preserves feature coverage, compiles tests without running them & covers local-path consumers", () => {
  const config = { rust: { [unit]: { featureSets: [[], ["--features", "remote"]], alsoSerial: true, paths: ["ci/consumers/example"], consumerRows: [{ manifestPath: "ci/consumers/example/Cargo.toml", args: ["--features", "engine"], locked: true }] } } };
  const rows = crateRows(unit, config);
  assert.equal(rows.filter((r) => r.kind === "doc").length, 3);
  assert.equal(rows.filter((r) => r.kind === "test").length, 2);
  assert.ok(rows.filter((r) => r.kind === "test").every((r) => r.args.includes("--no-run") && !r.args.includes("--test-threads=1")));
  assert.ok(rows.every((r) => r.args.includes("--locked")));
  const consumer = rows.find((r) => r.kind === "consumer");
  assert.equal(consumer.cwd, "ci/consumers/example", "a consumer row runs from its own directory (the attested client refuses a rewritten --manifest-path)");
  assert.ok(!consumer.args.includes("--manifest-path"));
  config.rust[unit].consumerRows[0].args = ["--manifest-path", "/tmp/foreign/Cargo.toml"];
  assert.throws(() => crateRows(unit, config), /may not contain/);
});

test("admission rejects forged shapes, stale identity, absent scans, omitted rows & unbrokered rows", () => {
  assert.equal(receiptRefusal(greenReceipt(), expected), null);
  const mutations = [
    (r) => { r.schema = "rk-preflight.v1"; }, (r) => { r.route = "ci"; },
    (r) => { r.sha = "b".repeat(40); }, (r) => { r.unit = "rightkit-other"; },
    (r) => { r.registry = "npm"; }, (r) => { r.version = "1.0.1"; },
    (r) => { r.scans = null; }, (r) => { r.scans.timedOut = true; },
    (r) => { r.planChecks = null; }, (r) => { r.planChecks.exit = 1; },
    (r) => { r.rows.pop(); }, (r) => { r.rows[0].brokerRequests = []; },
    (r) => { r.rows[0].command = "cargo check"; }, (r) => { delete r.finishedAt; },
    (r) => { r.rows[0].attestation.verified = false; }, (r) => { r.rows[0].attestation.signatureVerified = false; }, (r) => { r.rows[0].attestation.requestId = "22222222-2222-4222-8222-222222222222"; }, (r) => { delete r.rows[0].attestation; },
  ];
  for (const mutate of mutations) { const r = greenReceipt(); mutate(r); assert.ok(receiptRefusal(r, expected)); }
});

test("tool parity resolves pinned rustfmt & rejects prefix-only actionlint match", () => {
  const pins = { actionlint: "1.7.12", rustfmt: { toolchain: "1.98.0", version: "rustfmt pinned", darwin: { path: "/tools/1.98.0/rustfmt" } }, shellcheck: { darwin: { path: "/tools/shellcheck", sha256: "aa" } } };
  const calls = [];
  const spawnFn = (cmd, args) => { calls.push([cmd, args]); return { status: 0, stdout: cmd === "actionlint" ? "1.7.12\n" : cmd === "rustup" ? "/tools/1.98.0/rustfmt\n" : cmd.endsWith("rustfmt") ? "rustfmt pinned\n" : "ShellCheck\nversion: 0.10.0\n" }; };
  const base = { pins, spawnFn, exists: () => true, digest: () => "aa", platform: "darwin" };
  const good = checkTools(base);
  assert.deepEqual(good.problems, []);
  assert.equal(good.observed.rustfmt.path, "/tools/1.98.0/rustfmt");
  assert.ok(calls.some(([cmd]) => cmd === "/tools/1.98.0/rustfmt"));
  assert.match(checkTools({ ...base, spawnFn: (cmd, args) => cmd.endsWith("rustfmt") ? { status: 0, stdout: "wrong formatter" } : spawnFn(cmd, args) }).problems.join(" "), /pinned/);
  assert.match(checkTools({ ...base, digest: () => "bb" }).problems.join(" "), /sha256/);
  assert.match(checkTools({ ...base, spawnFn: (cmd, args) => cmd === "actionlint" ? { status: 0, stdout: "1.7.120" } : spawnFn(cmd, args) }).problems.join(" "), /not the pinned/);
});

test("rustdoc diagnostics & request IDs are captured; scoped counting takes only the unit's own rustdoc warnings", () => {
  assert.equal(countDocWarnings("warning: private intra-doc link\nwarning: `crate` generated 1 warning\n"), 1, "unscoped counting is unchanged");
  assert.deepEqual(brokerRequestIds("rustright: request 6891efd1-6e8b-4ef4-9600-613527d4515e\n"), ["6891efd1-6e8b-4ef4-9600-613527d4515e"]);
  const depWarning = ["    Checking rightkit-crash v0.3.6 (/w/crates/rightkit-crash)", "warning: function `scrub_for_bundle` is never used", "   --> crates/rightkit-crash/src/native.rs:208:19", "    |", " Documenting rightkit-job-queue v0.5.0 (/w/crates/rightkit-job-queue)", "    Finished"].join("\n");
  assert.equal(countDocWarnings(depWarning, "rightkit-job-queue"), 0, "a dependency's compile warning is not the unit's rustdoc warning");
  const own = [" Documenting rightkit-job-queue v0.5.0 (/w/crates/rightkit-job-queue)", "warning: unresolved link to `Nope`", "  --> crates/rightkit-job-queue/src/lib.rs:9:5", "warning: `rightkit-job-queue` (lib doc) generated 1 warning"].join("\n");
  assert.equal(countDocWarnings(own, "rightkit-job-queue"), 1, "the unit's own rustdoc warning counts");
  const before = ["warning: unresolved link to `Nope`", "  --> crates/rightkit-job-queue/src/lib.rs:9:5", " Documenting rightkit-job-queue v0.5.0 (/w/crates/rightkit-job-queue)"].join("\n");
  assert.equal(countDocWarnings(before, "rightkit-job-queue"), 0, "nothing counts before cargo starts documenting the unit");
  const unlocated = ["    Checking rightkit-onnx v0.2.7 (/w/crates/rightkit-onnx)", "warning: unresolved link to `provision_runtime`", "  |", "  = note: the link appears in this line:", "    Finished"].join("\n");
  assert.equal(countDocWarnings(unlocated, "rightkit-onnx"), 1, "a unit warning without a location line (module docs) counts once the unit has started");
  const depAfter = [" Documenting rightkit-onnx v0.2.7 (/w/crates/rightkit-onnx)", "warning: function `x` is never used", "   --> crates/rightkit-crash/src/native.rs:208:19"].join("\n");
  assert.equal(countDocWarnings(depAfter, "rightkit-onnx"), 0, "a warning located in another crate never counts");
});


test("per-row attestation: broker status fields must match, then the envelope signature is verified against the enrolled key", () => {
  const nonce = "a".repeat(64), job = "11111111-1111-4111-8111-111111111111";
  const pins = { host: "mac.local", keyId: "b".repeat(64), publicKey: "c".repeat(64), brokerDigest: "d".repeat(64), cargoSha256: "1".repeat(64) };
  const status = (over = {}) => ({ receipt: { state: "SUCCEEDED", exitCode: 0, inputBoundary: { status: "unrequested" }, provenance: { schema: "rustright.provenance.v1", signature: { alg: "ES256", keyId: pins.keyId }, payload: { host: pins.host, broker: { brokerDigest: pins.brokerDigest }, nonce, jobId: job, requestDigest: "e".repeat(64), executable: { sha256: pins.cargoSha256 }, result: { status: "SUCCEEDED", exitCode: 0 }, request: { kind: "cargo", requestedArgv: ["check", "-p", "x", "--locked"], manifestDigest: null } } }, ...over } });
  const spawnFor = (doc, verifyStatus = 0) => (cmd, args) => (args[0] === "status" ? { status: 0, stdout: JSON.stringify(doc) } : { status: verifyStatus, stdout: "" });
  const calls = [];
  const good = verifyAttestation(job, { nonce, argv: ["check", "-p", "x", "--locked"], pins, tmpDir: process.env.TMPDIR ?? "/tmp", spawnFn: (cmd, args) => { calls.push(args); return spawnFor(status())(cmd, args); } });
  assert.equal(good.verified, true);
  assert.equal(good.signatureVerified, true);
  const verify = calls.find((a) => a[0] === "verify-receipt");
  assert.ok(verify.includes("--public-key") && verify[verify.indexOf("--public-key") + 1] === pins.publicKey, "verifier gets the enrolled public key");
  assert.ok(verify[verify.indexOf("--broker-digest") + 1] === pins.brokerDigest && verify[verify.indexOf("--nonce") + 1] === nonce && verify[verify.indexOf("--job-id") + 1] === job);
  const bad = (over, n = nonce, argv = ["check", "-p", "x", "--locked"], verifyStatus = 0) => verifyAttestation(job, { nonce: n, argv, pins, tmpDir: process.env.TMPDIR ?? "/tmp", spawnFn: spawnFor(status(over), verifyStatus) }).reason;
  assert.equal(bad({}, "c".repeat(64)), "nonce mismatch");
  assert.equal(bad({}, nonce, ["check", "-p", "y", "--locked"]), "argv mismatch");
  const failedSigned = status(); failedSigned.receipt.provenance.payload.result = { status: "FAILED", exitCode: 101 };
  assert.equal(verifyAttestation(job, { nonce, argv: ["check", "-p", "x", "--locked"], pins, spawnFn: spawnFor(failedSigned) }).reason, "signed job result did not succeed");
  assert.equal(bad({}, nonce, ["check", "-p", "x", "--locked"], 1), "signature verification failed");
  const foreignCargo = status(); foreignCargo.receipt.provenance.payload.executable.sha256 = "2".repeat(64);
  assert.match(verifyAttestation(job, { nonce, argv: ["check", "-p", "x", "--locked"], pins, spawnFn: spawnFor(foreignCargo) }).reason, /not this host's enrolled runtime/);
  assert.match(bad({ route: "ci" }), /routed this row to hosted CI/);
  const foreignKey = status(); foreignKey.receipt.provenance.signature.keyId = "9".repeat(64);
  assert.equal(verifyAttestation(job, { nonce, argv: ["check", "-p", "x", "--locked"], pins, spawnFn: spawnFor(foreignKey) }).reason, "signature key is not the enrolled key");
  const unsigned = status(); delete unsigned.receipt.provenance;
  assert.equal(verifyAttestation(job, { nonce, argv: ["check", "-p", "x", "--locked"], pins, spawnFn: spawnFor(unsigned) }).reason, "no signed provenance envelope");
  assert.equal(verifyAttestation(job, { nonce, argv: [], pins: {} }).reason, "no enrollment pins for this host");
  assert.equal(verifyAttestation(null, { nonce, argv: [], pins }).reason, "no broker request or nonce");
});


test("ignored-test policy: #[ignore] attributes are counted statically across src, tests, benches and examples", () => {
  const fs = require("node:fs"), os = require("node:os"), path = require("node:path");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-ignored-"));
  fs.mkdirSync(path.join(dir, "src")); fs.mkdirSync(path.join(dir, "tests", "nested"), { recursive: true });
  fs.writeFileSync(path.join(dir, "src", "lib.rs"), "#[cfg(test)]\nmod t {\n    #[test]\n    #[ignore = \"needs a second account\"]\n    fn a() {}\n    #[test]\n    fn b() {}\n}\n");
  fs.writeFileSync(path.join(dir, "tests", "nested", "x.rs"), "#[test]\n#[ignore]\nfn c() {}\n// #[ignore] in a comment does not count\n");
  fs.writeFileSync(path.join(dir, "tests", "notes.md"), "#[ignore]\n");
  assert.equal(countIgnoredTests(dir), 2);
  assert.equal(countIgnoredTests(path.join(dir, "missing")), 0);
});


test("the plan-job contract commands come from publish.yml itself, never from a copied list", () => {
  const fs = require("node:fs"), path = require("node:path");
  const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
  const commands = planCommandsFromWorkflow(root, { execPath: "NODE" });
  assert.ok(commands.length >= 4, "the real workflow step runs at least four commands");
  assert.ok(commands.every(([exe, args]) => exe === "NODE" && args.length >= 1 && args.every((a) => /^[A-Za-z0-9_./@-]+$/.test(a))));
  assert.ok(commands.some(([, args]) => args.join(" ") === "scripts/ci/workflow-contract.mjs .github/workflows/publish.yml"));
  const real = fs.readFileSync(path.join(root, ".github", "workflows", "publish.yml"), "utf8");
  const renamed = real.replace(PLAN_CONTRACT_STEP, "Something else");
  assert.throws(() => planCommandsFromWorkflow(root, { readFile: () => renamed }), /no plan step named/);
  const shelly = real.replace("node scripts/ci/reviewed-workflow-contract.mjs", "node scripts/ci/reviewed-workflow-contract.mjs && rm -rf /");
  assert.throws(() => planCommandsFromWorkflow(root, { readFile: () => shelly }), /not a plain node command/);
});


test("consumer-recipe policy: a valid reviewed recipe passes; a missing recipe and a stale inventory hash refuse before any cargo row", () => {
  const fs = require("node:fs"), os = require("node:os"), path = require("node:path"), crypto = require("node:crypto");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-recipe-"));
  const unit = "rightkit-example";
  fs.mkdirSync(path.join(root, "crates", unit), { recursive: true });
  fs.writeFileSync(path.join(root, "crates", unit, "Cargo.toml"), '[package]\nname = "rightkit-example"\nversion = "1.2.3"\n');
  const dir = path.join(root, "ci", "consumers", unit);
  fs.mkdirSync(path.join(dir, "src"), { recursive: true });
  const manifest = '[package]\nname = "rightkit-example-registry-consumer"\nversion = "0.0.0"\nedition = "2021"\npublish = false\n[workspace]\n[dependencies]\nrightkit-example = { version = "=1.2.3" }\n';
  fs.writeFileSync(path.join(dir, "Cargo.toml"), manifest);
  fs.writeFileSync(path.join(dir, "src", "main.rs"), "fn main() {}\n");
  const sha = (text) => crypto.createHash("sha256").update(text).digest("hex");
  const files = { "Cargo.toml": sha(manifest), "src/main.rs": sha("fn main() {}\n") };
  const inventory = { schema: 1, cargo: { [unit]: { pkId: "PK99", id: `ci/consumers/${unit}/src/main.rs`, files } } };
  fs.writeFileSync(path.join(root, "ci", "consumer-recipes.json"), JSON.stringify(inventory));
  const good = consumerRecipePolicy(root, unit);
  assert.equal(good.ok, true, good.reason);
  assert.equal(good.pkId, "PK99");
  fs.writeFileSync(path.join(dir, "src", "main.rs"), "fn main() { println!(\"changed\"); }\n");
  assert.match(consumerRecipePolicy(root, unit).reason, /differs from reviewed PK99 inventory/);
  fs.rmSync(dir, { recursive: true, force: true });
  assert.match(consumerRecipePolicy(root, unit).reason, /missing committed PK example/);
  assert.match(consumerRecipePolicy(root, "rightkit-absent").reason, /Cargo.toml is missing/);
});
