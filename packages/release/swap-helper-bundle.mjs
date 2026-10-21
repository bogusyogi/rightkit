// The Developer-ID-signed `bin/darwin/rightkit-swap` that @rightkit/release ships (Orthic-Labs/rightkit#258).
// rightkit-swap is the bin target of crates/rightkit-fs (renamex_np RENAME_SWAP). Two signing routes produce it:
// (a) local publish — `right-release publish npm --local` on the Mac calls `prepareSwapHelper` before packing;
// (b) CI qualification — `publish.yml` build-swap -> sign-swap -> pack (docs/CI.md), where sign-swap imports the
// Developer ID from the `release` environment secrets. Route (b) stays blocked until root provisions
// APPLE_CERTIFICATE_BASE64/APPLE_CERTIFICATE_PASSWORD there; exporting that identity needs an Aqua login session
// (SSH export fails errSecInteractionNotAllowed/-25308 & the signing agent has no key-export capability — never
// retry either). install-app calls `assertShippedSwapHelperSigned` before it trusts the helper.
import crypto from "node:crypto";
import nodeFs from "node:fs";
import path from "node:path";
import { execFileSync as nodeExecFileSync, spawnSync as nodeSpawnSync } from "./background-process.mjs";
import { signMacosEntries } from "./mac-sign-core.mjs";
import { resolveMacosDeveloperIdIdentity } from "./macos-signing-identity.mjs";
import { runMacCodesignCommands } from "./macos-signing-session.mjs";

export const SWAP_HELPER_PACKAGE = "@rightkit/release";
export const SWAP_HELPER_BIN = ["bin", "darwin", "rightkit-swap"];
export const SWAP_HELPER_RECEIPT = ["bin", "darwin", "rightkit-swap.sign.json"];
// rightkit-swap is gated on the crate feature "fs" (crates/rightkit-fs/Cargo.toml [[bin]] required-features).
export const SWAP_HELPER_CARGO_ARGS = ["build", "--release", "-p", "rightkit-fs", "--features", "fs", "--bin", "rightkit-swap"];

/** Typed error. Codes: SWAP_HELPER_BUILD_FAILED, SWAP_HELPER_SIGN_FAILED, SWAP_HELPER_MISSING, SWAP_HELPER_UNSIGNED. */
export class SwapHelperError extends Error {
  constructor(code, message) { super(message); this.name = "SwapHelperError"; this.code = code; }
}

const helperPath = (packageDir) => path.join(packageDir, ...SWAP_HELPER_BIN);
const receiptPath = (packageDir) => path.join(packageDir, ...SWAP_HELPER_RECEIPT);
const sha256 = (fs, file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const tail = (text, lines = 30) => String(text ?? "").split(/\r?\n/).slice(-lines).join("\n");

/** True when `codesign --verify --strict` accepts the file. Only meaningful on macOS. */
export function isSwapHelperSigned(file, { execFileSync = nodeExecFileSync } = {}) {
  try { execFileSync("codesign", ["--verify", "--strict", file], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }); return true; }
  catch { return false; }
}

/**
 * Local-pack step. darwin: build through the managed cargo wrapper (`run("cargo", ...)` is `rightkit cargo` in
 * local-publish), copy to <packageDir>/bin/darwin/rightkit-swap, Developer ID sign with the shared identity
 * resolver and `macCodesignArgs` (--options runtime --timestamp), verify strictly, write a sha256 receipt.
 * Anywhere else: refuse unless the helper is present and its receipt matches its bytes. Fails closed with SwapHelperError.
 */
