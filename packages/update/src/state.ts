// Repository state files, hashing, host platform and artifact delivery paths.

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { Platform, ToolSource } from "./types.js";
import { PLATFORMS } from "./types.js";

export const STATE_DIR = ".rightkit/update";
export const APPLY_REPORT = `${STATE_DIR}/apply-report.json`;
export const GATE_REPORT = `${STATE_DIR}/gate-report.json`;
export const HELD_REPORT = `${STATE_DIR}/held.json`;
export const CANDIDATES_DIR = `${STATE_DIR}/candidates`;
export const GATE_RUNS_DIR = `${STATE_DIR}/gate`;

export function stateFile(repo: string, rel: string): string {
  return path.join(repo, ...rel.split("/"));
}

/** Create the state dir and make it ignore itself, so state never shows up as repository changes. */
export function ensureStateDir(repo: string): string {
  const dir = stateFile(repo, STATE_DIR);
  fs.mkdirSync(dir, { recursive: true });
  const ignore = path.join(dir, ".gitignore");
  if (!fs.existsSync(ignore)) fs.writeFileSync(ignore, "*\n");
  return dir;
}

export function sha256Bytes(bytes: Uint8Array | string): string {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

/** sha256 of a file, or null when it does not exist. */
export function sha256File(file: string): string | null {
  let fd: number;
  try {
    fd = fs.openSync(file, "r");
  } catch {
    return null;
  }
  try {
    const hash = crypto.createHash("sha256");
    const buffer = Buffer.alloc(1024 * 1024);
    for (;;) {
      const n = fs.readSync(fd, buffer, 0, buffer.length, null);
      if (n === 0) break;
      hash.update(buffer.subarray(0, n));
    }
    return hash.digest("hex");
  } finally {
    fs.closeSync(fd);
  }
}

/** Write a file atomically (temp file in the same directory, then rename). */
export function writeFileAtomic(file: string, content: string | Uint8Array): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const temp = `${file}.tmp-${process.pid}-${crypto.randomBytes(4).toString("hex")}`;
  fs.writeFileSync(temp, content);
  fs.renameSync(temp, file);
}

export function writeJsonAtomic(file: string, value: unknown): void {
  writeFileAtomic(file, `${JSON.stringify(value, null, 2)}\n`);
}

export function readJson<T>(file: string): { ok: true; value: T; raw: string } | { ok: false; error: string } {
  let raw: string;
  try {
    raw = fs.readFileSync(file, "utf8");
  } catch (error) {
    return { ok: false, error: `cannot read ${file}: ${(error as Error).message}` };
  }
  try {
    return { ok: true, value: JSON.parse(raw) as T, raw };
  } catch (error) {
    return { ok: false, error: `${file} is not valid JSON: ${(error as Error).message}` };
  }
}

/** RightKit platform id for this host, or null when it is not one of the five. */
export function hostPlatform(platform: NodeJS.Platform = process.platform, arch: string = process.arch): Platform | null {
  const key = `${platform}-${arch}`;
  const map: Record<string, Platform> = {
    "darwin-arm64": "macos-arm64",
    "darwin-x64": "macos-x86_64",
    "win32-x64": "windows-x86_64",
    "win32-arm64": "windows-arm64",
    "linux-x64": "linux-x86_64",
  };
  const found = map[key];
  return found && (PLATFORMS as readonly string[]).includes(found) ? found : null;
}

export interface HostEnv {
  platform?: NodeJS.Platform;
  env?: NodeJS.ProcessEnv;
  home?: string;
}

/** Root of the RightKit artifact directory: `$RIGHTKIT_ARTIFACT_DIR`, else the per-OS default. */
export function artifactRoot(host: HostEnv = {}): string {
  const platform = host.platform ?? process.platform;
  const env = host.env ?? process.env;
  const home = host.home ?? os.homedir();
  if (env.RIGHTKIT_ARTIFACT_DIR) return path.resolve(env.RIGHTKIT_ARTIFACT_DIR);
  if (platform === "win32") {
    const base = env.LOCALAPPDATA ?? path.join(home, "AppData", "Local");
    return path.win32.join(base, "RightKit", "artifacts");
  }
  if (platform === "darwin") return path.posix.join(home, "Library", "Application Support", "RightKit", "artifacts");
  const data = env.XDG_DATA_HOME ?? path.posix.join(home, ".local", "share");
  return path.posix.join(data, "RightKit", "artifacts");
}

/** Delivery directory for one tool version: `<artifact root>/<name>/<version>`. */
export function deliveryDir(name: string, version: string, host: HostEnv = {}): string {
  const root = artifactRoot(host);
  const join = (host.platform ?? process.platform) === "win32" ? path.win32.join : path.join;
  return join(root, name, version);
}

/** Version string as used in directories: the raw tag without a leading `v`. */
export function toolVersionDir(latest: string): string {
  return latest.replace(/^v/, "");
}

export function candidateDir(repo: string, tool: Pick<ToolSource, "name">, version: string): string {
  return path.join(repo, ...CANDIDATES_DIR.split("/"), tool.name, version);
}

/** Short stable id for a repository path, used in schedule labels and log names. */
export function repoSlug(repo: string): { slug: string; base: string } {
  const resolved = path.resolve(repo);
  const base = path.basename(resolved).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "repo";
  const hash = sha256Bytes(resolved).slice(0, 8);
  return { slug: `${base.slice(0, 40)}-${hash}`, base };
}

export function nowIso(): string {
  return new Date().toISOString();
}
