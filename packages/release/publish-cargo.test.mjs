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
  runCargoReadOnly,
  scanCrateForSecrets,
} from "./publish-cargo.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { HOST_UPLOAD_REFUSED } from "./host-upload-refusal.mjs";
import { mintTestCapability } from "./test-support/publish-capability-fixture.mjs";
import { checkPublicationGate } from "./publication-gate.mjs";
import { localTestPlan } from "./local-publish.mjs";

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
  assert.deepEqual(plan.find((step) => step.label === "package archive").args, ["package", "--locked", "--no-verify", "-p", "rightkit-license"]);
  assert.equal(plan.find((step) => step.label === "archive hash check").internal, "verify-archive");
  assert.deepEqual(plan.find((step) => step.label === "registry publish"), {
    label: "registry publish", command: "cargo", args: ["publish", "--no-verify", "-p", "rightkit-license"],
  });
  assert.equal(plan.at(-1)?.internal, "registry-readback");
  assert.deepEqual(plan.find((step) => step.label === "format").args, ["fmt", "-p", "rightkit-license", "--check"]);
  assert.deepEqual(plan.find((step) => step.label === "tests").args, ["test", "-p", "rightkit-license", "--", "--test-threads=1"]);
});

test("local offline mode scopes offline to package inspection, then keeps upload online", () => {
  const plan = buildCargoPublishPlan({ crate: "rightkit-license", crateDir: "C:/rightkit/crates/rightkit-license", dryRun: false, offlineReadOnly: true, verifyOnPublish: true });
  assert.deepEqual(plan.find((step) => step.label === "package contents").args, ["package", "--list", "--offline", "-p", "rightkit-license"]);
  // The archive is packaged online and locked even in offline mode: an offline package resolves its
  // Cargo.lock from downloaded .crate files only and can drift from the qualified archive.
  assert.deepEqual(plan.find((step) => step.label === "package archive").args, ["package", "--locked", "--no-verify", "-p", "rightkit-license"]);
  assert.deepEqual(plan.find((step) => step.label === "package location").args, ["metadata", "--offline", "--format-version", "1", "--no-deps"]);
  assert.deepEqual(plan.find((step) => step.label === "format").args, ["fmt", "-p", "rightkit-license", "--check"]);
  assert.deepEqual(plan.find((step) => step.label === "tests").args, ["test", "-p", "rightkit-license", "--", "--test-threads=1"]);
  assert.deepEqual(plan.find((step) => step.label === "registry publish").args, ["publish", "--locked", "-p", "rightkit-license"]);
  assert.equal(plan.find((step) => step.label === "registry dry-run"), undefined);
});

test("offline inspection retries one explicit cache miss online and preserves both failures", async () => {
  const calls = [], logged = [];
  const execute = async (_command, args) => {
    calls.push(args);
    return calls.length === 1
      ? { status: 101, stdout: "first out", stderr: "attempting to make an HTTP request, but --offline was specified" }
      : { status: 101, stdout: "second out", stderr: "second error" };
  };
  const result = await runCargoReadOnly(["metadata", "--offline", "--format-version", "1"], {}, execute, (failure) => logged.push(failure.stderr));
  assert.deepEqual(calls, [["metadata", "--offline", "--format-version", "1"], ["metadata", "--format-version", "1"]]);
  assert.deepEqual(logged, ["attempting to make an HTTP request, but --offline was specified"]);
  assert.equal(result.offlineFallbackFailed, true);
  assert.match(result.stdout, /first out[\s\S]*second out/);
  assert.match(result.stderr, /attempting to make an HTTP request[\s\S]*second error/);
});

test("offline cache miss succeeds with one online retry", async () => {
  const calls = [];
  const execute = async (_command, args) => {
    calls.push(args);
    return calls.length === 1
      ? { status: 101, stderr: "As a reminder, you're using offline mode (--offline)" }
      : { status: 0, stdout: "{}", stderr: "" };
  };
  const result = await runCargoReadOnly(["package", "--offline", "--no-verify"], {}, execute);
  assert.equal(result.usedOfflineFallback, true);
  assert.deepEqual(calls, [["package", "--offline", "--no-verify"], ["package", "--no-verify"]]);
});

