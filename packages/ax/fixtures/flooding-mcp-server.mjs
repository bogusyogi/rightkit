#!/usr/bin/env node
// Fixture MCP server used ONLY by this package's own tests (F13 regression:
// unsolicited-message retention must be bounded). Responds to initialize
// normally, then — the moment the client acks with notifications/initialized
// — floods a large number of unsolicited (no matching request id)
// notifications down stdout before ever answering tools/list. A client that
// retains every unsolicited message without a cap would grow without bound
// against a server shaped like this one.

import { createInterface } from "node:readline";

const FLOOD_COUNT = 5000;

const rl = createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let message;
  try { message = JSON.parse(trimmed); } catch { return; }
  if (message.method === "notifications/initialized") {
    for (let index = 0; index < FLOOD_COUNT; index += 1) {
      process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/flood", params: { index } })}\n`);
    }
    return;
  }
  const respond = (result) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result })}\n`);
  if (message.method === "initialize") return respond({ protocolVersion: "2025-03-26", serverInfo: { name: "right-ax-flood-fixture", version: "0.1.0" }, capabilities: { tools: {} } });
  if (message.method === "tools/list") return respond({ tools: [] });
  if (message.method === "resources/list") return respond({ resources: [] });
  if (message.method === "prompts/list") return respond({ prompts: [] });
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: `method not found: ${message.method}` } })}\n`);
});
