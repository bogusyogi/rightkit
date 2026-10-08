#!/usr/bin/env node
import { installSigningAgent, uninstallSigningAgent, signingAgentStatus } from "./signing-agent-install.mjs";

const [command, ...args] = process.argv.slice(2);
try {
  let result;
  if (command === "install") {
    const options = { cargoTargetDirs: [] };
    for (let i = 0; i < args.length; i += 2) {
      if (!["--managed-root", "--cargo-target-dir"].includes(args[i]) || !args[i + 1]) throw new TypeError("Expected install [--managed-root <directory>] [--cargo-target-dir <directory>]...");
      if (args[i] === "--managed-root") options.managedRoot = args[i + 1];
      else options.cargoTargetDirs.push(args[i + 1]);
    }
    result = installSigningAgent(options);
  } else if (command === "uninstall" && !args.length) result = uninstallSigningAgent();
  else if (command === "status" && !args.length) result = signingAgentStatus();
  else throw new TypeError("Expected signing-agent install|uninstall|status");
  console.log(JSON.stringify(result));
} catch (error) {
  console.error(`right-release signing-agent: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
  process.exitCode = 1;
}
