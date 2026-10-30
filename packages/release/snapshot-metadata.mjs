// The gate's own read-only Cargo calls inside a managed snapshot on a broker host (rightkit#1311).
//
// RustRight refuses a ROOT request whose workspace is inside a managed snapshot ("a root request cannot run inside a managed snapshot",
// rustright job_service/service.rs). The gate reads the snapshot's Cargo metadata to discover units, and every Class A Cargo scan of a
// snapshot (metadata, fetch, cargo-deny, cargo-audit) reads the snapshot too, never the primary checkout (docs/GATE-CONTRACT.md: no
// dirty-tree condition; class A for a build reads the snapshot). So on a broker host each such call is a CARRIED read-only request: the
// same carrier, nonce, signed-receipt verification and ownership mark as every other carried call (snapshot-broker.mjs carryBrokerCargo).
// The broker admits a carried call that compiles nothing only for the exact shapes below (rustright_policy::command_policy::
// snapshot_read_only). This module carries exactly those and refuses every other call inside the snapshot with RK_SNAPSHOT_CARRIED_REFUSED,
// never running it as a root request.
//
//   cargo metadata [--no-deps] [--locked] [--frozen] [--offline] [--format-version 1]   (with dependencies: --locked or --frozen)
//   cargo fetch --locked
//   cargo deny [--frozen] [--metadata-path <file>] [--config <file>] check              (each flag at most once, spaced form, nothing after `check`)
//   cargo audit --db <dir> --no-fetch
//
// Cargo's own `--config` and `-Z`, a `+toolchain`, `--manifest-path`, a `--` and every other word never ride along. The files deny reads and
// the database directory audit reads are absolute, exist (a file; a directory) and lie outside the live checkout.
import fs from "node:fs";
import path from "node:path";
import { BROKER_REFUSAL_STATUS, brokerSection, carryBrokerCargo } from "./snapshot-broker.mjs";

export const CARRIED_METADATA_REFUSED = "RK_SNAPSHOT_CARRIED_REFUSED";

const real = (p) => { try { return fs.realpathSync.native(p); } catch { return path.resolve(p); } };
const within = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || !(rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)); };

/** The flags of `rest` as a Map (flag to value, or true); null when one repeats, is unknown (`--flag=value` included) or lacks its value. */
function flagsOf(rest, spec) {
  const found = new Map();
  for (let i = 0; i < rest.length; i += 1) {
    const w = rest[i];
    if (!Object.hasOwn(spec, w) || found.has(w)) return null;
    if (spec[w] === "value") {
      const v = rest[i + 1];
      if (typeof v !== "string" || v === "" || v.startsWith("-")) return null;
      found.set(w, v); i += 1;
    } else found.set(w, true);
  }
  return found;
}

/**
 * Why the broker would not admit `words` (cargo's argv without `cargo`) as a carried read-only request, or null when it does. Without
 * `checkout` only the shape (and that the named paths are absolute) is judged; with it (the live primary checkout) the files the call
 * names must also exist, be of the right kind, and lie outside that checkout.
 */
export function carriedScanRefusal(words, { checkout = null } = {}) {
  if (!Array.isArray(words) || !words.every((w) => typeof w === "string")) return "the argv is not a list of words";
  const [sub, ...rest] = words;
  const pathProblem = (flag, value, kind) => {
    if (!path.isAbsolute(value)) return `${flag} ${value} is not absolute`;
    if (checkout === null) return null;
    let stat; try { stat = fs.statSync(value); } catch { return `${flag} ${value} does not exist`; }
    if (kind === "file" ? !stat.isFile() : !stat.isDirectory()) return `${flag} ${value} is not ${kind === "file" ? "a file" : "a directory"}`;
    return within(real(value), real(checkout)) ? `${flag} ${value} is inside the live checkout ${checkout}` : null;
  };
  if (sub === "metadata") {
    const f = flagsOf(rest, { "--no-deps": "flag", "--locked": "flag", "--frozen": "flag", "--offline": "flag", "--format-version": "value" });
    if (f === null) return "`metadata` takes only --no-deps, --locked, --frozen, --offline and --format-version 1, each once";
    if (f.has("--format-version") && f.get("--format-version") !== "1") return "`metadata` takes --format-version 1 only";
    if (!f.has("--no-deps") && !f.has("--locked") && !f.has("--frozen")) return "`metadata` with dependencies must be --locked or --frozen";
    return null;
  }
  if (sub === "fetch") return rest.length === 1 && rest[0] === "--locked" ? null : "`fetch` is admitted as `fetch --locked` only";
  if (sub === "deny") {
    if (rest.at(-1) !== "check") return "`deny` must end with `check`, with nothing after it";
    const f = flagsOf(rest.slice(0, -1), { "--frozen": "flag", "--metadata-path": "value", "--config": "value" });
    if (f === null) return "`deny` takes only --frozen, --metadata-path <file> and --config <file> before `check`, each once, spaced form";
    for (const flag of ["--metadata-path", "--config"]) if (f.has(flag)) { const problem = pathProblem(flag, f.get(flag), "file"); if (problem) return problem; }
    return null;
  }
  if (sub === "audit") {
    const f = flagsOf(rest, { "--db": "value", "--no-fetch": "flag" });
    if (f === null || f.size !== 2 || !f.has("--db") || !f.has("--no-fetch")) return "`audit` is admitted as `audit --db <dir> --no-fetch` only";
    return pathProblem("--db", f.get("--db"), "directory");
  }
  // rustright command_policy.rs:407-409: class_a_scan "package" -> observation_only_cargo. Exactly: -p <unit> --list --locked [--allow-dirty].
  if (sub === "package") {
    const unit = rest[1];
    // `-p <unit>` first, then --list and --locked and optionally --allow-dirty, each once in either order (the gate plans `--allow-dirty --locked`).
    const tail = rest.slice(2);
    const shapeOk = rest[0] === "-p" && typeof unit === "string" && unit !== "" && !unit.startsWith("-") && new Set(tail).size === tail.length && tail.every((w) => ["--list", "--locked", "--allow-dirty"].includes(w)) && tail.includes("--list") && tail.includes("--locked");
    return shapeOk ? null : "`package` is admitted as `package -p <unit> --list --locked [--allow-dirty]` only";
  }
  return `\`cargo ${words.join(" ")}\` is not one of the carried read-only shapes (metadata, fetch --locked, deny, audit --db --no-fetch, package -p <unit> --list --locked)`;
}

