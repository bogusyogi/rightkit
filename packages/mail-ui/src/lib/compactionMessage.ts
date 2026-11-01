// Pure result copy for "Compact database". No React, no IPC — unit-tested in
// tests/compactionMessage.test.ts.
import { formatBytes } from "@rightkit/settings-ui/core";
import type { CompactionResult } from "./ipc/system.js";

export function compactionResultMessage(
  result: Pick<CompactionResult, "before_bytes" | "after_bytes" | "seconds" | "rebuild_registered">,
): string {
  const reclaimed = Math.max(0, result.before_bytes - result.after_bytes);
  const seconds = Math.max(0, Math.round(result.seconds));
  const base = `Reclaimed ${formatBytes(reclaimed)} in ${seconds} s.`;
  return result.rebuild_registered
    ? `${base} Search is being re-indexed in the background.`
    : base;
}
