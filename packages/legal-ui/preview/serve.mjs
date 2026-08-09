#!/usr/bin/env node
// Tiny static file server for the legal-ui preview harness. No npx/http-server
// dependency (avoids an ENOENT PATH issue with npx in some shells) — plain
// Node http + fs. Local-only, loopback default.
import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL(".", import.meta.url));
const port = Number(process.env.PORT ?? 4310);
const host = "127.0.0.1";

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
};
const axeReports = new Map();

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url ?? "/", `http://${host}`);
    if (url.pathname === "/__axe-report") {
      const key = `${url.searchParams.get("id") ?? ""}/${url.searchParams.get("state") ?? ""}`;
      if (req.method === "POST") {
        let body = "";
        for await (const chunk of req) body += chunk;
        const report = JSON.parse(body);
        if (!Array.isArray(report.violations)) throw new Error("invalid axe report");
        axeReports.set(key, report);
        res.writeHead(204).end();
        return;
      }
      const report = axeReports.get(key);
      if (!report) {
        res.writeHead(404).end("report not found");
        return;
      }
      res.writeHead(200, { "content-type": "application/json; charset=utf-8" }).end(JSON.stringify(report));
      return;
    }
    let pathname = decodeURIComponent(url.pathname);
    if (pathname === "/") pathname = "/index.html";
    const filePath = normalize(join(root, pathname));
    if (!filePath.startsWith(normalize(root))) {
      res.writeHead(403).end("forbidden");
      return;
    }
    const info = await stat(filePath);
    const target = info.isDirectory() ? join(filePath, "index.html") : filePath;
    const body = await readFile(target);
    res.writeHead(200, { "content-type": MIME[extname(target)] ?? "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
});

server.listen(port, host, () => {
  console.log(`legal-ui preview listening on http://${host}:${port}`);
});
