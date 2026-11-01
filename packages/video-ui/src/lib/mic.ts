import { engineCall } from "./engine";

// Typed client for the external-microphone alignment method served by
// cutright-engine (crates/cutright-engine/src/mic_cmd.rs). The engine exposes
// `mic.align` only; there are no attach/list/detach methods.

/** `offset` is microphone source time minus camera source time, in ticks. */
export type MicAlignment = {
  offset: number;
  offset_ms: number;
  lag_samples: number;
  sample_rate: number;
  /** 0..1 normalised correlation score. */
  confidence: number;
  confidence_milli: number;
  overlap_samples: number;
};

export type MicAttachment = {
  camera: string;
  microphone: string;
  offset: number;
  sample_rate: number;
  lag_samples: number;
  confidence_milli: number;
};

export type MicAlignResult = {
  /** `attached` on a new mapping; `already_attached` when the same mapping exists. */
  status: "attached" | "already_attached";
  alignment: MicAlignment;
  attachment: MicAttachment;
  revision: unknown;
};

export type MicAlignOptions = {
  /** Search window in milliseconds, 1..=180000 (engine default 30000). */
  maxOffsetMs?: number;
  /** Fail with a cursor mismatch unless the project is at this cursor. */
  expectCursor?: number;
};

/**
 * Aligns the external microphone source to the camera source and records the
 * source-level mapping. `camera` and `microphone` are source BLAKE3 ids or
 * unambiguous source names. Long-running: pass a `tag` to correlate progress.
 */
export function micAlign(
  dir: string,
  camera: string,
  microphone: string,
  options: MicAlignOptions = {},
  tag?: string,
) {
  const params: Record<string, unknown> = { dir, camera, microphone };
  if (options.maxOffsetMs !== undefined) params.max_offset_ms = options.maxOffsetMs;
  if (options.expectCursor !== undefined) params.expect_cursor = options.expectCursor;
  return engineCall<MicAlignResult>("mic.align", params, tag);
}
