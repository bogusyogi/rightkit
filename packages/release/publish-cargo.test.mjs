import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, writeFileSync, rmSync, mkdirSync, readFileSync, readdirSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  buildCargoPublishPlan,
  packageFileViolations,
  parseCargoPublishArgs,
  runCargoPublish,
} from "./publish-cargo.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { checkPublicationGate } from "./publication-gate.mjs";

test("cargo publish requires a safe explicit crate name", () => {
  assert.throws(() => parseCargoPublishArgs([]), /--crate/);
  assert.throws(() => parseCargoPublishArgs(["--crate", "../secret"]), /invalid crate/);
  assert.throws(() => parseCargoPublishArgs(["--crate", "rightkit-license", "--allow-dirty"]), /only valid with --dry-run/);
  assert.equal(parseCargoPublishArgs(["--crate", "rightkit-license"]).crate, "rightkit-license");
});

test("cargo publish plan always runs local safety gates before upload", () => {
  const plan = buildCargoPublishPlan({ crate: "rightkit-license", crateDir: "C:/rightkit/crates/rightkit-license", dryRun: false });
  assert.deepEqual(plan.map((step) => step.label), [
    "secret scan",
    "runtime budget",
    "format",
    "tests",
    "clippy",
    "package contents",
    "registry dry-run",
    "package archive",
    "package location",
    "archive hash check",
    "registry publish",
    "registry readback",
  ]);
  assert.deepEqual(plan.find((step) => step.label === "package archive").args, ["package", "--no-verify", "-p", "rightkit-license"]);
  assert.equal(plan.find((step) => step.label === "archive hash check").internal, "verify-archive");
  assert.deepEqual(plan.find((step) => step.label === "registry publish"), {
    label: "registry publish", command: "cargo", args: ["publish", "--no-verify", "-p", "rightkit-license"],
  });
  assert.equal(plan.at(-1)?.internal, "registry-readback");
  assert.deepEqual(plan.find((step) => step.label === "format").args, ["fmt", "-p", "rightkit-license", "--check"]);
  assert.deepEqual(plan.find((step) => step.label === "tests").args, ["test", "-p", "rightkit-license", "--", "--test-threads=1"]);
});

test("dry-run plan never contains a real registry upload", () => {
  const plan = buildCargoPublishPlan({ crate: "rightkit-logs", crateDir: "C:/rightkit/crates/rightkit-logs", dryRun: true });
  assert.equal(plan.at(-1)?.label, "registry dry-run");
});

test("package inspection rejects secret and build-output paths", () => {
  assert.deepEqual(packageFileViolations([
    "Cargo.toml",
    "src/lib.rs",
    ".env",
    "keys/signing.p8",
    "target/release/app",
  ]), [".env", "keys/signing.p8", "target/release/app"]);
});

function fixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-publish-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const name = "rightkit-fixture", version = "1.0.0", bytes = Buffer.from("fixture crate archive");
  const crateDir = path.join(root, "crates", name), target = path.join(root, "generated-target");
  mkdirSync(crateDir, { recursive: true });
  writeFileSync(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${name}"\nversion = "${version}"\n[package.metadata.rightkit]\ncontinuous = false\ncontinuous_reason = "fixture library with no continuous work"\n`);
  mkdirSync(path.join(target, "package"), { recursive: true });
  const crateFile = path.join(target, "package", `${name}-${version}.crate`);
  writeFileSync(crateFile, bytes);
  const evidence = { artifactSha256: digestBytes(bytes) }, events = [];
  const execute = (command, args, settings) => {
    assert.equal(command, "cargo");
    assert.equal(settings.cwd, root);
    assert.equal(settings.windowsHide, true);
    if (args[0] === "package" && args.includes("--no-verify")) events.push("package");
    if (args[0] === "publish" && !args.includes("--dry-run")) {
      assert.deepEqual(args, ["publish", "--no-verify", "-p", name]);
      events.push("publish");
    }
    return { status: 0, stdout: args[0] === "metadata" ? JSON.stringify({ target_directory: target })
      : args.includes("--list") ? "Cargo.toml\nsrc/lib.rs\n" : "" };
  };
  let gateCalls = 0;
  const deps = {
    enforceGate: () => { if (++gateCalls === 2) events.push("hash check"); return { ok: true, evidence }; },
    fetch: async () => {
      events.push("readback");
      return { ok: true, text: async () => JSON.stringify({ name, vers: version, cksum: evidence.artifactSha256 }) };
    },
  };
  return { root, name, version, bytes, crateFile, evidence, events, execute, deps, options: { workspaceRoot: root, crate: name } };
}