export function prepareSwapHelper({
  workspaceRoot, packageDir = path.join(workspaceRoot, "packages", "release"), platform = process.platform, run, env = process.env, fs = nodeFs,
  resolveIdentity = resolveMacosDeveloperIdIdentity,
  signOne = (codesignArgs) => runMacCodesignCommands([codesignArgs])[0],
  // codesign --verify / -dvv write to stderr: return both streams (as sign-dev-artifact defaultInspect does).
  inspect = (cmd, args) => {
    const result = nodeSpawnSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
    if (result.error || result.status !== 0) throw new Error(`${cmd} ${args.join(" ")} failed: ${output.trim() || result.error?.message}`);
    return output;
  },
  log = () => {},
}) {
  const dest = helperPath(packageDir);
  if (platform !== "darwin") {
    let receipt = null;
    try { receipt = JSON.parse(fs.readFileSync(receiptPath(packageDir), "utf8")); } catch { /* refused below */ }
    if (!fs.existsSync(dest) || receipt?.schema !== 1 || receipt.sha256 !== sha256(fs, dest)) {
      throw new SwapHelperError("SWAP_HELPER_MISSING", `${SWAP_HELPER_PACKAGE} must ship a signed ${SWAP_HELPER_BIN.join("/")}; it is absent or unsigned here (${platform}). Run \`right-release publish npm --package ${SWAP_HELPER_PACKAGE} --local\` on the Mac, where the Developer ID identity lives.`);
    }
    return { file: dest, signed: true, built: false, identity: receipt.identity ?? null };
  }
  const fail = (code, message) => { throw new SwapHelperError(code, message); };
  const built = run("cargo", SWAP_HELPER_CARGO_ARGS, { cwd: workspaceRoot, env });
  if (built.error || built.status !== 0) fail("SWAP_HELPER_BUILD_FAILED", `rightkit cargo ${SWAP_HELPER_CARGO_ARGS.join(" ")} failed (${built.error?.message ?? `exit ${built.status}`})\n${tail(`${built.stdout ?? ""}\n${built.stderr ?? ""}`)}`);
  const meta = run("cargo", ["metadata", "--format-version", "1", "--no-deps"], { cwd: workspaceRoot, env });
  let targetDir;
  try { targetDir = JSON.parse(meta.stdout).target_directory; } catch { /* refused below */ }
  if (meta.status !== 0 || typeof targetDir !== "string" || !path.isAbsolute(targetDir)) fail("SWAP_HELPER_BUILD_FAILED", "cannot read the Cargo target directory to find rightkit-swap");
  // The RustRight broker builds with an explicit --target triple (target/<triple>/release); plain cargo uses target/release.
  const triple = `${process.arch === "arm64" ? "aarch64" : "x86_64"}-apple-darwin`;
  const candidates = [path.join(targetDir, triple, "release", "rightkit-swap"), path.join(targetDir, "release", "rightkit-swap")];
  const built_ = candidates.find((file) => fs.existsSync(file));
  if (!built_) fail("SWAP_HELPER_BUILD_FAILED", `the build did not produce rightkit-swap (looked in ${candidates.join(", ")})`);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.rmSync(dest, { force: true });
  fs.copyFileSync(built_, dest);
  fs.chmodSync(dest, 0o755);
  let identity; let records;
  try {
    identity = resolveIdentity({ env });
    // Same Developer ID Application identity, same argv (--force --options runtime --timestamp --sign) and the same strict verify as sign-macos.
    records = signMacosEntries([{ file: dest, options: [], hardenedRuntime: true }], { identity, signOne, inspect });
  } catch (error) {
    fs.rmSync(dest, { force: true });
    fail("SWAP_HELPER_SIGN_FAILED", `cannot Developer-ID sign ${SWAP_HELPER_BIN.join("/")}: ${error.message}`);
  }
  fs.writeFileSync(receiptPath(packageDir), `${JSON.stringify({ schema: 1, sha256: sha256(fs, dest), identity, codesign: records[0]?.codesign ?? "strict-valid" }, null, 2)}\n`);
  log(`[right-release] signed ${SWAP_HELPER_BIN.join("/")} with ${identity}`);
  return { file: dest, signed: true, built: true, identity };
}

/**
 * Runtime guard (install-app, macOS). A shipped helper that exists must be signed; an unsigned one is a typed error.
 * A helper that is absent is NOT an error here: install-app keeps its documented journaled two-rename fallback and
 * emits `swap-fallback` / `replaced {swap}` so the mode that ran is always logged.
 * @returns {{ present: boolean, file: string|null }}
 */
export function assertShippedSwapHelperSigned({ file, fs = nodeFs, execFileSync = nodeExecFileSync } = {}) {
  if (!file || !fs.existsSync(file)) return { present: false, file: file ?? null };
  if (!isSwapHelperSigned(file, { execFileSync })) {
    throw new SwapHelperError("SWAP_HELPER_UNSIGNED", `${file} is not validly code-signed, so the atomic swap helper cannot be trusted. Reinstall @rightkit/release from a build published with \`right-release publish npm --package ${SWAP_HELPER_PACKAGE} --local\` on the Mac (Developer ID signed), or set RIGHTKIT_SWAP to a signed helper.`);
  }
  return { present: true, file };
}
