import assert from "node:assert/strict";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { main, trustCompare } from "./ownership-command.mjs";
import { assertCommittedCheckoutClean } from "./committed-source.mjs";

const releaseRoot = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(releaseRoot, "..", "..", "..");

test("ownership help documents explicit local new-capability approval & conflict refusal", async () => {
  const printed = [];
  const status = await main(["--help"], { evidenceFactory: () => { throw new Error("help must not scan"); }, print: (line) => printed.push(line) });
  assert.equal(status, 0);
  assert.match(printed[0], /--approve-new-capabilities/);
  assert.match(printed[0], /local generation only/);
  assert.match(printed[0], /owner conflicts are always refused/);
});

test("ownership CLI refuses pending enrollment with a diagnostic while preserving findings", async () => {
  const printed = [];
  const result = { rows: [], violations: [{ fingerprint: "pending", file: "source.rs" }], covered: [], problems: ["approval enrollment pending"], coverage: [], metadata: {} };
  const status = await main(["--app", repositoryRoot, "--json"], {
    gitRunner: () => ({ status: 0, stdout: `${repositoryRoot}\n` }),
    evidenceFactory: () => ({ ok: false, enrollmentPending: true, warning: "WARNING: enrollment pending; findings remain backlog diagnostics", result, floor: { ok: false, problems: ["floor unavailable"] } }),
    print: (line) => printed.push(line),
  });
  assert.equal(status, 1);
  const output = JSON.parse(printed[0]);
  assert.equal(output.ok, false);
  assert.equal(output.enrollmentPending, true);
  assert.match(output.warning, /^WARNING:/);
  assert.equal(output.violations[0].fingerprint, "pending");
  assert.equal(output.error, undefined);
  const human = [];
  await main(["--app", repositoryRoot], {
    gitRunner: () => ({ status: 0, stdout: `${repositoryRoot}\n` }),
    evidenceFactory: () => ({ ok: false, enrollmentPending: true, warning: "WARNING: enrollment pending; findings remain backlog diagnostics", result, floor: { ok: false, problems: [] } }),
    print: (line) => human.push(line),
  });
  assert.ok(human.some((line) => line.startsWith("  DIAGNOSTIC ")));
  assert.ok(human.at(-1).includes("WARNING:"));
});

test("ownership CLI --advisory-when-pending exits 0 only while enrollment is pending", async () => {
  const result = { rows: [], violations: [{ fingerprint: "pending", file: "source.rs" }], covered: [], problems: [], coverage: [], metadata: {} };
  const run = (checked) => main(["--app", repositoryRoot, "--advisory-when-pending"], {
    gitRunner: () => ({ status: 0, stdout: `${repositoryRoot}\n` }),
    evidenceFactory: () => ({ result, floor: { ok: false, problems: [] }, ...checked }),
    print: () => {},
  });
  assert.equal(await run({ ok: false, enrollmentPending: true, warning: "WARNING: pending" }), 0);
  assert.equal(await run({ ok: false }), 1);
});

