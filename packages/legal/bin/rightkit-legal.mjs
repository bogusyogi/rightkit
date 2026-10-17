#!/usr/bin/env node
import { readFileSync } from "node:fs";
import path from "node:path";
import { generateNpmLicenseAppendix } from "../dist/tooling.js";
import { options } from "./options.mjs";

const USAGE = [
  "rightkit-legal npm-licenses [--root APP] [--product-name NAME] [--output legal/THIRD_PARTY_NPM_LICENSES.html] [--licenses-json FILE]",
  "rightkit-legal rust-licenses [--root APP]",
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
    const args = options(rest, ["--root"], ["--help"]);
    if (args["--help"]) {
      console.log(USAGE);
    } else {
      // BLOCKED: no existing module parses Rust dependency metadata, and this package vendors no
      // registry metadata. Reading crate licence fields needs a decided source (Cargo.lock plus the
      // registry cache, or a metadata export from the owning repository); not invented here.
      throw new Error("BLOCKED rust-licenses: no existing licence-metadata source in @rightkit/legal; see packages/legal/README.md");
    }
  } else {
    throw new Error(`unknown command: ${command}\n${USAGE}`);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
