import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { main } from "./release-dependencies-command.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

function fixture(policy = '{ forbiddenCrates: ["rightkit-record", "rightkit-qa"] }') {
  const root = tempRoot("release-dependencies-integration-");
  mkdirSync(path.join(root, "src-tauri"));
  writeFileSync(path.join(root, "src-tauri/Cargo.toml"), '[package]\nname = "cutright"\nversion = "0.1.0"\n');
  writeFileSync(path.join(root, "package.json"), JSON.stringify({ scripts: { package: "cargo build --release --features export" } }));
  writeFileSync(path.join(root, "right-release.config.mjs"), `export default {
    releaseDependencies: ${policy},
    targets: {
      mac: { cargoTarget: "aarch64-apple-darwin", package: { cmd: "pnpm", args: ["run", "package"] } },
      win: { cargoTarget: "x86_64-pc-windows-msvc", package: { cmd: "pnpm", args: ["run", "package"] } }
    }
  };`);
  return root;
}
const clean = { status: 0, stdout: "cutright v0.1.0\nserde v1.0.0\n", stderr: "" };

test("loads real config & package scripts, host-only by default, all shipped targets on request", async () => {
  const root = fixture();
  const calls = [];
  const run = (cmd, args) => {
    assert.equal(cmd, "cargo");
    assert.equal(args[args.indexOf("-e") + 1], "normal");
    assert.equal(args[args.indexOf("--features") + 1], "export");
    calls.push(args[args.indexOf("--target") + 1]);
    return clean;
  };
  assert.equal((await main([], { cwd: root, platform: "darwin", arch: "arm64", run })).length, 1);
  assert.deepEqual(calls, ["aarch64-apple-darwin"]);
  calls.length = 0;
  assert.equal((await main(["--all-targets"], { cwd: root, run })).length, 2);
  assert.deepEqual(calls, ["aarch64-apple-darwin", "x86_64-pc-windows-msvc"]);
});

test("cross-target forbidden dependency or unresolvable tree refuses standalone check", async () => {
  const root = fixture();
  await assert.rejects(main(["--all-targets"], { cwd: root, run: (cmd, args) => args.includes("x86_64-pc-windows-msvc")
    ? { ...clean, stdout: "cutright v0.1.0\nrightkit-record v0.2.0\n" } : clean }), /rightkit-record.*x86_64-pc-windows-msvc/);
  await assert.rejects(main([], { cwd: root, platform: "darwin", run: () => ({ status: 101, stderr: "target not registered" }) }), /release dependencies.*target not registered/);
});

test("custom config resolves relative manifest from config directory", async () => {
  const root = fixture();
  const cwd = tempRoot("release-dependencies-cwd-");
  const results = await main(["--config", path.join(root, "right-release.config.mjs"), "--platform", "mac"], { cwd, run: (cmd, args, options) => {
    assert.equal(options.cwd, root);
    assert.equal(args[args.indexOf("--manifest-path") + 1], path.join(root, "src-tauri/Cargo.toml"));
    return clean;
  } });
  assert.equal(results[0].manifest, path.join("src-tauri", "Cargo.toml"));
});

test("unknown host, invalid CLI flags & missing policy fail before Cargo", async () => {
  const root = fixture();
  const run = () => assert.fail("Cargo invoked");
  for (const args of [["--skip-checks"], ["--platform"], ["--config"], ["--platform", "win", "--all-targets"]]) await assert.rejects(main(args, { cwd: root, run }));
  await assert.rejects(main([], { cwd: root, platform: "unknown", run }), /no release target/);
  await assert.rejects(main([], { cwd: fixture("undefined"), platform: "darwin", run }), /requires releaseDependencies/);
});

test("CLI dispatch reaches dependency command & reports invalid input", () => {
  const result = spawnSync(process.execPath, [fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url)), "check-release-dependencies", "--skip-checks"], {
    cwd: fixture(), encoding: "utf8",
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /unknown check-release-dependencies argument: --skip-checks/);
});

test("release gate runs outside dry-run fallback & before skippable app checks", () => {
  const source = readFileSync(new URL("./release.mjs", import.meta.url), "utf8");
  const gate = source.indexOf("assertReleaseDependencies({ root, config, target, commands: closureCommands });");
  assert.ok(gate > 0);
  assert.ok(gate < source.indexOf("if (!opts.dryRun) throw error;", gate));
  assert.ok(gate < source.indexOf("await runConfiguredChecks("));
  const dev = readFileSync(new URL("./development.mjs", import.meta.url), "utf8");
  assert.ok(!dev.includes("assertReleaseDependencies"));
});


test("default standalone host check follows Intel macOS architecture", async () => {
  const results = await main([], { cwd: fixture(), platform: "darwin", arch: "x64", run: (cmd, args) => {
    assert.equal(args[args.indexOf("--target") + 1], "x86_64-apple-darwin");
    return clean;
  } });
  assert.equal(results[0].target, "x86_64-apple-darwin");
});