test("Cargo publisher packages, checks evidence hash, publishes through same runner & reads back", async (t) => {
  const fx = fixture(t);
  const result = await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.deepEqual(fx.events, ["package", "hash check", "publish", "readback"]);
  assert.deepEqual(result, { name: fx.name, version: fx.version, sha256: fx.evidence.artifactSha256 });
});

test("runtime-budget crate without a measured runtime-budget.json is refused before packaging or upload", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${fx.name}"\nversion = "${fx.version}"\n[package.metadata.rightkit]\nruntime_budget = true\n`);
  writeFileSync(path.join(crateDir, "README.md"), "# fixture\n\n## Runtime budget\n\n- Resident memory: 40 MB per 1,000 entries\n");
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), (error) => {
    assert.equal(error.code, "RUNTIME_BUDGET_MISSING");
    assert.match(error.message, /runtime budget refused/);
    assert.match(error.message, /runtime-budget\.json is missing/);
    return true;
  });
  assert.deepEqual(fx.events, []);
});

test("runtime-budget crate dry-run reports the failing check and still validates", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(crateDir, "Cargo.toml"), `[package]\nname = "${fx.name}"\nversion = "${fx.version}"\n[package.metadata.rightkit]\nruntime_budget = true\n`);
  writeFileSync(path.join(crateDir, "README.md"), "# fixture\n");
  const lines = [];
  const write = process.stdout.write.bind(process.stdout);
  process.stdout.write = (chunk) => { lines.push(String(chunk)); return true; };
  let result;
  try {
    result = await runCargoPublish({ ...fx.options, dryRun: true }, fx.execute, fx.deps);
  } finally {
    process.stdout.write = write;
  }
  assert.deepEqual(result, { name: fx.name, version: fx.version, dryRun: true });
  assert.match(lines.join(""), /\[FAIL\] runtime-budget: rightkit-fixture: runtime-budget\.json is missing/);
  assert.deepEqual(fx.events, []);
});

test("archive hash mismatch refuses publication before Cargo upload or readback", async (t) => {
  const fx = fixture(t);
  writeFileSync(fx.crateFile, "different archive");
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), { code: "PUBLICATION_HASH_MISMATCH" });
  assert.deepEqual(fx.events, ["package", "hash check"]);
});

test("changed publication evidence refuses Cargo upload", async (t) => {
  const fx = fixture(t);
  const execute = (command, args, settings) => {
    const result = fx.execute(command, args, settings);
    if (args[0] === "package" && args.includes("--no-verify")) fx.evidence.artifactSha256 = "b".repeat(64);
    return result;
  };
  await assert.rejects(runCargoPublish(fx.options, execute, fx.deps), /evidence changed during packaging/);
  assert.deepEqual(fx.events, ["package", "hash check"]);
});

test("failed Cargo publish prevents registry readback", async (t) => {
  const fx = fixture(t);
  const execute = (command, args, settings) => {
    const result = fx.execute(command, args, settings);
    return args[0] === "publish" && !args.includes("--dry-run") ? { status: 1 } : result;
  };
  await assert.rejects(runCargoPublish(fx.options, execute, fx.deps), /registry publish failed/);
  assert.deepEqual(fx.events, ["package", "hash check", "publish"]);
});

test("registry mismatch propagates from publisher without executing yank", async (t) => {
  const fx = fixture(t);
  fx.deps.fetch = async () => ({ ok: true, text: async () => JSON.stringify({ name: fx.name, vers: fx.version, cksum: "b".repeat(64) }) });
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), { code: "PUBLICATION_REGISTRY_MISMATCH" });
  assert.deepEqual(fx.events, ["package", "hash check", "publish"]);
});