test("offline inspection does not retry ordinary failures or non-inspection commands", async () => {
  let calls = 0;
  const execute = async () => { calls += 1; return { status: 101, stdout: "compile failed", stderr: "build error" }; };
  await runCargoReadOnly(["package", "--offline", "--list"], {}, execute);
  await runCargoReadOnly(["test", "--offline", "-p", "x"], {}, execute);
  assert.equal(calls, 2);
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

test("secret scan: an env template is refused by name in a crate, read by content in the repository gate", (t) => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-envtpl-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(path.join(root, "sub"));
  writeFileSync(path.join(root, ".env.example"), "API_URL=\nAPI_TOKEN=\n");
  writeFileSync(path.join(root, "sub", ".env.test.example"), "TOKEN=" + "gh" + "p_" + "B".repeat(24) + "\n");
  writeFileSync(path.join(root, ".env.sample"), "A=\n");
  writeFileSync(path.join(root, ".env"), "A=1\n");
  writeFileSync(path.join(root, ".env.local"), "A=1\n");
  assert.deepEqual(scanCrateForSecrets(root), [".env", ".env.example", ".env.local", ".env.sample", "sub/.env.test.example"]);
  assert.deepEqual(scanCrateForSecrets(root, { envTemplates: true }), [".env", ".env.local", "sub/.env.test.example"], "a template with a credential is still found; .env and .env.local stay refused by name");
});

test("secret scan: a populated env template is refused by content, an empty or placeholder one passes", (t) => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-envcred-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  // Credentials are assembled from parts so this file holds none of the shapes it tests.
  const found = {
    "stripe.example": "STRIPE_SECRET_KEY=" + "sk" + "_live_" + "a1B2c3D4e5F6g7H8i9J0k1L2\n",
    "dburl.example": "DATABASE_URL=postgres://admin:" + "S3cr3t" + "Pw0rd@db.internal/app\n",
    "openai.example": "OPENAI_API_KEY=" + "sk" + "-proj-" + "abcdEFGH1234abcdEFGH1234abcd\n",
    "slack.example": "NOTIFY=" + "xox" + "b-" + "1234567890-abcdefghij\n",
    "jwt.example": "JWT_SECRET=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\n",
    "google.example": "MAPS_KEY=" + "AI" + "za" + "SyA1234567890abcdefghijklmnopqrstuv\n",
    "quoted.sample": "export DB_PASSWORD='hunter2hunter2' # prod\n",
    "commented.template": "# CLIENT_SECRET=abc123def456\n",
    "url-in-comment.example": "# use postgres://admin:" + "S3cr3t" + "Pw0rd@db.internal/app locally\n",
  };
  const clean = {
    "empty.example": "API_URL=\nAPI_TOKEN=\nDB_PASSWORD=\"\"\nCLIENT_SECRET=   # fill in\n",
    "placeholders.example": "API_TOKEN=<your token>\nJWT_SECRET=${JWT_SECRET}\nDB_PASSWORD=changeme\nSTRIPE_SECRET_KEY=sk-...\nAPI_KEY=xxxxxxxx\nCLIENT_SECRET=your-secret-here\n",
    "urls.example": "DATABASE_URL=postgres://user:<password>@localhost/app\nREDIS_URL=redis://:${REDIS_PASSWORD}@localhost\nSITE=https://example.com/a:b\nBARE=postgres://user@host/db\n",
    "vendor-placeholders.example": "A=sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxx\nB=" + "sk" + "_live_" + "xxxxxxxxxxxxxxxx\nC=" + "xox" + "b-<token>\nD=" + "AI" + "za" + "X".repeat(35) + "\n",
    "prose.example": "# Token for the API. Get one at https://example.com/token\n# SECRET_KEY=generate with openssl rand -hex 32\nPORT=3000\nNODE_ENV=development\nTOKEN_REFRESH=true\n",
  };
  for (const [name, body] of Object.entries({ ...found, ...clean })) writeFileSync(path.join(root, ".env." + name), body);
  const dotted = (names) => names.map((name) => ".env." + name).sort();
  assert.deepEqual(scanCrateForSecrets(root, { envTemplates: true }), dotted(Object.keys(found)), "each credential shape in a template is found; empty and placeholder templates pass");
  assert.deepEqual(scanCrateForSecrets(root), dotted([...Object.keys(found), ...Object.keys(clean)]), "a crate package still refuses every template by name");
});

