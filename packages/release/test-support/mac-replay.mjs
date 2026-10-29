// A `spawn` that replays the recorded macOS process table (test-support/fixtures/mac-process-table.json, captured by
// capture-mac-process-table.mjs) to build-ownership's `processProbe`, so the hidden-environment rule (item 5 row 5.24a') is judged
// against a real Mac's table deterministically, on any host. `ps`, `ps eww`, `lsof` and `launchctl blame` are answered from the
// fixture; any other command (sysctl, ...) goes to `real`, so a boot identity is the live host's.
//
//   replaySpawn(fixture, { uid, overlay, drop, blame, real })
//     uid      the uid the replaying user's rows get (default process.getuid()); rows the fixture marks `self: false` get uid 0
//     overlay  rows added to (or replacing, by pid) the table: the live test driver's own chain, `liveChainRows()`
//     drop     pids removed from the table (a killed gate and its sleeper are gone)
//     blame    { [pid]: { rc, line } } answers that replace the fixture's
//   liveChainRows() the rows for THIS process and its ancestors, read from the live `ps`, as visible and inspected.
import fs from "node:fs";
import { spawnSync } from "../background-process.mjs";

export const MAC_TABLE_FIXTURE = new URL("./fixtures/mac-process-table.json", import.meta.url);
export const loadMacTable = () => JSON.parse(fs.readFileSync(MAC_TABLE_FIXTURE, "utf8"));
/** The fixture's `capturedAt` ("Thu Oct 29 12:15:05 2026", a local time as `ps lstart` prints it) in ms. */
export const capturedMs = (fixture) => Date.parse(fixture.capturedAt.replace(/\s+/g, " "));

/**
 * `{ pid, ppid, pgid, self, stat, lstart, argv0, hidden:false, lsofInspected:true }` for each of `pids` (default: this process) and every
 * ancestor of each, from the live `ps`. A pid that is not running is left out.
 */
export function liveChainRows(pids = [process.pid]) {
  const r = spawnSync("ps", ["-A", "-o", "pid=,ppid=,pgid=,stat=,lstart=,comm="], { encoding: "utf8", env: { ...process.env, LC_ALL: "C", LANG: "C" } });
  const byPid = new Map();
  for (const line of String(r.stdout).split("\n")) {
    const m = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\S+)\s+(\w{3} \w{3}\s+\d+ \d\d:\d\d:\d\d \d{4})\s+(.*)$/.exec(line);
    if (m) byPid.set(Number(m[1]), { pid: Number(m[1]), ppid: Number(m[2]), pgid: Number(m[3]), self: true, stat: m[4], lstart: m[5].replace(/\s+/g, " "), argv0: "chain", hidden: false, lsofInspected: true });
  }
  const rows = [];
  for (const start of pids) for (let pid = start; pid > 1 && byPid.has(pid) && !rows.some((x) => x.pid === pid); pid = byPid.get(pid).ppid) rows.push(byPid.get(pid));
  return rows;
}

export function replaySpawn(fixture, { uid = process.getuid?.(), overlay = [], drop = [], blame = {}, real = spawnSync } = {}) {
  const dropped = new Set(drop);
  const table = new Map(fixture.rows.filter((r) => !dropped.has(r.pid)).map((r) => [r.pid, r]));
  for (const r of overlay) table.set(r.pid, r);
  const rows = [...table.values()];
  const uidOf = (r) => (r.self ? uid : 0);
  const done = (stdout, status = 0, stderr = "") => ({ status, stdout, stderr, pid: 910_000 + (rows.length % 1000) });
  const column = (n, w) => String(n).padStart(w);
  const lstartOf = (r) => r.lstart;
  const commandOf = (r) => r.argv0;
  const eww = (r) => (r.hidden ? commandOf(r) : `${commandOf(r)} REPLAY_ENV=1`);
  return (command, args, options) => {
    const a = args ?? [];
    if (command === "ps" && a[0] === "eww") return done(rows.map((r) => `${column(r.pid, 6)} ${eww(r)}`).join("\n"));
    if (command === "ps" && a.includes("-p")) {
      const want = new Set(String(a[a.indexOf("-p") + 1]).split(",").map(Number));
      const found = rows.filter((r) => want.has(r.pid));
      return done(found.map((r) => `${column(r.pid, 6)} ${column(r.ppid, 6)} ${lstartOf(r)}`).join("\n"), found.length ? 0 : 1);
    }
    if (command === "ps" && a.includes("lstart=") && a.includes("uid=")) return done(rows.map((r) => `${column(r.pid, 6)} ${column(r.ppid, 6)} ${column(r.pgid, 6)} ${column(uidOf(r), 5)} ${String(r.stat).padEnd(3)} ${lstartOf(r)}     ${commandOf(r)}`).join("\n"));
    if (command === "ps" && a.includes("lstart=")) return done(rows.map((r) => `${column(r.pid, 6)} ${column(r.ppid, 6)} ${column(r.pgid, 6)} ${lstartOf(r)}`).join("\n"));
    if (command === "ps") return done(rows.map((r) => `${column(r.pid, 6)} ${commandOf(r)}`).join("\n"));
    if (command === "lsof") return done(rows.filter((r) => r.lsofInspected).map((r) => `p${r.pid}\nfcwd\nn/`).join("\n"));
    if (command === "launchctl" && a[0] === "blame") {
      const pid = Number(a[1]); const b = blame[pid] ?? table.get(pid)?.blame ?? { rc: 3, line: "Process is not managed by launchd or does not exist." };
      return done(b.rc === 0 ? `${b.line}\n` : "", b.rc, b.rc === 0 ? "" : `${b.line}\n`);
    }
    return real(command, args, options);
  };
}
