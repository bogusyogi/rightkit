#!/usr/bin/env node
// Fixture MCP server used ONLY by this package's own tests. Initializes
// successfully; tools/list behavior is picked by argv[2]:
//   "error" (default) — returns a JSON-RPC error (F4's repro: a real
//                        upstream failure, not a missing method).
//   "empty"            — returns a legitimate empty tools array (F4's other
//                        repro shape: no error, but nothing to probe).

import { createInterface } from "node:readline";

const mode = process.argv[2] === "empty" ? "empty" : "error";

const rl = createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let message;
  try { message = JSON.parse(trimmed); } catch { return; }
  if (message.method === "notifications/initialized") return;
  const respond = (result) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result })}\n`);
  const respondError = (error) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, error })}\n`);
  if (message.method === "initialize") return respond({ protocolVersion: "2025-03-26", serverInfo: { name: "right-ax-tools-list-error-fixture", version: "0.1.0" }, capabilities: { tools: {} } });
  if (message.method === "tools/list") return mode === "empty" ? respond({ tools: [] }) : respondError({ code: -32000, message: "upstream tool registry unavailable" });
  if (message.method === "resources/list") return respond({ resources: [] });
  if (message.method === "prompts/list") return respond({ prompts: [] });
  return respondError({ code: -32601, message: `method not found: ${message.method}` });
});