test("secret scan detects credentials beside NUL bytes while accepting benign binary assets", (t) => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-cargo-secrets-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const credential = Buffer.from("gh" + "p_" + "A".repeat(24));
  writeFileSync(path.join(root, "prefix.bin"), Buffer.concat([Buffer.from([0]), credential]));
  writeFileSync(path.join(root, "suffix.bin"), Buffer.concat([credential, Buffer.from([0])]));
  writeFileSync(path.join(root, "benign.bin"), Buffer.from([0, 255, 128, 1, 0, 254]));
  assert.deepEqual(scanCrateForSecrets(root), ["prefix.bin", "suffix.bin"]);
});

function fixture(t, { verifyOnPublish = false } = {}) {
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
      assert.deepEqual(args, ["publish", verifyOnPublish ? "--locked" : "--no-verify", "-p", name]);
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
  // A capability for exactly this crate archive, created through the production admission validator.
  const capability = mintTestCapability({ registry: "crates", archives: [{ name, version, sha256: evidence.artifactSha256 }] });
  return { root, name, version, bytes, crateFile, evidence, events, execute, deps, capability, options: { workspaceRoot: root, crate: name, capability } };
}

test("Cargo publisher packages, checks evidence hash, publishes through same runner & reads back", async (t) => {
  const fx = fixture(t);
  const result = await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.deepEqual(fx.events, ["package", "hash check", "publish", "readback"]);
  assert.deepEqual(result, { name: fx.name, version: fx.version, sha256: fx.evidence.artifactSha256 });
});

