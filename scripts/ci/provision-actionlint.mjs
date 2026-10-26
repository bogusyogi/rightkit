#!/usr/bin/env node
// Provision ci/actionlint.json's pinned actionlint on a publish verify runner, so the tests that lint rendered workflows run
// instead of skipping (@rightkit/ci). The archive is downloaded from the pinned version's GitHub release, refused unless its
// sha256 is the pinned one, unpacked into <dest>, and refused again unless `actionlint -version` reports the pinned version.
// The directory is appended to GITHUB_PATH. Nothing here falls back to another version or an unpinned download.
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const PINS_FILE = path.join(ROOT, "ci", "actionlint.json");

const refuse = (message) => { throw new Error(`provision-actionlint refused: ${message}`); };

export function platformKey(platform = process.platform, arch = process.arch) {
  const key = `${platform}-${arch}`;
  return { "linux-x64": "linux-x64", "darwin-arm64": "darwin-arm64", "darwin-x64": "darwin-x64", "win32-x64": "win32-x64" }[key] ?? refuse(`no pinned actionlint for ${key}`);
}

/** The pin for one platform: { version, archive, sha256, url }. The URL is derived from the version and archive name only. */
export function pinFor(pins, key) {
  if (!/^\d+\.\d+\.\d+$/.test(pins?.version ?? "")) refuse("ci/actionlint.json has no pinned version");
  const entry = pins.platforms?.[key];
  if (!entry || typeof entry.archive !== "string" || !/^[0-9a-f]{64}$/.test(entry.sha256 ?? "")) refuse(`ci/actionlint.json has no complete entry for ${key}`);
  if (entry.archive !== `actionlint_${pins.version}_${{ "linux-x64": "linux_amd64.tar.gz", "darwin-arm64": "darwin_arm64.tar.gz", "darwin-x64": "darwin_amd64.tar.gz", "win32-x64": "windows_amd64.zip" }[key]}`) refuse(`ci/actionlint.json names an archive for ${key} that is not the pinned version's`);
  return { version: pins.version, archive: entry.archive, sha256: entry.sha256, url: `https://github.com/rhysd/actionlint/releases/download/v${pins.version}/${entry.archive}` };
}

/** Download, verify, unpack and version-check. Returns the directory holding the executable. */
export async function provision({ pins, key, dest, fetchImpl = fetch, run = spawnSync }) {
  const pin = pinFor(pins, key);
  const response = await fetchImpl(pin.url, { redirect: "follow", signal: AbortSignal.timeout(120_000) });
  if (!response.ok) refuse(`download of ${pin.url} returned ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  const digest = crypto.createHash("sha256").update(bytes).digest("hex");
  if (digest !== pin.sha256) refuse(`${pin.archive} sha256 is ${digest}, pinned ${pin.sha256}`);
  fs.mkdirSync(dest, { recursive: true });
  const archive = path.join(dest, pin.archive);
  fs.writeFileSync(archive, bytes);
  const exe = key.startsWith("win32") ? "actionlint.exe" : "actionlint";
  // bsdtar (macOS, Windows) and GNU tar (Linux) both read .tar.gz; bsdtar reads .zip.
  const unpacked = run("tar", ["-xf", archive, "-C", dest, exe], { encoding: "utf8" });
  if (unpacked.status !== 0) refuse(`could not unpack ${exe} from ${pin.archive}`);
  fs.rmSync(archive);
  const probe = run(path.join(dest, exe), ["-version"], { encoding: "utf8" });
  const reported = String(probe.stdout ?? "").split(/\r?\n/)[0].trim();
  if (probe.status !== 0 || reported !== pin.version) refuse(`${exe} reports "${reported}", pinned ${pin.version}`);
  return dest;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  try {
    const dest = path.join(process.env.RUNNER_TEMP ?? refuse("RUNNER_TEMP is required"), "actionlint-bin");
    const dir = await provision({ pins: JSON.parse(fs.readFileSync(PINS_FILE, "utf8")), key: platformKey(), dest });
    if (process.env.GITHUB_PATH) fs.appendFileSync(process.env.GITHUB_PATH, `${dir}\n`);
    process.stdout.write(`actionlint pinned at ${dir}\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
