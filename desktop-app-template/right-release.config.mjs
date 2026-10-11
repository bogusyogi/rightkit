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
// Raw exe built by the win prePackage step; Authenticode-signed before NSIS packaging.
const windowsRawExe = path.join(targetRoot, "release", "rightapp.exe");
const macDmg = `RightApp_${version}_aarch64.dmg`;

// right-release dev runs `tauri build`, whose beforeBuildCommand reaches the engine. Dev selects the
// release-iterate engine by overriding that command to the dev pair (the env RIGHTKIT_BUILD_MODE=dev
// is set on the child but no script can branch on it portably).
const DEVELOPMENT_BEFORE_BUILD = "pnpm build && pnpm build:engine:dev";

export default {
  schema: 1,
  app: "rightapp",
  // Public products: "github-releases". Private products: "r2-rightapps" (R2). Pick exactly one.
  distribution: { provider: "r2-rightapps" },
  version,
  packageManager: "pnpm",
  deps: { workdirs: ["."] },
  checks: ["typecheck"],
  // Files whose change invalidates a release build; right-release requires a non-empty include list.
  buildInputs: {
    include: [
      "right-release.config.mjs",
      "package.json",
      "pnpm-lock.yaml",
      "pnpm-workspace.yaml",
      "index.html",
      "tsconfig.json",
      "vite.config.ts",
      "src/**",
      "assets/**",
      "Cargo.toml",
      "Cargo.lock",
      "rust-toolchain.toml",
      "crates/**",
      "src-tauri/Cargo.toml",
      "src-tauri/build.rs",
      "src-tauri/src/**",
      "src-tauri/capabilities/**",
      "src-tauri/icons/**",
      "src-tauri/tauri.conf.json",
      "scripts/build-mac-notarized.sh",
    ],
    exclude: [
      "**/*.test.*",
      "**/tests/**",
      "node_modules/**",
      ".right-release/**",
      ".cache/**",
      "dist/**",
      "target/**",
      "src-tauri/target/**",
      "*.dmg",
      "*.exe",
      "*.sig",
    ],
  },
  development: {
    targets: {
      win: {
        build: [{
          cmd: "pnpm",
          args: ["exec", "tauri", "build", "--target", "x86_64-pc-windows-msvc", "--bundles", "nsis", "--no-sign", "--config", JSON.stringify({ build: { beforeBuildCommand: DEVELOPMENT_BEFORE_BUILD }, bundle: { createUpdaterArtifacts: false, windows: { signCommand: null } } }), "--", "--profile", "release-iterate", "--locked"],
        }],
        install: { kind: "tauri", productName: "RightApp", manifestPath: "src-tauri/Cargo.toml", targetTriple: "x86_64-pc-windows-msvc", expectedInstalledPath: path.join(process.env.LOCALAPPDATA || "", "RightApp", "rightapp.exe") },
      },
      mac: {
        build: [{
          cmd: "pnpm",
          args: ["exec", "tauri", "build", "--target", "aarch64-apple-darwin", "--bundles", "app", "--no-sign", "--config", JSON.stringify({ build: { beforeBuildCommand: DEVELOPMENT_BEFORE_BUILD }, bundle: { createUpdaterArtifacts: false, macOS: { signingIdentity: "-" } } }), "--", "--profile", "release-iterate", "--locked"],
        }],
        // PLACEHOLDER script: copy scripts/install-macos-development.mjs from GenRight/ScrapeRight.
        install: { kind: "command", cmd: "node", args: ["scripts/install-macos-development.mjs"] },
      },
    },
  },
  targets: {
    mac: {
      signed: true,
      signingContract: "macos-developer-id-notarized-portable-v1",
      // Prepare: build, deep-sign, notarize the app and build the DMG. Finalize (package): notarize + staple the DMG.
      prePackage: { cmd: "bash", args: ["scripts/build-mac-notarized.sh", "--prepare-right-release"] },
      sign: { prePackageFiles: [macDmg] },
      notarize: { file: macDmg, profile: "apple-dev-notary" },
      package: { cmd: "pnpm", args: ["run", "rightkit:package:mac"] },
      artifacts: [macDmg],
      hardening: [macDmg],
      publish: { cmd: "right-release", args: ["publish-update", "--config", "right-release.config.mjs", "--platform", "mac"] },
      installer: { artifacts: [{ file: macDmg, key: "rightapp/installers/mac/current/RightApp_aarch64.dmg" }] },
      updater: { artifacts: [] },
    },
    win: {
      signed: true,
      signingContract: "windows-raw-exe-authenticode-before-nsis-v1",
      // Prepare: build only the raw exe (no bundle) so right-release can Authenticode-sign it before NSIS packaging.
      prePackage: {
        cmd: "pnpm",
        args: ["exec", "tauri", "build", "--no-bundle", "--config", JSON.stringify({ bundle: { createUpdaterArtifacts: false } })],
        timeoutMs: 20 * 60 * 1000,
      },
      // PLACEHOLDER script: scripts/win-nsis.mjs (GenRight) must build the NSIS installer from the signed exe.
      package: { cmd: "pnpm", args: ["run", "rightkit:package:win"], timeoutMs: 20 * 60 * 1000 },
      artifacts: [windowsBundle],
      sign: { prePackageFiles: [windowsRawExe], files: [windowsInstaller] },
      hardening: [windowsBundle],
      publish: { cmd: "right-release", args: ["publish-update", "--config", "right-release.config.mjs", "--platform", "win"] },
      installer: { artifacts: [{ file: windowsInstaller, key: "rightapp/installers/windows/current/RightApp_x64-setup.exe" }] },
      updater: { artifacts: [] },
    },
  },
  // Licensed apps add (see README add-on): legal: { manifest: "legal/legal-manifest.json", tauriConfig: "src-tauri/tauri.conf.json" }
};
