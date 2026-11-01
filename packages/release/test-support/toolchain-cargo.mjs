// Test-only: the TOOLCHAIN cargo for throwaway fixtures. Not shipped: lives under test-support/.
//
// On the Linux CI qualify route the tests run inside the gate's runtime cargo wrapper (cargo-target-shim.mjs): first on PATH and named in
// $CARGO, with a workspace map that binds none of a test's temp fixtures, so it refuses every cargo call. A "real cargo" test builds a
// fixture it created under os.tmpdir(), never the snapshot under test, so it reaches the toolchain cargo the wrapper records.
// `assertTempFixture` is the fence: the toolchain cargo is for temp fixtures only; anything in the snapshot goes through the wrapper.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "../background-process.mjs";
import { directCargoPath, realCargoOf } from "../managed-cargo.mjs";

export const TOOLCHAIN_CARGO = (() => {
  if (process.platform !== "linux") return null;
  const r = spawnSync("sh", ["-c", "command -v cargo"], { encoding: "utf8" });
  const at = String(r.stdout ?? "").trim();
  if (r.status !== 0 || !at) return null;
  const real = realCargoOf([at]);
  return real && path.isAbsolute(real) && !real.includes(".rightkit-managed") ? real : null;
})();

/** `env` for a cargo spawned on a temp fixture: no $CARGO, PATH without the outer gate wrapper's directory or RustRight shims. Unchanged off Linux. */
export function toolchainCargoEnv(env = process.env) {
  if (process.platform !== "linux") return env;
  const out = { ...env };
  delete out.CARGO;
  let direct = directCargoPath({ env, gateWrappers: true, platform: "linux" }).path;
  // The toolchain cargo may live only behind the wrapper's map (its directory dropped with the wrapper's): keep it reachable.
  if (TOOLCHAIN_CARGO) {
    const dir = path.dirname(TOOLCHAIN_CARGO);
    if (!direct.split(":").includes(dir)) direct = direct ? `${direct}:${dir}` : dir;
  }
  out.PATH = direct;
  return out;
}

const inside = (child, parent) => child === parent || child.startsWith(parent.endsWith(path.sep) ? parent : parent + path.sep);
const real = (p) => { try { return fs.realpathSync(p); } catch { return path.resolve(p); } };

const PACKAGE_ROOT = real(path.dirname(path.dirname(fileURLToPath(import.meta.url))));
const REPO_ROOT = real(path.resolve(PACKAGE_ROOT, "..", ".."));

/** Throws unless `dir` is a throwaway fixture: inside the OS temp dir (or /tmp) and outside this package and the repository. */
export function assertTempFixture(dir) {
  const at = real(dir);
  const tmps = [real(os.tmpdir()), real("/tmp")];
  if (!tmps.some((t) => inside(at, t))) throw new Error(`toolchain cargo refused: ${at} is not under the OS temp directory`);
  if (inside(at, PACKAGE_ROOT) || inside(at, REPO_ROOT)) throw new Error(`toolchain cargo refused: ${at} is inside the package or repository`);
}
