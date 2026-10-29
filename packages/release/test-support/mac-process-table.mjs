// The recorded real-Mac process table (test-support/fixtures/mac-process-table.json) as a build-ownership probe, for tests that must
// judge the hidden-environment rule (item 5 row 5.24a') against a real Mac's table on any host, deterministically.
//
//   macTableProbe({ own, uid, overlay, drop, blame }) -> a probe for `acquireBuildOwnership({ probe })`
//     own      pids that are THIS test's own (default [process.pid]): each, and every ancestor of each, is taken from the live `ps` and
//              laid over the recorded table as a visible, inspected process, so the probe's "own chain" is the live one;
//     uid      the replaying user's uid (default process.getuid()); recorded rows marked `self` get it, the others uid 0;
//     overlay  more rows to add (or replace by pid); drop  pids to remove (a killed gate and its sleeper are gone);
//     blame    { [pid]: { rc, line } } answers that replace the recorded `launchctl blame` ones.
// Anything the table cannot answer (sysctl: the boot identity) is the live host's. `kill` answers "alive" for every pid, since the
// recorded pids are not this host's: liveness comes from the table.
import { processProbe } from "../build-ownership.mjs";
import { liveChainRows, loadMacTable, replaySpawn } from "./mac-replay.mjs";

export { capturedMs, loadMacTable, replaySpawn } from "./mac-replay.mjs";

export function macTableProbe({ own = [process.pid], uid = process.getuid?.(), overlay = [], drop = [], blame = {}, fixture = loadMacTable() } = {}) {
  const spawn = replaySpawn(fixture, { uid, overlay: [...liveChainRows(own), ...overlay], drop, blame });
  return processProbe({ platform: "darwin", spawn, kill: () => true, uid });
}
