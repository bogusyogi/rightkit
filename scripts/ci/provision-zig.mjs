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
// cc-rs passes clang-style `--target=<rust triple>` (or `--target <triple>`), which zig's own target syntax does not accept: the wrapper drops
// every caller --target and keeps zig's `-target <zig triple>` (cargo-zigbuild does the same). One small node filter serves every tool.
export const FILTER = `import { spawnSync } from "node:child_process";
const [zig, ...fixed] = JSON.parse(process.argv[2]);
const rest = [];
for (let i = 3; i < process.argv.length; i += 1) {
  const a = process.argv[i];
  if (a === "--target") { i += 1; continue; }
  if (a.startsWith("--target=")) continue;
  rest.push(a);
}
// zig's caches live inside the build's own target folder (one build folder per app, GATE-CONTRACT GC-A15): its default global cache is a
// host folder outside the build, which the build-helper trace failed for every crate with C code (ring, rustls, blake3, libsqlite3-sys).
const env = { ...process.env };
const base = env.CARGO_TARGET_DIR || env.OUT_DIR;
if (base) {
  env.ZIG_GLOBAL_CACHE_DIR ??= base + "/zig-cache/global";
  env.ZIG_LOCAL_CACHE_DIR ??= base + "/zig-cache/local";
}
const r = spawnSync(zig, [...fixed, ...rest], { stdio: "inherit", env });
// Apple SDK stand-in. A cross row type-checks Rust for the other OS and never links (the same reason pkg-config-cross and windres-cross exist).
// C code for a macOS target needs Apple SDK headers (os/log.h, CommonCrypto, Security) that exist on a Mac only; zig carries libc headers, not the
// SDK. So when a C or C++ compile for a macOS target fails on a host that is not a Mac, the wrapper compiles an empty translation unit to the
// requested output instead: the build script gets its object, the archive is empty, and the Rust type-check of the crate goes on. The Mac
// compiles the same C for real. (HeardRight on the Dell, 2026-11-10: libsqlite3-sys's bundled SQLCipher, "os/log.h file not found".)
const macTarget = fixed.some((a, i) => fixed[i - 1] === "-target" && /-macos/.test(a));
// The compile (-c) and the preprocess (-E, which a compiler cache runs first) are both stood in: the same command with every source file replaced by an empty one.
const SOURCE = /\\.(c|cc|cpp|cxx|m|mm|s|S|i|ii)$/;
const sources = rest.filter((a) => !a.startsWith("-") && SOURCE.test(a));
const compiling = (fixed[0] === "cc" || fixed[0] === "c++") && (rest.includes("-c") || rest.includes("-E")) && sources.length > 0;
if ((r.status ?? 1) !== 0 && macTarget && compiling && process.platform !== "darwin") {
  const { writeFileSync, mkdtempSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join, extname } = await import("node:path");
  const dir = mkdtempSync(join(env.OUT_DIR || tmpdir(), "rightkit-cc-standin-"));
  const swapped = rest.map((a) => {
    if (a.startsWith("-") || !SOURCE.test(a)) return a;
    const empty = join(dir, "empty" + extname(a));
    writeFileSync(empty, extname(a) === ".s" || extname(a) === ".S" ? "" : "/* RightKit cross type-check stand-in: the real source needs Apple SDK headers this host does not have. */\\n");
    return empty;
  });
  const s = spawnSync(zig, [...fixed, ...swapped], { stdio: "inherit", env });
  if ((s.status ?? 1) === 0) { process.stderr.write("rightkit cross-cc: Apple SDK stand-in used for " + sources.join(", ") + " (C for a macOS target cannot be compiled on this host; the row type-checks Rust only)\\n"); process.exit(0); }
}
process.exit(r.status ?? 1);
`;

// Node by its real path: on the Dell C:\nvm4w\nodejs is a junction, and an elevated shell refuses to traverse it ("untrusted mount point"),
// so every cross C compile through a wrapper naming the junction failed.
const realNode = () => { try { return fs.realpathSync.native(process.execPath); } catch { return process.execPath; } };
export function wrappers(zig, platform = process.platform, { node = realNode(), dir } = {}) {
  const files = { "zig-filter.mjs": FILTER };
  for (const target of Object.values(CROSS_TARGETS)) {
    const zt = ZIG_TARGETS[target];
    const tools = { [`cc-${target}`]: ["cc", "-target", zt], [`cxx-${target}`]: ["c++", "-target", zt], [`ar-${target}`]: ["ar"] };
    for (const [name, args] of Object.entries(tools)) {
      const spec = JSON.stringify([zig, ...args]);
      files[platform === "win32" ? `${name}.cmd` : name] = platform === "win32"
        ? `@echo off\r\n"${node}" "%~dp0zig-filter.mjs" ${JSON.stringify(spec).replace(/%/g, "%%")} %*\r\n`
        : `#!/bin/sh\nexec "${node}" "$(dirname "$0")/zig-filter.mjs" '${spec.replace(/'/g, "'\\''")}' "$@"\n`;
    }
  }
  // pkg-config for a foreign target (cross-targets.mjs PKG_CONFIG_CROSS): cross rows type-check and never link, so it reports every
  // package present, with a high version for --modversion and no flags. Used only through PKG_CONFIG_<foreign target>.
  files[platform === "win32" ? "pkg-config-cross.cmd" : "pkg-config-cross"] = platform === "win32"
    ? `@echo off\r\nfor %%a in (%*) do if "%%~a"=="--modversion" echo 99.0.0\r\nexit /b 0\r\n`
    : `#!/bin/sh\n# Foreign-target pkg-config for type-check-only cross rows: every package present, no flags.\nfor a in "$@"; do [ "$a" = "--modversion" ] && echo 99.0.0; done\nexit 0\n`;
  // windres for the windows-gnu cross row off Windows (cross-targets.mjs WINDRES_CROSS): answers the -V probe as GNU windres and writes
  // an empty archive to --output; the row type-checks and never links.
  if (platform !== "win32") files["windres-cross"] = `#!/bin/sh\n# Foreign-target windres for type-check-only cross rows.\nout=""; prev=""\nfor a in "$@"; do\n  case "$a" in -V|--version) echo "GNU windres (RightKit cross type-check stub) 2.42"; exit 0;; --output=*) out="\${a#--output=}";; esac\n  [ "$prev" = "--output" ] && out="$a"\n  prev="$a"\ndone\n[ -n "$out" ] && printf '!<arch>\\n' > "$out"\nexit 0\n`;
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
  // Windows: the system's own bsdtar reads zip; Git Bash puts GNU tar first on PATH, which cannot read zip and treats D:\ as a remote host.
  const tar = platform === "win32" ? path.join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe") : "tar";
  const unpacked = run(tar, ["-xf", archive, "-C", dest], { encoding: "utf8" });
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
