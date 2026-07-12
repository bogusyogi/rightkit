#!/usr/bin/env node
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const commands = new Map([
  ["release", "release.mjs"],
  ["build", "release.mjs"],
  ["package", "release.mjs"],
  ["deps", "deps.mjs"],
  ["publish-update", "publish-update.mjs"],
  ["hardening", "hardeningscan.mjs"],
  ["harden", "hardeningscan.mjs"],
  ["hardeningscan", "hardeningscan.mjs"],
  ["sign-windows", "sign-windows.mjs"],
  ["sign-updater", "sign-updater.mjs"],
  ["create-mac-updater", "create-mac-updater.mjs"],
]);

const args = process.argv.slice(2);
const first = args[0];

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
} else if (first === "publish") {
  const rest = args.slice(1);
  run("release.mjs", rest.includes("--upload") || rest.includes("--publish") ? rest : [...rest, "--upload"]);
} else if (first === "lsclean") {
  runBinary("bash", [path.join(packageRoot, "lsclean.sh"), ...args.slice(1)]);
} else if (commands.has(first)) {
  run(commands.get(first), args.slice(1));
} else if (first.startsWith("-")) {
  run("release.mjs", args);
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

function runTest(testArgs) {
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
  child.on("exit", (code) => process.exit(code ?? 1));
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
  release [--platform mac|win] --tier patch|update   Build/package through the signed release lane
  publish [--platform mac|win] --tier patch|update   Release plus R2 upload + RightApps registration
  doctor [--platform mac|win]                        Inspect one app's release config
  doctor --all                                       Verify all Right Suite app release contracts
  deps --check|--audit|--update                      Shared dependency lane
  hardening <artifact...>                            Run the Right Suite hardening scan
  lsclean <AppName.app>                              Clear macOS LaunchServices duplicates

Direct flags are treated as: right-release release <flags>.
Publishing requires an explicit tier. Unsigned/local smoke builds stay app-local unless declared in right-release.config.mjs.`);
}
