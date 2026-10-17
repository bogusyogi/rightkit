#!/usr/bin/env node
import { readFileSync } from "node:fs";
import path from "node:path";
import { syncLegalSnapshot, assertFinalLegalSnapshot } from "../dist/tooling.js";
import { options } from "./options.mjs";

try {
  const args = options(process.argv.slice(2), ["--root", "--config", "--source-root"], ["--final", "--help"]);
  if (args["--help"]) {
    console.log("legal-snapshot [--root APP] [--config legal-snapshot.config.json] [--source-root MASTERS] [--final]");
  } else {
    const config = JSON.parse(readFileSync(path.resolve(args.root, args["--config"] ?? "legal-snapshot.config.json"), "utf8"));
    if (args["--source-root"]) config.sourceRoot = args["--source-root"];
    if (args["--final"]) {
      assertFinalLegalSnapshot(config, args.root);
      console.log("legal: every shipped document is final; snapshot hashes & gate contract verified");
    } else {
      const { manifest, contract } = syncLegalSnapshot(config, args.root);
      console.log(`legal snapshot ${manifest.acceptanceVersion}${contract.draft ? " (DRAFT: not releasable)" : ""}`);
    }
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
