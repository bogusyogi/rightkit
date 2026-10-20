import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { classifyResponse, createRpcClient, openMcpSession } from "./ax/lib/mcp-stdio-client.mjs";
import { containsSecretShapedString, redactText } from "./lib/redact.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));

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

test("SDK Input validation error messages are stable invalid-arguments typed rejections", () => {
  const classification = classifyResponse({ result: { isError: true, content: [{ type: "text", text: "Input validation error: Invalid arguments for tool strict_tool: data must NOT have additional properties" }] } });
  assert.equal(classification.kind, "typed-error");
  assert.equal(classification.code, "invalid_arguments");
  assert.equal(classification.retryable, false);
  assert.match(classification.message, /strict_tool/);
});

test("arbitrary tool error text remains untyped", () => {
  const classification = classifyResponse({ result: { isError: true, content: [{ type: "text", text: "Input validation error: unexpected custom implementation failure" }] } });
  assert.equal(classification.kind, "untyped-error");
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

// F9 (round 2) regression: redactText must catch the FAMILY of common
// credential shapes, not only the one `sk-` example from the original QA
// repro. Each shape below is asserted redacted; ordinary text (including a
// harmless `key=value`, which uses neither a recognized label word nor a
// recognized value shape) must survive untouched.
test("F9: redactText redacts each known credential shape and leaves ordinary text alone", () => {
  const shapes = {
    openai: "sk-test-EXAMPLE1234567890",
    anthropic: "sk-ant-api03-EXAMPLE1234567890-ABCDEFGHIJ",
    githubClassicPat: "ghp_" + "123456789012345678901234567890",
    githubOauth: "gho_" + "123456789012345678901234567890",
    githubServer: "ghs_" + "123456789012345678901234567890",
    githubFineGrainedPat: "github_pat_11AA0_exampleTOKEN1234567890",
    slackBot: "xoxb-123456789012-123456789012-abcdefghijklmnop",
    slackApp: "xoxp-123456789012-123456789012-abcdefghijklmnop",
    aws: "AKIA" + "1234567890ABCDEF",
    google: "AIzaSyABCDEFGHIJKLMNOPQRSTUVWXYZ01234",
    bearer: "Bearer abcdefghij1234567890",
    jwt: "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
    genericKeyValue: "api_key: abcdefgh1234",
    pem: "-----BEGIN " + "PRIVATE KEY-----\nMIIBogIBAAJBAMY=\n-----END PRIVATE KEY-----",
    pemRsa: "-----BEGIN RSA " + "PRIVATE KEY-----\nMIIBogIBAAJBAMY=\n-----END RSA PRIVATE KEY-----",
  };
  for (const [shapeName, secret] of Object.entries(shapes)) {
    const text = `context before ${secret} context after`;
    const redacted = redactText(text);
    assert.ok(!redacted.includes(secret), `${shapeName}: expected redaction, got ${JSON.stringify(redacted)}`);
    assert.ok(redacted.includes("[REDACTED]"), `${shapeName}: expected [REDACTED] marker, got ${JSON.stringify(redacted)}`);
    assert.equal(containsSecretShapedString(text), true, `${shapeName}: containsSecretShapedString should detect it`);
  }

  const harmlessTexts = ["nothing sensitive here", "key=value", "the word token appears with no value attached"];
  for (const text of harmlessTexts) {
    assert.equal(redactText(text), text, `expected no change to ${JSON.stringify(text)}`);
    assert.equal(containsSecretShapedString(text), false, `expected no detection in ${JSON.stringify(text)}`);
  }
});

// F9 (round 3) regression: the bearer pattern previously required >=10 token
// characters, so a literal short bearer like "Bearer x" survived untouched —
// the property is "a Bearer scheme with SOME credential material is
// redacted", not "only long tokens count". Both a short and a long bearer
// token are redacted; ordinary text and a harmless key=value stay untouched.
test("F9: both a short and a long Bearer token are redacted; ordinary text and key=value are preserved", () => {
  const shortBearer = "Authorization header: Bearer x";
  const longBearer = "Authorization header: Bearer abcdefghij1234567890";
  const shortRedacted = redactText(shortBearer);
  const longRedacted = redactText(longBearer);
  assert.ok(!shortRedacted.includes("Bearer x"), shortRedacted);
  assert.ok(shortRedacted.includes("[REDACTED]"), shortRedacted);
  assert.equal(containsSecretShapedString(shortBearer), true);
  assert.ok(!longRedacted.includes("Bearer abcdefghij1234567890"), longRedacted);
  assert.ok(longRedacted.includes("[REDACTED]"), longRedacted);
  assert.equal(containsSecretShapedString(longBearer), true);

  // Ordinary text with no "Bearer <token>" shape at all, and a harmless
  // key=value with neither a recognized label word nor a recognized value
  // shape, are both left untouched — only the Bearer-scheme minimum length
  // changed, not what counts as a Bearer occurrence in the first place.
  const harmlessTexts = ["nothing sensitive here", "key=value"];
  for (const text of harmlessTexts) {
    assert.equal(redactText(text), text, `expected no change to ${JSON.stringify(text)}`);
    assert.equal(containsSecretShapedString(text), false, `expected no detection in ${JSON.stringify(text)}`);
  }
});

// F13 (round 2) regression: createRpcClient's own onUnsolicited dispatch
// (the mechanism openMcpSession's cap is built on) must let a caller cap
// count deterministically — a flooding server cannot grow memory without
// bound just because it never sends a matching request/response pair. Uses
// the same fakeChild harness as the other F13 tests (no real subprocess).
test("F13: a flood of unsolicited notifications is capped when the caller bounds retention", () => {
  const child = fakeChild();
  const capLimit = 50;
  let dropped = 0;
  const bounded = [];
  createRpcClient(child, {
    timeoutMs: 5000,
    onUnsolicited: (message) => {
      if (bounded.length >= capLimit) { dropped += 1; return; }
      bounded.push(message);
    },
  });
  const totalFlooded = 20000;
  for (let index = 0; index < totalFlooded; index += 1) {
    child.stdout.emit("data", `${JSON.stringify({ jsonrpc: "2.0", method: "notifications/flood", params: { index } })}\n`);
  }
  assert.equal(bounded.length, capLimit, "capped array must stop growing at the declared limit");
  assert.equal(dropped, totalFlooded - capLimit, "every message past the cap must be counted as dropped, not silently lost or buffered");
});

// F13 (round 2) regression, real integration shape: openMcpSession itself
// (not a hand-rolled harness) must bound unsolicited-message retention
// against an actual flooding server, and its getUnsolicited() must report
// the truncation explicitly rather than returning a partial list that looks
// complete. fixtures/flooding-mcp-server.mjs sends 5000 unsolicited
// notifications immediately after the handshake, before ever answering
// tools/list.
test("F13: openMcpSession bounds unsolicited-message retention against a real flooding server", async () => {
  const session = await openMcpSession({
    command: process.execPath,
    args: [path.join(HERE, "fixtures", "flooding-mcp-server.mjs")],
    cwd: process.cwd(),
    timeoutMs: 10000,
    maxUnsolicitedMessages: 100,
  });
  try {
    assert.equal(session.initialize.status, "pass", JSON.stringify(session.initialize));
    const unsolicited = session.getUnsolicited();
    assert.equal(unsolicited.messages.length, 100, "retained messages must stop growing at the declared cap");
    assert.equal(unsolicited.truncated, true);
    assert.ok(unsolicited.droppedCount > 0, "messages past the cap must be counted as dropped");
  } finally {
    await session.close();
  }
});
