import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import childProcess from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import test, { after } from "node:test";
import { brokerLaunch, LOG_ENV, OPT_IN_ENV } from "./test-support/broker-guard.mjs";

const optedIn = process.env[OPT_IN_ENV] === "1";
const skipWhenOptedIn = optedIn ? `${OPT_IN_ENV}=1 runs real broker calls on purpose` : false;
const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-broker-guard-"));
after(() => rmSync(root, { recursive: true, force: true }));

test("broker guard classifies broker-reaching launches and leaves temp-directory fixtures alone", () => {
  assert.equal(brokerLaunch("rightkit"), true);
  assert.equal(brokerLaunch("cargo"), true);
  assert.equal(brokerLaunch("/Library/Application Support/RightKit/build-control/runtime-current/rustright"), true);
  assert.equal(brokerLaunch("/Volumes/D/.rightkit-managed/agent-bin/cargo"), true);
  assert.equal(brokerLaunch(path.join(root, "runtime-current", "rustright")), false, "a temp fixture client is not the broker");
  assert.equal(brokerLaunch(process.execPath), false);
});

test("broker guard propagates to child Node processes through NODE_OPTIONS", { skip: skipWhenOptedIn }, () => {
  assert.match(process.env.NODE_OPTIONS ?? "", /--import=file:\/\/\/.*broker-guard\.mjs/);
});

test("broker guard refuses live broker launches and broker socket connects", { skip: skipWhenOptedIn }, () => {
  assert.throws(() => childProcess.spawnSync("rightkit", ["cargo", "build"]), /RightKit broker guard: a test tried to reach the live RightKit build broker/);
  assert.throws(() => childProcess.spawnSync("cargo", ["build"]), /RightKit broker guard/);
  assert.throws(() => childProcess.execFileSync("rustright", ["cargo", "build"]), /RightKit broker guard/);
  assert.throws(() => net.connect({ path: path.join(root, "broker.sock") }), /RightKit broker guard: .*broker socket/);
});

test("broker guard answers Cargo metadata with a stub computed from the manifest and records the call", { skip: skipWhenOptedIn }, () => {
  const dir = path.join(root, "stub-app");
  mkdirSync(dir, { recursive: true });
  const manifest = path.join(dir, "Cargo.toml");
  writeFileSync(manifest, '[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\n\n[dependencies]\nrightkit-license = "=0.1.1"\nserde = "1"\n', "utf8");
  const log = path.join(root, "stub-calls.jsonl");
  const previous = process.env[LOG_ENV];
  process.env[LOG_ENV] = log;
  try {
    const result = childProcess.spawnSync("cargo", ["metadata", "--no-deps", "--offline", "--format-version", "1", "--manifest-path", manifest], { cwd: dir, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    const metadata = JSON.parse(result.stdout);
    assert.equal(path.resolve(metadata.packages[0].manifest_path).toLowerCase(), path.resolve(manifest).toLowerCase());
    assert.equal(path.isAbsolute(metadata.target_directory), true);
    const license = metadata.packages[0].dependencies.find((dependency) => dependency.name === "rightkit-license");
    assert.equal(license.req, "=0.1.1");
    assert.equal(license.source, "registry+https://github.com/rust-lang/crates.io-index");
    assert.equal(license.path, null);
    const calls = readFileSync(log, "utf8").trim().split(/\r?\n/).map((line) => JSON.parse(line));
    assert.equal(calls.length, 1);
    assert.equal(calls[0].action, "stubbed");
    assert.equal(calls[0].status, 0);
  } finally {
    if (previous === undefined) delete process.env[LOG_ENV];
    else process.env[LOG_ENV] = previous;
  }
});

test("broker guard reports a manifest the stub cannot parse as a failed Cargo metadata call", { skip: skipWhenOptedIn }, () => {
  const manifest = path.join(root, "broken", "Cargo.toml");
  mkdirSync(path.dirname(manifest), { recursive: true });
  writeFileSync(manifest, "not valid toml", "utf8");
  const result = childProcess.spawnSync("cargo", ["metadata", "--no-deps", "--manifest-path", manifest], { encoding: "utf8" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /failed to parse manifest/);
});

test("broker guard passes a temp-directory fixture client through to the process", { skip: process.platform === "win32" || skipWhenOptedIn }, () => {
  const client = path.join(root, "runtime-current", "rustright");
  mkdirSync(path.dirname(client), { recursive: true });
  writeFileSync(client, "#!/bin/sh\nprintf 'fixture-client'\n", "utf8");
  chmodSync(client, 0o755);
  assert.equal(existsSync(client), true);
  const result = childProcess.spawnSync(client, ["cargo", "metadata"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "fixture-client");
});
