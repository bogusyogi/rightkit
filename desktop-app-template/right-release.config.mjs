// Release contract for `right-release` (@rightkit/release). Mature shared pattern across
// ScrapeRight/ViewRight/HeardRight/GenRight. Apps replace RightApp/rightapp and artifact names.
// Build is upload-free; upload is build-free and needs an explicit --tier patch|update.
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { resolveTargetRoot } from "@rightkit/release/cargo-target.mjs";

const tauri = JSON.parse(readFileSync(new URL("./src-tauri/tauri.conf.json", import.meta.url), "utf8"));
const version = tauri.version;
const targetRoot = resolveTargetRoot(fileURLToPath(new URL("./Cargo.toml", import.meta.url)));
const windowsBundle = path.join(targetRoot, "release", "bundle", "nsis");
const windowsInstaller = path.join(windowsBundle, `RightApp_${version}_x64-setup.exe`);
const macDmg = `RightApp_${version}_aarch64.dmg`;

export default {
  schema: 1,
  app: "rightapp",
  // Public products: "github-releases". Private products: "r2-rightapps" (R2). Pick exactly one.
  distribution: { provider: "r2-rightapps" },
  version,
  packageManager: "pnpm",
  deps: { workdirs: ["."] },
  checks: ["typecheck"],
  development: {
    targets: {
      win: {
        build: [{
          cmd: "pnpm",
          args: ["exec", "tauri", "build", "--target", "x86_64-pc-windows-msvc", "--bundles", "nsis", "--no-sign", "--config", JSON.stringify({ bundle: { createUpdaterArtifacts: false, windows: { signCommand: null } } }), "--", "--profile", "release-iterate", "--locked"],
        }],
        install: { kind: "tauri", productName: "RightApp", manifestPath: "src-tauri/Cargo.toml", targetTriple: "x86_64-pc-windows-msvc", expectedInstalledPath: path.join(process.env.LOCALAPPDATA || "", "RightApp", "rightapp.exe") },
      },
      mac: {
        build: [{
          cmd: "pnpm",
          args: ["exec", "tauri", "build", "--target", "aarch64-apple-darwin", "--bundles", "app", "--no-sign", "--config", JSON.stringify({ bundle: { createUpdaterArtifacts: false, macOS: { signingIdentity: "-" } } }), "--", "--profile", "release-iterate", "--locked"],
        }],
        // PLACEHOLDER script: copy scripts/install-macos-development.mjs from GenRight/ScrapeRight.
        install: { kind: "command", cmd: "node", args: ["scripts/install-macos-development.mjs"] },
      },
    },
  },
  targets: {
    mac: {
      signed: true,
      // PLACEHOLDER scripts: scripts/mac-dmg.mjs + sign-macos-bundle.mjs (GenRight) are the mature
      // packaging pair; they resolve the managed Cargo target root and mirror the DMG to repo root.
      package: { cmd: "pnpm", args: ["run", "rightkit:package:mac"] },
      artifacts: [macDmg],
      hardening: [macDmg],
      publish: { cmd: "right-release", args: ["publish-update", "--config", "right-release.config.mjs", "--platform", "mac"] },
      installer: { artifacts: [{ file: macDmg, key: "rightapp/installers/mac/current/RightApp_aarch64.dmg" }] },
      updater: { artifacts: [] },
    },
    win: {
      signed: true,
      // PLACEHOLDER script: scripts/win-nsis.mjs (GenRight).
      package: { cmd: "pnpm", args: ["run", "rightkit:package:win"], timeoutMs: 20 * 60 * 1000 },
      artifacts: [windowsBundle],
      sign: { files: [windowsInstaller] },
      hardening: [windowsBundle],
      publish: { cmd: "right-release", args: ["publish-update", "--config", "right-release.config.mjs", "--platform", "win"] },
      installer: { artifacts: [{ file: windowsInstaller, key: "rightapp/installers/windows/current/RightApp_x64-setup.exe" }] },
      updater: { artifacts: [] },
    },
  },
  // Licensed apps add (see README add-on): legal: { manifest: "legal/legal-manifest.json", tauriConfig: "src-tauri/tauri.conf.json" }
};
