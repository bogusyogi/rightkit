import { resolve } from "node:path";
import readline from "node:readline";
import { chmodSync, existsSync, readFileSync, writeFileSync, realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

export const TOOL_NAMES = [
  "room_join", "room_status", "room_post", "room_raise", "room_contest",
  "room_dispose", "room_resolve", "room_rule", "room_motion", "room_next",
  "room_vote", "room_tasks",
] as const;

type ToolName = typeof TOOL_NAMES[number];
type JsonRpcRequest = { jsonrpc: "2.0"; id: string | number | null; method: string; params?: any };
type HandlerOptions = { baseUrl: string; fetchImpl: typeof fetch; credentialPath?: string };

export function createMcpHandler({ baseUrl, fetchImpl, credentialPath }: HandlerOptions) {
  const endpoint = baseUrl.replace(/\/$/, "");
  let stored = loadCredentials(credentialPath);
  let bearerToken: string | undefined = stored?.bearer_token;
  let resumeToken: string | undefined = stored?.resume_token;

  return async function handle(request: JsonRpcRequest): Promise<any> {
    try {
      if (request.method === "initialize") {
        return ok(request.id, {
          protocolVersion: request.params?.protocolVersion ?? "2025-03-26",
          capabilities: { tools: { listChanged: false } },
          serverInfo: { name: "agent-room-mcp", version: "0.1.0" },
        });
      }
      if (request.method === "notifications/initialized") return undefined;
      if (request.method === "tools/list") {
        return ok(request.id, { tools: TOOL_NAMES.map((name) => TOOL_SCHEMAS[name]) });
      }
      if (request.method !== "tools/call") return rpcError(request.id, -32601, "Method not found");
      const name = request.params?.name as ToolName;
      if (!TOOL_NAMES.includes(name)) return rpcError(request.id, -32602, "Unknown room tool");
      const args = { ...(request.params?.arguments ?? {}) };
      if (name === "room_join" && stored?.bootstrap === true && sameIdentity(stored, args)) {
        return ok(request.id, {
          content: [{
            type: "text",
            text: JSON.stringify({
              seat_id: stored.seat_id,
              charter: stored.charter,
              at_budget: stored.at_budget,
              budget: stored.budget,
            }),
          }],
          isError: false,
        });
      }
      let response: Response;
      if (name === "room_join") {
        if (resumeToken && args.resume_token === undefined) args.resume_token = resumeToken;
        response = await fetchImpl(`${endpoint}/api/join`, jsonRequest(args));
      } else {
        if (!bearerToken) return toolError(request.id, "room_join_required");
        const path = name === "room_status" ? "/api/status" : `/api/tool/${name.slice(5)}`;
        response = await fetchImpl(
          `${endpoint}${path}`,
          name === "room_status" ? { headers: authHeaders(bearerToken) } : jsonRequest(args, bearerToken),
        );
      }
      const value = await response.json();
      if (!response.ok) return toolError(request.id, value?.error ?? `http_${response.status}`, value);
      if (name === "room_join") {
        bearerToken = value.bearer_token;
        resumeToken = value.resume_token;
        stored = {
          room_id: args.room_id, name: args.name, model: args.model, role: args.role,
          bearer_token: bearerToken, resume_token: resumeToken,
        };
        saveCredentials(credentialPath, stored);
        delete value.bearer_token;
        delete value.resume_token;
      } else if (stored?.bootstrap === true) {
        stored.bootstrap = false;
        saveCredentials(credentialPath, stored);
      }
      return ok(request.id, {
        content: [{ type: "text", text: JSON.stringify(value) }],
        isError: false,
      });
    } catch (error) {
      return toolError(request.id, "shim_failure", { detail: String(error) });
    }
  };
}

function sameIdentity(stored: any, args: any) {
  return stored.room_id === args.room_id
    && stored.name === args.name
    && stored.model === args.model
    && stored.role === args.role;
}

function loadCredentials(path?: string): any | undefined {
  if (!path || !existsSync(path)) return undefined;
  try {
    const value = JSON.parse(readFileSync(path, "utf8"));
    return value?.resume_token && value?.bearer_token ? value : undefined;
  } catch {
    return undefined;
  }
}

function saveCredentials(path: string | undefined, value: unknown) {
  if (!path) return;
  writeFileSync(path, `${JSON.stringify(value)}\n`, { encoding: "utf8", mode: 0o600 });
  try { chmodSync(path, 0o600); } catch { /* Windows ACL is pre-applied by the launcher. */ }
}

function authHeaders(token: string): HeadersInit {
  return { authorization: `Bearer ${token}` };
}

function jsonRequest(body: unknown, token?: string): RequestInit {
  return {
    method: "POST",
    headers: { "content-type": "application/json", ...(token ? authHeaders(token) : {}) },
    body: JSON.stringify(body),
  };
}

function ok(id: JsonRpcRequest["id"], result: unknown) {
  return { jsonrpc: "2.0", id, result };
}

function rpcError(id: JsonRpcRequest["id"], code: number, message: string, data?: unknown) {
  return { jsonrpc: "2.0", id, error: { code, message, ...(data === undefined ? {} : { data }) } };
}

function toolError(id: JsonRpcRequest["id"], message: string, data?: unknown) {
  return ok(id, {
    content: [{ type: "text", text: JSON.stringify({ error: message, ...(data ?? {}) }) }],
    isError: true,
  });
}

const descriptions: Record<ToolName, string> = {
  room_join: "Join or resume one stable room seat.",
  room_status: "Get the authoritative seat-visible snapshot and budgets.",
  room_post: "Post question, evidence, or info without mutating finding state.",
  room_raise: "Raise one typed finding.",
  room_contest: "Contest exactly one service-issued finding.",
  room_dispose: "Propose a folded or refuted exit with receipts.",
  room_resolve: "Original author accepts or re-contests one disposition.",
  room_rule: "Human rules on one re-contested disposition.",
  room_motion: "Create a typed, digest-bound motion.",
  room_next: "Wait for seat-visible events after a cursor.",
  room_vote: "Cast one reasoned vote on a service-issued motion.",
  room_tasks: "Use the task ledger in work-splitting mode.",
};

const string = { type: "string" };
const strings = { type: "array", items: string };
const receipt = {
  type: "object", additionalProperties: false, required: ["kind", "locator"],
  properties: {
    kind: { type: "string", enum: ["file_line", "artifact", "measurement", "web", "review_scope"] },
    locator: string, line: { type: ["integer", "null"], minimum: 1 },
    digest: { type: ["string", "null"], pattern: "^[a-f0-9]{64}$" },
  },
};

function schemaFor(name: ToolName): Record<string, unknown> {
  const base = (properties: Record<string, unknown>, required: string[] = []) => ({
    type: "object", additionalProperties: false, properties, ...(required.length ? { required } : {}),
  });
  switch (name) {
    case "room_join": return base({ room_id: string, name: string, model: string, role: string }, ["room_id", "name", "model", "role"]);
    case "room_status": return base({});
    case "room_post": return base({
      request_id: string, kind: { type: "string", enum: ["position", "question", "evidence", "info"] },
      body: string, reply_to: { type: ["integer", "null"], minimum: 1 },
    }, ["request_id", "kind", "body"]);
    case "room_raise": return base({
      request_id: string, claim: string, severity: { type: "string", enum: ["P0", "P1", "P2"] },
      rationale: string, evidence_refs: strings, proposed_change: { type: ["string", "null"] },
      confidence: { type: ["number", "null"], minimum: 0, maximum: 1 },
    }, ["request_id", "claim", "severity", "rationale", "evidence_refs"]);
    case "room_contest": return base({ request_id: string, finding_id: string, rationale: string, evidence_refs: strings }, ["request_id", "finding_id", "rationale", "evidence_refs"]);
    case "room_dispose": return base({
      request_id: string, finding_id: string, action: { type: "string", enum: ["folded", "refuted"] },
      reason_code: { type: ["string", "null"] }, receipt_refs: { type: "array", items: receipt, minItems: 1 },
    }, ["request_id", "finding_id", "action", "receipt_refs"]);
    case "room_resolve": return base({
      request_id: string, disposition_id: string, choice: { type: "string", enum: ["accept", "recontest"] },
      reason: string, evidence_refs: strings,
    }, ["request_id", "disposition_id", "choice", "reason", "evidence_refs"]);
    case "room_rule": return base({
      request_id: string, disposition_id: string, action: { type: "string", enum: ["folded", "refuted"] },
      reason: string, receipt_refs: { type: "array", items: receipt, minItems: 1 },
    }, ["request_id", "disposition_id", "action", "reason", "receipt_refs"]);
    case "room_motion": return base({
      request_id: string, kind: { type: "string", enum: ["review_scope"] },
      scope_refs: { type: "array", minItems: 1, items: base({ locator: string, digest: { type: "string", pattern: "^[a-f0-9]{64}$" } }, ["locator", "digest"]) },
      scope_digest: { type: "string", pattern: "^[a-f0-9]{64}$" },
    }, ["request_id", "kind", "scope_refs", "scope_digest"]);
    case "room_next": return base({ cursor: { type: "integer", minimum: 0 }, wait_ms: { type: "integer", minimum: 30000, maximum: 60000, default: 45000 } });
    case "room_vote": return base({ request_id: string, motion_id: string, choice: string, reason: string }, ["request_id", "motion_id", "choice", "reason"]);
    case "room_tasks": return base({ request_id: string, action: { type: "string", enum: ["claim", "complete", "list"] } }, ["request_id", "action"]);
  }
}

const TOOL_SCHEMAS: Record<ToolName, Record<string, unknown>> = Object.fromEntries(
  TOOL_NAMES.map((name) => [name, {
    name,
    description: descriptions[name],
    inputSchema: schemaFor(name),
  }]),
) as Record<ToolName, Record<string, unknown>>;

async function runStdio() {
  const baseUrl = process.env.AGENT_ROOM_URL;
  if (!baseUrl) throw new Error("AGENT_ROOM_URL is required");
  const handler = createMcpHandler({
    baseUrl,
    fetchImpl: fetch,
    credentialPath: process.env.AGENT_ROOM_CREDENTIALS,
  });
  const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  for await (const line of input) {
    if (!line.trim()) continue;
    const response = await handler(JSON.parse(line));
    if (response !== undefined) process.stdout.write(`${JSON.stringify(response)}\n`);
  }
}

const realOrSelf = (file: string) => { try { return realpathSync(file); } catch { return resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  runStdio().catch((error) => {
    process.stderr.write(`${String(error)}\n`);
    process.exitCode = 1;
  });
}
