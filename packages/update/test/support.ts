import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { FetchLike } from "../src/http.js";

export const SHA = `sha256:${"a".repeat(64)}`;

export function mkRepo(files: Record<string, string | object>): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "right-update-"));
  for (const [rel, content] of Object.entries(files)) {
    const file = path.join(dir, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, typeof content === "string" ? content : JSON.stringify(content, null, 2));
  }
  return dir;
}

export function validToolSource(overrides: Record<string, unknown> = {}) {
  return {
    kind: "tool",
    name: "yt-dlp",
    pin: "2025.01.12",
    assets: {
      "macos-arm64": { asset: "yt-dlp_macos", checksum: SHA },
      "windows-x86_64": { asset: "yt-dlp.exe", checksum: SHA },
    },
    upstream: { type: "github-release", repo: "yt-dlp/yt-dlp" },
    journey: { command: ["node", "scripts/x.mjs", "--resource-dir", "{candidate}", "--out", "{out}"], report: "{out}/report.json" },
    ...overrides,
  };
}

export function manifest(sources: unknown[], policy: Record<string, unknown> = { default: "auto-land" }) {
  return { schemaVersion: 1, sources, policy };
}

type Route = string | object | Response | ((url: string) => string | object | Response);

/** Fetch double. Unknown URLs return 404. Every requested URL is recorded. */
export function mockFetch(routes: Record<string, Route>): { fetch: FetchLike; calls: string[] } {
  const calls: string[] = [];
  const fetch: FetchLike = async (url) => {
    calls.push(url);
    const route = routes[url];
    if (route === undefined) return new Response("not found", { status: 404 });
    const value = typeof route === "function" ? route(url) : route;
    if (value instanceof Response) return value;
    if (typeof value === "string") return new Response(value, { status: 200 });
    return new Response(JSON.stringify(value), { status: 200, headers: { "content-type": "application/json" } });
  };
  return { fetch, calls };
}
