#!/usr/bin/env node
import path from "node:path";
import { spawn } from "../background-process.mjs";
import { fileURLToPath } from "node:url";
import { assertReleaseExecutionAllowed } from "../execution-policy.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const commands = new Map([
  ["release", "build-release.mjs"],
  ["build", "build-release.mjs"],
  ["package", "build-release.mjs"],
  ["publish", "upload-release.mjs"],
  ["upload", "upload-release.mjs"],
  ["deps", "deps.mjs"],
  ["publish-update", "publish-update.mjs"],
  ["hardening", "hardeningscan.mjs"],
  ["harden", "hardeningscan.mjs"],
  ["hardeningscan", "hardeningscan.mjs"],
  ["sign-windows", "sign-windows.mjs"],
  ["sign-updater", "sign-updater.mjs"],
  ["signing-agent", "signing-agent-command.mjs"],
  ["create-mac-updater", "create-mac-updater.mjs"],
  ["mirror-root-artifact", "mirror-root-artifact.mjs"],
  ["github", "github-release.mjs"],
  ["cargo-target", "cargo-target.mjs"],
  ["qa", "release-qa-command.mjs"],
  ["ownership", "ownership-command.mjs"],
  ["ownership-approval", "ownership-approval-command.mjs"],
  ["ownership-report", "ownership-report-command.mjs"],
  ["ownership-mandatory", "ownership-mandatory-set.mjs"],
  ["swift-package", "swift-package-command.mjs"],
  ["bundle-scan", "runtime-bundle-scan.mjs"],
  ["identity-scan", "packaged-app-identity.mjs"],
  ["apple-profile-create", "apple-profile-create.mjs"],
]);

