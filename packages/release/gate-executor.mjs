// The executor behind the one door (Adrian, 2026-10-25): rightkit-gate chooses it, never the caller.
//   broker  - `rustright` is on PATH, or the first `cargo` is the broker's managed shim: every cargo call goes through the broker.
//   refuse  - a broker-managed machine (Mac/Dell markers, broker socket, managed agent-bin) whose broker cannot be reached from here:
//             the gate never compiles around it.
//   direct  - every Linux host (WSL2, VM, cloud, CI, Linux hosted on the Dell) always, and any unmanaged Mac or Windows host: cargo
//             runs directly, through the same gates and stages.
import { closeSync, openSync, readSync } from "node:fs";
import path from "node:path";
import { brokerManagedHost, managedCargoShimOnPath } from "./cargo-contract.mjs";

const MANAGED_AGENT_BIN = /(?:^|\/)(?:\.rightkit-managed|rightkitmanagedagent|rightkit\/build-control)\/agent-bin\/?$/i;
/** The first 4 KiB of a file, or "" (a shim script names its guard there; a toolchain binary is never read whole). */
function head(file) {
  let fd = null;
  try { fd = openSync(file, "r"); const b = Buffer.alloc(4096); const n = readSync(fd, b, 0, 4096, 0); return b.subarray(0, n).toString("latin1"); }
  catch { return ""; } finally { if (fd !== null) try { closeSync(fd); } catch { /* closed */ } }
}

/**
 * On a direct (Linux) host: PATH without the directories whose `cargo` is a RustRight shim (the managed agent-bin, a configured
 * RIGHTKIT_MANAGED_AGENT_BIN, or a script that routes through the broker), and the first non-shim cargo left on it. Plain `cargo`
 * then can only be a real toolchain; with none, cargo scans fail as a missing tool, never through a shim.
 */
export function directCargoPath({ env = process.env, exists, readHead = head }) {
  const configured = env.RIGHTKIT_MANAGED_AGENT_BIN ? path.posix.resolve(env.RIGHTKIT_MANAGED_AGENT_BIN) : null;
  const dirs = String(env.PATH ?? "").split(":").filter(Boolean);
  const kept = []; const dropped = []; let cargo = null;
  for (const dir of dirs) {
    const candidate = path.posix.join(dir, "cargo");
    const isShim = exists(candidate) && (MANAGED_AGENT_BIN.test(dir) || (configured && path.posix.resolve(dir) === configured) || /cargo-guard\.mjs|rustright/i.test(readHead(candidate)));
    if (isShim) { dropped.push(dir); continue; }
    kept.push(dir);
    if (!cargo && exists(candidate)) cargo = candidate;
  }
  return { path: kept.join(":"), dropped, cargo };
}

export const EXECUTOR_KINDS = Object.freeze(["broker", "refuse", "direct"]);

/**
 * @param {{ env?: object, platform?: string, exists?: (p: string) => boolean, which: (cmd: string) => string[], readFile: (p: string) => string }} o
 * @returns {{ kind: "broker" | "refuse" | "direct", cargo: string[] | null, problem?: string, env?: { PATH: string }, note?: string }}
 */
export function resolveExecutor({ env = process.env, platform = process.platform, exists, which, readFile }) {
  // Every Linux (and any other non-Mac, non-Windows) execution host runs direct, whatever is on PATH or in the environment: WSL2, a VM,
  // a cloud or CI runner, or Linux hosted on the Dell (GATE-CONTRACT §1 "One entry"; joint plan step 4). Only Adrian's Mac and Windows
  // hosts use the broker.
  // `cargo` there must be a real toolchain: RustRight shim directories are taken off the PATH the gate hands its children (`env`).
  if (platform !== "darwin" && platform !== "win32") {
    const direct = directCargoPath({ env, exists });
    return { kind: "direct", cargo: ["cargo"], ...(direct.dropped.length ? { env: { PATH: direct.path }, note: `RustRight shims taken off PATH for direct cargo: ${direct.dropped.join(", ")}` } : {}), ...(direct.cargo ? {} : { note: `no direct cargo on PATH${direct.dropped.length ? ` (RustRight shims skipped: ${direct.dropped.join(", ")})` : ""}; cargo scans fail as a missing tool` }) };
  }
  if (which("rustright").length) return { kind: "broker", cargo: ["rustright", "cargo"] };
  const first = which("cargo")[0];
  if (first) {
    try { if (readFile(first).includes("cargo-guard.mjs")) return { kind: "broker", cargo: ["cargo"] }; } catch { /* unreadable: not the shim */ }
  }
  // RustRight's own agent-bin `cargo` shim first on PATH: plain `cargo` already goes through the broker.
  if (managedCargoShimOnPath(env, platform, exists)) return { kind: "broker", cargo: ["cargo"] };
  if (brokerManagedHost(env, platform, exists)) {
    return {
      kind: "refuse",
      cargo: null,
      problem: "this machine is broker-managed (RustRight) but `rustright` is not reachable from this shell; start or repair the broker, then run again. The gate never compiles around the broker",
    };
  }
  return { kind: "direct", cargo: ["cargo"] };
}
