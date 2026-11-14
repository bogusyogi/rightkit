import { formatChord as formatPlatformChord } from "@rightkit/platform-ui";
import { hostChordLabels } from "../host.js";
import type { OS } from "./types.js";

/** A chord as key labels for `os`; the host may relabel keys (`DictationHostConfig.chordLabels`). */
export function formatChord(chord: string, os: OS): string[] {
  const labels = hostChordLabels()[os];
  return formatPlatformChord(chord, os, labels ? { labels } : {});
}
