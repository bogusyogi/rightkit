// GB-51 / GB-62 (BROKER-CONTRACT section 16, "HANDSHAKE.capabilities"): the gate reads the capability list from the HANDSHAKE the broker sends (the first event of every reply, GB-75) itself,
// as well as through `rustright doctor --json`, and plans only against names that are advertised. The broker here is a fake: a real local socket (a Unix socket; the Windows pipe is
// reached through the same injected connect) that answers the client's `STATUS runtime-health-probe` the way the real front gate does, HANDSHAKE first.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import test from "node:test";
import { tempRoot } from "./test-support/temp-roots.mjs";
import { BUILD_CONTROL_PROTOCOL, HANDSHAKE_PROBE_ID, mergeCapabilityViews, parseHandshake, parseRuntimeIdentity, queryBrokerHandshake, readBrokerHandshake } from "./broker-handshake.mjs";
import { brokerCapabilityStateWithHandshake } from "./bin/rightkit-gate.mjs";
import { nativeToolRefusal, nativeToolUses } from "./native-tool-planning.mjs";

const IDENTITY = { schema: "rustright.build-control.runtime.v2", version: "1.2.3", clientDigest: "a".repeat(64), brokerDigest: "b".repeat(64) };
const ALL = ["journal-v1", "snapshot-readonly-v1", "stage-grant-v1", "snapshot-adapter-v1", "adapter:rightkit-xcodegen", "adapter:rightkit-xcodebuild", "external:cargo-ndk"];
const handshake = (capabilities, extra = {}) => JSON.stringify({ type: "HANDSHAKE", runtime: IDENTITY, ...(capabilities === undefined ? {} : { capabilities }), ...extra });
const status = JSON.stringify({ type: "STATUS", id: HANDSHAKE_PROBE_ID, state: "READY" });

/** A fake broker on a real Unix socket: records each request line and answers with `reply(request)` (an array of lines, or a function that writes). */
async function fakeBroker(reply) {
  const dir = tempRoot("hs-broker-");
  const socket = path.join(dir, "hs-fake.sock");
  const requests = [];
  const server = net.createServer((conn) => {
    let buffer = "";
    conn.on("data", (chunk) => {
      buffer += chunk;
      const at = buffer.indexOf("\n");
      if (at < 0) return;
      const request = JSON.parse(buffer.slice(0, at)); requests.push(request); buffer = "";
      const lines = reply(request, conn);
      if (Array.isArray(lines)) conn.write(`${lines.join("\n")}\n`);
    });
    conn.on("error", () => {});
  });
  await new Promise((resolve) => server.listen(socket, resolve));
  return { socket, dir, requests, close: () => new Promise((resolve) => { server.close(resolve); server.closeAllConnections?.(); }) };
}
const win = process.platform === "win32" ? "a Unix socket fixture" : false;

test("[GB-51] parseHandshake reads the contract's HANDSHAKE.capabilities exactly: absent means none, a list is strings each once, anything else is a problem naming why", () => {
  assert.deepEqual(parseHandshake(handshake(ALL)), { ok: true, capabilities: ALL, runtime: IDENTITY });
  assert.deepEqual(parseHandshake(JSON.parse(handshake(["snapshot-readonly-v1"]))).capabilities, ["snapshot-readonly-v1"], "an event object parses as its line does");
  assert.deepEqual(parseHandshake(handshake(undefined)), { ok: true, capabilities: [], runtime: IDENTITY }, "additive: a broker that predates capabilities advertises none (not a malformed handshake)");
  assert.deepEqual(parseHandshake(handshake([])).capabilities, []);
  for (const [label, text, why] of [
    ["not json", "{nope", /not JSON/], ["not an object", "[1]", /not a HANDSHAKE/], ["another event first", JSON.stringify({ type: "STATUS", id: "x" }), /first event .*STATUS.*HANDSHAKE/],
    ["untyped", JSON.stringify({ capabilities: [] }), /not a HANDSHAKE/],
    ["capabilities not an array", handshake("journal-v1"), /capabilities must be an array/], ["a non-string name", handshake(["a", 7]), /capabilities\[1\] is not a string/],
    ["an empty name", handshake([""]), /empty/], ["a name twice", handshake(["a", "a"]), /twice/], ["null list", handshake(null), /capabilities must be an array/],
    ["no runtime", JSON.stringify({ type: "HANDSHAKE", capabilities: [] }), /runtime identity/], ["a bad runtime digest", JSON.stringify({ type: "HANDSHAKE", runtime: { ...IDENTITY, brokerDigest: "xyz" }, capabilities: [] }), /runtime identity/],
  ]) {
    const r = parseHandshake(text);
    assert.equal(r.ok, false, label); assert.match(r.problem, why, label);
  }
  // Only the exact name satisfies a plan: a padded, re-cased or prefixed name is another name (nativeToolRefusal compares exact strings).
  const uses = nativeToolUses({ config: { development: { targets: { ios: { projectSpec: "ios/project.yml", build: [] } } } }, to: "dev", devPlatform: "ios" });
  const padded = parseHandshake(handshake(["adapter:rightkit-xcodegen ", "ADAPTER:RIGHTKIT-XCODEBUILD"]));
  assert.match(nativeToolRefusal(uses, { capabilities: padded }), /does not advertise adapter:rightkit-xcodegen/);
});

