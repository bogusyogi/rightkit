#!/usr/bin/env node
// `right-release update <args>`: run the @rightkit/outdated updater (its `right-update` bin) that this package pins.
//
// Why a command and not only the dependency: pnpm links just the bins of a project's direct dependencies into
// node_modules/.bin, so an app that depends on @rightkit/release alone has no `right-update` on its path even though
// @rightkit/outdated is installed beneath release. This module finds the updater installed for @rightkit/release and
// runs its bin, so `pnpm exec right-release update run` works in every app that can run `right-release`.
import { existsSync, readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "./background-process.mjs";

export const OUTDATED_PACKAGE = "@rightkit/outdated";
export const OUTDATED_BIN = "right-update";

/** The exact command a refusal gives an app that must move a stale RightKit pin. */
export const UPDATE_COMMAND = "pnpm exec right-release update run";

export class OutdatedUnavailableError extends Error {
  constructor(message) {
    super(message);
    this.name = "OutdatedUnavailableError";
    this.code = "OUTDATED_UNAVAILABLE";
  }
}

const here = () => path.dirname(fileURLToPath(import.meta.url));

/**
 * Directory of the @rightkit/outdated installed for the package at `fromDir`, or null. This is Node's lookup
 * (each ancestor's node_modules, never one named node_modules itself) done on the filesystem, because
 * @rightkit/outdated exports only "." under an `import` condition and a caller must not depend on that shape.
 */
export function findOutdatedPackage(fromDir = here()) {
  let directory = realpathSync(fromDir);
  for (;;) {
    if (path.basename(directory) !== "node_modules") {
      const candidate = path.join(directory, "node_modules", ...OUTDATED_PACKAGE.split("/"));
      if (existsSync(path.join(candidate, "package.json"))) return realpathSync(candidate);
    }
    const parent = path.dirname(directory);
    if (parent === directory) return null;
    directory = parent;
  }
}

/** Absolute path of the `right-update` script of the @rightkit/outdated installed for the package at `fromDir`. */
export function resolveOutdatedBin(fromDir = here()) {
  const root = findOutdatedPackage(fromDir);
  if (root === null) {
    throw new OutdatedUnavailableError(`${OUTDATED_PACKAGE} is not installed for @rightkit/release; reinstall @rightkit/release so its pinned dependency is present`);
  }
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8"));
  } catch {
    throw new OutdatedUnavailableError(`${OUTDATED_PACKAGE} has an unreadable package.json at ${root}`);
  }
  const named = typeof manifest.bin === "string" ? manifest.bin : manifest.bin?.[OUTDATED_BIN];
  if (manifest.name !== OUTDATED_PACKAGE || typeof named !== "string" || named === "") {
    throw new OutdatedUnavailableError(`${root} is not an ${OUTDATED_PACKAGE} that provides ${OUTDATED_BIN}`);
  }
  const file = path.resolve(root, named);
  const relative = path.relative(root, file);
  if (relative === "" || relative.startsWith("..") || path.isAbsolute(relative) || !existsSync(file)) {
    throw new OutdatedUnavailableError(`${OUTDATED_PACKAGE} ${manifest.version ?? ""} lists ${OUTDATED_BIN} at ${named}, which is not installed`);
  }
  return file;
}

/** Run the updater with `argv` and resolve to its exit status. */
export function runOutdated(argv, { fromDir = here(), cwd = process.cwd(), env = process.env, stdio = "inherit" } = {}) {
  const bin = resolveOutdatedBin(fromDir);
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [bin, ...argv], { cwd, env, stdio, windowsHide: true });
    child.on("error", reject);
    child.on("exit", (code) => resolve(code ?? 1));
  });
}

export async function main(argv = process.argv.slice(2)) {
  try {
    process.exitCode = await runOutdated(argv);
  } catch (error) {
    console.error(`right-release update: ${error.message}`);
    process.exitCode = error instanceof OutdatedUnavailableError ? 2 : 1;
  }
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) await main();
