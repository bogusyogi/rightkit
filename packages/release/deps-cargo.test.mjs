import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { cargoAuditWorkdirs, runCargoAudits } from "./deps-cargo.mjs";

const root = path.resolve("/app");
const locks = (...dirs) => (file) => dirs.some((dir) => file === path.join(root, dir, "Cargo.lock"));

test("no deps.cargoWorkdirs audits no Rust", () => {
  assert.deepEqual(cargoAuditWorkdirs({}, root), []);
  assert.deepEqual(cargoAuditWorkdirs({ deps: { workdirs: ["."] } }, root), []);
  assert.deepEqual(cargoAuditWorkdirs({ deps: { cargoWorkdirs: [] } }, root), []);
});

test("declared workdirs resolve inside the app, are de-duplicated and need a Cargo.lock", () => {
  const exists = locks("src-tauri", "crates/engine");
  const dirs = cargoAuditWorkdirs({ deps: { cargoWorkdirs: ["src-tauri", "./src-tauri", "crates\\engine"] } }, root, { exists });
  assert.deepEqual(dirs.map((item) => item.cwd), [path.join(root, "src-tauri"), path.join(root, "crates", "engine")]);
  assert.throws(() => cargoAuditWorkdirs({ deps: { cargoWorkdirs: ["src-tauri", "other"] } }, root, { exists }), /other has no Cargo\.lock/);
});

test("absolute, parent and malformed entries are refused", () => {
  const exists = () => true;
  for (const entry of ["../x", "a/../../x", "/abs", "C:\\abs", "", 3]) {
    assert.throws(() => cargoAuditWorkdirs({ deps: { cargoWorkdirs: [entry] } }, root, { exists }), /deps\.cargoWorkdirs/);
  }
  assert.throws(() => cargoAuditWorkdirs({ deps: { cargoWorkdirs: "src-tauri" } }, root, { exists }), /list of app-relative folders/);
});

test("cargo audit runs once per workdir through the managed runner and every failure is reported", () => {
  const calls = [];
  const statuses = [0, 1, null];
  const run = (args, options) => {
    calls.push({ args, cwd: options.cwd });
    const status = statuses[calls.length - 1];
    return status === null ? { status: null, error: Object.assign(new Error("RightKit broker socket is unavailable"), { code: "RK_BROKER_UNAVAILABLE" }) } : { status };
  };
  const workdirs = ["a", "b", "c"].map((rel) => ({ rel, cwd: path.join(root, rel) }));
  const { failures } = runCargoAudits(workdirs, { run, log() {} });
  assert.deepEqual(calls.map((call) => call.args), [["audit"], ["audit"], ["audit"]]);
  assert.deepEqual(calls.map((call) => call.cwd), workdirs.map((item) => item.cwd));
  assert.deepEqual(failures, ["b: cargo audit exited 1", "c: cargo audit could not start (RightKit broker socket is unavailable)"]);
  assert.deepEqual(runCargoAudits([], { run: () => assert.fail("nothing to audit"), log() {} }), { failures: [] });
});
