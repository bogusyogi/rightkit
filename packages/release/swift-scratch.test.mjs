// GB-67: SwiftPM's scratch and build path belong to the broker. The gate sends neither and refuses a caller's, in every spelling, with the canonical route named.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { SWIFT_SCRATCH_REFUSED, swiftScratchOption, swiftScratchRefusal } from "./swift-scratch.mjs";
import { brokerSection, carryBrokerAdapter } from "./snapshot-broker.mjs";
import { makeFakeBroker, subHandleStub } from "./test-support/fake-broker.mjs";

const SPELLINGS = [["--scratch-path", "/x"], ["--scratch-path=/x"], ["-scratch-path", "/x"], ["-scratch-path=/x"], ["--build-path", "/x"], ["--build-path=/x"], ["-build-path", "/x"]];

test("[GB-67] a caller's --scratch-path, -scratch-path or --build-path (also `=`-spelled) is refused, naming the canonical route; one after `--` is the program's and not judged", () => {
  for (const extra of SPELLINGS) {
    const words = ["build", "--package-path", "pkg", ...extra];
    const refusal = swiftScratchRefusal(words);
    assert.equal(refusal?.code, SWIFT_SCRATCH_REFUSED, extra.join(" "));
    assert.match(refusal.message, /build --show-bin-path/); assert.match(refusal.message, /nothing is stripped or rewritten/);
  }
  assert.equal(swiftScratchRefusal(["build", "--package-path", "pkg", "--show-bin-path"]), null);
  assert.equal(swiftScratchRefusal(["build", "--", "--scratch-path", "x"]), null);
  assert.equal(swiftScratchOption(["build", "-c", "release"]), null);
});

test("[GB-67] the door refuses a swift step that passes a scratch or build path before anything is launched, and launches the canonical one", () => {
  const door = (args) => buildLaunch("swift", args, { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === "/managed/agent-bin/rustright", which: () => ["/managed/agent-bin/rustright"], readFile: () => "" });
  assert.deepEqual(door(["build", "--package-path", "pkg"]).args, ["run", "rightkit-swift", "--", "build", "--package-path", "pkg"]);
  assert.deepEqual(door(["build", "--package-path", "pkg", "--show-bin-path"]).args.slice(-2), ["pkg", "--show-bin-path"]);
  for (const extra of SPELLINGS) assert.throws(() => door(["build", "--package-path", "pkg", ...extra]), (e) => e.code === BUILD_LAUNCH_REFUSED && /RK_SWIFT_SCRATCH_OWNED/.test(e.message), extra.join(" "));
});

test("[GB-67] a carried rightkit-swift call that names a scratch path is refused before the carrier, the ownership mark and the send", (t) => {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const marks = [];
  const section = brokerSection({ ...fx.sb, markBrokerSubmit: subHandleStub(fx.handle, (h) => marks.push(h)) });
  const env = { PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" };
  const records = [];
  const run = (words) => carryBrokerAdapter({ adapter: "rightkit-swift", words, cwd: fx.snapshot, env, section, record: (e) => records.push(e), err: () => {}, stdio: "capture" });
  const refused = run(["build", "--package-path", "crates/a", "--scratch-path", "/tmp/mine"]);
  assert.equal(refused.outcome.status, "refused"); assert.equal(refused.outcome.code, SWIFT_SCRATCH_REFUSED);
  assert.deepEqual(marks, [], "nothing marked, so nothing sent");
  assert.equal(fx.adapterCalls().length, 0, "zero transport calls");
  assert.equal(run(["build", "--package-path", "crates/a"]).outcome.status, "verified");
  assert.equal(marks.length, 1);
});
