#!/usr/bin/env node
// Writes (or with --check, verifies) packages/release/shell-contract-bridge.json: the command map of @rightkit/tauri-shell that rule
// SC-R3 `shell-handler` reads in every installed copy of @rightkit/release, generated from packages/tauri-shell/src/index.ts.
//   node scripts/generate-shell-contract-bridge.mjs           rewrite the file
//   node scripts/generate-shell-contract-bridge.mjs --check   exit 1 when the committed file differs from the source
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { bridgeMap } from "../packages/release/shell-contract.mjs";

const SOURCE = fileURLToPath(new URL("../packages/tauri-shell/src/index.ts", import.meta.url));
const TARGET = fileURLToPath(new URL("../packages/release/shell-contract-bridge.json", import.meta.url));

export const bridgeText = () => `${JSON.stringify(bridgeMap(fs.readFileSync(SOURCE, "utf8")), null, 2)}\n`;

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const text = bridgeText();
  if (process.argv.includes("--check")) {
    const current = fs.existsSync(TARGET) ? fs.readFileSync(TARGET, "utf8") : null;
    if (current !== text) {
      console.error("packages/release/shell-contract-bridge.json is out of date with packages/tauri-shell/src/index.ts; run: node scripts/generate-shell-contract-bridge.mjs");
      process.exit(1);
    }
    console.log("shell-contract-bridge.json is current");
  } else {
    fs.writeFileSync(TARGET, text);
    console.log(`wrote ${TARGET}`);
  }
}
