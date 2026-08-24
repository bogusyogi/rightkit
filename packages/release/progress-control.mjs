import { spawn } from "node:child_process";

const MANAGED_REQUEST_PATTERN = /\brightkit(?: managed-agent)?: request ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\b/gi;
export const RELEASE_LIVENESS_OWNER_MARGIN_MS = 5 * 60 * 1000;

export function latestManagedRequestId(value, fallback = null) {
  let latest = fallback;
  for (const match of String(value ?? "").matchAll(MANAGED_REQUEST_PATTERN)) latest = match[1].toLowerCase();
  return latest;
}

export function firstManagedRequestId(value) {
  MANAGED_REQUEST_PATTERN.lastIndex = 0;
  return MANAGED_REQUEST_PATTERN.exec(String(value ?? ""))?.[1]?.toLowerCase() ?? null;
}

export function releaseProgressLimits(env = process.env, { livenessOwnerTimeoutMs = null } = {}) {
  const configuredInactivityMs = Number(env.RIGHT_RELEASE_STALL_MS ?? 10 * 60 * 1000);
  const absoluteMs = Number(env.RIGHT_RELEASE_ABSOLUTE_MS ?? 4 * 60 * 60 * 1000);
  const ownerTimeoutMs = livenessOwnerTimeoutMs === null ? null : Number(livenessOwnerTimeoutMs);
  if (![configuredInactivityMs, absoluteMs, ...(ownerTimeoutMs === null ? [] : [ownerTimeoutMs])].every((value) => Number.isFinite(value) && value > 0)) {
    throw new Error("release progress limits must be positive finite milliseconds");
  }
  const inactivityMs = ownerTimeoutMs === null
    ? configuredInactivityMs
    : Math.max(configuredInactivityMs, ownerTimeoutMs + RELEASE_LIVENESS_OWNER_MARGIN_MS);
  if (absoluteMs <= inactivityMs) throw new Error("RIGHT_RELEASE_ABSOLUTE_MS must exceed effective release inactivity limit");
  return Object.freeze({ inactivityMs, absoluteMs });
}

function runRightkitControl(args, { cwd, env, timeoutMs = 30_000 } = {}) {
  return new Promise((resolve) => {
    const control = spawn("rightkit", args, { cwd, env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true, shell: process.platform === "win32" });
    let output = "";
    let timedOut = false;
    const append = (chunk) => { output = `${output}${chunk}`.slice(-64 * 1024); };
    control.stdout.on("data", append);
    control.stderr.on("data", append);
    const timer = setTimeout(() => {
      timedOut = true;
      control.kill("SIGTERM");
      setTimeout(() => control.kill("SIGKILL"), 1_000).unref();
    }, timeoutMs);
    timer.unref();
    control.once("error", (error) => {
      clearTimeout(timer);
      resolve(Object.freeze({ code: 125, output, timedOut, error: error.message }));
    });
    control.once("exit", (code, signal) => {
      clearTimeout(timer);
      resolve(Object.freeze({ code: code ?? 125, signal, output, timedOut }));
    });
  });
}

export function terminalResultFromOutput(output) {
  for (const line of String(output ?? "").split(/\r?\n/).reverse()) {
    try {
      const candidate = JSON.parse(line);
      if (candidate?.type === "RESULT" && typeof candidate.status === "string") return candidate;
    } catch { /* streamed build output is not JSON */ }
  }
  return null;
}

export async function cancelAndReapManagedRequest(id, { cwd = process.cwd(), env = process.env, run = runRightkitControl } = {}) {
  if (!id) return Object.freeze({ status: "NO_MANAGED_REQUEST" });
  const cancel = await run(["cancel", id], { cwd, env });
  const attach = await run(["attach", id], { cwd, env });
  const terminal = terminalResultFromOutput(attach.output);
  if (!terminal) {
    throw new Error(`broker cancellation for ${id} lacked terminal RESULT (cancel=${cancel.code}, attach=${attach.code}${attach.timedOut ? ", attach-timeout" : ""})`);
  }
  return Object.freeze({ status: terminal.status, id, exitCode: terminal.exitCode, receipt: terminal.receipt ?? null, cancelExitCode: cancel.code, attachExitCode: attach.code });
}
