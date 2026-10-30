// The gate's own read-only Cargo calls inside a managed snapshot on a broker host (rightkit#1311).
//
// RustRight refuses a ROOT request whose workspace is inside a managed snapshot ("a root request cannot run inside a managed snapshot",
// rustright job_service/service.rs). The gate reads the snapshot's Cargo metadata to discover units (and compares workspace members), so on
// a broker host every such call is a CARRIED read-only request: the same carrier, nonce, signed-receipt verification and ownership mark as
// every other carried call (snapshot-broker.mjs carryBrokerCargo). The broker admits a carried call that compiles nothing only when it is on
// its read-only allowlist (rustright_policy::command_policy::snapshot_read_only): `metadata --no-deps`, or `metadata` lock-pinned and
// offline (`--frozen`, or `--locked --offline`). This module carries exactly those shapes and refuses every other call inside the snapshot,
// never running it as a root request.
import fs from "node:fs";
import path from "node:path";
import { BROKER_REFUSAL_STATUS, brokerSection, carryBrokerCargo } from "./snapshot-broker.mjs";
import { cargoSubcommandOf } from "./snapshot-request.mjs";

export const CARRIED_METADATA_REFUSED = "RK_SNAPSHOT_CARRIED_REFUSED";

/** Whether the broker admits `words` (cargo's argv without `cargo`) as a carried read-only request: the `metadata` shapes the gate sends. */
export function carriedReadOnlyMetadata(words) {
  if (words.some((w) => /^(--config|-Z)(=|$)|^-[A-Za-z]*[Z]/.test(w))) return false;
  const end = words.indexOf("--");
  const own = end < 0 ? words : words.slice(0, end);
  if (end >= 0 || cargoSubcommandOf(words) !== "metadata") return false;
  if (own.includes("--no-deps")) return true;
  return own.includes("--frozen") || (own.includes("--locked") && own.includes("--offline"));
}

const real = (p) => { try { return fs.realpathSync.native(p); } catch { return path.resolve(p); } };
const within = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || !(rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)); };

/**
 * A `spawn(cmd, args, opts)` for the gate's Cargo calls in `snapshotRoot` on a broker host. A call to `cargoCommand` whose cwd is inside the
 * snapshot becomes one carried read-only request (capture mode, a spawnSync-shaped result; `status` is non-zero when the call was refused or
 * its signed receipt did not verify). Any other call passes to `spawn` unchanged (git, and Cargo in the primary checkout).
 * `broker()` returns the gate's snapshot broker (built once); `records` collects each call's ledger record.
 */
export function snapshotCargoSpawn({ spawn, cargoCommand, snapshotRoot, broker, env, err = () => {}, records = [] }) {
  const prefix = cargoCommand.slice(1);
  let marked = false;
  const refused = (code, message) => { err(`rightkit-gate: ${code}: ${message}`); return { status: BROKER_REFUSAL_STATUS, signal: null, error: null, stdout: "", stderr: `rightkit-gate: ${code}: ${message}\n` }; };
  return (cmd, args, opts = {}) => {
    const cargo = cmd === cargoCommand[0] && prefix.every((w, i) => args[i] === w);
    if (!cargo || typeof opts.cwd !== "string" || !within(real(opts.cwd), real(snapshotRoot))) return spawn(cmd, args, opts);
    const words = args.slice(prefix.length);
    if (!carriedReadOnlyMetadata(words)) return refused(CARRIED_METADATA_REFUSED, `\`cargo ${words.join(" ")}\` would run inside the managed snapshot ${snapshotRoot}, and the broker admits only a carried \`metadata --no-deps\` (or a lock-pinned offline \`metadata\`) there; it is never sent as a root request`);
    try {
      const sb = broker();
      // The ownership records the broker submit first, under this gate's handle (as every other carried call).
      if (!marked) {
        const m = sb.markBrokerSubmit?.();
        if (m?.broker?.submitted !== true || m.handle !== sb.handle) throw new Error(`the build ownership did not record the broker submit${sb.handle ? ` under ${sb.handle}` : ""}`);
        marked = true;
      }
      const r = carryBrokerCargo({ words, cwd: opts.cwd, env: { ...env, ...(opts.env ?? {}) }, section: brokerSection(sb), workspace: real(snapshotRoot), checkoutWorkspace: sb.checkout, record: (e) => records.push(e), spawn, err, stdio: "capture" });
      return { status: r.status, signal: null, error: null, stdout: r.stdout, stderr: r.stderr };
    } catch (e) { return refused(e.code ?? "RK_SNAPSHOT_BROKER", e.message); }
  };
}
