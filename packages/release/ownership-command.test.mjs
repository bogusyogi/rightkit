import assert from "node:assert/strict";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { main } from "./ownership-command.mjs";

const releaseRoot = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(releaseRoot, "..", "..", "..", "..");

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
