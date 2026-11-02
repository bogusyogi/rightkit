// The three OS targets every crate is checked for on every host (Adrian, 2026-11-02: "anything run through the Mac or the Windows or
// Linux must all check for all 3"). The host builds and tests its own OS; the other two are `cargo clippy --target` checks, so code under
// cfg(target_os = "macos") or cfg(windows) fails at the gate instead of at publish.yml native verify (five crates did on 2026-11-02:
// system-monitor, local-gen, app-manager, dictation-models, credentials). Windows is checked as the GNU target: zig supplies its C
// toolchain from any host (scripts/ci/provision-zig.mjs), and cfg(windows) code is the same for both Windows ABIs.
// Shared by local-publish.mjs (unit plans and batch clippy), deterministic-scans.mjs (the gate's class B scans), stage-sidecar.mjs and
// scripts/ci/gate.mjs. Imports only node:fs and node:path.
import fs from "node:fs";
import path from "node:path";

export const CROSS_TARGETS = Object.freeze({ linux: "x86_64-unknown-linux-gnu", darwin: "aarch64-apple-darwin", win32: "x86_64-pc-windows-gnu" });
/** zig's name for each cross target, for the C compiler wrappers scripts/ci/provision-zig.mjs writes. */
export const ZIG_TARGETS = Object.freeze({ "x86_64-unknown-linux-gnu": "x86_64-linux-gnu", "aarch64-apple-darwin": "aarch64-macos", "x86_64-pc-windows-gnu": "x86_64-windows-gnu" });
export const CROSS_CC_ENV = "RIGHTKIT_CROSS_CC_DIR";

/** The host's own build target, as the package stage names it: Windows builds MSVC (win-package-recipe), the Mac and Linux their native arch. */
export const HOST_TARGETS = Object.freeze({ "darwin-arm64": "aarch64-apple-darwin", "darwin-x64": "x86_64-apple-darwin", "win32-x64": "x86_64-pc-windows-msvc", "win32-arm64": "aarch64-pc-windows-msvc", "linux-x64": "x86_64-unknown-linux-gnu", "linux-arm64": "aarch64-unknown-linux-gnu" });
export function hostTarget(platform = process.platform, arch = process.arch) {
  const target = HOST_TARGETS[`${platform}-${arch}`];
  if (!target) throw new Error(`no host Rust target for ${platform}-${arch}`);
  return target;
}

/**
 * The foreign targets every crate is checked for on `platform`: the other two of CROSS_TARGETS, always. There is no opt-out (Adrian,
 * 2026-11-03): a crate that cannot be type-checked for an OS has a bug to fix (cfg-gate its OS code, or make its build script return early
 * for a foreign CARGO_CFG_TARGET_OS). A `crossTargets` key in ci/gate.config.json is refused.
 */
export function crossTargetsFor(entry = {}, platform = "darwin") {
  if (entry?.crossTargets !== undefined || entry?.crossTargetsSkip !== undefined) throw new Error("ci/gate.config.json crossTargets/crossTargetsSkip is not allowed: every crate is checked for all three OS targets");
  const native = CROSS_TARGETS[platform];
  if (!native) throw new Error(`no native target for ${platform}`);
  return Object.values(CROSS_TARGETS).filter((target) => target !== native);
}

/**
 * The provisioned zig wrapper directory: RIGHTKIT_CROSS_CC_DIR, else `<managed data root>/zig/cross-cc` (where scripts/ci/provision-zig.mjs
 * writes on a Mac or Dell), else null.
 */
export function crossCcDir({ env = process.env, exists = fs.existsSync, platform = process.platform } = {}) {
  if (env[CROSS_CC_ENV]) return env[CROSS_CC_ENV];
  const managed = platform === "win32" ? "D:\\.rightkit-managed" : platform === "darwin" ? "/Volumes/D/.rightkit-managed" : null;
  const dir = managed ? path.join(managed, "zig", "cross-cc") : null;
  return dir && exists(dir) ? dir : null;
}

