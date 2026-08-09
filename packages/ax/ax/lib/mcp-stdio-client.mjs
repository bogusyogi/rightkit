// Hand-rolled JSON-RPC-over-stdio MCP client, generalized from Cortex's
// evals/ax/run-conformance.mjs. No @modelcontextprotocol/sdk dependency (this
// package ships zero runtime deps by contract) — the wire protocol needed for
// initialize/tools-list/tools-call/resources-list/prompts-list is a handful
// of newline-delimited JSON-RPC messages, so a minimal client is sufficient
// and is shared by ax/static.mjs (tools/list only) and ax/conformance.mjs
// (full probes).

import { spawn } from "node:child_process";
import { redactText } from "../../lib/redact.mjs";

const PROTOCOL_VERSION = "2025-03-26";
const DEFAULT_MAX_BUFFER_BYTES = 10 * 1024 * 1024; // F13: bound stdout line-buffer growth

// F13: a missing executable (ENOENT) or a server that exits before it
// responds must fail fast and loudly, never crash on an unhandled child
// 'error' event and never sit out the full request timeout waiting for a
// response that can no longer arrive. A stdout stream with no newline (or an
// enormous single line) must not grow the buffer without bound.
export function createRpcClient(child, { timeoutMs = 60000, onUnsolicited, maxBufferBytes = DEFAULT_MAX_BUFFER_BYTES } = {}) {
  let buffer = "";
  let nextId = 0;
  const pending = new Map();
  let fatalError = null;

  function failAllPending(error) {
    if (!fatalError) fatalError = error;
    for (const entry of pending.values()) { clearTimeout(entry.timer); entry.reject(error); }
    pending.clear();
  }

  child.on("error", (error) => {
    failAllPending(new Error(`child process error: ${error?.message ?? error}`));
  });
  child.on("exit", (code, signal) => {
    if (pending.size) failAllPending(new Error(`child process exited (code=${code ?? "null"}, signal=${signal ?? "null"}) before responding`));
  });

  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    if (fatalError) return;
    buffer += chunk;
    if (buffer.length > maxBufferBytes) {
      failAllPending(new Error(`child process stdout exceeded ${maxBufferBytes} bytes without a newline-terminated message; aborting to bound memory`));
      return;
    }
    let index;
    while ((index = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, index).trim();
      buffer = buffer.slice(index + 1);
      if (!line) continue;
      let message;
      try { message = JSON.parse(line); } catch { onUnsolicited?.({ kind: "non-json", line: line.slice(0, 200) }); continue; }
      if (message && message.id !== undefined && pending.has(message.id)) {
        const entry = pending.get(message.id);
        pending.delete(message.id);
        clearTimeout(entry.timer);
        entry.resolve(message);
      } else onUnsolicited?.(message);
    }
  });
  return {
    request(method, params) {
      if (fatalError) return Promise.reject(fatalError);
      const id = ++nextId;
      return new Promise((resolvePromise, rejectPromise) => {
        const timer = setTimeout(() => { pending.delete(id); rejectPromise(new Error(`request timed out after ${timeoutMs}ms: ${method}`)); }, timeoutMs);
        pending.set(id, { resolve: resolvePromise, reject: rejectPromise, timer });
        try {
          child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
        } catch (error) {
          clearTimeout(timer);
          pending.delete(id);
          rejectPromise(new Error(`failed to write to child stdin: ${error?.message ?? error}`));
        }
      });
    },
    notify(method, params) {
      try { child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, ...(params === undefined ? {} : { params }) })}\n`); } catch { /* best-effort; a dead pipe surfaces via the next request() */ }
    },
    close() {
      for (const entry of pending.values()) clearTimeout(entry.timer);
      pending.clear();
    },
  };
}

// Spawns `command` (already split into argv), performs initialize + tools/list
// (+ optionally resources/list and prompts/list), and returns the session plus
// a `close()` that tears the child process down. Callers that need to issue
// further tools/call probes keep using the returned rpc/child directly
// (ax/conformance.mjs); ax/static.mjs calls close() immediately after listing.
const MAX_STDERR_BYTES = 1 * 1024 * 1024; // F13: cap diagnostic stderr accumulation
// F13 (round 2): stdout line-buffer growth (maxBufferBytes) and stderr
// accumulation (MAX_STDERR_BYTES) were bounded, but UNSOLICITED messages
// (JSON-RPC messages/notifications with no matching pending request — see
// createRpcClient's `else onUnsolicited?.(message)` branch) were retained in
// an always-growing array with no cap at all: a target process that floods
// out-of-band notifications could grow memory without bound even though
// every other stream in this client was already bounded. Both a count cap
// and a byte cap apply (belt-and-braces: a flood of huge messages is bounded
// by bytes even if it never reaches the count cap, and vice versa); once
// either is hit, further messages are DROPPED (not buffered, not blocked)
// and the drop is recorded so callers can tell the retained list is partial.
const DEFAULT_MAX_UNSOLICITED_MESSAGES = 500;
const DEFAULT_MAX_UNSOLICITED_BYTES = 1 * 1024 * 1024;

export async function openMcpSession({ command, args = [], cwd, timeoutMs = 60000, listResourcesAndPrompts = false, maxBufferBytes, maxUnsolicitedMessages = DEFAULT_MAX_UNSOLICITED_MESSAGES, maxUnsolicitedBytes = DEFAULT_MAX_UNSOLICITED_BYTES } = {}) {
  const child = spawn(command, args, { cwd, stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
  let stderrText = "";
  let stderrTruncated = false;
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    if (stderrTruncated) return;
    stderrText += chunk;
    if (stderrText.length > MAX_STDERR_BYTES) { stderrText = stderrText.slice(0, MAX_STDERR_BYTES); stderrTruncated = true; }
  });
  const unsolicited = [];
  let unsolicitedBytes = 0;
  let unsolicitedDropped = 0;
  const rpc = createRpcClient(child, {
    timeoutMs,
    maxBufferBytes,
    onUnsolicited: (message) => {
      if (unsolicited.length >= maxUnsolicitedMessages) { unsolicitedDropped += 1; return; }
      let size = 0;
      try { size = JSON.stringify(message)?.length ?? 0; } catch { size = 0; }
      if (unsolicitedBytes + size > maxUnsolicitedBytes) { unsolicitedDropped += 1; return; }
      unsolicitedBytes += size;
      unsolicited.push(message);
    },
  });

  const initializeResponse = await rpc.request("initialize", {
    protocolVersion: PROTOCOL_VERSION,
    capabilities: {},
    clientInfo: { name: "right-ax", version: "0.1.0" },
  });
  rpc.notify("notifications/initialized");
  const toolsResponse = await rpc.request("tools/list", {});
  // F4: a JSON-RPC error on tools/list must never collapse into an empty
  // tool list that a naive `rows.every(...)` then passes vacuously — surface
  // the error so callers (ax/conformance.mjs) can treat it as fatal.
  const toolsListError = toolsResponse.error ?? null;
  const tools = toolsResponse.result?.tools ?? [];

  let resources = null;
  let prompts = null;
  if (listResourcesAndPrompts) {
    const resourcesResponse = await rpc.request("resources/list", {});
    resources = resourcesResponse.error ? { advertised: false, error: resourcesResponse.error } : { advertised: true, count: (resourcesResponse.result?.resources ?? []).length };
    const promptsResponse = await rpc.request("prompts/list", {});
    prompts = promptsResponse.error ? { advertised: false, error: promptsResponse.error } : { advertised: true, count: (promptsResponse.result?.prompts ?? []).length };
  }

  return {
    child,
    rpc,
    tools,
    toolsListError,
    resources,
    prompts,
    initialize: initializeResponse.error
      ? { status: "fail", error: initializeResponse.error }
      : { status: "pass", protocolVersion: initializeResponse.result?.protocolVersion ?? null, serverInfo: initializeResponse.result?.serverInfo ?? null },
    // F13: `messages` is capped (see maxUnsolicitedMessages/maxUnsolicitedBytes
    // above); `truncated`/`droppedCount` make the cap explicit to callers
    // instead of silently returning a partial list that looks complete.
    getUnsolicited: () => ({ messages: unsolicited, truncated: unsolicitedDropped > 0, droppedCount: unsolicitedDropped }),
    getStderr: () => stderrText + (stderrTruncated ? "\n...[stderr truncated at 1MiB]" : ""),
    async close() {
      rpc.close();
      try { child.stdin.end(); } catch { /* already closed */ }
      let graceTimer;
      const exit = await Promise.race([
        new Promise((resolvePromise) => child.once("exit", (code, signal) => resolvePromise({ code, signal }))),
        new Promise((resolvePromise) => { graceTimer = setTimeout(() => { try { child.kill(); } catch { /* ignore */ } resolvePromise({ code: null, signal: "killed-after-grace" }); }, 5000); }),
      ]);
      // Whichever branch wins, the loser's timer/listener must not keep the
      // event loop (or a caller's process) alive — an uncleared 5s grace
      // timer was previously stalling every close() by a full 5s even on
      // the fast, clean-exit path.
      clearTimeout(graceTimer);
      return exit;
    },
  };
}

export function findStackTraces(text) {
  const hits = [];
  const lines = String(text ?? "").split(/\r?\n/);
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const next = lines[index + 1] ?? "";
    if (/^\s*at\s+/.test(line)) hits.push({ line: index + 1, text: line.trim().slice(0, 200) });
    else if (/^\s*(?:Error|TypeError|ReferenceError|RangeError|SyntaxError):/.test(line) && /^\s*at\s+/.test(next)) hits.push({ line: index + 1, text: line.trim().slice(0, 200) });
    else if (/^\s*node:internal\//.test(line)) hits.push({ line: index + 1, text: line.trim().slice(0, 200) });
  }
  return hits;
}

export function tryParseJson(text) {
  try { return JSON.parse(text); } catch { return null; }
}

// F9: every string field derived from the target process's own output
// (error messages, raw-text samples) is redacted before it can reach a
// report — a server that echoes a credential back in an error message must
// never have that credential copied verbatim into conformance evidence.
export function classifyResponse(message) {
  if (message?.error) return { kind: "jsonrpc-error", code: message.error.code === undefined || message.error.code === null ? "unknown" : String(message.error.code), message: redactText(String(message.error.message ?? "")) };
  const result = message?.result ?? {};
  const texts = (result.content ?? []).filter((part) => part && part.type === "text").map((part) => String(part.text ?? ""));
  const joined = texts.join("\n");
  const stack = findStackTraces(joined);
  if (stack.length) return { kind: "stack-trace", stack: stack.map((hit) => ({ ...hit, text: redactText(hit.text) })) };
  if (result.isError === true) {
    const typed = texts.map((text) => ({ text, parsed: tryParseJson(text) })).find(({ parsed }) => parsed && parsed.error && parsed.error.code !== undefined);
    if (typed) return { kind: "typed-error", code: String(typed.parsed.error.code), message: redactText(String(typed.parsed.error.message ?? "")), remediation: typed.parsed.error.remediation ?? null, sample: redactText(typed.text.slice(0, 400)) };
    return { kind: "untyped-error", sample: redactText(joined.slice(0, 500)) };
  }
  return { kind: "success", structuredContent: result.structuredContent ?? null, sample: redactText(joined.slice(0, 300)) };
}
