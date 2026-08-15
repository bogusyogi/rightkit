import { spawnSync } from "node:child_process";

/**
 * Total CPU seconds reported by `ps` for a process group, summed across every
 * member. Accepts the `[[dd-]hh:]mm:ss[.ff]` forms `ps` emits.
 * Returns null when no row parses, so callers can tell "no CPU used" apart from
 * "could not measure".
 */
export function parseCpuSeconds(text) {
  if (typeof text !== "string") return null;
  let total = null;
  for (const line of text.split("\n")) {
    const value = line.trim();
    if (!value) continue;
    const match = value.match(/^(?:(\d+)-)?(?:(\d+):)?(\d+):(\d+(?:\.\d+)?)$/);
    if (!match) continue;
    const [, days, hours, minutes, seconds] = match;
    total = (total ?? 0)
      + Number(days ?? 0) * 86_400
      + Number(hours ?? 0) * 3_600
      + Number(minutes) * 60
      + Number(seconds);
  }
  return total;
}

/**
 * A link or LTO pass can run for many minutes producing no console output and
 * no new files, which output alone cannot tell apart from a hang. Consuming CPU
 * is the signal that separates the two, so a step burning CPU is never stalled.
 * Returns null when CPU cannot be sampled, which leaves the caller on its
 * output-and-mtime behaviour rather than inventing a verdict.
 */
export function processGroupCpuSeconds(pid, { platform = process.platform, run = spawnSync } = {}) {
  if (!pid || platform === "win32") return null;
  const probe = run("ps", ["-o", "cputime=", "-g", String(pid)], { encoding: "utf8" });
  if (!probe || probe.status !== 0) return null;
  return parseCpuSeconds(probe.stdout ?? "");
}
