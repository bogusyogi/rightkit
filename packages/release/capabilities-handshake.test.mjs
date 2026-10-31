// GB-51 / GB-62 (BROKER-CONTRACT section 16): what the broker advertises in HANDSHAKE.capabilities (`rustright doctor --json` `.broker.capabilities`, the same list the client read from the
// HANDSHAKE) decides what the gate plans. A native tool is planned only against `adapter:rightkit-xcodegen|xcodebuild|gradle|cmake` and `cargo ndk` only against `external:cargo-ndk`, each an
// exact string the broker advertises for an exactly-registered adapter; anything else refuses at planning naming the missing capability. A list is read only from a handshake the client
// actually received (a broker that is not reachable advertises nothing, which is not the same as a broker that advertises little), and no capability name exists in RightKit that RustRight
// does not advertise.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { queryBrokerCapabilities } from "./bin/rightkit-gate.mjs";
import { NATIVE_ADAPTER_CAPABILITIES } from "./native-adapters.mjs";
import { NDK_EXTERNAL_CAPABILITY, nativeToolRefusal, nativeToolUses } from "./native-tool-planning.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
// Every capability RustRight advertises: rustright-protocol's BROKER_CAPABILITIES (journal-v1, snapshot-readonly-v1, stage-grant-v1, snapshot-adapter-v1) and, per registration, the
// native adapters and cargo-ndk (rustright-policy native_adapters.rs).
const ADVERTISED = Object.freeze(["journal-v1", "snapshot-readonly-v1", "stage-grant-v1", "snapshot-adapter-v1", "adapter:rightkit-xcodegen", "adapter:rightkit-xcodebuild", "adapter:rightkit-gradle", "adapter:rightkit-cmake", "external:cargo-ndk"]);
const o = (spawn) => ({ platform: "darwin", env: {}, spawn, exists: () => true });
const doctor = (doc, status = 0) => (c) => (c === "which" ? { status: 0, stdout: "/managed/agent-bin/rustright\n" } : { status, stdout: JSON.stringify(doc), stderr: "" });

test("[GB-51] the capability list is read from a handshake the client received: an unreachable broker advertises nothing (a problem naming why), never an empty list that looks like a broker with few capabilities", () => {
  const reachable = queryBrokerCapabilities(o(doctor({ healthy: true, broker: { reachable: true, capabilities: ["snapshot-readonly-v1", "adapter:rightkit-cmake"] } })));
  assert.deepEqual(reachable, { ok: true, capabilities: ["snapshot-readonly-v1", "adapter:rightkit-cmake"] });
  const down = queryBrokerCapabilities(o(doctor({ healthy: false, broker: { reachable: false, error: "RK_BROKER_UNAVAILABLE: connection refused", capabilities: [] } }, 1)));
  assert.equal(down.ok, false);
  assert.match(down.problem, /not reachable|no handshake/i); assert.match(down.problem, /RK_BROKER_UNAVAILABLE/);
  const undeclared = queryBrokerCapabilities(o(doctor({ healthy: true, broker: { capabilities: ["snapshot-readonly-v1"] } })));
  assert.equal(undeclared.ok, false, "a doctor that does not say the broker was reached proves no handshake");
  // Names are strings, each once.
  for (const caps of [["snapshot-readonly-v1", 7], ["a", "a"], [null]]) assert.equal(queryBrokerCapabilities(o(doctor({ healthy: true, broker: { reachable: true, capabilities: caps } }))).ok, false, JSON.stringify(caps));
});

test("[GB-51] native tools are planned only against the exact advertised names: a label-only look-alike, another case, a padded name or a prefix never satisfies a use; each missing capability is named", () => {
  const ios = { development: { targets: { ios: { projectSpec: "ios/project.yml", build: [] } } } };
  const uses = nativeToolUses({ config: ios, to: "dev", devPlatform: "ios" });
  for (const lookalike of ["adapter:xcodegen", "rightkit-xcodegen", "ADAPTER:RIGHTKIT-XCODEGEN", "adapter:rightkit-xcodegen ", "adapter:rightkit-xcodegen-v1", "adapter:rightkit-", "xcodegen"]) {
    const refusal = nativeToolRefusal(uses, { capabilities: { ok: true, capabilities: [lookalike, "adapter:rightkit-xcodebuild"] } });
    assert.match(refusal, /does not advertise adapter:rightkit-xcodegen/, lookalike);
  }
  assert.equal(nativeToolRefusal(uses, { capabilities: { ok: true, capabilities: ["adapter:rightkit-xcodegen", "adapter:rightkit-xcodebuild"] } }), null);
  const android = nativeToolUses({ config: { development: { targets: { android: { ndk: { jniLibs: "app/src/main/jniLibs" }, build: [] } } } }, to: "dev", devPlatform: "android" });
  assert.match(nativeToolRefusal(android, { capabilities: { ok: true, capabilities: ["adapter:rightkit-gradle", "cargo-ndk", "external:cargo-ndk-v2"] } }), /does not advertise external:cargo-ndk/);
  assert.equal(nativeToolRefusal(android, { capabilities: { ok: true, capabilities: ["external:cargo-ndk"] } }), null);
});

test("[GB-51] no capability name in RightKit's gate source is one RustRight does not advertise: every `adapter:` and `external:` name is in the advertised set, and the sets match", () => {
  assert.deepEqual([...NATIVE_ADAPTER_CAPABILITIES, NDK_EXTERNAL_CAPABILITY].sort(), ADVERTISED.filter((c) => c.startsWith("adapter:") || c.startsWith("external:")).sort());
  const named = new Set();
  for (const name of fs.readdirSync(HERE)) {
    if (!name.endsWith(".mjs") || name.endsWith(".test.mjs")) continue;
    for (const m of fs.readFileSync(path.join(HERE, name), "utf8").matchAll(/(?<![\w-])((?:adapter|external):[a-z][a-z0-9-]*[a-z0-9])/g)) named.add(m[1]);
  }
  for (const name of fs.readdirSync(path.join(HERE, "bin"))) for (const m of fs.readFileSync(path.join(HERE, "bin", name), "utf8").matchAll(/(?<![\w-])((?:adapter|external):[a-z][a-z0-9-]*[a-z0-9])/g)) named.add(m[1]);
  assert.deepEqual([...named].filter((n) => !ADVERTISED.includes(n)), [], "a capability name the broker never advertises is a label that can never be satisfied");
});
