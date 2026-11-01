export type PillEvent =
  | { kind: "hide" }
  | { kind: "idle" }
  | { kind: "starting" }
  | { kind: "recording" }
  | { kind: "voice"; level: number }
  | { kind: "processing"; cancellable: boolean }
  | { kind: "success" }
  | { kind: "status"; label: string }
  | { kind: "error"; label: string };

export type Mode = "idle" | "starting" | "recording" | "processing" | "status" | "error";

export function modeFor(ev: PillEvent): Mode | null {
  switch (ev.kind) {
    case "idle":
      return "idle";
    case "starting":
      return "starting";
    case "recording":
      return "recording";
    case "processing":
      return "processing";
    case "success":
      return "status";
    case "status":
      return "status";
    case "error":
      return "error";
    default:
      return null; // hide / voice — no mode change
  }
}

// Re-exported from the shared helper so PillOverlay.tsx's existing
// `import { ..., mmss, ... } from "./pillBars.js"` keeps working unchanged (see
// src/lib/time.ts for why this moved out of an inline definition here).
export { mmss } from "../lib/time.js";

// --- Recording / processing bar readout -----------------------------------
// Direct port of the Swift pill's bar buffer (PillModel.swift:386-413,
// integrateLevels) so the Windows pill shows the same live audio bars during
// recording and the same "thinking" sweep during processing.
// Swift integrates a 25-slot rolling buffer every display-link tick, then
// slices a sub-range per state (RecordingCore.bars / ProcessingWaveform.
// centeredLevels, PillRootView.swift:232-234,281-284). We port the same
// buffer + slice shape rather than inventing a smaller-buffer approximation,
// so the displayed segment matches exactly.
export const BAR_BUFFER_SIZE = 25;
export const RECORDING_BAR_COUNT = 6;
export const PROCESSING_BAR_COUNT = 11;

// Recording branch of PillModel.integrateLevels (PillModel.swift:402-412):
// fast attack / slow release per-slot smoothing (0.58 rising, 0.22 falling)
// so speech reads as alive instead of uniformly pulsing.
export function nextRecordingBuffer(previous: number[], level: number, now: number): number[] {
  const base = Math.max(0, (Math.max(0, level) - 0.09) / 0.91) ** 0.72;
  return Array.from({ length: BAR_BUFFER_SIZE }, (_, index) => {
    const position = index / 24;
    const envelope = 0.3 + 0.7 * Math.sin(position * Math.PI) ** 0.72;
    const flutter = base > 0 ? Math.sin(now * (8.4 + (index % 5) * 0.62) + index * 1.41) * 0.22 : 0;
    const target = Math.min(1, Math.max(0.08, base * envelope + flutter * base));
    const prev = previous[index] ?? 0;
    return prev + (target - prev) * (target > prev ? 0.58 : 0.22);
  });
}

// RecordingCore.bars reads the LAST 6 slots of the 25-slot buffer
// (levels.dropFirst(17).prefix(6), PillRootView.swift:232-234).
export function recordingDisplayBars(buffer: number[]): number[] {
  return buffer.slice(17, 17 + RECORDING_BAR_COUNT);
}

// Processing branch of PillModel.integrateLevels (PillModel.swift:387-394): a
// pure sine sweep, not reactive to mic level — there is no live signal to
// react to while transcribing, so Swift deliberately animates a "thinking"
// wave instead.
export function processingBuffer(now: number): number[] {
  return Array.from({ length: BAR_BUFFER_SIZE }, (_, index) => {
    const phase = now * 5.6 - index * 0.48;
    return 0.18 + 0.82 * ((Math.sin(phase) + 1) / 2) ** 2.8;
  });
}

// ProcessingWaveform.centeredLevels reads the CENTER 11 slots of the 25-slot
// buffer (PillRootView.swift:281-284).
export function processingDisplayBars(buffer: number[]): number[] {
  const start = Math.max(0, Math.floor((buffer.length - PROCESSING_BAR_COUNT) / 2));
  return buffer.slice(start, start + PROCESSING_BAR_COUNT);
}

// Bar pixel heights — RecordingBars / ProcessingWaveform frame math
// (PillRootView.swift:250-264, 266-285).
export function recordingBarPixelHeight(level: number): number {
  return Math.max(3.5, 3.5 + Math.min(1, Math.max(0, level)) * 16.5);
}

export function processingBarPixelHeight(level: number): number {
  return Math.max(3, 3 + Math.min(1, Math.max(0, level)) * 16);
}