test("local offline inspections capture output while registry publish stays online", async (t) => {
  const fx = fixture(t, { verifyOnPublish: true }), seen = [];
  const execute = (command, args, settings) => {
    seen.push({ args, stdio: settings.stdio });
    return fx.execute(command, args, settings);
  };
  await runCargoPublish({ ...fx.options, offlineReadOnly: true, verifyOnPublish: true }, execute, fx.deps);
  // The archive step (`package --no-verify`) is packaged online and locked; only inspections are offline.
  for (const entry of seen.filter(({ args }) => ["package", "metadata"].includes(args[0]) && !args.includes("--no-verify"))) {
    assert.ok(entry.args.includes("--offline"));
    assert.equal(entry.stdio, "pipe");
  }
  const upload = seen.find(({ args }) => args[0] === "publish" && !args.includes("--dry-run"));
  assert.ok(upload);
  assert.equal(upload.args.includes("--offline"), false);
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

// A direct import must not publish: a real run without a capability from a validated admission refuses before the gate, packaging
// or any command; a capability for other bytes refuses before the registry upload.
test("a real publish refuses without a publish capability from a validated admission, before the gate or any command", async (t) => {
  const fx = fixture(t);
  let gated = 0;
  const deps = { ...fx.deps, enforceGate: () => { gated += 1; return { ok: true, evidence: fx.evidence }; } };
  const other = mintTestCapability({ registry: "crates", archives: [{ name: "rightkit-other", version: fx.version, sha256: fx.evidence.artifactSha256 }] });
  for (const [label, capability] of Object.entries({
    "no capability": undefined,
    "an object literal shaped like one": { kind: "rightkit-publish-capability", registry: "crates" },
    "a copy of a real capability": { ...fx.capability },
    "the capability of another crate": other,
    "an npm capability": mintTestCapability({ registry: "npm", archives: [{ name: fx.name, version: fx.version, sha256: fx.evidence.artifactSha256 }] }),
  })) {
    await assert.rejects(runCargoPublish({ ...fx.options, capability }, fx.execute, deps), (error) => error.code === HOST_UPLOAD_REFUSED && /host uploads are retired/.test(error.message), label);
  }
  assert.equal(gated, 0, "refused before the gate");
  assert.deepEqual(fx.events, [], "and before any command");
  await assert.rejects(runCargoPublish({ ...fx.options, fromEvidence: true, evidence: "gate.json", capability: undefined }, fx.execute, deps), { code: HOST_UPLOAD_REFUSED });
});

test("a capability for other bytes refuses before the registry upload and readback", async (t) => {
  const fx = fixture(t);
  const wrongBytes = mintTestCapability({ registry: "crates", archives: [{ name: fx.name, version: fx.version, sha256: "e".repeat(64) }] });
  await assert.rejects(runCargoPublish({ ...fx.options, capability: wrongBytes }, fx.execute, fx.deps), (error) => error.code === HOST_UPLOAD_REFUSED && /are not the admitted archive/.test(error.message));
  assert.deepEqual(fx.events, ["package", "hash check"], "packaged and hash-checked, never uploaded");
  const wrongVersion = mintTestCapability({ registry: "crates", archives: [{ name: fx.name, version: "9.9.9", sha256: fx.evidence.artifactSha256 }] });
  await assert.rejects(runCargoPublish({ ...fx.options, capability: wrongVersion }, fx.execute, fx.deps), (error) => error.code === HOST_UPLOAD_REFUSED && /is not the admitted 9\.9\.9/.test(error.message));
});

test("a dry run needs no capability", async (t) => {
  const fx = fixture(t);
  assert.deepEqual(await runCargoPublish({ ...fx.options, capability: undefined, dryRun: true }, fx.execute, fx.deps), { name: fx.name, version: fx.version, dryRun: true });
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
  const forbidden = /\bCARGO_REGISTRY_TOKEN\b|\bCARGO_REGISTRIES_[A-Z0-9_]+_TOKEN\b|["'`]credentials\.toml["'`]|\.cargo[/\\]credentials(?:\.toml)?/;
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
  assert.deepEqual(stepsRun, [`fmt -p ${fx.name} --check`, `package --list -p ${fx.name}`, `package --locked --no-verify -p ${fx.name}`, "metadata --format-version 1 --no-deps", `publish --locked -p ${fx.name}`]);
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

test('from-evidence is explicit and refuses ambiguous or dry-run options', () => {
  const base = ['--crate', 'rightkit-fixture', '--from-evidence'];
  assert.throws(() => parseCargoPublishArgs(base), /requires a gate evidence file/);
  assert.throws(() => parseCargoPublishArgs([...base, '--dry-run']), /requires a gate evidence file/);
  assert.throws(() => parseCargoPublishArgs([...base, 'gate.json', '--dry-run']), /cannot be combined/);
  assert.throws(() => parseCargoPublishArgs([...base, 'gate.json', '--evidence', 'other.json']), /cannot be combined/);
  const parsed = parseCargoPublishArgs([...base, 'gate.json']);
  assert.equal(parsed.evidence, 'gate.json');
  assert.equal(parsed.fromEvidence, true);
});

function qualifiedFixture(t) {
  const fx = fixture(t);
  const commit = 'a'.repeat(40);
  mkdirSync(path.join(fx.root, 'ci'), { recursive: true });
  writeFileSync(path.join(fx.root, 'ci', 'gate.config.json'), '{}');
  const planned = localTestPlan({ kind: 'cargo', unit: fx.name, dir: `crates/${fx.name}`, config: {}, platform: 'win32' });
  const evidence = {
    schema: 1, kind: 'cargo', name: fx.name, version: fx.version, commit,
    artifactSha256: fx.evidence.artifactSha256, hostPolicy: 'win32-only', hostPolicyReason: 'local-build grant',
    runs: [{ host: { os: 'win32', arch: 'x64' }, commit, artifactSha256: fx.evidence.artifactSha256,
      features: ['default'], command: 'qualification', operator: 'agent', timestamp: '2026-10-18T00:00:00Z', passed: 3, failed: 0,
      steps: [
        ...planned.steps.map((step) => ({ command: `${step.command} ${step.args.join(' ')}`, ok: true, ...(step.test ? { executed: 3, failed: 0 } : {}) })),
        { command: `cargo package --locked -p ${fx.name}`, ok: true },
      ],
    }],
  };
  const file = path.join(fx.root, 'gate.json');
  const save = () => writeFileSync(file, JSON.stringify(evidence));
  save();
  const commands = [];
  const execute = (_command, args) => {
    commands.push(args);
    return { status: 0, stdout: args[0] === 'metadata' ? JSON.stringify({ target_directory: path.join(fx.root, 'generated-target') }) : args.includes('--list') ? 'Cargo.toml\nsrc/lib.rs\n' : '' };
  };
  const deps = { ...fx.deps, workspaceStatus: () => ({ status: 0, stdout: '' }), enforceGate: (options) => {
    const result = checkPublicationGate({ ...options, git: (args) => args[0] === 'rev-parse' ? commit : '' });
    if (!result.ok) throw new Error(`publication gate not satisfied: ${result.missing.join('; ')}`);
    return result;
  } };
  return { ...fx, evidence, save, commands, execute, deps, options: { ...fx.options, evidence: file, fromEvidence: true } };
}

test('Windows qualification can publish with no repeated tests, clippy or compile; locked archive hash remains enforced', async (t) => {
  const fx = qualifiedFixture(t);
  await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.equal(fx.commands.some((args) => ['test', 'clippy', 'check', 'build'].includes(args[0])), false);
  assert.equal(fx.commands.some((args) => args.includes('--dry-run')), false);
  assert.deepEqual(fx.commands.find((args) => args[0] === 'publish'), ['publish', '--locked', '--no-verify', '-p', fx.name]);
  assert.ok(fx.commands.some((args) => args[0] === 'package' && args.includes('--no-verify')));
});

test('from-evidence refuses unverified packaging before any command', async (t) => {
  const fx = qualifiedFixture(t);
  fx.evidence.runs[0].steps.pop(); fx.save();
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /requires a verified cargo package/);
  assert.equal(fx.commands.length, 0);
});

test('from-evidence refuses changed commit or version before any command', async (t) => {
  const fx = qualifiedFixture(t);
  fx.evidence.commit = 'b'.repeat(40); fx.save();
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /current HEAD/);
  fx.evidence.commit = 'a'.repeat(40); fx.evidence.version = '9.9.9'; fx.save();
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /evidence version/);
  assert.equal(fx.commands.length, 0);
});

test('from-evidence refuses differing local archive bytes before upload', async (t) => {
  const fx = qualifiedFixture(t);
  writeFileSync(fx.crateFile, 'different cross-host archive');
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), { code: 'PUBLICATION_HASH_MISMATCH' });
  assert.equal(fx.commands.some((args) => args[0] === 'publish'), false);
});


