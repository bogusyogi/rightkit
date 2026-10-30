// rightkit#1301: a call right-release itself makes through the seam carries its own per-call request, never the stage grant its
// third-party siblings run under: an inherited RUSTRIGHT_SNAPSHOT_GRANT is dropped from the call's environment.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { installCarrier } from "./carried-cargo.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { SNAPSHOT_GRANT_ENV } from "./snapshot-request.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

test("the seam drops an inherited stage grant: the call is a per-call request and the fixture broker sees no grant beside it", { skip: process.platform === "win32" && "POSIX fixture client" }, (t) => {
  const fx = makeFakeBroker(t);
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout, native: path.join(fx.checkout, "native") }, section: brokerSection(fx.sb), platform: "win32", env: {}, grant: { primary: fx.checkout, callingGate: "/gate/bin/rightkit-gate.mjs" } });
  t.after(() => installed.cleanup());
  assert.ok(installed.env[SNAPSHOT_GRANT_ENV]);
  const r = runCargoSync(["build", "--locked"], { cwd: path.join(fx.snapshot, "crates", "a"), env: { PATH: process.env.PATH, ...installed.env }, encoding: "utf8" }, { platform: "win32" });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(fx.calls().length, 1, "one per-call request");
  assert.equal(fx.grantCalls().length, 0, "and no grant call");
  assert.equal(r.outputDir, fx.outputDir);
});
