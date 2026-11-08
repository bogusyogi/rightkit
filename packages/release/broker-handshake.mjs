// GB-51 / GB-62 (BROKER-CONTRACT section 16, "HANDSHAKE.capabilities"): the gate receives the broker's HANDSHAKE itself and reads the capability list from it.
//
// `rustright doctor --json` reports the same list (`.broker.capabilities`), but through the client; a gate that plans a native tool or `cargo ndk` from a list it was handed second-hand has
// not seen what the broker advertised. This module speaks the one request the client's own health probe sends (`STATUS runtime-health-probe`, a by-id request the front gate answers with
// the HANDSHAKE as its FIRST event whatever else it says, GB-75), reads that first event, and parses it strictly. The runtime identity the request carries is the manifest beside the
// installed client (`runtime-manifest.json`, the file the client itself reads); the endpoint is the one RustRight's discovery names (managed-roots.mjs discoverBrokerSocket: the
// explicit variable, the config's pipe on Windows, `broker.sock` of a managed control root elsewhere). A list is advertised only by a handshake that was received, and the gate plans only
// against the names it carries: capability names are compared exactly, never by prefix or case.
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { resolveBrokerClient } from "./broker-client.mjs";
import { discoverBrokerSocket } from "./managed-roots.mjs";

/** `rustright_protocol::BUILD_CONTROL_PROTOCOL`: carried in every request envelope. */
export const BUILD_CONTROL_PROTOCOL = "rustright.build-control.client.v2";
/** The id of the client's own health probe (`rustright-cli` doctor: PROBE_ID); a by-id request, so it is not held to the exact runtime digest. */
export const HANDSHAKE_PROBE_ID = "runtime-health-probe";
export const RUNTIME_MANIFEST_FILE = "runtime-manifest.json";
export const RUNTIME_MANIFEST_SCHEMA = "rustright.build-control.runtime.v2";
const LEGACY_RUNTIME_MANIFEST_SCHEMA = "rightkit.build-control.runtime.v2";
/** A client accepts at most 1 MiB per line (rustright-protocol MAX_BROKER_LINE_BYTES). */
export const MAX_HANDSHAKE_LINE_BYTES = 1 << 20;
export const HANDSHAKE_TIMEOUT_MS = 20_000;

const isDigest = (v) => typeof v === "string" && /^[0-9a-f]{64}$/.test(v);
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/** The runtime identity of a parsed object (`schema` current or legacy, a version, both digests lowercase 64-hex), as RustRight's `RuntimeIdentity::new` validates it. */
function normalizeIdentity(raw) {
  if (!isObject(raw) || ![RUNTIME_MANIFEST_SCHEMA, LEGACY_RUNTIME_MANIFEST_SCHEMA].includes(raw.schema) || typeof raw.version !== "string" || raw.version === "" || !isDigest(raw.clientDigest) || !isDigest(raw.brokerDigest)) return null;
  return { schema: RUNTIME_MANIFEST_SCHEMA, version: raw.version, clientDigest: raw.clientDigest, brokerDigest: raw.brokerDigest };
}

/** The runtime identity a `runtime-manifest.json` text holds: `{ ok: true, identity }` or `{ ok: false, problem }`. */
export function parseRuntimeIdentity(text) {
  let doc;
  try { doc = JSON.parse(text); } catch { return { ok: false, problem: "the runtime manifest is not JSON" }; }
  const identity = normalizeIdentity(doc);
  return identity ? { ok: true, identity } : { ok: false, problem: "the runtime manifest is not a runtime identity (schema, version, clientDigest and brokerDigest are required)" };
}

/**
 * The broker's HANDSHAKE event (a line of JSON or the parsed object) as `{ ok: true, capabilities, runtime }` or `{ ok: false, problem }`. `capabilities` is additive on the wire: absent means the broker
 * advertises none (a broker that predates it), which is an answer; a present value must be an array of non-empty strings, each once, or the handshake is refused (a name that is not a string is no
 * name, and a list that repeats itself is not read).
 */
