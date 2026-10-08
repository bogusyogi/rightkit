import childProcess from "node:child_process";
import { promisify } from "node:util";

// Windows PowerShell's inbox ACL cmdlets can fail while importing legacy
// module type data when inherited PSModulePath entries are present.
export function windowsPowerShellEnvironment(overrides = {}, base = process.env) {
  const environment = { ...base, ...overrides };
  for (const key of Object.keys(environment)) {
    if (key.toLowerCase() === "psmodulepath") delete environment[key];
  }
  return environment;
}

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

// Native ownership ceremonies must be able to show their Windows Hello UI.
// Keep this exception explicit so ordinary release subprocesses stay hidden.
export function spawnSyncInteractive(command, args, options) {
  return childProcess.spawnSync(command, ...(Array.isArray(args)
    ? [args, { ...options, shell: false, windowsHide: false }]
    : [[], { ...(args ?? options), shell: false, windowsHide: false }]));
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
