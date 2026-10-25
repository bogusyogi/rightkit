#!/usr/bin/env node
import path from "node:path";
import { spawn } from "../background-process.mjs";
import { fileURLToPath } from "node:url";
import { assertReleaseExecutionAllowed } from "../execution-policy.mjs";
import { hostUploadRefusalMessage } from "../host-upload-refusal.mjs";

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
  ["sign-dev-artifact", "sign-dev-artifact.mjs"],
  ["sign-qa-bundle", "sign-qa-bundle.mjs"],
  ["sign-candidate", "sign-candidate.mjs"],
  ["engine-locate", "engine-locator.mjs"],
  ["stage-sidecar", "stage-sidecar-command.mjs"],
  ["init", "init-command.mjs"],
  ["pe", "pe-content.mjs"],
  ["icons", "icon-pack.mjs"],
  ["recipe-build", "recipe-build.mjs"],
  ["signing-agent", "signing-agent-command.mjs"],
  ["create-mac-updater", "create-mac-updater.mjs"],
  ["mirror-root-artifact", "mirror-root-artifact.mjs"],
  ["test", "test-command.mjs"],
  ["web-build", "web-build.mjs"],
  ["license-notices", "npm-license-notices.mjs"],
  ["toolchain", "toolchain-swift.mjs"],
  ["github", "github-release.mjs"],
  ["cargo-target", "cargo-target.mjs"],
  ["qa", "release-qa-command.mjs"],
  ["ownership", "ownership-command.mjs"],
  ["ownership-approval", "ownership-approval-command.mjs"],
  ["ownership-report", "ownership-report-command.mjs"],
  ["ownership-mandatory", "ownership-mandatory-set.mjs"],
  ["adoption-evidence", "adoption-evidence.mjs"],
  ["swift-package", "swift-package-command.mjs"],
  ["bundle-scan", "runtime-bundle-scan.mjs"],
  ["check-release-dependencies", "release-dependencies-command.mjs"],
  ["dist-scan", "dist-scan.mjs"],
  ["identity-scan", "packaged-app-identity.mjs"],
  ["apple-profile-create", "apple-profile-create.mjs"],
  ["ios-credentials", "ios-credentials-command.mjs"],
]);

