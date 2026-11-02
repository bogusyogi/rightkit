#!/usr/bin/env node
// Provision ci/zig.json's pinned zig and write the C, C++ and archiver wrappers the gate's cross-target clippy steps name
// (packages/release/local-publish.mjs crossCompileEnv): `cc-<rust target>`, `cxx-<rust target>`, `ar-<rust target>` for each
// of CROSS_TARGETS. The archive is refused unless its sha256 is the pinned one and `zig version` reports the pinned version.
// The wrapper directory is exported as RIGHTKIT_CROSS_CC_DIR (GITHUB_ENV on Actions; printed for a local host's profile).
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { CROSS_CC_ENV, CROSS_TARGETS, ZIG_TARGETS } from "../../packages/release/cross-targets.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const PINS_FILE = path.join(ROOT, "ci", "zig.json");

const refuse = (message) => { throw new Error(`provision-zig refused: ${message}`); };

export function platformKey(platform = process.platform, arch = process.arch) {
  const key = `${platform}-${arch}`;
  return { "linux-x64": "linux-x64", "darwin-arm64": "darwin-arm64", "darwin-x64": "darwin-x64", "win32-x64": "win32-x64" }[key] ?? refuse(`no pinned zig for ${key}`);
}

const ARCHIVE_STEM = { "linux-x64": "x86_64-linux", "darwin-arm64": "aarch64-macos", "darwin-x64": "x86_64-macos", "win32-x64": "x86_64-windows" };

/** The pin for one platform: { version, archive, sha256, url, dir }. The URL and directory derive from the version and key only. */
export function pinFor(pins, key) {
  if (!/^\d+\.\d+\.\d+$/.test(pins?.version ?? "")) refuse("ci/zig.json has no pinned version");
  const entry = pins.platforms?.[key];
  if (!entry || typeof entry.archive !== "string" || !/^[0-9a-f]{64}$/.test(entry.sha256 ?? "")) refuse(`ci/zig.json has no complete entry for ${key}`);
  const dir = `zig-${ARCHIVE_STEM[key]}-${pins.version}`;
  if (entry.archive !== `${dir}${key.startsWith("win32") ? ".zip" : ".tar.xz"}`) refuse(`ci/zig.json names an archive for ${key} that is not the pinned version's`);
  return { version: pins.version, archive: entry.archive, sha256: entry.sha256, url: `https://ziglang.org/download/${pins.version}/${entry.archive}`, dir };
}

/** The wrapper scripts for one host: name -> file body. Each forwards to `zig cc|c++|ar` for the zig target of its Rust target. */
export function wrappers(zig, platform = process.platform) {
  const files = {};
  for (const target of Object.values(CROSS_TARGETS)) {
    const zt = ZIG_TARGETS[target];
    const tools = { [`cc-${target}`]: ["cc", "-target", zt], [`cxx-${target}`]: ["c++", "-target", zt], [`ar-${target}`]: ["ar"] };
    for (const [name, args] of Object.entries(tools)) {
      files[platform === "win32" ? `${name}.cmd` : name] = platform === "win32"
        ? `@echo off\r\n"${zig}" ${args.join(" ")} %*\r\n`
        : `#!/bin/sh\nexec "${zig}" ${args.join(" ")} "$@"\n`;
    }
  }
  return files;
}

/** Download, verify, unpack, version-check and write the wrappers. Returns the wrapper directory. */
export async function provision({ pins, key, dest, fetchImpl = fetch, run = spawnSync, platform = process.platform }) {
  const pin = pinFor(pins, key);
  const response = await fetchImpl(pin.url, { redirect: "follow", signal: AbortSignal.timeout(300_000) });
  if (!response.ok) refuse(`download of ${pin.url} returned ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  const digest = crypto.createHash("sha256").update(bytes).digest("hex");
  if (digest !== pin.sha256) refuse(`${pin.archive} sha256 is ${digest}, pinned ${pin.sha256}`);
  fs.mkdirSync(dest, { recursive: true });
  const archive = path.join(dest, pin.archive);
  fs.writeFileSync(archive, bytes);
  const unpacked = run("tar", ["-xf", archive, "-C", dest], { encoding: "utf8" });
  if (unpacked.status !== 0) refuse(`could not unpack ${pin.archive}`);
  fs.rmSync(archive);
  const zig = path.join(dest, pin.dir, platform === "win32" ? "zig.exe" : "zig");
  const probe = run(zig, ["version"], { encoding: "utf8" });
  const reported = String(probe.stdout ?? "").trim();
  if (probe.status !== 0 || reported !== pin.version) refuse(`zig reports "${reported}", pinned ${pin.version}`);
  const bin = path.join(dest, "cross-cc");
  fs.mkdirSync(bin, { recursive: true });
  for (const [name, body] of Object.entries(wrappers(zig, platform))) fs.writeFileSync(path.join(bin, name), body, { mode: 0o755 });
  return bin;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  try {
    const dest = process.argv[2] ? path.resolve(process.argv[2]) : path.join(process.env.RUNNER_TEMP ?? refuse("RUNNER_TEMP or a destination argument is required"), "zig");
    const dir = await provision({ pins: JSON.parse(fs.readFileSync(PINS_FILE, "utf8")), key: platformKey(), dest });
    if (process.env.GITHUB_ENV) fs.appendFileSync(process.env.GITHUB_ENV, `${CROSS_CC_ENV}=${dir}\n`);
    process.stdout.write(`${CROSS_CC_ENV}=${dir}\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
