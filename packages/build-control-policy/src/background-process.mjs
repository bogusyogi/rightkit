import childProcess from "node:child_process";
import { promisify } from "node:util";

// Release tooling has no interactive console children. Keep this policy at
// every Node launch boundary, including fixtures & nested release commands.
function hiddenArguments(args, options) {
  return Array.isArray(args)
    ? [args, { ...options, windowsHide: true }]
    : [[], { ...(args ?? options), windowsHide: true }];
}

export function spawn(command, args, options) {
  return childProcess.spawn(command, ...hiddenArguments(args, options));
}

export function spawnSync(command, args, options) {
  return childProcess.spawnSync(command, ...hiddenArguments(args, options));
}

export function execFileSync(file, args, options) {
  return childProcess.execFileSync(file, ...hiddenArguments(args, options));
}

export function execFile(file, args, options, callback) {
  if (typeof args === "function") { callback = args; args = undefined; }
  if (typeof options === "function") { callback = options; options = undefined; }
  return childProcess.execFile(file, ...hiddenArguments(args, options), callback);
}

// Node's custom promisifier returns { stdout, stderr } & exposes promise.child.
// Preserve that contract for release-token's existing promisify(execFile).
execFile[promisify.custom] = (file, args, options) =>
  childProcess.execFile[promisify.custom](file, ...hiddenArguments(args, options));