test("[GB-51] parseRuntimeIdentity accepts the manifest the client reads (current and legacy schema, lowercase 64-hex digests) and refuses the rest", () => {
  assert.deepEqual(parseRuntimeIdentity(JSON.stringify(IDENTITY)), { ok: true, identity: IDENTITY });
  assert.equal(parseRuntimeIdentity(JSON.stringify({ ...IDENTITY, schema: "rightkit.build-control.runtime.v2" })).identity.schema, "rustright.build-control.runtime.v2", "the legacy spelling is stored as the current one");
  for (const bad of [{ ...IDENTITY, schema: "other" }, { ...IDENTITY, version: "" }, { ...IDENTITY, clientDigest: "A".repeat(64) }, { ...IDENTITY, brokerDigest: "a".repeat(63) }, null, []]) assert.equal(parseRuntimeIdentity(JSON.stringify(bad)).ok, false, JSON.stringify(bad));
  assert.equal(parseRuntimeIdentity("{x").ok, false);
});

test("[GB-51] readBrokerHandshake receives the HANDSHAKE from a (fake) broker over its endpoint: the probe is the client's own `STATUS runtime-health-probe`, the capabilities are the broker's advertised list", { skip: win }, async () => {
  const broker = await fakeBroker(() => [handshake(ALL), status]);
  try {
    const r = await readBrokerHandshake({ endpoint: broker.socket, identity: IDENTITY });
    assert.deepEqual(r, { ok: true, capabilities: ALL, runtime: IDENTITY });
    assert.deepEqual(broker.requests, [{ type: "STATUS", protocol: BUILD_CONTROL_PROTOCOL, runtime: IDENTITY, id: HANDSHAKE_PROBE_ID }]);
    // A broker that predates capabilities advertises none: an answer, not a failure.
    const old = await fakeBroker(() => [handshake(undefined), status]);
    try { assert.deepEqual((await readBrokerHandshake({ endpoint: old.socket, identity: IDENTITY })).capabilities, []); } finally { await old.close(); }
  } finally { await broker.close(); }
});

test("[GB-51] readBrokerHandshake fails closed with a named problem: no endpoint, a refusing socket, a first event that is not the HANDSHAKE, malformed capabilities, an oversized line, a silent broker", { skip: win }, async () => {
  const dead = await readBrokerHandshake({ endpoint: path.join(tempRoot("hs-none-"), "none.sock"), identity: IDENTITY });
  assert.equal(dead.ok, false); assert.match(dead.problem, /cannot connect to the broker/);
  const cases = [
    ["RESULT before HANDSHAKE", () => [JSON.stringify({ type: "RESULT", status: "REJECTED", error: "digest mismatch" })], /first event .*RESULT.*HANDSHAKE/],
    ["malformed capabilities", () => [handshake(["a", "a"]), status], /twice/],
    ["not JSON", () => ["garbage"], /not JSON/],
    ["oversized line", () => ["x".repeat((1 << 20) + 10)], /exceeds/],
    ["closed with no answer", (_r, conn) => { conn.end(); return null; }, /closed before/],
  ];
  for (const [label, reply, why] of cases) {
    const broker = await fakeBroker(reply);
    try { const r = await readBrokerHandshake({ endpoint: broker.socket, identity: IDENTITY }); assert.equal(r.ok, false, label); assert.match(r.problem, why, label); } finally { await broker.close(); }
  }
  const silent = await fakeBroker(() => null);
  try { const r = await readBrokerHandshake({ endpoint: silent.socket, identity: IDENTITY, timeoutMs: 150 }); assert.equal(r.ok, false); assert.match(r.problem, /no HANDSHAKE within 150 ms/); } finally { await silent.close(); }
});

