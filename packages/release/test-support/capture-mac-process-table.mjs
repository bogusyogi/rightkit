// Captures the real macOS process table once, for the recorded-table replay of build-ownership's hidden-environment rule (item 5
// row 5.24a'; signed/item5-sage-decisions-2.md D1): `node test-support/capture-mac-process-table.mjs <out.json>` on a Mac.
//
// Each row is { pid, ppid, pgid, self, stat, lstart, argv0, hidden, lsofInspected, blame? }:
//   self          the process is this user's (the replay gives it the replaying user's uid, others uid 0);
//   argv0         the executable BASENAME only (`ps -o comm=`), cut at the first of `:`, `@`, `[` (sshd-session titles carry a user name); never arguments, never a path;
//   hidden        `ps eww` printed no environment for it (its line is its command line, nothing more);
//   lsofInspected `lsof -u <uid>` listed it;
//   blame         `{ rc, line }` of `launchctl blame <pid>`, for every process whose parent is pid 1 (a walk from this user's process can end at a root-owned launchd job).
// Environment text and arguments are read in memory to decide `hidden`, and are never written: they can hold secrets.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "../background-process.mjs";

if (process.platform !== "darwin") { console.error("capture-mac-process-table: run on a Mac"); process.exit(2); }
const out = process.argv[2];
if (!out) { console.error("usage: node test-support/capture-mac-process-table.mjs <out.json>"); process.exit(2); }
const env = { ...process.env, LC_ALL: "C", LANG: "C" };
const run = (cmd, args) => {
  const r = spawnSync(cmd, args, { encoding: "utf8", env, timeout: 60_000, maxBuffer: 1 << 27 });
  if (r.error || (r.status !== 0 && cmd !== "launchctl")) throw new Error(`${cmd} ${args.join(" ")}: ${r.error?.message ?? `exit ${r.status}`}`);
  return r;
};
const uid = process.getuid();
const lstartOf = (d) => `${["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][d.getDay()]} ${["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][d.getMonth()]} ${String(d.getDate()).padStart(2, " ")} ${[d.getHours(), d.getMinutes(), d.getSeconds()].map((n) => String(n).padStart(2, "0")).join(":")} ${d.getFullYear()}`;
const capturedAt = lstartOf(new Date());
const table = run("ps", ["-A", "-ww", "-o", "pid=,ppid=,pgid=,uid=,stat=,lstart=,comm="]).stdout.split("\n");
const commands = new Map(); for (const l of run("ps", ["-A", "-ww", "-o", "pid=", "-o", "command="]).stdout.split("\n")) { const m = /^\s*(\d+)\s+(.*)$/.exec(l); if (m) commands.set(Number(m[1]), m[2].trim()); }
const withEnv = new Map(); for (const l of run("ps", ["eww", "-A", "-o", "pid=", "-o", "command="]).stdout.split("\n")) { const m = /^\s*(\d+)\s+(.*)$/.exec(l); if (m) withEnv.set(Number(m[1]), m[2].trim()); }
const inspected = new Set(run("lsof", ["-n", "-P", "-a", "-u", String(uid), "-Fp"]).stdout.split("\n").filter((l) => l.startsWith("p")).map((l) => Number(l.slice(1))));
const rows = [];
for (const line of table) {
  const m = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\S+)\s+(\w{3} \w{3}\s+\d+ \d\d:\d\d:\d\d \d{4})\s+(.*)$/.exec(line);
  if (!m) continue;
  const pid = Number(m[1]); const cmd = commands.get(pid) ?? ""; const shown = withEnv.get(pid);
  const row = { pid, ppid: Number(m[2]), pgid: Number(m[3]), self: Number(m[4]) === uid, stat: m[5], lstart: m[6].replace(/\s+/g, " "), argv0: path.basename(m[7].trim()).split(/[:@\[]/)[0].trim(), hidden: !(shown !== undefined && shown.length > cmd.length && shown.startsWith(`${cmd} `)), lsofInspected: inspected.has(pid) };
  if (row.ppid === 1) { // any user's: a walk from this user's process can end at a root-owned launchd job (sshd-session)
    const b = spawnSync("launchctl", ["blame", String(pid)], { encoding: "utf8", env, timeout: 5000 });
    row.blame = { rc: b.error ? null : b.status, line: (`${b.stdout ?? ""}${b.stderr ?? ""}`.split("\n").find((l) => l.trim() !== "") ?? "").trim() };
  }
  rows.push(row);
}
fs.writeFileSync(out, `${JSON.stringify({ schema: "rightkit.mac-process-table/1", capturedAt, os: `${process.platform} ${process.arch}`, rows }, null, 1)}\n`);
console.log(`captured ${rows.length} processes (${rows.filter((r) => r.self).length} this user's, ${rows.filter((r) => r.self && r.hidden).length} hidden, ${rows.filter((r) => r.blame).length} blamed) at ${capturedAt} -> ${out}`);