test("dry-run publisher never checks an archive, uploads or reads registry", async (t) => {
  const fx = fixture(t);
  assert.deepEqual(await runCargoPublish({ ...fx.options, dryRun: true }, fx.execute, fx.deps),
    { name: fx.name, version: fx.version, dryRun: true });
  assert.deepEqual(fx.events, []);
});

test("publication gate compares actual artifact buffer to evidence", () => {
  const commit = "a".repeat(40), bytes = Buffer.from("fixture crate archive"), name = "rightkit-fixture", version = "1.0.0";
  const options = { kind: "cargo", name, version, packagePath: "/fixture", evidencePath: "/evidence", artifactBytes: bytes,
    git: (args) => args[0] === "rev-parse" ? commit : "",
    readFile: () => JSON.stringify({ schema: 1, kind: "cargo", name, version, commit, artifactSha256: digestBytes(bytes),
      runs: ["darwin", "win32"].map((os) => ({ host: { os, arch: "arm64" }, commit, features: ["default"], artifactSha256: digestBytes(bytes), command: "node --test", passed: 1, failed: 0, operator: "test", timestamp: "2026-10-08" })) }),
  };
  assert.equal(checkPublicationGate(options).ok, true);
  assert.equal(checkPublicationGate({ ...options, artifactBytes: Buffer.from("changed") }).ok, false);
});

test("release source never accesses Cargo credential files or registry token environment variables", () => {
  const root = fileURLToPath(new URL(".", import.meta.url));
  const forbidden = /\bCARGO_REGISTRY_TOKEN\b|\bCARGO_REGISTRIES_[A-Z0-9_]+_TOKEN\b|["'`]credentials(?:\.toml)?["'`]|\.cargo[/\\]credentials(?:\.toml)?/;
  const inspect = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory() && !["node_modules", "fixtures", "test"].includes(entry.name)) inspect(file);
      else if (entry.isFile() && /\.(?:mjs|js|ts|py|sh|ps1)$/.test(entry.name) && !/\.test\./.test(entry.name)) {
        assert.doesNotMatch(readFileSync(file, "utf8"), forbidden, path.relative(root, file));
      }
    }
  };
  inspect(root);
});

test("publish refuses a Cargo.toml whose [package].name is not the requested crate", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(crateDir, "Cargo.toml"), '[package]\nname = "rightkit-renamed"\nversion = "1.0.0"\n');
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /\[package\]\.name "rightkit-renamed" differs from --crate rightkit-fixture/);
  assert.deepEqual(fx.events, []);
});

test("publish refuses an undeclared crate: neither runtime-budget.json nor continuous = false", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(crateDir, "Cargo.toml"), '[package]\nname = "rightkit-fixture"\nversion = "1.0.0"\n');
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), (error) => {
    assert.equal(error.code, "RUNTIME_BUDGET_MISSING");
    assert.match(error.message, /undeclared/);
    return true;
  });
  assert.deepEqual(fx.events, []);
});

test("publish refuses continuous = false when src spawns a thread and the reason does not name it", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  mkdirSync(path.join(crateDir, "src"), { recursive: true });
  writeFileSync(path.join(crateDir, "src", "lib.rs"), "pub fn go() { std::thread::spawn(|| {}); }\n");
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /thread-spawn \(src\/lib\.rs\)/);
  assert.deepEqual(fx.events, []);
});

test("version.workspace = true resolves through the workspace root instead of a regex miss", async (t) => {
  const fx = fixture(t);
  const crateDir = path.join(fx.root, "crates", fx.name);
  writeFileSync(path.join(fx.root, "Cargo.toml"), '[workspace]\nmembers = ["crates/*"]\n[workspace.package]\nversion = "1.0.0"\n');
  writeFileSync(path.join(crateDir, "Cargo.toml"), '[package]\nname = "rightkit-fixture"\nversion.workspace = true\n[package.metadata.rightkit]\ncontinuous = false\ncontinuous_reason = "fixture library"\n');
  const result = await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.equal(result.version, "1.0.0");
});