test("[GB-51] a Windows pipe is read through the same connect: the endpoint is the pipe name, the probe is the same line", async () => {
  const written = [];
  const connect = (endpoint) => {
    const listeners = {};
    const conn = {
      endpoint, setTimeout() {}, destroy() {}, on(name, fn) { listeners[name] = fn; return conn; }, once(name, fn) { listeners[name] = fn; return conn; },
      write(data) { written.push([endpoint, String(data)]); queueMicrotask(() => listeners.data?.(Buffer.from(`${handshake(["snapshot-readonly-v1"])}\n${status}\n`))); return true; }, end() {},
    };
    queueMicrotask(() => listeners.connect?.());
    return conn;
  };
  const pipe = "\\\\.\\pipe\\rustright-broker";
  const r = await readBrokerHandshake({ endpoint: pipe, identity: IDENTITY, connect });
  assert.deepEqual(r.capabilities, ["snapshot-readonly-v1"]);
  assert.equal(written[0][0], pipe);
  assert.deepEqual(JSON.parse(written[0][1]), { type: "STATUS", protocol: BUILD_CONTROL_PROTOCOL, runtime: IDENTITY, id: HANDSHAKE_PROBE_ID });
});

test("[GB-51] the doctor's list and the received HANDSHAKE are two views of one broker: they must agree, and a plan uses names advertised by what answered", () => {
  const hs = (caps) => ({ ok: true, capabilities: caps, runtime: IDENTITY });
  const dr = (caps) => ({ ok: true, capabilities: caps });
  const same = mergeCapabilityViews({ doctor: dr(ALL), handshake: hs([...ALL].reverse()) });
  assert.equal(same.ok, true); assert.deepEqual([...same.capabilities].sort(), [...ALL].sort()); assert.equal(same.via, "handshake+doctor");
  // They disagree: refuse, naming each side's extra names (a restart between the two reads, or a doctor that does not report what the broker said).
  const apart = mergeCapabilityViews({ doctor: dr([...ALL, "adapter:rightkit-gradle"]), handshake: hs(ALL) });
  assert.equal(apart.ok, false); assert.match(apart.problem, /disagree/); assert.match(apart.problem, /adapter:rightkit-gradle/);
  const other = mergeCapabilityViews({ doctor: dr(["journal-v1"]), handshake: hs(["journal-v1", "external:cargo-ndk"]) });
  assert.equal(other.ok, false); assert.match(other.problem, /external:cargo-ndk/);
  // One view answers: that one plans, and the other's problem is kept beside it.
  const only = mergeCapabilityViews({ doctor: { ok: false, problem: "doctor did not finish" }, handshake: hs(ALL) });
  assert.equal(only.ok, true); assert.equal(only.via, "handshake"); assert.match(only.doctorProblem, /did not finish/);
  const doctorOnly = mergeCapabilityViews({ doctor: dr(ALL), handshake: { ok: false, problem: "no endpoint" } });
  assert.equal(doctorOnly.ok, true); assert.equal(doctorOnly.via, "doctor"); assert.match(doctorOnly.handshakeProblem, /no endpoint/);
  // Neither: refused, with both reasons.
  const none = mergeCapabilityViews({ doctor: { ok: false, problem: "doctor down" }, handshake: { ok: false, problem: "hs down" } });
  assert.equal(none.ok, false); assert.match(none.problem, /doctor down/); assert.match(none.problem, /hs down/);
  // No handshake asked (an injected fixture broker): the doctor's state stands unchanged.
  const d = dr(ALL); assert.equal(mergeCapabilityViews({ doctor: d, handshake: null }), d);
});

