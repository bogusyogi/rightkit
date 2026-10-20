import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";

/**
 * `rightkit-legal rust-licenses` has one source for Rust licence texts: the
 * `right-release license-notices cargo` route (`cargo-license-notices.mjs` in
 * `@rightkit/release`, which runs `cargo about` through the managed cargo client). This package
 * keeps no licence metadata and no cargo logic of its own; it finds the app's installed
 * `@rightkit/release`, forwards the options and returns the exit status unchanged.
 */
const RELEASE_CLI = "@rightkit/release/cli/right-release.mjs";

/** The `right-release` entry file: an explicit path, else the one installed for the app at `root`. */
export function resolveRightRelease(root, explicit) {
  if (explicit !== undefined) {
    const file = path.resolve(root, explicit);
    if (!existsSync(file)) throw new Error(`rust-licenses: --right-release ${explicit} does not exist`);
    return file;
  }
  try {
    return createRequire(path.join(root, "package.json")).resolve(RELEASE_CLI);
  } catch {
    throw new Error(
      "rust-licenses: @rightkit/release is not installed for this app. "
      + "Install it (pnpm add -D @rightkit/release) and declare legal.cargoNotices in right-release.config.mjs.",
    );
  }
}

/** The argument list passed to the `right-release` entry file. */
export function rustLicensesArgs({ root, config, output, check }) {
  return [
    "license-notices", "cargo",
    "--config", path.resolve(root, config ?? "right-release.config.mjs"),
    ...(output !== undefined ? ["--output", output] : []),
    ...(check ? ["--check"] : []),
  ];
}

/** Run the route in `root`; resolves to its exit status. Output is inherited so the user sees cargo-about's messages. */
export function runRustLicenses(options) {
  const entry = resolveRightRelease(options.root, options.rightRelease);
  const result = spawnSync(process.execPath, [entry, ...rustLicensesArgs(options)], {
    cwd: options.root,
    stdio: "inherit",
  });
  if (result.error) throw new Error(`rust-licenses: could not start right-release (${result.error.message})`);
  return result.status ?? 1;
}
