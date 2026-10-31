// The executor behind the one door (Adrian, 2026-10-25): rightkit-gate chooses it, never the caller.
//   broker  - `rustright` is on PATH, or the first `cargo` is the broker's managed shim: every cargo call goes through the broker.
//   refuse  - a broker-managed machine (Mac/Dell markers, broker socket, managed agent-bin) whose broker cannot be reached from here:
//             the gate never compiles around it.
//   direct  - every Linux host (WSL2, VM, cloud, CI, Linux hosted on the Dell) that has not declared a broker route, and any unmanaged Mac or
//             Windows host: cargo runs directly, through the same gates and stages.
//   Linux optional broker (Adrian, 2026-10-31: Linux normally needs no broker, but a broker is allowed there): a Linux host that EXPLICITLY declares
//   RIGHTKIT_LINUX_BROKER_CLIENT + RIGHTKIT_LINUX_BROKER_SOCKET (managed-cargo.mjs resolveLinuxBrokerRoute) gets kind "broker" with route
//   "linux-broker" when the client, endpoint and optional pinned digest validate, and kind "refuse" when they do not. A declared Linux broker never
//   falls back to direct cargo; a `rustright` on PATH, a shim directory or a socket named in another variable never declares it.
import { brokerManagedHost, managedCargoShimOnPath } from "./cargo-contract.mjs";
import { LINUX_BROKER_ROUTE, directCargoPath, directPlatform, resolveLinuxBrokerRoute } from "./managed-cargo.mjs";

export { directCargoPath, LINUX_BROKER_ROUTE };

export const EXECUTOR_KINDS = Object.freeze(["broker", "refuse", "direct"]);

/**
 * @param {{ env?: object, platform?: string, exists?: (p: string) => boolean, which: (cmd: string) => string[], readFile: (p: string) => string, fsOps?: object }} o
 * @returns {{ kind: "broker" | "refuse" | "direct", cargo: string[] | null, problem?: string, code?: string, route?: "linux-broker", client?: string, socket?: string, clientSha256?: string, env?: { PATH?: string, RUSTRIGHT_BUILD_BROKER_SOCKET?: string }, note?: string }}
 */
export function resolveExecutor({ env = process.env, platform = process.platform, exists, which, readFile, fsOps }) {
  // Every Linux (and any other non-Mac, non-Windows) execution host runs direct, whatever is on PATH or in the environment: WSL2, a VM,
  // a cloud or CI runner, or Linux hosted on the Dell (GATE-CONTRACT §1 "One entry"; joint plan step 4), unless the host explicitly declared
  // the optional Linux broker route (below). Adrian's Mac and Windows hosts use the broker as before.
  // `cargo` there must be a real toolchain: RustRight shim directories are taken off the PATH the gate hands its children (`env`).
  if (directPlatform(platform)) {
    const linux = resolveLinuxBrokerRoute({ env, platform, fsOps });
    if (linux.declared) {
      // One typed choice for the gate, managed-cargo, the carried adapters and every child (they read it again from the same environment): the
      // validated client and endpoint, never a PATH lookup. `env` is what the gate hands its children: the endpoint the client must talk to.
      if (!linux.ok) return { kind: "refuse", cargo: null, route: LINUX_BROKER_ROUTE, code: linux.code, problem: linux.problem };
      return {
        kind: "broker", route: LINUX_BROKER_ROUTE, cargo: [linux.client, "cargo"], client: linux.client, socket: linux.socket, clientSha256: linux.clientSha256,
        env: { RUSTRIGHT_BUILD_BROKER_SOCKET: linux.socket },
        note: `declared Linux broker route: client ${linux.client} (sha256 ${linux.clientSha256.slice(0, 12)}), endpoint ${linux.socket}`,
      };
    }
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
