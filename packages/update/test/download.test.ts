import { afterAll, beforeAll, describe, expect, it } from "vitest";
import http from "node:http";
import type { AddressInfo } from "node:net";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { download, extractCommand } from "../src/download.js";

const BODY = Buffer.from("x".repeat(100_000) + "tail");
let server: http.Server;
let base = "";

beforeAll(async () => {
  server = http.createServer((req, res) => {
    if (req.url === "/ok") {
      res.writeHead(200, { "content-length": String(BODY.length) });
      res.end(BODY);
    } else if (req.url === "/chunked") {
      res.writeHead(200);
      res.write(BODY.subarray(0, 50_000));
      setTimeout(() => res.end(BODY.subarray(50_000)), 20);
    } else if (req.url === "/empty") {
      res.writeHead(200, { "content-length": "0" });
      res.end();
    } else if (req.url === "/slow") {
      res.writeHead(200);
      res.write("a");
    } else {
      res.writeHead(404);
      res.end("nope");
    }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
});

afterAll(async () => {
  server.closeAllConnections();
  await new Promise<void>((resolve) => server.close(() => resolve()));
});

const tmp = () => fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), "right-update-dl-"));

describe("download", () => {
  it("streams to disk and returns the sha256 and size", async () => {
    const dest = path.join(tmp(), "out.bin");
    const result = await download(`${base}/ok`, dest);
    expect(result.size).toBe(BODY.length);
    expect(result.sha256).toBe(crypto.createHash("sha256").update(BODY).digest("hex"));
    expect(fs.readFileSync(dest).equals(BODY)).toBe(true);
    expect(fs.readdirSync(path.dirname(dest))).toEqual(["out.bin"]);
  });

  it("hashes a chunked body without a content-length, and can discard it", async () => {
    const result = await download(`${base}/chunked`, null);
    expect(result.sha256).toBe(crypto.createHash("sha256").update(BODY).digest("hex"));
  });

  it("enforces the byte bound for declared and undeclared sizes and leaves no partial file", async () => {
    const dir = tmp();
    await expect(download(`${base}/ok`, path.join(dir, "a"), { maxBytes: 1000 })).rejects.toThrow(/declares 100004 bytes/);
    await expect(download(`${base}/chunked`, path.join(dir, "b"), { maxBytes: 1000 })).rejects.toThrow(/exceeded the 1000 byte bound/);
    expect(fs.readdirSync(dir)).toEqual([]);
  });

  it("rejects HTTP errors, empty bodies and timeouts", async () => {
    const dir = tmp();
    await expect(download(`${base}/missing`, path.join(dir, "a"))).rejects.toThrow(/HTTP 404/);
    await expect(download(`${base}/empty`, path.join(dir, "b"))).rejects.toThrow(/empty body/);
    await expect(download(`${base}/slow`, path.join(dir, "c"), { timeoutMs: 200 })).rejects.toThrow(/timed out/);
    expect(fs.readdirSync(dir)).toEqual([]);
  });

  it("refuses non-https URLs other than loopback, and redirects that leave https", async () => {
    await expect(download("http://example.com/x", null)).rejects.toThrow(/non-https/);
    const redirected = async () => Object.defineProperty(new Response("data"), "url", { value: "http://evil.example/x" });
    await expect(download("https://example.com/x", null, { fetch: redirected as never })).rejects.toThrow(/redirected to a non-https URL/);
  });
});

describe("extractCommand", () => {
  it("picks tar for tar formats, tar or unzip for zip, and nothing for unknown formats", () => {
    expect(extractCommand("/s/a.tar.gz", "/d", "bin/x", "darwin")).toEqual(["tar", "-xf", "/s/a.tar.gz", "-C", "/d", "--", "bin/x"]);
    expect(extractCommand("/s/a.tar.gz", "/d", "bin/x", "linux")).toEqual(["tar", "-xf", "/s/a.tar.gz", "-C", "/d", "--", "bin/x"]);
    // Windows: the system bsdtar by full path, never whichever `tar` is first on PATH.
    const windowsTar = extractCommand("C:\\s\\a.zip", "C:\\d", "x", "win32");
    expect(windowsTar?.[0]).toMatch(/\\System32\\tar\.exe$/);
    expect(windowsTar?.slice(1)).toEqual(["-xf", "C:\\s\\a.zip", "-C", "C:\\d", "--", "x"]);
    expect(extractCommand("/s/a.zip", "/d", "x", "darwin")).toEqual(["tar", "-xf", "/s/a.zip", "-C", "/d", "--", "x"]);
    expect(extractCommand("/s/a.zip", "/d", "x", "linux")).toEqual(["unzip", "-o", "-q", "/s/a.zip", "x", "-d", "/d"]);
    expect(extractCommand("/s/a.exe", "/d", "x")).toBeNull();
  });
});