export function parseHandshake(input) {
  let event = input;
  if (typeof input === "string") { try { event = JSON.parse(input); } catch { return { ok: false, problem: "the broker's first reply line is not JSON" }; } }
  if (!isObject(event) || typeof event.type !== "string") return { ok: false, problem: "the broker's first reply is not a HANDSHAKE event (it has no type)" };
  if (event.type !== "HANDSHAKE") return { ok: false, problem: `the broker's first event is ${event.type}, not the HANDSHAKE (every reply begins with it, GB-75): its capabilities were not received` };
  const runtime = normalizeIdentity(event.runtime);
  if (!runtime) return { ok: false, problem: "the broker's HANDSHAKE carries no valid runtime identity" };
  let capabilities = [];
  if (Object.hasOwn(event, "capabilities")) {
    const list = event.capabilities;
    if (!Array.isArray(list)) return { ok: false, problem: "the broker's HANDSHAKE capabilities must be an array of strings" };
    for (const [i, name] of list.entries()) {
      if (typeof name !== "string") return { ok: false, problem: `the broker's HANDSHAKE capabilities[${i}] is not a string` };
      if (name === "") return { ok: false, problem: `the broker's HANDSHAKE capabilities[${i}] is empty` };
    }
    if (new Set(list).size !== list.length) return { ok: false, problem: "the broker's HANDSHAKE lists a capability twice: a list that repeats itself is not read" };
    capabilities = [...list];
  }
  return { ok: true, capabilities, runtime };
}

/**
 * Receives the broker's HANDSHAKE: connects to `endpoint` (a Unix socket path or a Windows pipe name, through `connect`, `net.connect` by default), sends the client's own health probe with `identity`
 * and parses the FIRST event of the reply. Resolves, never rejects: `{ ok: true, capabilities, runtime }` or `{ ok: false, problem }` (no endpoint, refused, closed, silent for `timeoutMs`, an
 * oversized line, anything but the HANDSHAKE first, a malformed list).
 */
export function readBrokerHandshake({ endpoint, identity, connect = (target) => net.connect({ path: target }), timeoutMs = HANDSHAKE_TIMEOUT_MS, maxLineBytes = MAX_HANDSHAKE_LINE_BYTES }) {
  return new Promise((resolve) => {
    let settled = false; let connected = false; let buffer = Buffer.alloc(0); let conn = null; let timer = null;
    const finish = (result) => {
      if (settled) return;
      settled = true; clearTimeout(timer);
      try { conn?.destroy(); } catch { /* already gone */ }
      resolve(result);
    };
    const problem = (text) => finish({ ok: false, problem: text });
    try { conn = connect(endpoint); } catch (error) { problem(`cannot connect to the broker at ${endpoint} (${error?.code ?? error?.message ?? error})`); return; }
    timer = setTimeout(() => problem(`the broker at ${endpoint} sent no HANDSHAKE within ${timeoutMs} ms`), timeoutMs);
    conn.on("connect", () => {
      connected = true;
      try { conn.write(`${JSON.stringify({ type: "STATUS", protocol: BUILD_CONTROL_PROTOCOL, runtime: identity, id: HANDSHAKE_PROBE_ID })}\n`); } catch (error) { problem(`cannot send the probe to the broker at ${endpoint} (${error?.message ?? error})`); }
    });
    conn.on("data", (chunk) => {
      if (settled) return;
      buffer = Buffer.concat([buffer, Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk))]);
      const at = buffer.indexOf(0x0a);
      if (at < 0) { if (buffer.length > maxLineBytes) problem(`the broker's first line exceeds ${maxLineBytes} bytes`); return; }
      if (at + 1 > maxLineBytes) { problem(`the broker's first line exceeds ${maxLineBytes} bytes`); return; }
      finish(parseHandshake(buffer.subarray(0, at).toString("utf8")));
    });
    conn.on("error", (error) => problem(connected ? `the broker connection at ${endpoint} failed before its HANDSHAKE (${error?.code ?? error?.message ?? error})` : `cannot connect to the broker at ${endpoint} (${error?.code ?? error?.message ?? error})`));
    conn.on("close", () => problem(`the broker at ${endpoint} closed before its HANDSHAKE`));
    conn.on("end", () => problem(`the broker at ${endpoint} closed before its HANDSHAKE`));
  });
}

const isPipe = (endpoint) => /^\\\\\.\\pipe\\/i.test(String(endpoint));

/**
 * The broker's HANDSHAKE as the gate receives it: the installed `client` (an absolute path; a Windows .cmd shim resolves to its .exe), the runtime identity of its manifest, the endpoint RustRight's
 * discovery names, and the handshake read from it. A Unix endpoint must be a socket the current user owns (a planted file or another user's socket is not the broker). `{ ok, capabilities, runtime }`
 * or `{ ok: false, problem }`.
 */
