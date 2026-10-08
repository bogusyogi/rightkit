import { realpathSync } from "node:fs";
import { spawnSync } from "./background-process.mjs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function automationDenied({ parentPid = process.ppid, run = spawnSync, caller: declaredCaller } = {}) {
  const result = declaredCaller ? {} : run("/bin/ps", ["-p", String(parentPid), "-o", "command="], {
    encoding: "utf8", timeout: 1000, shell: false,
  });
  const caller = declaredCaller || result.status === 0 && result.stdout?.trim() || `PID ${parentPid}`;
  const error = new Error(`osascript blocked for caller ${caller}; use right-release dmg build for headless layout`);
  error.code = "MAC_AUTOMATION_DENIED";
  error.caller = caller;
  error.parentPid = parentPid;
  return error;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const error = automationDenied({ caller: process.argv[2] === "--caller" ? process.argv[3] : undefined });
  console.error(JSON.stringify({ event: "mac-automation-denied", code: error.code, caller: error.caller,
    parentPid: error.parentPid, message: error.message }));
  process.exitCode = 125;
}