test('from-evidence refuses dirty workspace manifests before any command', async (t) => {
  const fx = qualifiedFixture(t);
  fx.deps.workspaceStatus = () => ({ status: 0, stdout: ' M Cargo.toml\n' });
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /requires clean workspace/);
  assert.equal(fx.commands.length, 0);
});


test('from-evidence refuses missing configured feature, serial or all-feature checks', async (t) => {
  const fx = qualifiedFixture(t);
  const entry = { featureSets: [[], ['--features', 'async']], alsoSerial: true };
  writeFileSync(path.join(fx.root, 'ci', 'gate.config.json'), JSON.stringify({ rust: { [fx.name]: entry } }));
  const planned = localTestPlan({ kind: 'cargo', unit: fx.name, dir: `crates/${fx.name}`, config: { rust: { [fx.name]: entry } }, platform: 'win32' });
  const complete = [
    ...planned.steps.map((step) => ({ command: `${step.command} ${step.args.join(' ')}`, ok: true, ...(step.test ? { executed: 3, failed: 0 } : {}) })),
    { command: `cargo package --locked -p ${fx.name}`, ok: true },
  ];
  for (const absent of complete.filter((row) => /--features async|--test-threads=1|^cargo check/.test(row.command))) {
    fx.evidence.runs[0].steps = complete.filter((row) => row !== absent); fx.save();
    await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /missing configured qualification step/);
  }
  assert.equal(fx.commands.length, 0);
});

