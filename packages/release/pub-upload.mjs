#!/usr/bin/env node
import { accessSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

const ACCOUNT_ID = "03ae77ccd7a07bcbb2dcfde47fa7ba3a";
const BUCKETS = new Map([
  ["public", "rightapps-downloads"],
  ["private", "rightapps-updates"],
]);
const RUNNER = { cmd: "pnpm", prefix: ["dlx"] };

const [, , localFile, r2Key, bucketAlias = "public"] = process.argv;
if (!localFile || !r2Key) {
  console.error("Usage: node upload-large.mjs <localFile> <r2Key> [public|private]");
  process.exit(1);
}

const bucket = BUCKETS.get(bucketAlias);
if (!bucket) {
  console.error(`upload-large: invalid bucket alias ${bucketAlias}; expected public|private`);
  process.exit(1);
}

try {
  accessSync(localFile);
} catch {
  console.error(`upload-large: missing local file: ${localFile}`);
  process.exit(1);
}

const fileSize = statSync(localFile).size;
const object = `${bucket}/${r2Key}`;
const wranglerArgs = ["wrangler@4", "r2", "object", "put", object, "--file", path.resolve(localFile), "--remote"];
const env = cleanCloudflareEnv(process.env);

console.log(`Uploading ${path.basename(localFile)} (${(fileSize / 1024 / 1024).toFixed(1)} MB) -> ${object}`);
console.log(`wrangler r2 object put ${object} --file ${path.resolve(localFile)} --remote`);

// Resolve the package runner before the dry-run exit: a release box without one cannot upload,
// and that must fail here rather than after a signed build has already been produced.
const runner = resolveRunner(env);
if (!runner) {
  console.error(
    "upload-large: no package runner found. Tried: " +
      "pnpm dlx. Install the pinned workspace package manager before release.",
  );
  process.exit(1);
}
console.log(`runner: ${[runner.cmd, ...runner.prefix].join(" ")}`);

if (process.env.RIGHT_RELEASE_UPLOAD_DRY_RUN === "1") {
  process.exit(0);
}

const result = spawnSync(runner.cmd, [...runner.prefix, ...wranglerArgs], {
  env,
  stdio: "inherit",
  shell: process.platform === "win32",
  windowsHide: true,
});

if (result.error) {
  console.error(`upload-large: failed to start wrangler: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);

function resolveRunner(env) {
  const probe = spawnSync(RUNNER.cmd, ["--version"], {
    env,
    stdio: "ignore",
    shell: process.platform === "win32",
    windowsHide: true,
  });
  return !probe.error && probe.status === 0 ? RUNNER : null;
}

function cleanCloudflareEnv(source) {
  const env = { ...source, CLOUDFLARE_ACCOUNT_ID: source.CLOUDFLARE_ACCOUNT_ID || ACCOUNT_ID };
  delete env.CLOUDFLARE_API_KEY;
  delete env.CLOUDFLARE_EMAIL;
  delete env.CF_API_KEY;
  delete env.CF_EMAIL;
  return env;
}
