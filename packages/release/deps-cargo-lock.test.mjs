import assert from "node:assert/strict";
import { execFileSync } from "./background-process.mjs";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { CARRIED_CARGO_MAP_ENV } from "./carried-cargo.mjs";
import { CHECKOUT_ENV } from "./gate-stages.mjs";
import { cargoUpdateArgv, parsePins, runCargoLockCommand, updateCargoLock } from "./deps-cargo-lock.mjs";

const ORIGINAL_LOCK = "version = 4\n# before\n";

// Real git fixtures under the scratch volume (never the workspace root); the cargo runner is always a fake.
const scratchRoot = existsSync("/Volumes/D/scratch") ? "/Volumes/D/scratch" : os.tmpdir();
const git = (args, cwd) => execFileSync("git", args, { cwd, stdio: ["ignore", "pipe", "pipe"] }).toString();

function fixture(t) {
  mkdirSync(scratchRoot, { recursive: true });
  const base = mkdtempSync(path.join(scratchRoot, "lock-door-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const ws = path.join(base, "app");
  mkdirSync(path.join(ws, "src"), { recursive: true });
  writeFileSync(path.join(ws, "Cargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
  writeFileSync(path.join(ws, "Cargo.lock"), ORIGINAL_LOCK);
  writeFileSync(path.join(ws, "src", "lib.rs"), "pub fn value() -> u8 { 1 }\n");
  git(["init", "-q"], ws);
  git(["add", "-A"], ws);
  git(["-c", "user.name=test", "-c", "user.email=test@example.invalid", "commit", "-q", "-m", "init"], ws);
  return { base, ws, manifest: path.join(ws, "Cargo.toml"), lock: path.join(ws, "Cargo.lock"), lib: path.join(ws, "src", "lib.rs") };
}

/** A fake managed cargo runner: records each request and applies `effect(args)` as if cargo had run; `status` is its exit. */
function fakeCargo(effect = () => {}, result = () => ({ status: 0 })) {
  const calls = [];
  const run = (args, options) => {
    calls.push({ args, cwd: options.cwd, env: options.env });
    effect(args);
    return result(args);
  };
  return { calls, run };
}

test("one pin is one uncarried request, in the manifest's workspace, and only the lock is written", (t) => {
  const f = fixture(t);
  const { calls, run } = fakeCargo(() => writeFileSync(f.lock, "version = 4\n# refreshed\n"));
  const outcome = updateCargoLock({ manifest: f.manifest, pins: ["serde=1.0.200"], run });
  assert.deepEqual(calls.map((call) => call.args), [["update", "--manifest-path", f.manifest, "-p", "serde", "--precise", "1.0.200"]]);
  assert.equal(calls[0].cwd, f.ws);
  assert.equal(readFileSync(f.lock, "utf8"), "version = 4\n# refreshed\n");
  assert.deepEqual(outcome, { workspace: f.ws, lock: f.lock, pins: [{ crate: "serde", version: "1.0.200" }] });
});

test("several pins are sequential requests in the order given, and no request carries a redirect", (t) => {
  const f = fixture(t);
  const { calls, run } = fakeCargo(() => writeFileSync(f.lock, "version = 4\n# refreshed\n"));
  const outcome = updateCargoLock({ manifest: f.manifest, pins: ["rightkit-qa=0.10.0", "rightkit-asr=0.4.0"], run });
  assert.deepEqual(calls.map((call) => call.args), [
    ["update", "--manifest-path", f.manifest, "-p", "rightkit-qa", "--precise", "0.10.0"],
    ["update", "--manifest-path", f.manifest, "-p", "rightkit-asr", "--precise", "0.4.0"],
  ]);
  for (const { args } of calls) {
    assert.equal(args.includes("--config"), false);
    assert.equal(args.includes("--target-dir"), false);
    assert.equal(args.some((arg) => arg.startsWith("+")), false);
  }
  assert.equal(outcome.pins.length, 2);
});

test("cargoUpdateArgv never carries --config, --target-dir or a toolchain", () => {
  assert.deepEqual(cargoUpdateArgv({ manifest: "/w/Cargo.toml", crate: "a", version: "1.2.3" }), ["update", "--manifest-path", "/w/Cargo.toml", "-p", "a", "--precise", "1.2.3"]);
});

test("a change outside Cargo.lock is refused with RK_LOCK_UPDATE_OUTSIDE_LOCK and the lock is restored", (t) => {
  const f = fixture(t);
  const { run } = fakeCargo(() => {
    writeFileSync(f.lock, "version = 4\n# refreshed\n");
    writeFileSync(f.lib, "pub fn value() -> u8 { 2 }\n");
  });
  assert.throws(
    () => updateCargoLock({ manifest: f.manifest, pins: ["serde=1.0.200"], run }),
    (error) => error.code === "RK_LOCK_UPDATE_OUTSIDE_LOCK" && /src[\\/]lib\.rs/.test(error.message),
  );
  assert.equal(readFileSync(f.lock, "utf8"), ORIGINAL_LOCK);
});

test("a new untracked file outside the lock is refused too", (t) => {
  const f = fixture(t);
  const { run } = fakeCargo(() => writeFileSync(path.join(f.ws, "stray.txt"), "x\n"));
  assert.throws(() => updateCargoLock({ manifest: f.manifest, pins: ["serde=1.0.200"], run }), (error) => error.code === "RK_LOCK_UPDATE_OUTSIDE_LOCK" && /stray\.txt/.test(error.message));
});

test("a gate stage's snapshot is refused before any request", (t) => {
  const f = fixture(t);
  for (const name of [CHECKOUT_ENV, CARRIED_CARGO_MAP_ENV]) {
    const { calls, run } = fakeCargo();
    assert.throws(
      () => updateCargoLock({ manifest: f.manifest, pins: ["serde=1.0.200"], run, env: { [name]: "/some/path" } }),
      (error) => error.code === "RK_LOCK_UPDATE_SNAPSHOT",
    );
    assert.equal(calls.length, 0);
  }
});

test("a linked worktree is refused: the lock is updated only in the primary checkout", (t) => {
  const f = fixture(t);
  const worktree = path.join(f.base, "wt");
  git(["worktree", "add", "-q", "--detach", worktree], f.ws);
  const { calls, run } = fakeCargo();
  assert.throws(
    () => updateCargoLock({ manifest: path.join(worktree, "Cargo.toml"), pins: ["serde=1.0.200"], run }),
    (error) => error.code === "RK_LOCK_UPDATE_CHECKOUT" && /linked worktree/.test(error.message),
  );
  assert.equal(calls.length, 0);
});

test("a workspace with no Cargo.lock is refused, and an unparsable pin is refused before any request", (t) => {
  const f = fixture(t);
  const { calls, run } = fakeCargo();
  for (const pin of ["serde", "serde=1.0", "serde=v1.0.0", "1serde=1.0.0", "=1.0.0"]) {
    assert.throws(() => updateCargoLock({ manifest: f.manifest, pins: [pin], run }), (error) => error.code === "RK_LOCK_UPDATE_PIN", pin);
  }
  assert.equal(calls.length, 0);
  unlinkSync(f.lock);
  assert.throws(() => updateCargoLock({ manifest: f.manifest, pins: ["serde=1.0.200"], run }), (error) => error.code === "RK_LOCK_UPDATE_NO_LOCK");
  assert.equal(calls.length, 0);
});

test("parsePins accepts SemVer pins and refuses a missing list or a repeated crate", () => {
  assert.deepEqual(parsePins(["rightkit-qa=0.10.0", "x=1.2.3-rc.1+build.5"]), [
    { crate: "rightkit-qa", version: "0.10.0" },
    { crate: "x", version: "1.2.3-rc.1+build.5" },
  ]);
  assert.throws(() => parsePins([]), (error) => error.code === "RK_LOCK_UPDATE_ARGS");
  assert.throws(() => parsePins(["a=1.0.0", "a=1.0.1"]), (error) => error.code === "RK_LOCK_UPDATE_PIN");
});

test("a failed request and an unavailable broker both restore the lock", (t) => {
  const f = fixture(t);
  const failing = fakeCargo(() => writeFileSync(f.lock, "# partial\n"), () => ({ status: 101 }));
  assert.throws(() => updateCargoLock({ manifest: f.manifest, pins: ["serde=1.0.200"], run: failing.run }), (error) => error.code === "RK_LOCK_UPDATE_FAILED");
  assert.equal(readFileSync(f.lock, "utf8"), ORIGINAL_LOCK);
  const unavailable = fakeCargo(() => writeFileSync(f.lock, "# partial\n"), () => ({
    status: null,
    error: Object.assign(new Error("RightKit broker socket is unavailable"), { code: "RK_BROKER_UNAVAILABLE" }),
  }));
  assert.throws(() => updateCargoLock({ manifest: f.manifest, pins: ["serde=1.0.200"], run: unavailable.run }), (error) => error.code === "RK_BROKER_UNAVAILABLE");
  assert.equal(readFileSync(f.lock, "utf8"), ORIGINAL_LOCK);
});

test("the command line: usage errors exit 2, refusals exit 1 with the typed code, success exits 0", (t) => {
  const f = fixture(t);
  const logged = [];
  const failed = [];
  const log = (line) => logged.push(line);
  const fail = (line) => failed.push(line);
  assert.equal(runCargoLockCommand([], { log, fail }), 2);
  assert.equal(runCargoLockCommand(["--manifest", f.manifest], { log, fail }), 2);
  assert.equal(runCargoLockCommand(["--manifest"], { log, fail }), 2);
  assert.equal(runCargoLockCommand(["--manifest", f.manifest, "--pin", "a=1.0.0", "--bogus"], { log, fail }), 2);
  assert.equal(runCargoLockCommand(["--manifest", f.manifest, "--manifest", f.manifest, "--pin", "a=1.0.0"], { log, fail }), 2);

  const ok = fakeCargo(() => writeFileSync(f.lock, "version = 4\n# refreshed\n"));
  assert.equal(runCargoLockCommand(["--manifest", f.manifest, "--pin", "serde=1.0.200", "--pin", "rightkit-asr=0.4.0"], { run: ok.run, log, fail }), 0);
  assert.equal(ok.calls.length, 2);
  assert.ok(logged.some((line) => /updated \(serde 1\.0\.200, rightkit-asr 0\.4\.0\)/.test(line)));

  const outside = fakeCargo(() => writeFileSync(f.lib, "changed\n"));
  assert.equal(runCargoLockCommand(["--manifest", f.manifest, "--pin", "serde=1.0.200"], { run: outside.run, log, fail }), 1);
  assert.ok(failed.some((line) => /RK_LOCK_UPDATE_OUTSIDE_LOCK/.test(line)));
  assert.equal(readFileSync(f.lock, "utf8"), "version = 4\n# refreshed\n");
});