test("published ownership command runs without development sibling packages & refuses pending trust with a warning", () => {
  const temp = mkdtempSync(path.join(os.tmpdir(), "right-release-ownership-pack-"));
  try {
    const packageRoot = path.join(temp, "package");
    mkdirSync(packageRoot);
    // Mirror package.json's published root globs & cli directory without any
    // development sibling packages or workspace links.
    for (const entry of readdirSync(releaseRoot, { withFileTypes: true })) {
      const source = path.join(releaseRoot, entry.name);
      const destination = path.join(packageRoot, entry.name);
      if (entry.isDirectory() && entry.name === "cli") cpSync(source, destination, { recursive: true });
      else if (entry.isFile() && /\.(?:mjs|json|sh|py)$/.test(entry.name)
        && !entry.name.endsWith(".test.mjs") && entry.name !== "standalone-clone-evidence.json") {
        cpSync(source, destination);
      }
    }
    assert.ok(existsSync(path.join(packageRoot, "ownership-command.mjs")));
    assert.equal(existsSync(path.join(packageRoot, "build-control-clients")), false);
    const packageModules = path.join(packageRoot, "node_modules");
    mkdirSync(packageModules);
    cpSync(realpathSync(path.join(releaseRoot, "node_modules", "smol-toml")), path.join(packageModules, "smol-toml"), { recursive: true });

    const app = path.join(temp, "app");
    mkdirSync(app);
    for (const args of [["init", "-q"], ["config", "user.email", "test@example.invalid"], ["config", "user.name", "test"], ["remote", "add", "origin", "https://github.com/test/ownership-package.git"]]) {
      const git = spawnSync("git", ["-C", app, ...args], { encoding: "utf8", windowsHide: true });
      assert.equal(git.status, 0, git.stderr);
    }
    writeFileSync(path.join(app, "source.txt"), "test\n");
    for (const args of [["add", "."], ["commit", "-qm", "fixture"]]) {
      const git = spawnSync("git", ["-C", app, ...args], { encoding: "utf8", windowsHide: true });
      assert.equal(git.status, 0, git.stderr);
    }

    const command = spawnSync(process.execPath, [path.join(packageRoot, "ownership-command.mjs"), "--app", app, "--json"], { encoding: "utf8", windowsHide: true });
    assert.equal(command.status, 1, command.stderr);
    const output = JSON.parse(command.stdout);
    assert.equal(output.ok, false);
    // Host-state independent: a host still pending enrollment warns; an enrolled host refuses the
    // unregistered fixture repository. Both fail closed with a well-formed report.
    if (output.enrollmentPending) { assert.match(output.warning, /^WARNING:/); assert.equal(output.error, undefined); }
    else assert.match(output.error?.message ?? "", /not registered in protected trust/, JSON.stringify(output));
    assert.ok(Array.isArray(output.rows));
    assert.ok(Array.isArray(output.violations));
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

test("ownership --audit-json maps block and advise violations to Legion findings and exits 0", async () => {
  const printed = [];
  const errors = [];
  const originalError = console.error;
  console.error = (line) => errors.push(line);
  try {
    const result = { rows: [], covered: [], problems: [], coverage: [], metadata: {}, violations: [
      { fingerprint: "rk.source.hash", capability: "hashing", package: "rightkit-hash", tier: "block", file: "src/lib.rs", line: 12, text: "fn sha() {}" },
      { fingerprint: "rk.dep.zip", capability: "archive", package: "rightkit-zip", tier: "advise", file: "packages/app/package.json", line: 0, ecosystem: "npm", name: "zip-lib" },
    ] };
    const status = await main(["--app", repositoryRoot, "--audit-json"], {
      gitRunner: () => ({ status: 0, stdout: `${repositoryRoot}\n` }),
      evidenceFactory: () => ({ ok: false, floor: { ok: false, problems: ["floor"] }, result }),
      print: (line) => printed.push(line),
    });
    assert.equal(status, 0);
    assert.equal(printed.length, 1);
    assert.deepEqual(JSON.parse(printed[0]), { schemaVersion: 1, findings: [
      { rule: "rk.source.hash", path: "src/lib.rs", line: 12, message: "hashing duplicates RightKit owner rightkit-hash (block): fn sha() {}" },
      { rule: "rk.dep.zip", path: "packages/app/package.json", message: "advise: archive duplicates RightKit owner rightkit-zip (advise): npm zip-lib" },
    ] });
    assert.deepEqual(errors, []);
  } finally {
    console.error = originalError;
  }
});

test("ownership --audit-json prints an empty findings document and exits 0 on a clean scan", async () => {
  const printed = [];
  const status = await main(["--app", repositoryRoot, "--audit-json"], {
    gitRunner: () => ({ status: 0, stdout: `${repositoryRoot}\n` }),
    evidenceFactory: () => ({ ok: true, floor: { ok: true, problems: [] }, result: { rows: [], covered: [], problems: [], coverage: [], metadata: {}, violations: [] } }),
    print: (line) => printed.push(line),
  });
  assert.equal(status, 0);
  assert.equal(printed[0], JSON.stringify({ schemaVersion: 1, findings: [] }));
});

test("ownership --audit-json fails only when the scan itself fails or a path leaves the audited root", async () => {
  const printed = [];
  const originalError = console.error;
  console.error = () => {};
  try {
    const failed = await main(["--app", repositoryRoot, "--audit-json"], {
      gitRunner: () => ({ status: 0, stdout: `${repositoryRoot}\n` }),
      evidenceFactory: () => { throw new Error("trust missing"); },
      print: (line) => printed.push(line),
    });
    assert.equal(failed, 1);
    assert.deepEqual(printed, []);

    const escaping = await main(["--app", repositoryRoot, "--audit-json"], {
      gitRunner: () => ({ status: 0, stdout: `${repositoryRoot}\n` }),
      evidenceFactory: () => ({ ok: false, floor: { ok: false, problems: [] }, result: { rows: [], covered: [], problems: [], coverage: [], metadata: {}, violations: [{ fingerprint: "x", capability: "c", package: "p", tier: "block", file: "../outside.rs", line: 1, text: "t" }] } }),
      print: (line) => printed.push(line),
    });
    assert.equal(escaping, 1);
    assert.deepEqual(printed, []);
  } finally {
    console.error = originalError;
  }
});

test("trust-compare prints this host's trust digest, ignoring the per-host checkout path", async () => {
  const trust = { schema: 1, minimumScannerVersion: "2.6.0", minimumCatalogVersion: "1", repositories: [], keyAuthority: { repositoryIdentity: "https://github.com/o/r", checkoutPath: "/Volumes/D/claude" } };
  const run = async (state, args = ["trust-compare"]) => { const out = []; const status = await main(args, { print: (line) => out.push(line), trustLoader: () => state }); return { status, out }; };
  const mac = await run({ trust });
  const dell = await run({ trust: { ...trust, keyAuthority: { ...trust.keyAuthority, checkoutPath: "D:\\Claude" } } });
  assert.equal(mac.status, 0);
  assert.match(mac.out[0], /^trust digest \([a-z0-9]+\): [a-f0-9]{64}$/);
  assert.equal(mac.out[0].split(": ")[1], dell.out[0].split(": ")[1]);
  const tampered = await run({ trust: { ...trust, minimumScannerVersion: "0.0.1" } });
  assert.notEqual(tampered.out[0].split(": ")[1], mac.out[0].split(": ")[1]);
  const json = JSON.parse((await run({ trust }, ["trust-compare", "--json"])).out[0]);
  assert.equal(json.digest, mac.out[0].split(": ")[1]);
  assert.equal((await run({ trust: null })).status, 1);
});

test("trust-compare reports a pinned-bundle drift as a warning with a semantic diff and still exits 0", () => {
  const file = (path, sha256) => ({ path, sha256 });
  const pinned = { schema: 1, digest: "a".repeat(64), scannerVersion: "2.6.0", catalogDigest: "1".repeat(64), registryDigest: null, allowlistDigest: null, files: [file("ownership.mjs", "1".repeat(64)), file("gone.mjs", "2".repeat(64))] };
  const running = { ...pinned, digest: "b".repeat(64), scannerVersion: "2.7.0", catalogDigest: "9".repeat(64), files: [file("ownership.mjs", "3".repeat(64)), file("new.mjs", "4".repeat(64))] };
  const trust = { schema: 1, minimumScannerVersion: "2.6.0", minimumCatalogVersion: "1", repositories: [], policyBundle: pinned };
  const out = [];
  assert.equal(trustCompare(["trust-compare"], { print: (l) => out.push(l), trustLoader: () => ({ trust }), runningBundle: () => running }), 0);
  const text = out.join("\n");
  assert.match(text, /WARNING: ownership policy bundle .* differs from the bundle pinned in protected trust/);
  assert.match(text, /scanner version: 2\.6\.0 -> 2\.7\.0/);
  assert.match(text, /files added: new\.mjs/);
  assert.match(text, /files removed: gone\.mjs/);
  assert.match(text, /files changed: ownership\.mjs/);
  assert.match(text, /changed digests: catalog/);
  const json = [];
  trustCompare(["trust-compare", "--json"], { print: (l) => json.push(l), trustLoader: () => ({ trust }), runningBundle: () => running });
  assert.equal(JSON.parse(json[0]).warnings[0].diff.filesChanged[0], "ownership.mjs");
  const quiet = [];
  trustCompare(["trust-compare"], { print: (l) => quiet.push(l), trustLoader: () => ({ trust }), runningBundle: () => pinned });
  assert.ok(!quiet.join("\n").includes("WARNING"));
});

test("trust-sync is retired: prints the reason, exits 0, touches nothing", async () => {
  const out = [];
  const status = await main(["trust-sync"], { print: (l) => out.push(l), trustLoader: () => { throw new Error("must not load trust"); }, gitRunner: () => { throw new Error("must not run git"); } });
  assert.equal(status, 0);
  assert.match(out.join("\n"), /trust-sync is retired/);
});

const commandOptions = (overrides) => ({
  gitRunner: () => ({ status: 0, stdout: `${repositoryRoot}\n` }),
  print: () => {},
  stderr: () => {},
  ...overrides,
});
const scanResult = (violations, coverage = []) => ({ rows: [], covered: [], problems: [], coverage, metadata: {}, violations });
const advisory = (file, name) => ({ fingerprint: "log.println", capability: "logging", package: "rightkit-log", tier: "advise", file, line: 2, text: name });

test("a dirty protected checkout refuses by naming clean-checkout and its first ten paths, on stderr and in JSON", async () => {
  const temp = mkdtempSync(path.join(os.tmpdir(), "right-release-dirty-"));
  try {
    const init = spawnSync("git", ["-C", temp, "init", "-q"], { encoding: "utf8", windowsHide: true });
    assert.equal(init.status, 0, init.stderr);
    for (let index = 1; index <= 12; index += 1) writeFileSync(path.join(temp, `stray-${String(index).padStart(2, "0")}.txt`), "x\n");
    const printed = [];
    const errors = [];
    const status = await main(["--app", temp, "--json"], commandOptions({
      gitRunner: () => ({ status: 0, stdout: `${temp}\n` }),
      evidenceFactory: (root) => { assertCommittedCheckoutClean(root); return null; },
      print: (line) => printed.push(line),
      stderr: (line) => errors.push(line),
    }));
    assert.equal(status, 1);
    const output = JSON.parse(printed[0]);
    assert.equal(output.ok, false);
    assert.equal(output.error.type, "protected-evidence");
    assert.equal(output.blocked.length, 1);
    assert.equal(output.blocked[0].check, "clean-checkout");
    assert.equal(output.blocked[0].count, 12);
    assert.deepEqual(output.blocked[0].paths, Array.from({ length: 10 }, (_, index) => `stray-${String(index + 1).padStart(2, "0")}.txt`));
    assert.match(errors[0], /^\[right-release\] refused by check clean-checkout: committed ownership: protected release checkout/);
    assert.equal(errors.filter((line) => line.startsWith("  - ")).length, 10);
    assert.equal(errors.at(-1), "  ... and 2 more");
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

test("a scan refused only for coverage gaps names them instead of asking to move findings", async () => {
  const printed = [];
  const errors = [];
  const status = await main(["--app", repositoryRoot], commandOptions({
    evidenceFactory: () => ({ ok: false, floor: { ok: true, problems: [] }, result: scanResult([advisory("src/main.rs", "println!")], [{ path: "node_modules/pkg", status: "unscanned", reason: "installed-or-build-cache" }]) }),
    print: (line) => printed.push(line),
    stderr: (line) => errors.push(line),
  }));
  assert.equal(status, 1);
  assert.equal(printed.at(-1), "[right-release] ownership FAILED: ownership-coverage: 1 coverage gap(s): installed-or-build-cache");
  assert.doesNotMatch(printed.join("\n"), /move these/);
  assert.ok(errors.includes("  - node_modules/pkg"));
});

test("advise-only findings pass with per-tier counts, exit 0, and no move-these hint", async () => {
  const violations = [advisory("src/main.rs", "println!"), { ...advisory("package.json", "zip-lib"), fingerprint: "zip.dep", capability: "archive", package: "rightkit-zip", ecosystem: "npm", name: "zip-lib" }];
  const printed = [];
  const errors = [];
  const options = commandOptions({ evidenceFactory: () => ({ ok: true, floor: { ok: true, problems: [] }, result: scanResult(violations) }), print: (line) => printed.push(line), stderr: (line) => errors.push(line) });
  assert.equal(await main(["--app", repositoryRoot], options), 0);
  assert.equal(printed.at(-1), "[right-release] ownership ADVISE: block 0, advise 2 (reported, not refused)");
  assert.doesNotMatch(printed.join("\n"), /FAILED|move these/);
  assert.deepEqual(errors, []);
  const json = [];
  assert.equal(await main(["--app", repositoryRoot, "--json"], { ...options, print: (line) => json.push(line) }), 0);
  assert.equal(JSON.parse(json[0]).blocked, undefined);
});

test("block-tier findings still refuse, name the ownership-violation check, and keep the move-these hint", async () => {
  const printed = [];
  const errors = [];
  const block = { fingerprint: "rk.source.hash", capability: "hashing", package: "rightkit-hash", tier: "block", file: "src/lib.rs", line: 12, text: "fn sha() {}" };
  const status = await main(["--app", repositoryRoot], commandOptions({
    evidenceFactory: () => ({ ok: false, floor: { ok: true, problems: [] }, result: scanResult([block]) }),
    print: (line) => printed.push(line),
    stderr: (line) => errors.push(line),
  }));
  assert.equal(status, 1);
  assert.equal(printed.at(-1), "[right-release] ownership FAILED: ownership-violation: 1 block-tier finding(s) duplicate RightKit owners; move these to RightKit or add a reviewed, dated exception");
  assert.deepEqual(errors, ["[right-release] refused by check ownership-violation: 1 block-tier finding(s) duplicate RightKit owners", "  - src/lib.rs"]);
});
