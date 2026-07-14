#!/usr/bin/env node
// Deterministic source archive for RightKitSwift, for local Swift package-registry publishing.
//
// `swift package archive-source` is NOT reproducible: it stamps zip entries with the current mtime,
// so the same source hashes differently on every run, and a checksum that moves for no reason pins
// nothing. `git archive` is reproducible by construction — entry order, modes and timestamps all
// come from the commit — so a release archive is a pure function of the commit it is cut from.
//
// Publishing here means writing an archive and its checksum to a local directory. No hosted
// automation, no Git mirror, no registry push.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const SWIFT_DIR = path.resolve(import.meta.dirname, "..");
const PACKAGE_PREFIX = "swift/RightKitSwift";
const REPO = path.resolve(SWIFT_DIR, "../../..");

export function packageVersion() {
  const version = fs.readFileSync(path.join(SWIFT_DIR, "RightKitSwift/VERSION"), "utf8").trim();
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`VERSION must be semver, got "${version}"`);
  return version;
}

/** The archive is cut from a commit, never the working tree: a release must not contain scratch edits. */
export function buildArchive(treeish = "HEAD") {
  return execFileSync(
    "git",
    ["-C", REPO, "archive", "--format=zip", `--prefix=RightKitSwift/`, `${treeish}:tools/rightkit/${PACKAGE_PREFIX}`],
    { maxBuffer: 256 * 1024 * 1024 },
  );
}

export function checksum(archive) {
  return createHash("sha256").update(archive).digest("hex");
}

if (process.argv[1] === import.meta.filename) {
  const outDir = process.argv[2] ?? path.join(SWIFT_DIR, ".registry");
  const version = packageVersion();
  const archive = buildArchive();
  const digest = checksum(archive);

  fs.mkdirSync(outDir, { recursive: true });
  const zip = path.join(outDir, `RightKitSwift-${version}.zip`);
  fs.writeFileSync(zip, archive);
  fs.writeFileSync(`${zip}.sha256`, `${digest}  RightKitSwift-${version}.zip\n`);
  process.stdout.write(`${zip}\nversion ${version}\nsha256 ${digest}\n`);
}
