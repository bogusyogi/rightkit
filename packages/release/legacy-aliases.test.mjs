// GB-09: every outbound production call emits the current interface only (schema `rustright.snapshot-request/1`, `RUSTRIGHT_*` names). The fixture
// client reads exactly what RustRight's client reads with RUSTRIGHT_LEGACY_ALIASES=off, so each carried route below passes only if the gate sends no alias.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { spawnBrokerClient } from "./broker-client.mjs";
import { SNAPSHOT_REQUEST_SCHEMA, writeSnapshotRequest } from "./snapshot-request.mjs";
import { LEGACY_INTERFACE_ENV, currentInterfaceEnv, legacyInterfaceNamesIn } from "./rustright-names.mjs";
import { forwardedAdapterEnv } from "./adapter-env.mjs";
import { carryBrokerAdapter, carryBrokerCargo, brokerSection } from "./snapshot-broker.mjs";
import { makeFakeBroker, subHandleStub } from "./test-support/fake-broker.mjs";

test("[GB-09] the carrier schema and every per-call variable the gate writes are the current spelling", (t) => {
  const fx = makeFakeBroker(t);
  const made = writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.sb.commit, snapshot: fx.sb.snapshot, handle: fx.handle, primaryCwd: fx.checkout, argv: ["check"], requestIdFile: path.join(fx.root, "r.json") });
  assert.equal(made.carrier.schema, "rustright.snapshot-request/1");
  assert.equal(SNAPSHOT_REQUEST_SCHEMA, "rustright.snapshot-request/1");
  assert.ok(Object.keys(made.env).every((k) => k.startsWith("RUSTRIGHT_")), Object.keys(made.env).join());
  assert.deepEqual(legacyInterfaceNamesIn(made.env), []);
});

test("[GB-09] a legacy interface name in a call's environment is translated to the current one (the current wins) and never sent; gate-internal RIGHTKIT_ names are untouched", () => {
  const out = currentInterfaceEnv({ RIGHTKIT_ATTEST_NONCE: "a", RIGHTKIT_BUILD_BROKER_SOCKET: "/s", RUSTRIGHT_CALLING_GATE: "new", RIGHTKIT_CALLING_GATE: "old", RIGHTKIT_MANAGED_ROOT: "/m", RIGHTKIT_QA_APP_BINARY: "/q", PATH: "/bin" });
  assert.deepEqual(out, { RUSTRIGHT_ATTEST_NONCE: "a", RUSTRIGHT_BUILD_BROKER_SOCKET: "/s", RUSTRIGHT_CALLING_GATE: "new", RIGHTKIT_MANAGED_ROOT: "/m", RIGHTKIT_QA_APP_BINARY: "/q", PATH: "/bin" });
  const same = { PATH: "/bin" };
  assert.equal(currentInterfaceEnv(same), same);
  for (const [legacy, current] of Object.entries(LEGACY_INTERFACE_ENV)) assert.ok(legacy.startsWith("RIGHTKIT_") && current.startsWith("RUSTRIGHT_") && legacy.slice(9) === current.slice(10), legacy);
  // The one client spawn every production call uses.
  const seen = [];
  spawnBrokerClient((file, args, options) => { seen.push(options.env); return { status: 0 }; }, "rustright", ["status", "x"], { env: { RIGHTKIT_ATTEST_NONCE: "a", RIGHTKIT_ADAPTER_ENV: "X", X: "1" } });
  assert.deepEqual(seen[0], { RUSTRIGHT_ATTEST_NONCE: "a", RUSTRIGHT_ADAPTER_ENV: "X", X: "1" });
  // The legacy adapter-env list is not read either: the client with aliases off would not forward it.
  assert.deepEqual(forwardedAdapterEnv({ RIGHTKIT_ADAPTER_ENV: "X", X: "1" }), {});
});

test("[GB-09] with the aliases off, a carried cargo call and a carried adapter call both reach the broker", (t) => {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  fx.setBehavior({ registeredEnv: { "rightkit-pnpm": [] } });
  const section = brokerSection({ ...fx.sb, markBrokerSubmit: subHandleStub(fx.handle) });
  const records = [];
  const env = { PATH: fx.root, HOME: process.env.HOME, RIGHTKIT_MANAGED_ROOT: "/gate-internal" };
  const cwd = path.join(fx.snapshot, "crates", "a");
  const cargo = carryBrokerCargo({ words: ["check"], cwd, env: { ...env, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" }, section, workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: (e) => records.push(e), err: () => {}, stdio: "capture" });
  assert.equal(cargo.outcome.status, "verified", cargo.stderr);
  const adapter = carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: fx.snapshot, env: { ...env, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" }, section, record: (e) => records.push(e), err: () => {}, stdio: "capture" });
  assert.equal(adapter.outcome.status, "verified", adapter.stderr);
});
