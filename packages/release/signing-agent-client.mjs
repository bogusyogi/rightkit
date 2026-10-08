#!/usr/bin/env node
import * as fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { signingAgentPaths, privatePath, SigningAgentError, MAX_FRAME } from "./rightkit-signing-agent.mjs";

export function validateNotaryAgentReport(report) {
  const fields = (value, allowed) => value && typeof value === "object" && !Array.isArray(value) && Object.keys(value).every((key) => allowed.includes(key));
  const uuid = (value) => typeof value === "string" && /^[a-f0-9-]{36}$/i.test(value);
  const n = report?.notarization;
  const summary = n?.logSummary;
  if (!fields(report, ["schema", "ok", "id", "notarization", "steps", "stapling", "code", "message"])
    || report.schema !== 1 || typeof report.ok !== "boolean" || !uuid(report.id)
    || (report.code !== undefined && !["MAC_SIGNING_AGENT_TIMEOUT", "MAC_SIGNING_AGENT_JOB_FAILED", "MAC_SIGNING_NOTARIZATION_FAILED", "MAC_SIGNING_STAPLE_FAILED"].includes(report.code))
    || (report.message !== undefined && !["Notarization deadline expired", "Notarization or stapling failed inside signing agent"].includes(report.message))
    || !fields(n, ["id", "status", "logSummary"]) || !(n.id === null || uuid(n.id))
    || !["Accepted", "Invalid", "Unknown", "In Progress", "Rejected"].includes(n.status)
    || !(summary === null || (fields(summary, ["statusCode", "errors", "warnings", "issues"])
      && (summary.statusCode === null || Number.isInteger(summary.statusCode))
      && Number.isInteger(summary.errors) && summary.errors >= 0 && Number.isInteger(summary.warnings) && summary.warnings >= 0
      && Array.isArray(summary.issues) && summary.issues.length <= 100 && summary.issues.every((issue) => fields(issue, ["severity", "code"])
        && ["error", "warning", "unknown"].includes(issue.severity) && (issue.code === null || Number.isInteger(issue.code)))))
    || !Array.isArray(report.steps) || report.steps.length > 10 || report.steps.some((step) => !fields(step, ["step", "file", "exitCode"])
      || !["submit", "log", "probe", "staple", "validate"].includes(step.step)
      || !(step.exitCode === null || (Number.isInteger(step.exitCode) && step.exitCode >= 0 && step.exitCode <= 255)))
    || !Array.isArray(report.stapling) || report.stapling.length > 2 || report.stapling.some((item) => !fields(item, ["file", "alreadyStapled", "validated"])
      || typeof item.file !== "string" || !path.isAbsolute(item.file) || typeof item.alreadyStapled !== "boolean" || item.validated !== true)) {
    throw new SigningAgentError("MAC_SIGNING_AGENT_PROTOCOL", "Invalid notarization agent report");
  }
  return report;
}

