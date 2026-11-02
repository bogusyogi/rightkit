// The three OS targets every crate is checked for on every host (Adrian, 2026-11-02: "anything run through the Mac or the Windows or
// Linux must all check for all 3"). The host builds and tests its own OS; the other two are `cargo clippy --target` checks, so code under
// cfg(target_os = "macos") or cfg(windows) fails at the gate instead of at publish.yml native verify (five crates did on 2026-11-02:
// system-monitor, local-gen, app-manager, dictation-models, credentials). Windows is checked as the GNU target: zig supplies its C
// toolchain from any host (scripts/ci/provision-zig.mjs), and cfg(windows) code is the same for both Windows ABIs.
// Shared by local-publish.mjs (unit plans and batch clippy) and deterministic-scans.mjs (the gate's class B scans); no imports.
import path from "node:path";

export const CROSS_TARGETS = Object.freeze({ linux: "x86_64-unknown-linux-gnu", darwin: "aarch64-apple-darwin", win32: "x86_64-pc-windows-gnu" });
/** zig's name for each cross target, for the C compiler wrappers scripts/ci/provision-zig.mjs writes. */
export const ZIG_TARGETS = Object.freeze({ "x86_64-unknown-linux-gnu": "x86_64-linux-gnu", "aarch64-apple-darwin": "aarch64-macos", "x86_64-pc-windows-gnu": "x86_64-windows-gnu" });
export const CROSS_CC_ENV = "RIGHTKIT_CROSS_CC_DIR";

/** The foreign targets a crate is checked for on `platform`, or none when ci/gate.config.json gives a written reason to skip. */
export function crossTargetsFor(entry = {}, platform = "darwin") {
  if (entry?.crossTargets !== undefined) {
    if (entry.crossTargets !== false || typeof entry.crossTargetsSkip !== "string" || !entry.crossTargetsSkip.trim()) throw new Error("ci/gate.config.json crossTargets may only be false, with a written crossTargetsSkip reason");
    return [];
  }
  const native = CROSS_TARGETS[platform];
  if (!native) throw new Error(`no native target for ${platform}`);
  return Object.values(CROSS_TARGETS).filter((target) => target !== native);
}

/** cc-rs / cmake environment pointing a target's C, C++ and archiver at the provisioned zig wrappers (none when unprovisioned). */
export function crossCompileEnv(target, dir = process.env[CROSS_CC_ENV], platform = process.platform) {
  if (!dir) return undefined;
  const key = target.replaceAll("-", "_");
  const ext = platform === "win32" ? ".cmd" : "";
  return {
    [`CC_${key}`]: path.join(dir, `cc-${target}${ext}`),
    [`CXX_${key}`]: path.join(dir, `cxx-${target}${ext}`),
    [`AR_${key}`]: path.join(dir, `ar-${target}${ext}`),
  };
}
