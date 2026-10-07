#!/usr/bin/env node
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { checkApp } from "./ownership.mjs";

try {
  const [appRoot, manifestPath] = process.argv.slice(2);
  if (!appRoot || !manifestPath) throw new TypeError("app root and catalog path are required");
  const report = checkApp(path.resolve(appRoot), JSON.parse(readFileSync(path.resolve(manifestPath), "utf8")));
  process.stdout.write(`${JSON.stringify(report)}\n`);
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