test("[GB-51] the gate asks for the capabilities both ways and plans only against the advertised names: a name the fake broker's HANDSHAKE does not carry refuses at planning even if the doctor lists it", { skip: win }, async () => {
  const dataRoot = tempRoot("hs-gate-");
  const clientDir = path.join(dataRoot, "agent-bin"); fs.mkdirSync(clientDir, { recursive: true });
  fs.writeFileSync(path.join(clientDir, "rustright"), "#!/bin/sh\n", { mode: 0o755 });
  fs.writeFileSync(path.join(clientDir, "runtime-manifest.json"), JSON.stringify(IDENTITY));
  const advertised = ["snapshot-readonly-v1", "snapshot-adapter-v1", "adapter:rightkit-xcodebuild"]; // xcodegen is NOT advertised
  const broker = await fakeBroker(() => [handshake(advertised), status]);
  try {
    const doctor = (caps) => (c, a) => (c === "which" || c === "where" ? { status: 0, stdout: `${path.join(clientDir, "rustright")}\n` } : { status: 0, stdout: JSON.stringify({ healthy: true, broker: { reachable: true, capabilities: caps } }), stderr: "" });
    const o = (caps) => ({ platform: "linux", env: { RUSTRIGHT_BUILD_BROKER_SOCKET: broker.socket }, spawn: doctor(caps), snapshotBroker: { create() {} }, readFile: (f) => fs.readFileSync(f, "utf8") });
    const executor = { kind: "broker" };
    // The doctor and the received HANDSHAKE agree: the state is the advertised list and says it came from both.
    const state = await brokerCapabilityStateWithHandshake(o(advertised), { to: "dev", executor });
    assert.equal(state.ok, true); assert.deepEqual([...state.capabilities].sort(), [...advertised].sort()); assert.equal(state.via, "handshake+doctor");
    const uses = nativeToolUses({ config: { development: { targets: { ios: { projectSpec: "ios/project.yml", build: [] } } } }, to: "dev", devPlatform: "ios" });
    assert.match(nativeToolRefusal(uses, { capabilities: state }), /does not advertise adapter:rightkit-xcodegen/, "the plan names the missing capability");
    // A doctor that claims one more name than the HANDSHAKE carried is a disagreement: nothing is planned from it.
    const lying = await brokerCapabilityStateWithHandshake(o([...advertised, "adapter:rightkit-xcodegen"]), { to: "dev", executor });
    assert.equal(lying.ok, false); assert.match(lying.problem, /disagree.*adapter:rightkit-xcodegen/s);
    assert.match(nativeToolRefusal(uses, { capabilities: lying }), /does not advertise adapter:rightkit-xcodegen/);
    // The probe the gate sent is the client's own: protocol, the manifest's identity, the health-probe id.
    assert.deepEqual(broker.requests[0], { type: "STATUS", protocol: BUILD_CONTROL_PROTOCOL, runtime: IDENTITY, id: HANDSHAKE_PROBE_ID });
    // Not asked: a non-broker executor, hosted CI, --to check, or an injected fixture broker with its own declared capabilities.
    assert.equal(await brokerCapabilityStateWithHandshake(o(advertised), { to: "dev", executor: { kind: "direct" } }), null);
    assert.equal(await brokerCapabilityStateWithHandshake(o(advertised), { to: "check", executor }), null);
    const declared = { ok: true, capabilities: ["journal-v1"] };
    assert.deepEqual(await brokerCapabilityStateWithHandshake({ ...o(advertised), brokerCapabilities: () => declared }, { to: "dev", executor }), declared);
    // An injected handshake reader stands in for the socket (a test's fake broker handshake without a socket).
    const injected = await brokerCapabilityStateWithHandshake({ ...o(advertised), brokerHandshake: async () => ({ ok: true, capabilities: advertised, runtime: IDENTITY }) }, { to: "dev", executor });
    assert.equal(injected.via, "handshake+doctor");
    // No client, no manifest, no endpoint: the handshake view says why and the doctor's answer stands.
    const noManifest = await queryBrokerHandshake({ ...o(advertised), env: {} , readFile: () => { throw new Error("ENOENT"); } });
    assert.equal(noManifest.ok, false);
  } finally { await broker.close(); }
});