const args = process.argv.slice(2);
const first = args[0];
if (first === "dev") {
  import("../development.mjs").then(({ main }) => main(args.slice(1))).catch((error) => {
    console.error(`right-release dev: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
} else {
const githubBuildCommands = new Set(["release", "build", "package", "swift-package", "sign-windows", "sign-updater", "create-mac-updater", "cargo-target", "web-build", "test"]);

try {
  if (githubBuildCommands.has(first)
    || first?.startsWith("-")
    || (first === "addon" && args[1] === "build")
    || (first === "dmg" && args[1] === "build")
    || first === "qualify"
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
} else if (first === "qualify") {
  // Local qualification (default): both native hosts qualify the crates through the RustRight broker and a candidate is cut.
  // It uploads nothing; the approval commit and publish-reviewed.yml publish the pinned bytes (docs/CI.md "Local qualification").
  run("local-qualify.mjs", args.slice(1));
} else if (first === "publish") {
  const rest = args.slice(1);
  // RightKit crates and @rightkit packages reach a registry from CI only: publish.yml qualification, the approval
  // commit in ci/approved-artifacts.json, then publish-reviewed.yml uploads exactly the approved bytes (docs/CI.md).
  // Every host-side upload path is refused on every host; preflight, --dry-run and --qualify-only still run.
  if (rest[0] === "cargo" || rest[0] === "npm") {
    const uploads = rest[0] === "npm"
      ? rest.includes("--publish") || rest.includes("--local")
      : !rest.includes("--dry-run") && !rest.includes("--qualify-only");
    // Unconditional: a caller-set GITHUB_ACTIONS=true never lifts it (CI uploads use scripts/ci/publish.mjs and reviewed-candidate.mjs).
    if (uploads) {
      console.error(hostUploadRefusalMessage(`right-release publish ${rest[0]}`));
      process.exit(2);
    }
  }
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
} else if (first === "build" && args.includes("--web")) {
  run("web-build.mjs", args.slice(1).filter((arg) => arg !== "--web"));
} else if (first === "license-notices" && (args[1] === "cargo" || args[1] === "binaries")) {
  // `license-notices npm` stays on the commands map; the Rust and native-binary appendices have their own modules.
  run(args[1] === "cargo" ? "cargo-license-notices.mjs" : "binary-license-notices.mjs", args.slice(2));
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
  pe digest <file> [--algorithm sha256|sha384|sha512] [--json] | pe imports <file> [--allow dll,...] [--allow-file <path>]  Signature-independent PE content digest; import-closure check
  icons ico|icns --out <file> --frame <PNG>... [--dir <folder>]  Pack hand-tuned per-size PNG frames into an ICO or ICNS
  init --name <Product Name> --id <reverse.dns.bundle.id> [--slug <lowercase>] [--dir <root>] [--dry-run]  Rename the desktop app template into a real app, in place
  stage-sidecar [--config <file>] [--target <triple>] [--json]  Stage declarative sidecars into src-tauri/binaries (Tauri externalBin naming)
  recipe-build --plan <json> | --plan-file <path> [--app-root <dir>]  Run a package recipe: prepare steps, the Tauri build, postBuild steps (argv, no shell, fail closed)
  dmg build --app <.app> --out <dmg> --volname <name> [--background <png>] [--spec <appdmg.json>]
            [--icon-size N] [--app-position x,y] [--applications-position x,y] [--window w,h] [--window-position x,y]
            [--sign [--identifier <codesign-identifier>] [--receipt <path>]]
                                                      Build branded UDZO image without Finder; --sign then Developer ID signs the image (secure timestamp, no hardened runtime; never notarizes)
  install-app --dmg <path.dmg> [--app-name Pulse.app] [--bundle-id <id>] [--dest /Applications]
              [--relaunch|--no-relaunch] [--json]
                                                      Verify a signed DMG, quit, replace & relaunch hidden; no UI, no osascript (macOS)
                                                      A release install keeps the replaced app beside the fixed path as <App>.app.previous; --dev keeps NO old copy
  install-app --installer <Setup.exe> --dest <fixed install folder> --exe <App.exe> [--payload <file>]... [--payload-manifest <json>] [--json]
                                                      Windows: run the installer in place at the fixed folder; payload-only rollback (never the whole folder, user data beside the app is never copied)
  build [--platform mac|win] [--skip-checks] [--resume-from <phase>]
                                                      Resume macOS at prepackage, sign, package, post-sign, or harden
  build --web [--config <file>] [--dry-run]            Web frontend build only (config web.build steps); no Tauri, signing or packaging (alias: web-build)
  test [--config <file>] [--suite <name>[,<name>]]... [--runner <name>] [--list] [--json] [-- <args>]
                                                      Run the app's declared test suites (config tests.suites / tests.runners) in order; stops at the first failure
  license-notices npm [--config <file>] [--output <file>] [--product-name <name>] [--check]
                                                      Write the third-party npm license appendix HTML from pnpm licenses (config legal.npmNotices); --check only compares
  license-notices cargo [--config <file>] [--output <file>] [--check]
                                                      Write the third-party Cargo license appendix HTML through cargo about (config legal.cargoNotices); --check only compares
  license-notices binaries [--config <file>] [--output <file>] [--product-name <name>] [--receipt <path>] [--check]
                                                      Write the notices for shipped programs that are not Cargo or npm dependencies (config legal.binaryNotices); --check only compares
  toolchain swift --version <x.y.z> --sha256 <pin> [--url <https url>] [--github-env] [--json]
                                                      Windows: install the pinned swift.org Swift toolchain (sha256 verified before it runs), check swift --version, find the runtime bin; --github-env exports what the installer changed to GITHUB_ENV / GITHUB_PATH
  dev --platform win|mac [--config <path>] [--dry-run] [--build-only]
                                                      Incremental unsigned build & optional local install
  swift-package <windows-stage|msix|mac-app> --config <swift-package.json> [--dry-run] [--no-sign] [--sign] [--notarize] [--profile <name>]
                                                      Stage SwiftPM app; build+sign MSIX (Windows) or assemble+sign+notarize .app (macOS); never uploads
  bundle-scan [--allow-name <basename>]... <bundle-root>...
                                                      Fail when a bundle carries Node/Python runtimes or script sources
  dist-scan --dist <dir> --forbid <string>... [--forbid-file <path>] [--forbid-name <regex>]... [--ext js,css,...]
                                                      Fail when a built web dist/ carries a forbidden marker string or mock/fixture chunk name (dev, mock or fixture code in release)
  sign-dev-artifact --artifact <dir> --manifest <sign.json> [--metadata <payload.json>] [--evidence <path>] [--dry-run]
                                                      Developer ID sign an existing CI dev artifact per file with fixed identifiers; never builds, notarizes or publishes (macOS)
  sign-qa-bundle --app <abs .app> --qa-root <abs dir> [--entitlements <abs plist>] [--out <evidence.json>]
                                                      QA ONLY: ad hoc sign a bundle inside a QA scratch root, nested code first, outer last, never --deep, never the signing agent (macOS); not a release artifact
  sign-candidate --platform win|mac --artifact <dir> --candidate-receipt <receipt.json> --out <binding.json> [--manifest <sign.json>] [--publisher <subject>] [--dry-run]
                                                      Sign the exact unsigned candidate bytes (sha256 checked against the candidate receipt) instead of rebuilding; writes a candidate-sha -> signed-sha binding receipt
  engine-locate --engine cutright|genright [--sha256 <hex>] [--json]
                                                      Print the verified path of the installed CutRight/GenRight engine (fixed route, signature and identity checked, no override)
  identity-scan --target <path> [--identifier <id>] [--team <OU>] [--cdhash <hex>] [--arch <arch>]...
                                                      Assert codesign Identifier/Team/cdhash/lipo archs; reject ad-hoc
  upload --release <id> --platform mac|win --tier patch|update
                                                      Upload one sealed release; never builds
  upload --platform ios --release <id> [--apply]       Sealed IPA to TestFlight, then the exact build into the beta groups; dry run unless --apply (macOS)
  ios-credentials store --key-id <KEYID> --issuer <uuid> | check
                                                      Put the App Store Connect API key (stdin), key id and issuer into the login Keychain once, or check them; never a file (macOS)
  qa --stage source|installed --platform mac|win [--release <id>]   Run rightkit-qa (and the journeys lane when the config sets qa.journeys) and write the sealed QA receipt. Refused inside a QA run. The receipt must match the platform and release id, come from an approved runner digest, and show every release-critical journey passed
                                                      Admission is per app in qa-admission-policy.json: warn (default) records a refusal and continues; refuse blocks build, upload and publish. Only adrian moves an app from refuse back to warn
  qa audit [--app <dir>] [--json] [--since <YYYY-MM-DD>]            List source/installed QA status per release; exit 1 if an uploaded build lacks passing QA
  publish-update --release <id> --platform mac|win --tier patch|update
                                                      Upload sealed bytes to R2 & register RightApps
  release                                             Compatibility alias for build
  publish                                             Compatibility alias for upload
  check-release-dependencies [--platform mac|win|linux | --all-targets]  Check app-forbidden release crates
  publish npm --package @rightkit/<name> [--dry-run]  Gate preflight for npm publish
  publish npm --package @rightkit/<name> --publish --tarball <gated.tgz>
                                                      Publish the gated tarball with the Keychain token
                                                      (npm-rightkit-publish) and verify registry bytes
  publish cargo --crate <name> [--dry-run]           Test, inspect, scan, and publish one crates.io package
  publish npm --package @rightkit/<name> --local     Mac-only local publish under the committed local-build grant
  publish cargo --crate <name> --local               (.rightkit-local-build.json), on the Mac or Windows: runs the unit's CI tests, packs,
                                                      writes darwin-only (Mac) or win32-only (Windows) evidence, then the same
                                                      publication gate and publisher; refuses without the grant
  publish cargo --crate <name> --local --qualify-only --bundle <new-dir> [--jobs <1-64>]
                                                      Verify on source host; export crate bytes & gate evidence without uploading
  publish cargo --crate <name> --from-evidence <gate.json>
                                                      Reuse exact HEAD/artifact proof on upload host; recheck archive hash & registry bytes
  qualify cargo --crates <a,b> [--hosts win,mac] [--id <id>] [--allow-ignored-units <a,b>] [--jobs <1-64>] [--concurrency <1-8>] [--out <dir>] [--no-release] [--with-ci]
                                                      Qualify the crates on BOTH native hosts through the RustRight broker (Dell: Windows, Mac: macOS over ssh), seal one packed
                                                      asset and one receipt per host, attach them to the prerelease candidate-<id> and print the candidate and approval entry.
                                                      Needs a clean checkout with HEAD on origin/main (the other host at the same HEAD). Never uploads to a registry.
                                                      --with-ci also dispatches publish.yml for the same commit when the hosts start and pins that run in the candidate as clean-room proof
                                                      (Linux-required units and runtime owners no local workload covers are planned only with --with-ci, and then need that run to succeed).
                                                      Any repository: owner/name from its origin remote; hosts, approvals file and promotion workflow from .rightkit-gate.json
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
  doctor [--platform mac|win] [--gates]               Inspect one app's release config; --gates also runs the build's pre-build source gates
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
  adoption-evidence --manifest <published-release-manifest.json> [--packets <dir>] [--app <dir> [--adoption <file>] [--target <target>]] [--json]
                                                      Validate the handoff manifest (derived fields, AP set = frozen packets) and, with --app, an app's adoption record (lock pins, closure, receipts)
  cache status [--json]                              Inspect the shared local build cache
  cache prune [--dry-run|--apply] [--json]           Plan or apply safe shared-target pruning
  cache migrate --config <file> [--dry-run|--apply] [--json]
                                                      Move a same-volume legacy target without copying
  disk check [--json]                                Per-writer sizes vs caps and free-space floor (exit 3 on warning)
  disk sweep [--dry-run] [--days N] [--delete] [--json]
                                                      Move stale idle heavy data to the heavy volume (delete only with --delete)
  disk run [--auto|--light]                          self-check, then the full pass (check, sweep, purge, daily report); --light is the stat-only hourly pass (floors, temp-like writers, scratch; purges only below the agent volume's soft floor); --auto picks full once a day after 03:30, else light (the scheduled job)
  disk schedule install|uninstall|status             Hourly launchd job (Mac) / hidden Task Scheduler entry (Windows) running `disk run --auto`, bound to the stable per-user copy; status fails when no full pass finished in 26 h
  deps --check|--audit|--update                      Shared dependency lane
  hardening <artifact...>                            Run the Right Suite hardening scan
  lsclean <AppName.app>                              Clear macOS LaunchServices duplicates
  generate-dmg-background <options>                 Generate the branded multi-resolution DMG background
  mirror-root-artifact --file <path> --package-root <dir>
  mirror-root-artifact --config <file> --platform mac|win [--package-root <dir>]
                                                      Persist artifact(s) in RIGHT_RELEASE_ARTIFACT_DIR (config form: targets.<platform>.mirror entries, optional pruneSuffix)
  github --release <id> --tier patch|update --platform mac|win [--config <file>] [--repo owner/repo] [--dry-run]
                                                      Same checks and publisher as upload (explicit tier, github-releases provider + configured repository)
  github --addon <config> --platform mac|win [--repo owner/repo] [--dry-run]
                                                      Attach a sealed headless add-on to an immutable GitHub Release

Direct flags are treated as: right-release build <flags>.
Build is tier-neutral. Upload requires an explicit tier. Dev builds are incremental and unsigned.`);
}
}
