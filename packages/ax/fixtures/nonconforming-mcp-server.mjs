#!/usr/bin/env node
// Fixture MCP server used ONLY by this package's own tests. Deliberately
// non-conforming in the exact two shapes F3's QA repro found the old
// evaluateProbe() logic missed:
//   reject_all_tool     — additionalProperties:false, one required field,
//                         but rejects EVERY call (including one built to
//                         satisfy its own required-argument schema) with a
//                         typed {code,retryable,remediation} error. A tool
//                         this broken must fail conformance, not pass it.
//   lax_enforcement_tool — additionalProperties:false (so it promises to
//                         reject unknown fields) but actually accepts a call
//                         carrying an unknown field. Declaring a schema and
//                         then not enforcing it must fail conformance.
//   credential_leak_tool — always errors with a credential-shaped string
//                         (sk-test-EXAMPLE123) embedded in the error message,
//                         reproducing F9's exact repro shape: a report must
//                         never copy that string verbatim into its output.

import { createInterface } from "node:readline";

const TOOLS = [
  {
    name: "reject_all_tool",
    description: "Use this when you need a tool that rejects everything. Do not use it for real work.",
    inputSchema: { type: "object", additionalProperties: false, required: ["input"], properties: { input: { type: "string" } } },
  },
  {
    name: "lax_enforcement_tool",
    description: "Use this when you need a schema that lies. Do not use it for real work.",
    inputSchema: { type: "object", additionalProperties: false, required: ["input"], properties: { input: { type: "string" } } },
  },
  {
    name: "credential_leak_tool",
    description: "Use this when you need a tool that leaks a credential. Do not use it for real work.",
    inputSchema: { type: "object", additionalProperties: false, required: ["input"], properties: { input: { type: "string" } } },
  },
];

function typedErrorEnvelope(code, message) {
  return { content: [{ type: "text", text: JSON.stringify({ error: { code, message, retryable: false, remediation: "n/a — this fixture always rejects" } }) }], isError: true };
}

function successEnvelope(structuredContent) {
  return { content: [{ type: "text", text: JSON.stringify(structuredContent) }], structuredContent };
}

function handleToolCall(name, args) {
  if (name === "reject_all_tool") return typedErrorEnvelope("always_rejects", "this fixture rejects every call, including schema-valid ones");
  if (name === "lax_enforcement_tool") return successEnvelope({ status: "ok", received: args ?? {} }); // accepts unknown fields despite additionalProperties:false
  if (name === "credential_leak_tool") return typedErrorEnvelope("upstream_failed", "upstream call failed while using key sk-test-EXAMPLE123 for auth");
  return typedErrorEnvelope("unknown_tool", `no such tool: ${name}`);
}

const rl = createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let message;
  try { message = JSON.parse(trimmed); } catch { return; }
  if (message.method === "notifications/initialized") return;
  const respond = (result) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result })}\n`);
  if (message.method === "initialize") return respond({ protocolVersion: "2025-03-26", serverInfo: { name: "right-ax-nonconforming-fixture", version: "0.1.0" }, capabilities: { tools: {} } });
  if (message.method === "tools/list") return respond({ tools: TOOLS });
  if (message.method === "resources/list") return respond({ resources: [] });
  if (message.method === "prompts/list") return respond({ prompts: [] });
  if (message.method === "tools/call") return respond(handleToolCall(message.params?.name, message.params?.arguments));
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: `method not found: ${message.method}` } })}\n`);
});
