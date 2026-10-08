#!/usr/bin/env node
import * as fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { signingAgentPaths, privatePath, SigningAgentError, MAX_FRAME } from "./rightkit-signing-agent.mjs";

export async function requestSigningAgent(request, { paths = signingAgentPaths(), timeoutMs = 185000,
  io = fs, connect = net.createConnection } = {}) {
  try {
    privatePath(paths.directory, 0o700, { io, kind: "directory" });
    privatePath(paths.plist, 0o600, { io });
    privatePath(paths.socket, 0o600, { io, kind: "socket" });
    privatePath(paths.token, 0o600, { io });
  } catch (error) {
    if (error.code === "ENOENT") throw new SigningAgentError("MAC_SIGNING_AGENT_UNAVAILABLE", "Signing agent installation, socket, or token is missing");
    throw error;
  }
  const token = io.readFileSync(paths.token, "utf8");
  if (!/^[a-f0-9]{64}$/.test(token)) throw new SigningAgentError("MAC_SIGNING_AGENT_AUTH", "Invalid signing agent token");
  const frame = Buffer.from(`${JSON.stringify({ ...request, token })}\n`);
  if (frame.length > MAX_FRAME) throw new SigningAgentError("MAC_SIGNING_AGENT_PROTOCOL", "Signing request is too large");
  return new Promise((resolve, reject) => {
    const socket = connect({ path: paths.socket });
    let input = Buffer.alloc(0), settled = false;
    const finish = (error, report) => {
      if (settled) return;
      settled = true; clearTimeout(timer); socket.destroy();
      if (error) reject(error); else resolve(report);
    };
    // Wall deadline, rather than an activity timeout which a stalled peer can extend.
    const timer = setTimeout(() => finish(new SigningAgentError("MAC_SIGNING_AGENT_TIMEOUT", "Signing agent response timed out")), timeoutMs);
    socket.on("connect", () => socket.write(frame));
    socket.on("error", (error) => finish(new SigningAgentError(["ENOENT", "ECONNREFUSED"].includes(error.code)
      ? "MAC_SIGNING_AGENT_UNAVAILABLE" : "MAC_SIGNING_AGENT_CONNECTION", "Signing agent connection failed")));
    socket.on("end", () => { if (!settled) finish(new SigningAgentError("MAC_SIGNING_AGENT_PROTOCOL", "Signing agent returned no complete response")); });
    socket.on("data", (chunk) => {
      input = Buffer.concat([input, chunk]);
      if (input.length > MAX_FRAME) { finish(new SigningAgentError("MAC_SIGNING_AGENT_PROTOCOL", "Signing agent response is too large")); return; }
      const end = input.indexOf(10);
      if (end < 0) return;
      try {
        const report = JSON.parse(input.subarray(0, end).toString("utf8"));
        if (end !== input.length - 1 || report.schema !== 1 || typeof report.ok !== "boolean") throw new Error();
        if (!report.ok) {
          const code = typeof report.code === "string" && report.code.startsWith("MAC_SIGNING_") ? report.code : "MAC_SIGNING_AGENT_JOB_FAILED";
          finish(new SigningAgentError(code, request.type === "updater" ? "Updater signing failed inside signing agent" : String(report.message ?? "Signing agent rejected request")));
          return;
        }
        if (request.type === "health") {
          if (report.service !== "dev.rightkit.signing-agent" || !Number.isInteger(report.pid)) throw new Error();
        } else if (!Array.isArray(report.results) || report.results.length !== (request.items?.length ?? request.files?.length)
          || report.results.some((r) => r.status !== 0 || typeof r.stdout !== "string" || typeof r.stderr !== "string")
          || (request.type === "updater" && report.results.some((r) => r.stdout || r.stderr))) throw new Error();
        finish(null, report);
      } catch { finish(new SigningAgentError("MAC_SIGNING_AGENT_PROTOCOL", "Invalid signing agent response")); }
    });
  });
}
export function requestSigningAgentSync(request, { runClient = spawnSync, home, timeoutMs = 185000 } = {}) {
  // Existing signing call sites are synchronous. Only typed JSON goes to this
  // child's stdin; credentials & authentication token never enter argv/env.
  const result = runClient(process.execPath, [fileURLToPath(import.meta.url)], { encoding: "utf8", stdio: "pipe", shell: false,
    input: JSON.stringify({ request, home, timeoutMs }), timeout: timeoutMs + 1000, maxBuffer: MAX_FRAME,
    env: { PATH: process.env.PATH, HOME: process.env.HOME } });
  let report;
  try { report = JSON.parse(result.stdout); } catch {
    throw new SigningAgentError(result.error?.code === "ETIMEDOUT" ? "MAC_SIGNING_AGENT_TIMEOUT" : "MAC_SIGNING_AGENT_CONNECTION", "Signing agent client failed");
  }
  if (report.schema !== 1 || typeof report.ok !== "boolean") throw new SigningAgentError("MAC_SIGNING_AGENT_PROTOCOL", "Invalid signing agent client report");
  if (!report.ok) throw new SigningAgentError(report.code ?? "MAC_SIGNING_AGENT_JOB_FAILED", report.message ?? "Signing agent failed");
  if (result.error || result.status !== 0) throw new SigningAgentError("MAC_SIGNING_AGENT_CONNECTION", "Signing agent client failed");
  return report;
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const { request, home, timeoutMs } = JSON.parse(fs.readFileSync(0, "utf8"));
    const report = await requestSigningAgent(request, { paths: signingAgentPaths(home), timeoutMs });
    process.stdout.write(JSON.stringify(report));
  } catch (error) {
    process.stdout.write(JSON.stringify({ schema: 1, ok: false, code: error.code ?? "MAC_SIGNING_AGENT_PROTOCOL", message: error.message }));
    process.exitCode = 1;
  }
}