export async function requestSigningAgent(request, { paths = signingAgentPaths(), timeoutMs = (request.timeoutMs ?? 180000) + 5000,
  onProgress = () => {},
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
    let input = Buffer.alloc(0), settled = false, jobId;
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
      try {
        while (input.indexOf(10) >= 0 && !settled) {
          const end = input.indexOf(10);
          const report = JSON.parse(input.subarray(0, end).toString("utf8"));
          input = input.subarray(end + 1);
          const notary = ["notarize", "staple"].includes(request.type);
          if (report.event) {
            if (!notary || report.schema !== 1 || !["status", "heartbeat"].includes(report.event)
              || typeof report.id !== "string" || !/^[a-f0-9-]{36}$/i.test(report.id)
              || Object.keys(report).some((key) => !["schema", "event", "id", "step"].includes(key))
              || (report.step !== undefined && !["submit", "log", "probe", "staple", "validate"].includes(report.step))) throw new Error();
            if (jobId && jobId !== report.id) throw new Error();
            jobId = report.id;
            onProgress(report);
            continue;
          }
          if (input.length || report.schema !== 1 || typeof report.ok !== "boolean") throw new Error();
          if (notary && report.notarization) validateNotaryAgentReport(report);
          if (notary && jobId && jobId !== report.id) throw new Error();
          if (!report.ok) {
            const code = typeof report.code === "string" && report.code.startsWith("MAC_SIGNING_") ? report.code : "MAC_SIGNING_AGENT_JOB_FAILED";
            const error = new SigningAgentError(code, request.type === "updater" ? "Updater signing failed inside signing agent" : notary ? "Notarization or stapling failed inside signing agent" : String(report.message ?? "Signing agent rejected request"));
            if (notary && report.notarization) error.report = report;
            finish(error);
            return;
          }
          if (notary) {
            validateNotaryAgentReport(report);
          } else if (request.type === "health") {
            if (report.service !== "dev.rightkit.signing-agent" || !Number.isInteger(report.pid)) throw new Error();
          } else if (request.type === "codesign-argv") {
            if (!Array.isArray(report.results) || report.results.length !== 1
              || report.results.some((r) => !Number.isInteger(r.status) || r.status < 0 || r.status > 255
                || typeof r.stdout !== "string" || typeof r.stderr !== "string")) throw new Error();
          } else if (!Array.isArray(report.results) || report.results.length !== (request.items?.length ?? request.files?.length)
            || report.results.some((r) => r.status !== 0 || typeof r.stdout !== "string" || typeof r.stderr !== "string")
            || (request.type === "updater" && report.results.some((r) => r.stdout || r.stderr))) throw new Error();
          finish(null, report);
        }
      } catch { finish(new SigningAgentError("MAC_SIGNING_AGENT_PROTOCOL", "Invalid signing agent response")); }
    });
  });
}
export function requestSigningAgentSync(request, { runClient = spawnSync, home, timeoutMs = (request.timeoutMs ?? 180000) + 5000 } = {}) {
  // Existing signing call sites are synchronous. Only typed JSON goes to this
  // child's stdin; credentials & authentication token never enter argv/env.
  const result = runClient(process.execPath, [fileURLToPath(import.meta.url)], { encoding: "utf8", stdio: ["pipe", "pipe", "inherit"], shell: false,
    input: JSON.stringify({ request, home, timeoutMs }), timeout: timeoutMs + 1000, maxBuffer: MAX_FRAME,
    env: { PATH: process.env.PATH, HOME: process.env.HOME } });
  let report;
  try { report = JSON.parse(result.stdout); } catch {
    throw new SigningAgentError(result.error?.code === "ETIMEDOUT" ? "MAC_SIGNING_AGENT_TIMEOUT" : "MAC_SIGNING_AGENT_CONNECTION", "Signing agent client failed");
  }
  if (report.schema !== 1 || typeof report.ok !== "boolean") throw new SigningAgentError("MAC_SIGNING_AGENT_PROTOCOL", "Invalid signing agent client report");
  if (!report.ok) {
    const error = new SigningAgentError(report.code ?? "MAC_SIGNING_AGENT_JOB_FAILED", report.message ?? "Signing agent failed");
    if (["notarize", "staple"].includes(request.type) && report.notarization) error.report = validateNotaryAgentReport(report);
    throw error;
  }
  if (result.error || result.status !== 0) throw new SigningAgentError("MAC_SIGNING_AGENT_CONNECTION", "Signing agent client failed");
  return report;
}
const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try {
    const { request, home, timeoutMs } = JSON.parse(fs.readFileSync(0, "utf8"));
    const report = await requestSigningAgent(request, { paths: signingAgentPaths(home), timeoutMs,
      onProgress: (frame) => process.stderr.write(`right-release signing-agent: ${frame.id} ${frame.step ?? frame.event}\n`) });
    process.stdout.write(JSON.stringify(report));
  } catch (error) {
    process.stdout.write(JSON.stringify(error.report ?? { schema: 1, ok: false, code: error.code ?? "MAC_SIGNING_AGENT_PROTOCOL", message: error.message }));
    process.exitCode = 1;
  }
}
