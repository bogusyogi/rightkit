import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { classifyResponse, createRpcClient, openMcpSession } from "./ax/lib/mcp-stdio-client.mjs";
import { containsSecretShapedString, redactText } from "./lib/redact.mjs";

// A minimal fake child_process.ChildProcess: an EventEmitter with stdout
// (another EventEmitter) and a stdin.write() stub. Lets createRpcClient's
// error/exit/buffer-cap handling (F13) be tested deterministically and fast,
// with no real subprocess involved.
function fakeChild() {
  const child = new EventEmitter();
  child.stdout = new EventEmitter();
  child.stdout.setEncoding = () => {};
  child.stdin = { write: () => {} };
  return child;
}

test("F13: an 'error' event on the child (e.g. ENOENT) rejects pending requests instead of crashing", async () => {
  const child = fakeChild();
  const rpc = createRpcClient(child, { timeoutMs: 5000 });
  const pending = rpc.request("initialize", {});
  child.emit("error", new Error("spawn definitely-not-a-real-command ENOENT"));
  await assert.rejects(pending, /child process error/);
  // A request made after the fatal error must reject immediately too, not
  // hang for the full timeout.
  await assert.rejects(rpc.request("tools/list", {}), /child process error/);
});

test("F13: the child exiting before it responds fails fast, not after the full timeout", async () => {
  const child = fakeChild();
  const rpc = createRpcClient(child, { timeoutMs: 60000 });
  const pending = rpc.request("initialize", {});
  child.emit("exit", 1, null);
  await assert.rejects(pending, /exited \(code=1/);
});

test("F13: an oversized, newline-free stdout stream is rejected instead of buffered without bound", async () => {
  const child = fakeChild();
  const rpc = createRpcClient(child, { timeoutMs: 5000, maxBufferBytes: 1000 });
  const pending = rpc.request("initialize", {});
  child.stdout.emit("data", "x".repeat(2000)); // no newline — never resolves the pending request on its own
  await assert.rejects(pending, /exceeded 1000 bytes/);
});

test("F13: a clean request/response round trip still resolves normally (no regression)", async () => {
  const child = fakeChild();
  const sent = [];
  child.stdin.write = (line) => sent.push(line);
  const rpc = createRpcClient(child, { timeoutMs: 5000 });
  const pending = rpc.request("initialize", {});
  const request = JSON.parse(sent[0]);
  child.stdout.emit("data", `${JSON.stringify({ jsonrpc: "2.0", id: request.id, result: { ok: true } })}\n`);
  const response = await pending;
  assert.deepEqual(response.result, { ok: true });
});

test("F9: classifyResponse redacts a credential-shaped string from a typed-error sample", () => {
  const message = {
    result: {
      isError: true,
      content: [{ type: "text", text: JSON.stringify({ error: { code: "upstream_failed", message: "upstream call failed while using key sk-test-EXAMPLE123 for auth" } }) }],
    },
  };
  const classification = classifyResponse(message);
  assert.equal(classification.kind, "typed-error");
  assert.ok(!classification.message.includes("sk-test-EXAMPLE123"), classification.message);
  assert.ok(!classification.sample.includes("sk-test-EXAMPLE123"), classification.sample);
  assert.ok(classification.message.includes("[REDACTED]"));
});

test("F9: redactText preserves the key= prefix but redacts the value, and never crosses patterns into the wrong replacement (offset-as-prefix regression)", () => {
  // Regression for a real bug caught during implementation: the first
  // version of redactText used one callback `(match, prefix) => ...` for
  // EVERY pattern, but String.replace's callback receives (match, offset,
  // string) for patterns with NO capture group — so "prefix" was actually
  // the numeric match offset, and any match not at position 0 got a stray
  // number prepended (e.g. "12[REDACTED]" instead of "[REDACTED]").
  const text = "auth failed for sk-live-ABCDEFGHIJ; api_key: abcdefgh1234 was also present";
  const redacted = redactText(text);
  assert.ok(!redacted.includes("sk-live-ABCDEFGHIJ"), redacted);
  assert.ok(!/\d\[REDACTED\]/.test(redacted), redacted); // no stray offset digit before a redaction
  assert.match(redacted, /api_key:\s*\[REDACTED\]/);
  assert.equal(containsSecretShapedString(text), true);
  assert.equal(containsSecretShapedString("nothing sensitive here"), false);
});

test("F13: spawning a nonexistent command surfaces a typed rejection, not an unhandled crash", async () => {
  await assert.rejects(
    openMcpSession({ command: "definitely-not-a-real-right-ax-test-command", args: [], cwd: process.cwd(), timeoutMs: 2000 }),
    /child process error/,
  );
});

test("F13: a server that exits mid-handshake fails within a couple seconds, not the full timeout", async () => {
  const started = Date.now();
  await assert.rejects(
    openMcpSession({ command: process.execPath, args: ["-e", "process.exit(0)"], cwd: process.cwd(), timeoutMs: 30000 }),
    /exited \(code=0/,
  );
  assert.ok(Date.now() - started < 5000, `expected fast failure, took ${Date.now() - started}ms`);
});