const args = process.argv.slice(2);
const first = args[0];
if (first === "dev") {
  import("../development.mjs").then(({ main }) => main(args.slice(1))).catch((error) => {
    console.error(`right-release dev: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
} else {
const githubBuildCommands = new Set(["release", "build", "package", "swift-package", "sign-windows", "sign-updater", "create-mac-updater", "cargo-target"]);

try {
  if (githubBuildCommands.has(first)
    || first?.startsWith("-")
    || (first === "addon" && args[1] === "build")
    || (first === "dmg" && args[1] === "build")
    || (first === "publish" && ["cargo", "swift"].includes(args[1]))) {
    assertReleaseExecutionAllowed({ command: `right-release ${args.join(" ")}`.trim() });
  }
} catch (error) {
  console.error(`right-release: ${error.message}`);
  process.exit(125);
}

if (!first || first === "-h" || first === "--help") {
  printHelp();
  process.exit(0);
}

if (first === "--version" || first === "-v") {
  run("release.mjs", ["--version"]);
} else if (first === "doctor") {
  if (args.includes("--all")) {
    runTest(["--test", path.join(packageRoot, "right-suite-contract.test.mjs")]);
  } else {
    run("release.mjs", ["--doctor", ...args.slice(1)]);
  }
} else if (first === "hooks") {
  run("ownership-hooks.mjs", args.slice(1));
} else if (first === "suite-doctor") {
  runTest(
    ["--test", path.join(packageRoot, "right-suite-contract.test.mjs")],
    "[right-release] suite-doctor passed",
  );
} else if (first === "cache") {
  run("cache-command.mjs", args.slice(1));
} else if (first === "disk") {
  run("disk-command.mjs", args.slice(1));
} else if (first === "install-app") {
  run("install-app.mjs", args.slice(1));
} else if (first === "dmg") {
  run("dmg-build.mjs", args.slice(1));
} else if (first === "apple") {
  run("apple-profile-renew.mjs", args.slice(1));
} else if (first === "publish") {
  const rest = args.slice(1);
  if ((rest[0] === "cargo" || rest[0] === "npm") && rest.includes("--local")) {
    run("local-publish.mjs", rest);
  } else if (rest[0] === "cargo") {
    run("publish-cargo.mjs", rest.slice(1));
  } else if (rest[0] === "npm") {
    run("publication-gate.mjs", rest.slice(1));
  } else if (rest[0] === "swift") {
    run("publish-swift.mjs", rest.slice(1));
  } else {
    run("upload-release.mjs", rest);
  }
} else if (first === "inventory") {
  if (args[1] !== "check") {
    console.error("right-release inventory: expected check");
    process.exit(2);
  }
  run("inventory-check.mjs", args.slice(2));
} else if (first === "model") {
  const rest = args.slice(1);
  if (rest[0] !== "promote") {
    console.error("right-release model: expected promote");
    process.exit(2);
  }
  run("model-promote.mjs", rest.slice(1));
} else if (first === "addon") {
  run("addon-command.mjs", args.slice(1));
} else if (first === "lsclean") {
  runBinary("bash", [path.join(packageRoot, "lsclean.sh"), ...args.slice(1)]);
} else if (first === "generate-dmg-background") {
  runBinary("python3", [path.join(packageRoot, "generate-dmg-background.py"), ...args.slice(1)]);
} else if (commands.has(first)) {
  run(commands.get(first), args.slice(1));
} else if (first.startsWith("-")) {
  run("build-release.mjs", args);
} else {
  console.error(`right-release: unknown command: ${first}\n`);
  printHelp();
  process.exit(2);
}

function run(script, scriptArgs) {
  const child = spawn(process.execPath, [path.join(packageRoot, script), ...scriptArgs], {
    cwd: process.cwd(),
    env: process.env,
    stdio: "inherit",
    windowsHide: true,
  });
  child.on("error", (error) => {
    console.error(`right-release: failed to start ${script}: ${error.message}`);
    process.exit(1);
  });
  child.on("exit", (code) => process.exit(code ?? 1));
}

function runTest(testArgs, successMessage) {
  const child = spawn(process.execPath, testArgs, {
    cwd: process.cwd(),
    env: process.env,
    stdio: "inherit",
    windowsHide: true,
  });
  child.on("error", (error) => {
    console.error(`right-release: failed to start doctor --all: ${error.message}`);
    process.exit(1);
  });
  child.on("exit", (code) => {
    if (code === 0 && successMessage) console.log(successMessage);
    process.exit(code ?? 1);
  });
}

function runBinary(cmd, cmdArgs) {
  const child = spawn(cmd, cmdArgs, {
    cwd: process.cwd(),
    env: process.env,
    stdio: "inherit",
    windowsHide: true,
  });
  child.on("error", (error) => {
    console.error(`right-release: failed to start ${cmd}: ${error.message}`);
    process.exit(1);
  });
  child.on("exit", (code) => process.exit(code ?? 1));
}

function printHelp() {
  console.log(`right-release <command> [options]

Commands:
  dmg build --app <.app> --out <dmg> --volname <name> [--background <png>] [--spec <appdmg.json>]
            [--icon-size N] [--app-position x,y] [--applications-position x,y] [--window w,h] [--window-position x,y]
                                                      Build branded UDZO image without Finder
  install-app --dmg <path.dmg> [--app-name Pulse.app] [--bundle-id <id>] [--dest /Applications]
              [--relaunch|--no-relaunch] [--json]
                                                      Verify a signed DMG, quit, replace & relaunch hidden; no UI, no osascript (macOS)
  build [--platform mac|win] [--skip-checks] [--resume-from <phase>]
                                                      Resume macOS at prepackage, sign, package, post-sign, or harden
  dev --platform win|mac [--config <path>] [--dry-run] [--build-only]
                                                      Incremental unsigned build & optional local install
  swift-package <windows-stage|msix|mac-app> --config <swift-package.json> [--dry-run] [--no-sign] [--sign] [--notarize] [--profile <name>]
                                                      Stage SwiftPM app; build+sign MSIX (Windows) or assemble+sign+notarize .app (macOS); never uploads
  bundle-scan [--allow-name <basename>]... <bundle-root>...
                                                      Fail when a bundle carries Node/Python runtimes or script sources
  identity-scan --target <path> [--identifier <id>] [--team <OU>] [--cdhash <hex>] [--arch <arch>]...
                                                      Assert codesign Identifier/Team/cdhash/lipo archs; reject ad-hoc
  upload --release <id> --platform mac|win --tier patch|update
                                                      Upload one sealed release; never builds
  qa --stage source|installed --platform mac|win [--release <id>]   Run rightkit-qa and write the sealed QA receipt (a missing receipt warns and is recorded; it never blocks)
  qa audit [--app <dir>] [--json] [--since <YYYY-MM-DD>]            List source/installed QA status per release; exit 1 if an uploaded build lacks passing QA
  publish-update --release <id> --platform mac|win --tier patch|update
                                                      Upload sealed bytes to R2 & register RightApps
  release                                             Compatibility alias for build
  publish                                             Compatibility alias for upload
  publish npm --package @rightkit/<name> [--dry-run]  Gate preflight for npm publish
  publish npm --package @rightkit/<name> --publish --tarball <gated.tgz>
                                                      Publish the gated tarball with the Keychain token
                                                      (npm-rightkit-publish) and verify registry bytes
  publish cargo --crate <name> [--dry-run]           Test, inspect, scan, and publish one crates.io package
  publish npm --package @rightkit/<name> --local     Mac-only local publish under the committed local-build grant
  publish cargo --crate <name> --local               (.rightkit-local-build.json): runs the unit's CI tests, packs, writes darwin-only
                                                      evidence, then the same publication gate and publisher; refuses without the grant
  publish swift [--dry-run] [--evidence <file>] [--workspace <dir>]
                                                      Check main, a clean tree, and the publication evidence, then tag RightKitSwift at its VERSION on origin
  inventory check [--json] [--warn-only] [--md <file>] [--no-cache]
                                                      Registry, account, dependency, app & hygiene drift (public endpoints)
  model promote --authority <app-key> --config <file> [--dry-run]
                                                      Sign and promote one runtime/model artifact
  addon doctor|build|upload|adopt <options>           Manage a signed headless add-on
  apple profile renew [--config <file>] [--profile <file>] [--dry-run]
                                                      Renew macOS profile for current Keychain signer
  signing-agent install|uninstall|status              Manage invisible Aqua-session signing service
  doctor [--platform mac|win]                        Inspect one app's release config
  doctor --all                                       Verify all Right Suite app release contracts
  suite-doctor                                       Verify all local Right Suite repositories
  ownership [--app <dir>] [--json]                   Fail on app copies of RightKit-owned capabilities
  ownership --staged [--app <dir>] [--json]          Pre-commit gate: scan the staged index for block-tier findings
  hooks install [--app <dir>]                        Install the advisory pre-commit ownership gate (pinned @rightkit/release)
  ownership enroll [--root] [--name <device>] [--root-fingerprint <sha256>] [--app <dir>]
                                                      Enroll one native key or create a signed-device request
  ownership approve-key <fingerprint|request.json>    Approve one pending device key from an active host
  ownership revoke-key <fingerprint>                  Revoke one active device key
  ownership trust-sync                               Advance this host's protected trust to the committed key log
  ownership trust-sync --check                       Read-only: say whether a sync would ask for approval, and for what (exit 3 if so)
  ownership trust-compare [--json]                   Print this host's protected-trust digest (Mac is the reference)
  ownership-approval                                 Retired no-op: exceptions are plain entries in the repo exceptions file, edited directly and reviewed through git
  ownership-report --workspace <dir> [--markdown <file>] [--json-out <file>]
                                                      Report cross-app ownership & pending requests
  ownership-mandatory [--workspace <dir>] [--app <name>]... [--amendment v2.1] [--enforce] [--json]
                                                      Check each app declares every Table 1 owner at its required version (report by default)
  cache status [--json]                              Inspect the shared local build cache
  cache prune [--dry-run|--apply] [--json]           Plan or apply safe shared-target pruning
  cache migrate --config <file> [--dry-run|--apply] [--json]
                                                      Move a same-volume legacy target without copying
  disk check [--json]                                Per-writer sizes vs caps and free-space floor (exit 3 on warning)
  disk sweep [--dry-run] [--days N] [--delete] [--json]
                                                      Move stale idle heavy data to the heavy volume (delete only with --delete)
  disk run                                           self-check, check, dry-run sweep, move sweep, warn (the scheduled job)
  disk schedule install|uninstall|status             Daily launchd job (Mac) / hidden Task Scheduler entry (Windows), bound to the stable per-user copy
  deps --check|--audit|--update                      Shared dependency lane
  hardening <artifact...>                            Run the Right Suite hardening scan
  lsclean <AppName.app>                              Clear macOS LaunchServices duplicates
  generate-dmg-background <options>                 Generate the branded multi-resolution DMG background
  mirror-root-artifact --file <path> --package-root <dir>
                                                      Persist artifact in RIGHT_RELEASE_ARTIFACT_DIR
  github --release <id> --tier patch|update --platform mac|win [--config <file>] [--repo owner/repo] [--dry-run]
                                                      Same checks and publisher as upload (explicit tier, github-releases provider + configured repository)
  github --addon <config> --platform mac|win [--repo owner/repo] [--dry-run]
                                                      Attach a sealed headless add-on to an immutable GitHub Release

Direct flags are treated as: right-release build <flags>.
Build is tier-neutral. Upload requires an explicit tier. Dev builds are incremental and unsigned.`);
}
}
