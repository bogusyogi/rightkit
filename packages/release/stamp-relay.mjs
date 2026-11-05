#!/usr/bin/env node
// Line-stamping relay for the gate's inherited-output children (GATE-CONTRACT GC-A21, rightkit#1463/#1514): `node stamp-relay.mjs <cmd> [args...]`
// runs <cmd> with piped output and writes every complete line to its own stdout/stderr prefixed with the UTC time, the same form the gate's
// own lines carry. The gate keeps launching synchronously (spawnSync, stdio inherit) and only puts this relay in front, so a child's live output
// still streams. A partial last line is stamped and ended at exit. The relay passes stdin through, forwards SIGINT, SIGTERM and SIGHUP to the
// child, and ends the way the child ended: the same exit code, or the same signal re-raised on itself.
import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";

export function lineStamper(write, now = () => new Date().toISOString()) {
  let pending = "";
  return {
    push(chunk) {
      pending += chunk;
      let at;
      while ((at = pending.indexOf("\n")) >= 0) { write(`${now()} ${pending.slice(0, at + 1)}`); pending = pending.slice(at + 1); }
    },
    end() { if (pending) { write(`${now()} ${pending}\n`); pending = ""; } },
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [cmd, ...args] = process.argv.slice(2);
  if (!cmd) { process.stderr.write("usage: stamp-relay.mjs <cmd> [args...]\n"); process.exit(64); }
  const child = spawn(cmd, args, { stdio: ["inherit", "pipe", "pipe"], windowsHide: true });
  const out = lineStamper((s) => process.stdout.write(s));
  const err = lineStamper((s) => process.stderr.write(s));
  child.stdout.setEncoding("utf8").on("data", (c) => out.push(c));
  child.stderr.setEncoding("utf8").on("data", (c) => err.push(c));
  for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(sig, () => { try { child.kill(sig); } catch { /* already gone */ } });
  child.on("error", (e) => { process.stderr.write(`${new Date().toISOString()} stamp-relay: ${cmd} could not be started (${e.code ?? e.message})\n`); process.exit(127); });
  child.on("close", (code, signal) => {
    out.end(); err.end();
    if (signal) { process.removeAllListeners(signal); process.kill(process.pid, signal); return; }
    process.exitCode = code ?? 1;
  });
}