test("from-evidence refuses uncommitted gate config edits, which decide the replayed steps (0.4.1 review A3)", async (t) => {
  const fx = qualifiedFixture(t);
  const seen = [];
  fx.deps.workspaceStatus = (workspaceRoot) => { seen.push(workspaceRoot); return { status: 0, stdout: " M ci/gate.config.json\n" }; };
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /requires clean workspace Cargo\.toml, Cargo\.lock and ci gate config/);
  assert.equal(fx.commands.length, 0);
  // The default status command names the gate config files beside the workspace manifest and lock.
  const source = readFileSync(new URL("./publish-cargo.mjs", import.meta.url), "utf8");
  assert.match(source, /"status", "--porcelain", "--", "Cargo\.toml", "Cargo\.lock", "ci\/gate\.config\.json", "ci\/asset-gated\.json"/);
});

test("from-evidence refuses a batch receipt that lints only some feature sets of the crate, and accepts one row per set (0.4.1 review A2)", async (t) => {
  const fx = qualifiedFixture(t);
  const commit = "a".repeat(40);
  const entry = { featureSets: [[], ["--features", "async"]] };
  const config = { rust: { [fx.name]: entry } };
  writeFileSync(path.join(fx.root, "ci", "gate.config.json"), JSON.stringify(config));
  const planned = localTestPlan({ kind: "cargo", unit: fx.name, dir: `crates/${fx.name}`, config, batch: true, platform: "win32" });
  const check = (name, command, extra = {}) => ({ name, command, ok: true, commit, tree: commit, ...extra });
  const lint = (features = []) => check("clippy", `cargo clippy -p ${fx.name} --message-format=json-render-diagnostics --no-deps --all-targets${features.length ? ` ${features.join(" ")}` : ""} -- -D warnings`, { crates: [fx.name] });
  const run = fx.evidence.runs[0];
  run.steps = [
    ...planned.steps.map((step) => ({ command: `${step.command} ${step.args.join(" ")}`, ok: true, ...(step.test ? { executed: 3, failed: 0 } : {}) })),
    { command: `cargo package --locked -p ${fx.name}`, ok: true },
  ];
  assert.equal(run.steps.some((row) => / clippy /.test(row.command)), false, "a batch run records no per-crate clippy step");
  run.batch = { id: "batch-1", commit, tree: commit, crates: [fx.name], checks: [
    check("fmt", "cargo fmt --all --check"), check("secret-scan", `scanCrateForSecrets crates/${fx.name}`, { crates: [fx.name] }), lint(),
  ] };
  fx.save();
  // A 0.4.0 batch receipt: clippy ran on the first feature set only. The gate still accepts it; replay must not.
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /missing batch clippy for feature set: --features async/);
  assert.equal(fx.commands.length, 0);
  run.batch.checks.push(lint(["--features", "async"])); fx.save();
  await runCargoPublish(fx.options, fx.execute, fx.deps);
  assert.ok(fx.commands.some((args) => args[0] === "publish"));
  assert.equal(fx.commands.some((args) => ["test", "clippy", "check", "build"].includes(args[0])), false);
});

test('from-evidence refuses any failed recorded step even beside complete passing evidence', async (t) => {
  const fx = qualifiedFixture(t);
  fx.evidence.runs[0].steps.push({ command: 'cargo check --features broken', ok: false }); fx.save();
  await assert.rejects(runCargoPublish(fx.options, fx.execute, fx.deps), /unsuccessful qualification step/);
  assert.equal(fx.commands.length, 0);
});
