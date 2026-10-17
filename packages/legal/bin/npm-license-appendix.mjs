#!/usr/bin/env node
import { readFileSync } from "node:fs";
import path from "node:path";
import { generateNpmLicenseAppendix } from "../dist/tooling.js";
import { options } from "./options.mjs";

try {
  const args = options(process.argv.slice(2), ["--root", "--product-name", "--output", "--licenses-json"], ["--help"]);
  if (args["--help"]) {
    console.log("generate-npm-license-appendix [--root APP] [--product-name NAME] [--output legal/THIRD_PARTY_NPM_LICENSES.html] [--licenses-json FILE]");
  } else {
    const result = generateNpmLicenseAppendix({
      appRoot: args.root, productName: args["--product-name"], output: args["--output"],
      groups: args["--licenses-json"] ? JSON.parse(readFileSync(path.resolve(args.root, args["--licenses-json"]), "utf8")) : undefined,
    });
    console.log(`wrote ${result.packageCount} package notices (${result.fallbackCount} installed instances used an SPDX fallback text)`);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
