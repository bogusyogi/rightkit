// Host and storage checks shared by the iOS lane (ios-package-recipe.mjs, ios-upload.mjs, ios-dev.mjs).
//
// Everything that needs Xcode, a signing keychain or the login Keychain is macOS only. On Windows or Linux the lane allows
// config validation and --dry-run and nothing else. The signing session rule follows macos-signing-session.mjs: Xcode's
// automatic signing needs the unlocked login keychain of the GUI (Aqua) session, so an SSH or other non-Aqua session is
// refused unless the caller supplies a routing seam (`route`). The signing agent protocol has no xcodebuild request, so the
// production default is a clear refusal, not a silent fallback.
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { detectMacSigningSession } from "./macos-signing-session.mjs";

export class IosHostError extends Error {
  constructor(code, message) { super(message); this.name = "IosHostError"; this.code = code; }
}

export function assertIosHost({ operation, platform = process.platform }) {
  if (platform !== "darwin") {
    throw new IosHostError("IOS_HOST_UNSUPPORTED", `${operation} needs macOS with Xcode; on ${platform} only config validation and --dry-run are available`);
  }
}

/**
 * macOS-only stages run in the GUI signing session. `detect` and `route` are injectable.
 * Returns { routed, reason }. `route` (when given) is used for a non-Aqua session; without it that session is refused.
 */
export function assertGuiSigningSession({ operation, env = process.env, run = spawnSync, detect = detectMacSigningSession, route } = {}) {
  let session;
  try { session = detect({ env, run }); } catch (error) {
    throw new IosHostError("IOS_GUI_SESSION_UNKNOWN", `${operation}: cannot prove the GUI signing session (${error.message})`);
  }
  if (!session.guiRunner) return { routed: false, reason: session.reason };
  if (typeof route === "function") return { routed: true, reason: session.reason, route };
  throw new IosHostError("IOS_GUI_SESSION_REQUIRED", `${operation} needs the unlocked login keychain of the GUI (Aqua) session, but this is a ${session.reason}; run it from the logged-in macOS desktop session (the signing agent cannot run xcodebuild)`);
}

/**
 * DerivedData and result-bundle paths. With RIGHTKIT_RUN_ROOT set they come from `rustright storage xcode-args --run-root <root>`
 * (a JSON argv list holding -derivedDataPath and -resultBundlePath), the same source CodeRight and HeardRight use. Without a
 * managed run the fallback is a directory under the right-release state root, never an in-repo DerivedData.
 */
export function resolveXcodeStorage({ env = process.env, run = spawnSync, cwd = process.cwd(), fallbackRoot }) {
  const runRoot = typeof env.RIGHTKIT_RUN_ROOT === "string" ? env.RIGHTKIT_RUN_ROOT.trim() : "";
  if (!runRoot) {
    if (!fallbackRoot) throw new IosHostError("IOS_STORAGE_UNAVAILABLE", "no RIGHTKIT_RUN_ROOT and no fallback storage root; run via: rustright storage run --app <app> -- right-release ...");
    return { derivedData: path.join(fallbackRoot, "DerivedData"), resultBundle: undefined, source: "fallback" };
  }
  const result = run("rustright", ["storage", "xcode-args", "--run-root", runRoot], { cwd, env, encoding: "utf8", windowsHide: true, shell: false, timeout: 30_000 });
  if (result.error || result.status !== 0) throw new IosHostError("IOS_STORAGE_FAILED", `rustright storage xcode-args failed (${result.status ?? "unavailable"})`);
  let args;
  try { args = JSON.parse(String(result.stdout ?? "")); } catch { args = null; }
  if (!Array.isArray(args) || !args.length || args.some((arg) => typeof arg !== "string")) throw new IosHostError("IOS_STORAGE_FAILED", "rustright storage xcode-args returned no argv list");
  const value = (flag, required) => {
    const index = args.indexOf(flag);
    if (index < 0 || index + 1 >= args.length) {
      if (required) throw new IosHostError("IOS_STORAGE_FAILED", `rustright storage xcode-args is missing ${flag}`);
      return undefined;
    }
    const found = args[index + 1];
    if (!path.isAbsolute(found)) throw new IosHostError("IOS_STORAGE_FAILED", `rustright storage xcode-args ${flag} is not an absolute path`);
    return found;
  };
  return { derivedData: value("-derivedDataPath", true), resultBundle: value("-resultBundlePath", false), source: "rightkit-storage" };
}

/** Last lines of command output for an error message: bounded, with obvious key material masked. */
export function outputTail(result, lines = 40) {
  const text = `${result?.stdout ?? ""}${result?.stderr ?? ""}`
    .replace(/-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)/g, "[redacted key]")
    .replace(/Bearer\s+\S+/gi, "Bearer [redacted]")
    // a bare JWT (eyJ...), and any long base64 or hex run (a key, a token, a digest) with no path separator in it
    .replace(/eyJ[A-Za-z0-9_-]{8,}(?:\.[A-Za-z0-9_-]+){0,2}/g, "[redacted jwt]")
    .replace(/(?<![A-Za-z0-9+/_-])[A-Za-z0-9+_-]{40,}={0,2}(?![A-Za-z0-9+/_-])/g, "[redacted]")
    // standard base64 whose alphabet includes "/" (a run that does not start at a path root and has no dot or hyphen)
    .replace(/(?<![A-Za-z0-9+/_.:~-])(?![/.])[A-Za-z0-9+/]{64,}={0,2}(?![A-Za-z0-9+/_.-])/g, "[redacted]");
  return text.split(/\r?\n/).filter(Boolean).slice(-lines).join("\n");
}