test("proven steps are removed from the plan, never the runtime budget, package contents, archive hash or readback", () => {
  const all = { tests: true, clippy: true, fmt: true, secretScan: true };
  const plan = buildCargoPublishPlan({ crate: "rightkit-license", crateDir: "C:/rightkit/crates/rightkit-license", dryRun: false, proven: all, verifyOnPublish: true });
  assert.deepEqual(plan.map((step) => step.label), [
    "runtime budget", "package contents", "package archive", "package location", "archive hash check", "registry publish", "registry readback",
  ]);
  assert.deepEqual(plan.find((step) => step.label === "registry publish").args, ["publish", "--locked", "-p", "rightkit-license"]);
  // Partial proof removes only what is proven; an unproven step still runs.
  const partial = buildCargoPublishPlan({ crate: "rightkit-license", crateDir: "x", dryRun: false, proven: { tests: true, clippy: true } });
  assert.deepEqual(partial.map((step) => step.label).slice(0, 4), ["secret scan", "runtime budget", "format", "package contents"]);
  // No proof (the plain publisher) keeps every step; a dry-run keeps its dry-run even with verifyOnPublish.
  assert.equal(buildCargoPublishPlan({ crate: "c", crateDir: "x", dryRun: false, proven: { tests: false } }).length, 12);
  const dry = buildCargoPublishPlan({ crate: "c", crateDir: "x", dryRun: true, proven: all, verifyOnPublish: true });
  assert.equal(dry.at(-1).label, "registry dry-run");
  assert.equal(buildCargoPublishPlan({ crate: "c", crateDir: "x", dryRun: false, verifyOnPublish: true }).some((step) => step.label === "registry dry-run"), false);
});

test("trustEvidenceSteps skips only what the evidence proves; verifyOnPublish publishes with Cargo's verify build", async (t) => {
  const fx = fixture(t);
  const commit = "a".repeat(40);
  const stepsRun = [];
  const execute = (command, args) => {
    stepsRun.push(args.join(" "));
    return { status: 0, stdout: args[0] === "metadata" ? JSON.stringify({ target_directory: path.join(fx.root, "generated-target") }) : args.includes("--list") ? "Cargo.toml\nsrc/lib.rs\n" : "" };
  };
  const evidence = {
    commit, name: fx.name, artifactSha256: fx.evidence.artifactSha256,
    runs: [{ host: { os: "darwin" }, commit, steps: [{ command: `cargo test -p ${fx.name} --no-fail-fast -- --show-output`, ok: true, executed: 3, failed: 0 }, { command: `cargo clippy -p ${fx.name} --no-deps --all-targets -- -D warnings`, ok: true }] }],
  };
  const deps = { ...fx.deps, enforceGate: () => ({ ok: true, evidence }) };
  const lines = [];
  const write = process.stdout.write.bind(process.stdout);
  process.stdout.write = (chunk) => { lines.push(String(chunk)); return true; };
  try { await runCargoPublish({ ...fx.options, trustEvidenceSteps: true, verifyOnPublish: true }, execute, deps); } finally { process.stdout.write = write; }
  // fmt has no batch receipt, so it still runs; tests and clippy are proven; the dry-run is gone.
  assert.deepEqual(stepsRun, [`fmt -p ${fx.name} --check`, `package --list -p ${fx.name}`, `package --no-verify -p ${fx.name}`, "metadata --format-version 1 --no-deps", `publish --locked -p ${fx.name}`]);
  assert.match(lines.join(""), /cargo tests: proven by the gate evidence/);
  // Without trustEvidenceSteps the same evidence skips nothing (a plain publish proved nothing in advance).
  stepsRun.length = 0;
  await runCargoPublish({ ...fx.options }, (command, args) => { const result = execute(command, args); stepsRun.splice(stepsRun.length - 1, 1, args[0]); return result; }, deps).catch(() => {});
  assert.ok(stepsRun.includes("test") && stepsRun.includes("clippy") && stepsRun.includes("fmt"));
});

test("an async execute is awaited", async (t) => {
  const fx = fixture(t);
  const seen = [];
  const execute = async (command, args, settings) => { await new Promise((resolve) => setTimeout(resolve, 1)); seen.push(args[0]); return fx.execute(command, args, settings); };
  await runCargoPublish(fx.options, execute, fx.deps);
  assert.ok(seen.includes("publish"));
  assert.deepEqual(fx.events, ["package", "hash check", "publish", "readback"]);
});
