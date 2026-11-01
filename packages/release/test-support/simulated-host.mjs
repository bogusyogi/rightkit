// Test support: run the managed-host refusal logic as the Mac or Dell it is written for, on any host that runs the suite.
//
// "Linux is never broker-managed" is product behavior (managed-cargo.mjs directPlatform / managedHost: a Linux host runs direct, whatever socket or control root its environment names), so the
// refusals that apply on a managed host (a signing secret or token in the environment, the nested-broker fail-fast, the managed CARGO_HOME rule) are not reachable on a Linux run of these tests
// through the environment alone, and the modules that apply them read `process.platform` at call time rather than taking a platform. A test about such a refusal declares the host it is about:
//   simulateHostPlatform(t, "darwin")              this process reads `process.platform` as that host until the test ends
//   simulatedHostChild("darwin")                   { execArgv, env } for a child node process (`spawnSync(process.execPath, [...execArgv, script])`, env merged) that does the same
// Only the managed-host decision is exercised through these; every command a test would run on that host is still a stub or a fixture. Tests of what a host DOES (paths, shells, real tools) never use it.

export const SIMULATED_PLATFORM_ENV = "RK_TEST_SIMULATED_PLATFORM";
// Node resolves an --import specifier as a URL: a bare Windows path (D:\...) reads as a scheme.
const PRELOAD = new URL("./simulated-host-preload.mjs", import.meta.url).href;

/** Makes `process.platform` read as `platform` until `t` ends (node:test context), then restores the real descriptor. */
export function simulateHostPlatform(t, platform) {
  if (!["darwin", "win32"].includes(platform)) throw new Error(`a simulated managed host is darwin or win32, not ${JSON.stringify(platform)}`);
  const original = Object.getOwnPropertyDescriptor(process, "platform");
  Object.defineProperty(process, "platform", { ...original, value: platform });
  t.after(() => Object.defineProperty(process, "platform", original));
}

/** What a child node process needs to read `process.platform` as `platform`: `execArgv` (before the script) and `env` (merged over the child's own). */
export function simulatedHostChild(platform) {
  if (!["darwin", "win32"].includes(platform)) throw new Error(`a simulated managed host is darwin or win32, not ${JSON.stringify(platform)}`);
  return { execArgv: ["--import", PRELOAD], env: { [SIMULATED_PLATFORM_ENV]: platform } };
}