/** cc-rs / cmake environment pointing a target's C, C++ and archiver at the provisioned zig wrappers (none when unprovisioned). */
export function crossCompileEnv(target, dir = crossCcDir(), platform = process.platform) {
  if (!dir) return undefined;
  const key = target.replaceAll("-", "_");
  const ext = platform === "win32" ? ".cmd" : "";
  return {
    [`CC_${key}`]: path.join(dir, `cc-${target}${ext}`),
    [`CXX_${key}`]: path.join(dir, `cxx-${target}${ext}`),
    [`AR_${key}`]: path.join(dir, `ar-${target}${ext}`),
  };
}

// One build output and one warm cache per app per host (Adrian: "Flag, no flag, it should go into 1 directory"; GATE-CONTRACT GC-A01, GC-A07).
// Cargo picks the output directory from the profile (dev -> debug, release -> release) and from whether a target is named
// (target/<profile> vs target/<triple>/<profile>), so every compile the gate launches is rewritten here, whatever its caller passed:
// `--profile release` is always set (a request for any other profile, `--debug` included, is refused), and a call that names no target gets
// the host's own. A call that names another target keeps it (the cross-target check: that OS's own directory, still release).
// Non-compiling subcommands (metadata, tree, deny, audit, package --list, fmt) are returned unchanged.
const COMPILE_SUBCOMMANDS = new Set(["build", "b", "check", "c", "clippy", "rustc", "rustdoc", "doc", "d", "test", "t", "bench", "run", "r"]);
export function oneOutput(args, { platform = process.platform, arch = process.arch } = {}) {
  // The subcommand is the first word after an optional leading `cargo`, `+<toolchain>` and cargo's own global options.
  let at = 0;
  if (args[at] === "cargo") at += 1;
  while (at < args.length && (args[at].startsWith("+") || args[at].startsWith("-"))) at += ["--config", "-Z", "-C", "--color"].includes(args[at]) ? 2 : 1;
  if (!COMPILE_SUBCOMMANDS.has(args[at])) return [...args];
  const head = args.slice(0, at + 1); const rest = [];
  const tail = args.slice(at + 1);
  const sep = tail.indexOf("--");
  const own = sep < 0 ? tail : tail.slice(0, sep);
  // Hard block (Adrian, 2026-11-03: "i wnat all dev completely gone. all debug too ... never another, hard block"): a compile that asks for any
  // profile but release is refused, never rewritten. `--release` / `-r` is release and is normalized to `--profile release`.
  for (let i = 0; i < own.length; i += 1) {
    const a = own[i];
    if (a === "--release" || a === "-r") continue;
    const asked = a === "--debug" ? "dev" : a === "--profile" ? own[i + 1] : a.startsWith("--profile=") ? a.slice("--profile=".length) : null;
    if (asked !== null) {
      if (asked !== "release") throw new Error(`RK_PROFILE_REFUSED: cargo ${args[at]} asks for the ${JSON.stringify(asked)} profile; only release is allowed (one build output per app, GATE-CONTRACT GC-A07)`);
      if (a === "--profile") i += 1;
      continue;
    }
    rest.push(a);
  }
  // GATE-CONTRACT GC-A09: tests are isolated by injection; no `test-mode` feature exists in any build (Adrian: "No, no flag").
  const featureLists = rest.flatMap((a, i) => (a === "--features" || a === "-F" ? [rest[i + 1] ?? ""] : a.startsWith("--features=") ? [a.slice("--features=".length)] : []));
  if (featureLists.some((list) => list.split(/[\s,]+/).some((f) => f === "test-mode" || f.endsWith("/test-mode")))) throw new Error(`RK_TEST_MODE_REFUSED: cargo ${args[at]} enables a test-mode feature; tests are isolated by injection (GATE-CONTRACT GC-A09), no build has a test mode`);
  const named = rest.includes("--target") || rest.some((a) => a.startsWith("--target="));
  return [...head, "--profile", "release", ...(named ? [] : ["--target", hostTarget(platform, arch)]), ...rest, ...(sep < 0 ? [] : tail.slice(sep))];
}
