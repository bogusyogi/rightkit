#!/usr/bin/env node
// Minimal hand-rolled MCP JSON-RPC-over-stdio server used ONLY by this
// package's own tests (never spawned in a product repo). Exposes two tools
// deliberately shaped to exercise both the pass and fail paths of the static
// checklist and the conformance probes:
//   good_tool  — when-to-use/when-not description, strict input schema,
//                claimBoundary+outcome+truncation output schema, readOnlyHint
//                annotation, rejects missing-required/unknown-field with a
//                typed {code,retryable,remediation} error envelope.
//   bad_tool   — plain description, additionalProperties:true input schema,
//                no outputSchema, no annotations, accepts anything (so the
//                missing-required and unknown-field probes both fail).

import { createInterface } from "node:readline";

const TOOLS = [
  {
    name: "good_tool",
    description: "Use this when you need a well-formed conformance fixture. Do not use it for anything else.",
    inputSchema: { type: "object", additionalProperties: false, required: ["input"], properties: { input: { type: "string" } } },
    outputSchema: {
      type: "object",
      properties: {
        status: { type: "string" },
        truncated: { type: "boolean" },
        continuationCursor: { type: ["string", "null"] },
        claimBoundary: { type: "object", properties: { status: { type: "string" }, cleanClaimAllowed: { type: "boolean" }, safeClaims: { type: "array" }, prohibitedClaims: { type: "array" }, gaps: { type: "array" } } },
      },
    },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true },
  },
  {
    name: "bad_tool",
    description: "Does something.",
    inputSchema: { type: "object", additionalProperties: true, required: ["value"], properties: { value: { type: "string" } } },
  },
];

function typedErrorEnvelope(code, message) {
  return { content: [{ type: "text", text: JSON.stringify({ error: { code, message, retryable: false, remediation: "retry with a valid argument shape" } }) }], isError: true };
}

function successEnvelope(structuredContent) {
  return { content: [{ type: "text", text: JSON.stringify(structuredContent) }], structuredContent };
}

function handleToolCall(name, args) {
  const tool = TOOLS.find((entry) => entry.name === name);
  if (!tool) return typedErrorEnvelope("unknown_tool", `no such tool: ${name}`);
  if (name === "good_tool") {
    const known = new Set(Object.keys(tool.inputSchema.properties));
    const unknown = Object.keys(args ?? {}).filter((key) => !known.has(key));
    if (unknown.length) return typedErrorEnvelope("unknown_field", `unexpected field(s): ${unknown.join(", ")}`);
    if (args?.input === undefined) return typedErrorEnvelope("missing_required", "missing required field: input");
    return successEnvelope({ status: "ok", truncated: false, continuationCursor: null, claimBoundary: { status: "clear", cleanClaimAllowed: true, safeClaims: ["fixture result is current"], prohibitedClaims: [], gaps: [] } });
  }
  // bad_tool: accepts anything, including missing required fields and
  // unknown fields, and returns no structuredContent — deliberately fails
  // the missing-required probe and the outputSchema-presence rule.
  return { content: [{ type: "text", text: JSON.stringify({ received: args ?? {} }) }] };
}

const rl = createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let message;
  try { message = JSON.parse(trimmed); } catch { return; }
  if (message.method === "notifications/initialized") return;
  const respond = (result) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result })}\n`);
  if (message.method === "initialize") return respond({ protocolVersion: "2025-03-26", serverInfo: { name: "right-ax-fixture", version: "0.1.0" }, capabilities: { tools: {} } });
  if (message.method === "tools/list") return respond({ tools: TOOLS });
  if (message.method === "resources/list") return respond({ resources: [] });
  if (message.method === "prompts/list") return respond({ prompts: [] });
  if (message.method === "tools/call") return respond(handleToolCall(message.params?.name, message.params?.arguments));
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: `method not found: ${message.method}` } })}\n`);
});