export async function queryBrokerHandshake(o, { client = o?.client } = {}) {
  const platform = o?.platform ?? process.platform;
  const env = o?.env ?? process.env;
  const read = o?.readFile ?? ((file) => fs.readFileSync(file, "utf8"));
  if (typeof client !== "string" || !client) return { ok: false, problem: "the RustRight client is not on PATH, so its runtime identity cannot be read to receive the broker's HANDSHAKE" };
  let exe;
  try { exe = resolveBrokerClient(client, { platform, readFile: read }).file; } catch (error) { return { ok: false, problem: `the RustRight client cannot be resolved (${error.message})` }; }
  let real = exe;
  if (platform !== "win32") { try { real = (o?.realpath ?? fs.realpathSync.native)(exe); } catch { real = exe; } }
  const paths = platform === "win32" ? path.win32 : path.posix;
  const manifest = paths.join(paths.dirname(real), RUNTIME_MANIFEST_FILE);
  let text;
  try { text = read(manifest); } catch (error) { return { ok: false, problem: `cannot read the runtime manifest ${manifest} beside the client (${error?.message ?? error}), so the HANDSHAKE probe has no identity to carry` }; }
  const identity = parseRuntimeIdentity(text);
  if (!identity.ok) return { ok: false, problem: `${manifest}: ${identity.problem}` };
  let endpoint;
  try { endpoint = (o?.discoverEndpoint ?? discoverBrokerSocket)(env, { platform, exists: fs.existsSync }); } catch (error) { return { ok: false, problem: `the broker endpoint cannot be discovered (${error.message})` }; }
  if (!endpoint) return { ok: false, problem: "no broker endpoint is configured (no explicit socket variable and no broker.sock under a managed control root)" };
  if (platform !== "win32" && !isPipe(endpoint)) {
    try {
      const stat = (o?.lstat ?? fs.lstatSync)(endpoint);
      const uid = typeof (o?.uid ?? process.getuid?.()) === "number" ? (o?.uid ?? process.getuid()) : null;
      if (!stat.isSocket()) return { ok: false, problem: `${endpoint} is not a socket, so it is not the broker's endpoint` };
      if (uid !== null && stat.uid !== uid) return { ok: false, problem: `${endpoint} is owned by another user (uid ${stat.uid}), so it is not this user's broker` };
    } catch (error) { return { ok: false, problem: `cannot connect to the broker at ${endpoint} (${error?.code ?? error?.message ?? error})` }; }
  }
  return readBrokerHandshake({ endpoint, identity: identity.identity, ...(o?.connectBroker ? { connect: o.connectBroker } : {}), ...(o?.handshakeTimeoutMs ? { timeoutMs: o.handshakeTimeoutMs } : {}) });
}

/**
 * The two views of the broker's advertised capabilities: `doctor` (`rustright doctor --json` through the client, `{ ok, capabilities } | { ok: false, problem }`) and `handshake` (received by the gate,
 * `{ ok, capabilities, runtime } | { ok: false, problem } | null` when none was asked). Both answering must agree exactly (a disagreement refuses, naming each side's extra names: a broker that
 * restarted between the reads, or a doctor that does not report what the broker said, plans nothing); one answering plans from it with the other's problem kept beside it; neither refuses
 * with both reasons. `null` handshake leaves the doctor's state untouched.
 */
export function mergeCapabilityViews({ doctor, handshake }) {
  if (handshake === null || handshake === undefined) return doctor;
  const d = doctor?.ok === true, h = handshake.ok === true;
  if (d && h) {
    const fromDoctor = new Set(doctor.capabilities), fromHandshake = new Set(handshake.capabilities);
    const onlyDoctor = [...fromDoctor].filter((c) => !fromHandshake.has(c)), onlyHandshake = [...fromHandshake].filter((c) => !fromDoctor.has(c));
    if (onlyDoctor.length || onlyHandshake.length) {
      return { ok: false, problem: `\`rustright doctor --json\` and the broker's HANDSHAKE disagree about what it advertises (only the doctor lists: ${onlyDoctor.join(", ") || "none"}; only the HANDSHAKE lists: ${onlyHandshake.join(", ") || "none"}); nothing is planned from either` };
    }
    return { ok: true, capabilities: [...handshake.capabilities], via: "handshake+doctor" };
  }
  if (h) return { ok: true, capabilities: [...handshake.capabilities], via: "handshake", doctorProblem: doctor?.problem ?? "the doctor did not answer" };
  if (d) return { ok: true, capabilities: [...doctor.capabilities], via: "doctor", handshakeProblem: handshake.problem };
  return { ok: false, problem: `${doctor?.problem ?? "the doctor did not answer"}; and the HANDSHAKE could not be received: ${handshake.problem}` };
}
