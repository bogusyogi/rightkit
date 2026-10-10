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
const PACKAGE_DIR = path.join(SWIFT_DIR, "RightKitSwift");
// Fixed entry timestamp, as in the canonical publisher (packages/release/publish-swift.mjs): the
// archive bytes depend on the package tree alone, never on the wall clock.
const ARCHIVE_MTIME = "1970-01-01T00:00:00Z";

function git(args, options = {}) {
  return execFileSync("git", ["-C", PACKAGE_DIR, ...args], { encoding: "utf8", ...options }).trim();
}

export function packageVersion() {
  const version = fs.readFileSync(path.join(PACKAGE_DIR, "VERSION"), "utf8").trim();
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`VERSION must be semver, got "${version}"`);
  return version;
}

/**
 * The tree object of the package at `treeish`, derived the way the canonical publisher derives it:
 * the package's own prefix inside whatever repository contains it. No assumption about where the
 * repository root sits relative to this script.
 */
export function packageTree(treeish = "HEAD") {
  const prefix = git(["rev-parse", "--show-prefix"]).replace(/\/$/, "");
  return git(["rev-parse", prefix ? `${treeish}:${prefix}` : `${treeish}^{tree}`]);
}

/**
 * The archive is cut from a commit, never the working tree: a release must not contain scratch edits.
 * `git archive` resolves the current directory inside the tree it is given, so a bare package tree
 * run from the package subdirectory archives nothing; it runs from the repository top level instead.
 */
export function buildArchive(treeish = "HEAD") {
  const archive = execFileSync(
    "git",
    ["-C", git(["rev-parse", "--show-toplevel"]), "archive", "--format=zip", `--mtime=${ARCHIVE_MTIME}`, "--prefix=RightKitSwift/", packageTree(treeish)],
    { maxBuffer: 256 * 1024 * 1024 },
  );
  if (archive.length < 1024 || !archive.includes("Package.swift")) throw new Error("Swift source archive is empty or incomplete");
  return archive;
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
