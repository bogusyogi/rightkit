// The gate's own record of the broker it trusts on this host (Sage decision A, item 7 row 7.21; AR 256-258: pins come from the
// consumer's own enrollment, "never from the receipt, request or project").
//
//   record   `<managed data root>/pins/broker-<platform>.json`, `{schema, platform, host, keyId, publicKey, brokerDigest, enrolledAt}`,
//            outside every repository and snapshot, written only by `enrollBrokerPins` (an explicit operation; a second enrollment
//            for the same platform is a rotation and says so).
//   fallback RightKit's own checkout only: `ci/rk-pipeline.json` `attestation.<platform>`, read from the revision of the RUNNING gate
//            (`git show <gate repository HEAD>:ci/rk-pipeline.json`), never from the selected commit, the snapshot or the working tree.
//
// Never read here: the broker's control root, its runtime manifest, `attestation/key.json` (it holds the SECRET key), an environment
// variable, a flag, the snapshot or a receipt. A missing record in any repository but RightKit refuses the snapshot compile; nothing
// is enrolled automatically.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";

export const PINS_SCHEMA = "rightkit.broker-pins/1";
export const PINS_DIR = "pins";
export const PIPELINE_FILE = "ci/rk-pipeline.json";
export const PIPELINE_SCHEMA = "rk-pipeline.v1";

export class BrokerPinsError extends Error {
  constructor(code, message) { super(message); this.name = "BrokerPinsError"; this.code = code; }
}
const fail = (code, message) => { throw new BrokerPinsError(code, message); };
const sha256 = (data) => crypto.createHash("sha256").update(data).digest("hex");
const HEX = /^(?:[0-9a-f]{2})+$/;

/** The pin record of one platform under the managed data root. */
export const pinsFile = ({ dataRoot, platform = process.platform }) => {
  if (!dataRoot || !path.isAbsolute(dataRoot)) fail("RK_BROKER_PINS_ROOT", "the managed data root must be an absolute path");
  if (!["darwin", "win32"].includes(platform)) fail("RK_BROKER_PINS_PLATFORM", `${platform} is not a broker host (darwin and win32 only)`);
  return path.join(dataRoot, PINS_DIR, `broker-${platform}.json`);
};

/** `{host, keyId, publicKey, brokerDigest}` or a throw naming what is wrong. The key must be an uncompressed P-256 point whose sha256 is the key id. */
export function validatePins(pins, label = "the broker pins") {
  const bad = (why) => fail("RK_BROKER_PINS_INVALID", `${label}: ${why}`);
  if (pins === null || typeof pins !== "object" || Array.isArray(pins)) bad("not an object");
  const { host, keyId, publicKey, brokerDigest } = pins;
  if (typeof host !== "string" || !host.trim()) bad("host is missing");
  if (typeof publicKey !== "string" || !HEX.test(publicKey) || publicKey.length !== 130 || !publicKey.startsWith("04")) bad("publicKey is not an uncompressed SEC1 P-256 point in lowercase hex");
  if (typeof keyId !== "string" || keyId !== sha256(Buffer.from(publicKey, "hex"))) bad("keyId is not the sha256 of the public key");
  if (typeof brokerDigest !== "string" || !/^[0-9a-f]{64}$/.test(brokerDigest)) bad("brokerDigest is not a sha256 in lowercase hex");
  // Optional provenance of the enrolled build (M39): kept when present so an upgrade can be named, never required.
  const { brokerVersion, brokerSourceRevision } = pins;
  if (brokerVersion !== undefined && (typeof brokerVersion !== "string" || !brokerVersion.trim())) bad("brokerVersion is not a non-empty string");
  if (brokerSourceRevision !== undefined && (typeof brokerSourceRevision !== "string" || !/^[0-9a-f]{40}$/.test(brokerSourceRevision))) bad("brokerSourceRevision is not a 40-hex commit");
  return { host, keyId, publicKey, brokerDigest, ...(brokerVersion !== undefined ? { brokerVersion } : {}), ...(brokerSourceRevision !== undefined ? { brokerSourceRevision } : {}) };
}

/**
 * Compares the broker version a receipt carries with the enrolled pin's. The digest stays the trust anchor (provenance-verify.mjs refuses a
 * receipt signed by another runtime); this only NAMES an upgrade before that refusal. A pin without a brokerVersion compares nothing.
 * `{ ok: true }` or `{ ok: false, warning }`; never throws, never trusts a version over the digest.
 */
