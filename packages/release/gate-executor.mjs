// The executor behind the one door (Adrian, 2026-10-25): rightkit-gate chooses it, never the caller.
//   broker  - `rustright` is on PATH, or the first `cargo` is the broker's managed shim: every cargo call goes through the broker.
//   refuse  - a broker-managed machine (Mac/Dell markers, broker socket, managed agent-bin) whose broker cannot be reached from here:
//             the gate never compiles around it.
//   direct  - any other host (a throwaway cloud container, a CI runner): cargo runs directly, through the same gates and stages.
import { brokerManagedHost, managedCargoShimOnPath } from "./cargo-contract.mjs";

export const EXECUTOR_KINDS = Object.freeze(["broker", "refuse", "direct"]);

/**
 * @param {{ env?: object, platform?: string, exists?: (p: string) => boolean, which: (cmd: string) => string[], readFile: (p: string) => string }} o
 * @returns {{ kind: "broker" | "refuse" | "direct", cargo: string[] | null, problem?: string }}
 */
export function resolveExecutor({ env = process.env, platform = process.platform, exists, which, readFile }) {
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
