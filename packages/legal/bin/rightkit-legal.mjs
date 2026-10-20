#!/usr/bin/env node
import { readFileSync } from "node:fs";
import path from "node:path";
import { generateNpmLicenseAppendix } from "../dist/tooling.js";
import { options } from "./options.mjs";
import { runRustLicenses } from "./rust-licenses.mjs";

const USAGE = [
  "rightkit-legal npm-licenses [--root APP] [--product-name NAME] [--output legal/THIRD_PARTY_NPM_LICENSES.html] [--licenses-json FILE]",
  "rightkit-legal rust-licenses [--root APP] [--config right-release.config.mjs] [--output FILE] [--check] [--right-release FILE]",
].join("\n");

const [command, ...rest] = process.argv.slice(2);

try {
  if (!command || command === "--help") {
    console.log(USAGE);
  } else if (command === "npm-licenses") {
    const args = options(rest, ["--root", "--product-name", "--output", "--licenses-json"], ["--help"]);
    if (args["--help"]) {
      console.log(USAGE);
    } else {
      const result = generateNpmLicenseAppendix({
        appRoot: args.root, productName: args["--product-name"], output: args["--output"],
        groups: args["--licenses-json"] ? JSON.parse(readFileSync(path.resolve(args.root, args["--licenses-json"]), "utf8")) : undefined,
      });
      console.log(`wrote ${result.packageCount} package notices (${result.fallbackCount} installed instances used an SPDX fallback text)`);
    }
  } else if (command === "rust-licenses") {
    const args = options(rest, ["--root", "--config", "--output", "--right-release"], ["--help", "--check"]);
    if (args["--help"]) {
      console.log(USAGE);
    } else {
      // The one licence source is `right-release license-notices cargo` (cargo-about through the managed cargo client).
      const status = runRustLicenses({
        root: args.root,
        config: args["--config"],
        output: args["--output"],
        check: args["--check"] === true,
        rightRelease: args["--right-release"],
      });
      if (status !== 0) process.exitCode = status;
    }
  } else {
    throw new Error(`unknown command: ${command}\n${USAGE}`);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