export function compareBrokerVersion(pins, brokerVersion) {
  if (typeof pins?.brokerVersion !== "string" || !pins.brokerVersion) return { ok: true, compared: false };
  if (brokerVersion === pins.brokerVersion) return { ok: true, compared: true };
  return { ok: false, compared: true, warning: `the broker reports version ${JSON.stringify(brokerVersion ?? null)} but this host is enrolled at ${JSON.stringify(pins.brokerVersion)}${pins.brokerSourceRevision ? ` (${pins.brokerSourceRevision.slice(0, 12)})` : ""}: the broker was upgraded or replaced; its digest will not match the pin, so re-enroll the host (broker-pins.mjs enroll --rotate) and update ci/rk-pipeline.json before trusting its receipts` };
}

const isLink = (meta) => meta.isSymbolicLink();

/** The enrolled record, or null when none exists. A link, a non-file, another platform's record or a malformed one is a refusal, never null. */
export function readEnrolledPins({ dataRoot, platform = process.platform }) {
  const file = pinsFile({ dataRoot, platform });
  let meta;
  try { meta = fs.lstatSync(file); } catch (e) { if (e.code === "ENOENT") return null; throw e; }
  if (isLink(meta) || !meta.isFile()) fail("RK_BROKER_PINS_INVALID", `${file} is not a regular file`);
  const dir = fs.lstatSync(path.dirname(file));
  if (isLink(dir) || !dir.isDirectory()) fail("RK_BROKER_PINS_INVALID", `${path.dirname(file)} is not a real directory`);
  if (process.platform !== "win32" && (meta.mode & 0o022 || dir.mode & 0o022)) fail("RK_BROKER_PINS_INVALID", `${file} or its directory is writable by group or others`);
  let record;
  try { record = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { fail("RK_BROKER_PINS_INVALID", `${file} is not JSON (${e.message})`); }
  if (record?.schema !== PINS_SCHEMA) fail("RK_BROKER_PINS_INVALID", `${file} is not a ${PINS_SCHEMA} record`);
  if (record.platform !== platform) fail("RK_BROKER_PINS_INVALID", `${file} is the record of ${record.platform}, not ${platform}`);
  return { ...validatePins(record, file), source: "enrolled", file };
}

const git = (spawn, cwd, args) => {
  const r = spawn("git", args, { cwd, encoding: "utf8" });
  if (r.error || r.status !== 0) return null;
  return String(r.stdout ?? "").trim();
};

/**
 * RightKit's own pins, from the revision of the running gate: the repository that holds `gateFile` and its HEAD commit, only when
 * that repository IS the checkout being compiled (RightKit itself). Null when the gate is not run from a RightKit checkout.
 */
export function readGateRevisionPins({ gateFile, checkout, platform = process.platform, spawn = spawnSync, realpath = (p) => fs.realpathSync.native(p) }) {
  const here = path.dirname(gateFile);
  const top = git(spawn, here, ["rev-parse", "--show-toplevel"]);
  if (!top) return null;
  let same = false;
  try { same = realpath(top) === realpath(checkout); } catch { same = false; }
  if (!same) return null;
  const revision = git(spawn, here, ["rev-parse", "--verify", "HEAD^{commit}"]);
  if (!revision || !/^[0-9a-f]{40,64}$/.test(revision)) fail("RK_BROKER_PINS_INVALID", "the running gate's revision cannot be read");
  const blob = spawn("git", ["show", `${revision}:${PIPELINE_FILE}`], { cwd: here, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
  if (blob.error || blob.status !== 0) return null;
  let pipeline;
  try { pipeline = JSON.parse(String(blob.stdout)); } catch (e) { fail("RK_BROKER_PINS_INVALID", `${PIPELINE_FILE} at the gate's revision ${revision.slice(0, 12)} is not JSON (${e.message})`); }
  if (pipeline?.schema !== PIPELINE_SCHEMA) fail("RK_BROKER_PINS_INVALID", `${PIPELINE_FILE} at the gate's revision is not ${PIPELINE_SCHEMA}`);
  const entry = pipeline.attestation?.[platform];
  if (!entry) return null;
  return { ...validatePins(entry, `${PIPELINE_FILE} (${platform}) at the gate's revision ${revision.slice(0, 12)}`), source: "rk-pipeline", revision };
}

/**
 * The pins this gate verifies broker receipts against: the enrolled record, else (RightKit only) the running gate's own revision.
 * Throws RK_BROKER_PINS_MISSING when neither exists: an app repository without an enrollment compiles nothing through a broker.
 */
export function loadBrokerPins({ dataRoot, platform = process.platform, gateFile, checkout, spawn, realpath }) {
  const enrolled = readEnrolledPins({ dataRoot, platform });
  if (enrolled) return enrolled;
  const fallback = gateFile && checkout ? readGateRevisionPins({ gateFile, checkout, platform, spawn, realpath }) : null;
  if (fallback) return fallback;
  return fail("RK_BROKER_PINS_MISSING", `no broker enrollment for ${platform} (${pinsFile({ dataRoot, platform })}), and this is not the RightKit checkout whose gate revision carries ${PIPELINE_FILE} pins; enroll the host's broker explicitly, the gate never enrolls one itself`);
}

/**
 * The explicit enroll operation. Values come from the caller (the output of `rustright-broker --generate-attestation-key` and the
 * installed runtime's `brokerDigest`); nothing is read from the broker, its key file or the environment. An existing record is
 * replaced only with `rotate: true`.
 */
export function enrollBrokerPins({ dataRoot, platform = process.platform, host, keyId, publicKey, brokerDigest, brokerVersion, brokerSourceRevision, rotate = false, now = () => new Date() }) {
  const pins = validatePins({ host, keyId, publicKey, brokerDigest, ...(brokerVersion !== undefined ? { brokerVersion } : {}), ...(brokerSourceRevision !== undefined ? { brokerSourceRevision } : {}) }, "the enrollment");
  const file = pinsFile({ dataRoot, platform });
  const dir = path.dirname(file);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const meta = fs.lstatSync(dir);
  if (isLink(meta) || !meta.isDirectory()) fail("RK_BROKER_PINS_INVALID", `${dir} is not a real directory`);
  let existing = null;
  try { existing = readEnrolledPins({ dataRoot, platform }); } catch (e) { if (!rotate) throw e; }
  if (existing && !rotate) fail("RK_BROKER_PINS_EXISTS", `${file} already enrolls key ${existing.keyId.slice(0, 12)}; replacing it is a rotation (pass rotate: true)`);
  const record = { schema: PINS_SCHEMA, platform, ...pins, enrolledAt: now().toISOString(), ...(existing ? { rotatedFrom: { keyId: existing.keyId, brokerDigest: existing.brokerDigest } } : {}) };
  const tmp = `${file}.${crypto.randomUUID()}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify(record, null, 2)}\n`, { flag: "wx", mode: 0o600 });
  try { fs.renameSync(tmp, file); } finally { fs.rmSync(tmp, { force: true }); }
  return { file, record, rotated: Boolean(existing) };
}

/** `node broker-pins.mjs enroll --platform <darwin|win32> --data-root <dir> --host H --key-id K --public-key P --broker-digest D [--rotate]` or `show`. */
export function main(argv, { out = (m) => process.stdout.write(`${m}\n`), err = (m) => process.stderr.write(`${m}\n`) } = {}) {
  const [command, ...rest] = argv;
  const opt = (name) => { const i = rest.indexOf(name); return i < 0 ? undefined : rest[i + 1]; };
  try {
    const dataRoot = opt("--data-root"); const platform = opt("--platform") ?? process.platform;
    if (command === "enroll") {
      const r = enrollBrokerPins({ dataRoot, platform, host: opt("--host"), keyId: opt("--key-id"), publicKey: opt("--public-key"), brokerDigest: opt("--broker-digest"), brokerVersion: opt("--broker-version"), brokerSourceRevision: opt("--broker-revision"), rotate: rest.includes("--rotate") });
      out(`enrolled ${r.record.platform} ${r.record.host} key ${r.record.keyId} in ${r.file}${r.rotated ? " (rotation)" : ""}`);
      return 0;
    }
    if (command === "show") {
      const pins = readEnrolledPins({ dataRoot, platform });
      out(pins ? JSON.stringify(pins) : "no enrollment");
      return pins ? 0 : 1;
    }
    err("usage: broker-pins.mjs enroll|show --platform <darwin|win32> --data-root <dir> [--host H --key-id K --public-key P --broker-digest D [--rotate]]");
    return 2;
  } catch (e) { err(`${e.code ?? "error"}: ${e.message}`); return 1; }
}

if (process.argv[1] && (() => { try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })()) process.exit(main(process.argv.slice(2)));
