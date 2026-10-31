#!/usr/bin/env node
// rightkit#1322 owner publication: upload the exact .crate archives the publish-owner job packaged and checked, byte for
// byte, through the same crates.io helpers upload.mjs uses (metadata from publish.mjs, body and index readback from
// upload.mjs). The archive's sha256 recorded before any check must still match before the token is used, and the index
// checksum must equal it afterwards.
//
//   owner-upload.mjs <out-dir> <crate>...   (CARGO_REGISTRY_TOKEN in the environment)
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { buildCrateUploadMetadata, readCargoMetadata } from "./publish.mjs";
import { CRATES_PUBLISH_URL, buildCratePublishBody, childEnv, parseCratesResponse, sha, waitForCrateIndex } from "./upload.mjs";

const spawn = (command, args, options = {}) => spawnSync(command, args, { ...options, env: childEnv(options.env ?? process.env) });

const [outDir, ...crates] = process.argv.slice(2);
const token = process.env.CARGO_REGISTRY_TOKEN;
if (!outDir || crates.length === 0 || !token) throw new Error("usage: owner-upload.mjs <out-dir> <crate>... with CARGO_REGISTRY_TOKEN set");

for (const name of crates) {
  const record = JSON.parse(fs.readFileSync(path.join(outDir, `${name}.json`), "utf8"));
  const bytes = fs.readFileSync(record.archive);
  if (sha("sha256", bytes) !== record.sha256) throw new Error(`${name}: archive changed since it was packaged and checked`);
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-owner-"));
  const unpack = spawn("tar", ["-xzf", record.archive, "-C", scratch], { encoding: "utf8" });
  if (unpack.status !== 0) throw new Error(`${name}: cannot unpack ${record.archive}`);
  const inner = path.join(scratch, `${name}-${record.version}`);
  const pkg = readCargoMetadata(inner, spawn, ["--manifest-path", path.join(inner, "Cargo.toml")]).packages.find((p) => p.name === name && p.version === record.version);
  if (!pkg) throw new Error(`${name}: packaged Cargo.toml does not describe ${name}@${record.version}`);
  const readme = pkg.readme ? fs.readFileSync(path.join(inner, pkg.readme), "utf8") : null;
  const body = buildCratePublishBody(JSON.stringify(buildCrateUploadMetadata(pkg, readme)), bytes);
  const response = await fetch(CRATES_PUBLISH_URL, { method: "PUT", headers: { authorization: token, accept: "application/json", "content-type": "application/octet-stream", "user-agent": "rightkit-owner-approval" }, body, redirect: "error" });
  const text = await response.text();
  const answer = parseCratesResponse(text);
  if (!response.ok || answer.errors.length || answer.unreadable) throw new Error(`crates.io refused ${name}@${record.version}: HTTP ${response.status} ${answer.errors.join("; ").slice(0, 600)}`);
  await waitForCrateIndex({ name, version: record.version, cksum: record.sha256, fetchImpl: fetch });
  console.log(`${name}@${record.version} published; index checksum equals the checked archive ${record.sha256}`);
}
