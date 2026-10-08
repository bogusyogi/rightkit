#!/usr/bin/env node
import { installSigningAgent, uninstallSigningAgent, signingAgentStatus } from "./signing-agent-install.mjs";

const [command, ...args] = process.argv.slice(2);
try {
  let result;
  if (command === "install") {
    const options = {};
    if (args.length) {
      if (args.length !== 2 || args[0] !== "--managed-root" || !args[1]) throw new TypeError("Expected install [--managed-root <absolute-directory>]");
      options.managedRoot = args[1];
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