/** Whether the broker admits `words` as a carried read-only request (see carriedScanRefusal). */
export const carriedReadOnlyScan = (words, options) => carriedScanRefusal(words, options) === null;

/**
 * A `spawn(cmd, args, opts)` for the gate's Cargo calls in `snapshotRoot` on a broker host. A call to `cargoCommand` whose cwd is inside the
 * snapshot becomes one carried read-only request (capture mode, a spawnSync-shaped result; `status` is non-zero when the call was refused or
 * its signed receipt did not verify). Any other call passes to `spawn` unchanged (git, and Cargo in the primary checkout).
 * `broker()` returns the gate's snapshot broker (built once); `records` collects each call's ledger record. `carrySpawn` (default `spawn`) is
 * the synchronous spawn that starts the RustRight client and reads its receipt; `spawn` may be asynchronous for the pass-through calls.
 */
export function snapshotCargoSpawn({ spawn, carrySpawn = spawn, cargoCommand, snapshotRoot, broker, env, err = () => {}, records = [] }) {
  const prefix = cargoCommand.slice(1);
  let marked = false;
  const refused = (code, message) => { err(`rightkit-gate: ${code}: ${message}`); return { status: BROKER_REFUSAL_STATUS, signal: null, error: null, stdout: "", stderr: `rightkit-gate: ${code}: ${message}\n` }; };
  return (cmd, args, opts = {}) => {
    const cargo = cmd === cargoCommand[0] && prefix.every((w, i) => args[i] === w);
    if (!cargo || typeof opts.cwd !== "string" || !within(real(opts.cwd), real(snapshotRoot))) return spawn(cmd, args, opts);
    const words = args.slice(prefix.length);
    const shape = carriedScanRefusal(words);
    if (shape !== null) return refused(CARRIED_METADATA_REFUSED, `\`cargo ${words.join(" ")}\` would run inside the managed snapshot ${snapshotRoot}, and the broker admits only the carried read-only shapes there (${shape}); it is never sent as a root request`);
    try {
      const sb = broker();
      // The named files are checked against the live checkout before anything is sent (absolute, existing, of the right kind, outside it).
      const placed = carriedScanRefusal(words, { checkout: sb.checkout });
      if (placed !== null) return refused(CARRIED_METADATA_REFUSED, `\`cargo ${words.join(" ")}\` names a path the broker would not admit (${placed}); it is not sent`);
      // The ownership records the broker submit first, under this gate's handle (as every other carried call).
      if (!marked) {
        const m = sb.markBrokerSubmit?.();
        if (m?.broker?.submitted !== true || m.handle !== sb.handle) throw new Error(`the build ownership did not record the broker submit${sb.handle ? ` under ${sb.handle}` : ""}`);
        marked = true;
      }
      const r = carryBrokerCargo({ words, cwd: opts.cwd, env: { ...env, ...(opts.env ?? {}) }, section: brokerSection(sb), workspace: real(snapshotRoot), checkoutWorkspace: sb.checkout, record: (e) => records.push(e), spawn: carrySpawn, err, stdio: "capture" });
      return { status: r.status, signal: null, error: null, stdout: r.stdout, stderr: r.stderr };
    } catch (e) { return refused(e.code ?? "RK_SNAPSHOT_BROKER", e.message); }
  };
}
