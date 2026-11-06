// The three OS targets every crate is checked for on every host (Adrian, 2026-11-02: "anything run through the Mac or the Windows or
// Linux must all check for all 3"). The host builds and tests its own OS; the other two are `cargo clippy --target` checks, so code under
// cfg(target_os = "macos") or cfg(windows) fails at the gate instead of at publish.yml native verify (five crates did on 2026-11-02:
// system-monitor, local-gen, app-manager, dictation-models, credentials). Windows is checked as the GNU target: zig supplies its C
// toolchain from any host (scripts/ci/provision-zig.mjs), and cfg(windows) code is the same for both Windows ABIs.
// Shared by local-publish.mjs (unit plans and batch clippy), deterministic-scans.mjs (the gate's class B scans), stage-sidecar.mjs and
// scripts/ci/gate.mjs. Imports only node:fs and node:path.
import fs from "node:fs";
import os from "node:os";
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
 * Per-crate platform scope (rightkit#1248): `platforms` in a crate's ci/gate.config.json entry names the hosts (`process.platform` words:
 * darwin, win32, linux) whose gate plans the crate's compile and test rows. A crate whose dependencies exist only on Windows
 * (pulse-windows: windows-future) cannot compile on the Mac, so its rows run on the Windows host only; the other hosts plan none of them
 * and say so in the plan notes. Absent means every host (the default, and the only scope for a crate that builds everywhere). A bad value
 * throws, like `crossTargets`: a scope that quietly matched nothing would drop a crate's checks.
 */
export function crateRunsOn(entry = {}, platform = "darwin") {
  const scope = entry?.platforms;
  if (scope === undefined) return true;
  if (!Array.isArray(scope) || scope.length === 0 || scope.some((p) => !Object.hasOwn(CROSS_TARGETS, p)) || new Set(scope).size !== scope.length) {
    throw new Error(`ci/gate.config.json platforms must be a non-empty list of distinct hosts from ${Object.keys(CROSS_TARGETS).join(", ")}, not ${JSON.stringify(scope)}`);
  }
  // A scope is the one narrowing of "every crate on every host" (GC-A14), so it says why: its dependencies exist for those OSes only.
  if (typeof entry.platformsReason !== "string" || !entry.platformsReason.trim()) throw new Error("ci/gate.config.json platforms needs a non-empty platformsReason (why the crate builds on those hosts only)");
  return scope.includes(platform);
}

/** The cross targets a crate is checked for on `platform`: crossTargetsFor, narrowed to the OSes a platform scope names (all of them when it names none). */
export function scopedCrossTargets(entry = {}, platform = "darwin") {
  const targets = crossTargetsFor(entry, platform);
  return Array.isArray(entry?.platforms) ? targets.filter((target) => entry.platforms.some((p) => CROSS_TARGETS[p] === target)) : targets;
}

/**
 * The provisioned zig wrapper directory: RIGHTKIT_CROSS_CC_DIR, else `<managed data root>/zig/cross-cc` (where scripts/ci/provision-zig.mjs
 * writes on a Mac or Dell; on Linux the account's state directory, `$XDG_STATE_HOME/rightkit`, else `~/.local/state/rightkit`, as
 * managed-roots.mjs resolves it), else null. Linux had no default (cloud 2026-11-06: zig provisioned at ~/.local/state/rightkit/zig was not found).
 */
export function crossCcDir({ env = process.env, exists = fs.existsSync, platform = process.platform, home = os.homedir() } = {}) {
  if (env[CROSS_CC_ENV]) return env[CROSS_CC_ENV];
  const managed = platform === "win32" ? "D:\\.rightkit-managed" : platform === "darwin" ? "/Volumes/D/.rightkit-managed" : path.join(env.XDG_STATE_HOME || path.join(home, ".local", "state"), "rightkit");
  const dir = managed ? path.join(managed, "zig", "cross-cc") : null;
  return dir && exists(dir) ? dir : null;
}

/** The cross-target pkg-config stub's file name in the wrapper directory (scripts/ci/provision-zig.mjs writes it). */
export const PKG_CONFIG_CROSS = "pkg-config-cross";
/** The cross-target windres stub (non-Windows hosts; scripts/ci/provision-zig.mjs writes it). */
export const WINDRES_CROSS = "windres-cross";

/**
 * cc-rs / cmake environment pointing a target's C, C++ and archiver at the provisioned zig wrappers (none when unprovisioned).
 * When the wrapper directory holds the pkg-config stub, a foreign target's pkg-config is that stub (GC-A14): no host has another OS's
 * sysroot, and a cross-target row is a clippy type-check that never links, so a -sys build script (gdk-pixbuf-sys, webkit2gtk-sys under
 * Tauri on Linux) needs pkg-config only to answer, not to locate a library. Measured 2026-11-05: every Tauri consumer's Linux cross row
 * failed on the Mac with "pkg-config has not been configured to support cross-compilation".
 */
export function crossCompileEnv(target, dir = crossCcDir(), platform = process.platform, exists = fs.existsSync) {
  if (!dir) return undefined;
  const key = target.replaceAll("-", "_");
  const ext = platform === "win32" ? ".cmd" : "";
  const stub = path.join(dir, `${PKG_CONFIG_CROSS}${ext}`);
  return {
    [`CC_${key}`]: path.join(dir, `cc-${target}${ext}`),
    [`CXX_${key}`]: path.join(dir, `cxx-${target}${ext}`),
    [`AR_${key}`]: path.join(dir, `ar-${target}${ext}`),
    ...(exists(stub) ? { [`PKG_CONFIG_${key}`]: stub, [`PKG_CONFIG_ALLOW_CROSS_${key}`]: "1" } : {}),
    // A windows-gnu cross row off Windows: embed-resource (tauri-winres) needs a resource compiler; the stub answers as GNU windres and
    // writes an empty archive (the row never links). Reported by the Pulse lane, 2026-11-05: tauri-winres NotAttempted(x86_64-w64-mingw32-windres).
    ...(target.endsWith("-windows-gnu") && exists(path.join(dir, WINDRES_CROSS)) ? { [`RC_${key}`]: path.join(dir, WINDRES_CROSS) } : {}),
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
