// Bounded streaming download with a running sha256, and single-member archive extraction.
// Downloaded bytes are data: they are hashed, size-checked and staged, never executed here.

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { USER_AGENT, type FetchLike } from "./http.js";
import type { RunFn } from "./exec.js";

export const DEFAULT_MAX_BYTES = 512 * 1024 * 1024;
export const DOWNLOAD_TIMEOUT_MS = 15 * 60 * 1000;

export interface DownloadOptions {
  fetch?: FetchLike;
  maxBytes?: number;
  timeoutMs?: number;
}

export interface DownloadResult {
  sha256: string;
  size: number;
  url: string;
}

function writeChunk(stream: fs.WriteStream, chunk: Uint8Array): Promise<void> {
  return new Promise((resolve, reject) => {
    stream.write(chunk, (error) => (error ? reject(error) : resolve()));
  });
}

/**
 * GET `url` and stream it to `dest` (or discard it when `dest` is null), hashing as it goes.
 * Enforces https (including after redirects), a byte bound and a timeout. On any failure the
 * partial file is removed and nothing is left at `dest`.
 */
export async function download(url: string, dest: string | null, options: DownloadOptions = {}): Promise<DownloadResult> {
  const parsed = new URL(url);
  if (parsed.protocol !== "https:" && !isLoopbackHttp(parsed)) throw new Error(`refusing non-https download URL: ${url}`);
  const doFetch = options.fetch ?? (globalThis.fetch as unknown as FetchLike);
  const maxBytes = options.maxBytes ?? DEFAULT_MAX_BYTES;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs ?? DOWNLOAD_TIMEOUT_MS);
  const partial = dest ? `${dest}.partial-${process.pid}-${crypto.randomBytes(4).toString("hex")}` : null;
  let stream: fs.WriteStream | null = null;
  try {
    const response = await doFetch(url, {
      headers: { "user-agent": USER_AGENT, accept: "application/octet-stream, */*" },
      signal: controller.signal,
    });
    if (!response.ok) throw new Error(`GET ${url} returned HTTP ${response.status}`);
    if (response.url) {
      const finalUrl = new URL(response.url);
      if (finalUrl.protocol !== "https:" && !isLoopbackHttp(finalUrl)) throw new Error(`download redirected to a non-https URL: ${response.url}`);
    }
    const declared = Number(response.headers.get("content-length") ?? "0");
    if (declared > maxBytes) throw new Error(`GET ${url} declares ${declared} bytes, over the ${maxBytes} byte bound`);
    if (!response.body) throw new Error(`GET ${url} returned no body`);
    if (dest && partial) {
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      stream = fs.createWriteStream(partial, { flags: "wx", mode: 0o644 });
      // Failures surface through the write calls; this keeps a late open error from becoming uncaught.
      stream.on("error", () => undefined);
    }
    const hash = crypto.createHash("sha256");
    const reader = response.body.getReader();
    let size = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > maxBytes) {
        await reader.cancel();
        throw new Error(`GET ${url} exceeded the ${maxBytes} byte bound`);
      }
      hash.update(value);
      if (stream) await writeChunk(stream, value);
    }
    if (size === 0) throw new Error(`GET ${url} returned an empty body`);
    if (stream) {
      await new Promise<void>((resolve, reject) => {
        stream!.end((error?: Error | null) => (error ? reject(error) : resolve()));
      });
      stream = null;
      fs.renameSync(partial!, dest!);
    }
    return { sha256: hash.digest("hex"), size, url };
  } catch (error) {
    if ((error as Error).name === "AbortError") throw new Error(`GET ${url} timed out`);
    throw error;
  } finally {
    clearTimeout(timer);
    if (stream) {
      const open = stream;
      await new Promise<void>((resolve) => {
        open.once("close", () => resolve());
        open.destroy();
      });
    }
    if (partial) fs.rmSync(partial, { force: true });
  }
}

/** Plain http is accepted only for loopback, which tests and local mirrors use. */
function isLoopbackHttp(url: URL): boolean {
  return url.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname);
}

/** Pick the extractor command for an archive, or null when the format is unsupported. */
export function extractCommand(archive: string, dir: string, member: string, platform: NodeJS.Platform = process.platform): string[] | null {
  const lower = archive.toLowerCase();
  if (lower.endsWith(".zip")) {
    // bsdtar (macOS, Windows) reads zip; on Linux use unzip.
    return platform === "linux" ? ["unzip", "-o", "-q", archive, member, "-d", dir] : ["tar", "-xf", archive, "-C", dir, "--", member];
  }
  if (/\.(tar|tar\.gz|tgz|tar\.xz|txz|tar\.bz2|tbz2)$/.test(lower)) return ["tar", "-xf", archive, "-C", dir, "--", member];
  return null;
}

/** Extract exactly one named member into `dir`, then check it is a regular file inside `dir`. Returns its path. */
export async function extractMember(archive: string, dir: string, member: string, run: RunFn): Promise<string> {
  const argv = extractCommand(archive, dir, member);
  if (!argv) throw new Error(`unsupported archive format: ${path.basename(archive)}`);
  const result = await run(argv, { cwd: dir, timeoutMs: 5 * 60 * 1000 });
  if (result.code !== 0) throw new Error(`extracting ${member} failed: ${result.error ?? (result.stderr.trim() || `exit ${result.code}`)}`);
  const extracted = path.join(dir, ...member.split(/[\\/]/));
  const stat = fs.lstatSync(extracted);
  if (!stat.isFile()) throw new Error(`extracted member ${member} is not a regular file`);
  const realDir = fs.realpathSync(dir);
  const realFile = fs.realpathSync(extracted);
  if (!realFile.startsWith(realDir + path.sep)) throw new Error(`extracted member ${member} resolved outside the staging directory`);
  return extracted;
}
