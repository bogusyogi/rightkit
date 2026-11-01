/**
 * Explicit capability gating between the editor gestures and what the live
 * engine actually accepts. Every gesture resolves through `supports()`;
 * unsupported controls render inert with a reason, never toast-only success.
 *
 * `extensions` is the optional list of negotiated engine extensions that the
 * engine advertises (rightkit-video-project, project.rs). A capability
 * opens when its extension id is advertised, with no frontend change.
 */

export type EditorCapability =
  /** Single-action `edit` with an `expect_cursor` optimistic guard. Needs engine hunk `edit-expect-cursor`. */
  | "edit.expect_cursor"
  /** One revision for a multi-action gesture. Needs engine hunk `edit.batch`. */
  | "edit.batch"
  /** `split_clip` action on video/audio clips. Needs timeline hunk `action-split`. */
  | "edit.split"
  /** Slip (move source range, keep output span). Needs timeline hunk `action-slip`. */
  | "edit.slip"
  /** `ripple_delete_range` excises an output range across video tracks. */
  | "edit.ripple_delete"
  /** Per-source decoded peak data for waveforms. Needs engine hunk `media.waveform`. */
  | "media.waveform"
  /** Caption-track editing on the timeline. Needs caption contract work. */
  | "edit.captions"
  /** Revision-bound rendered/native preview frames. Host-provided callback. */
  | "preview.frames";

export type CapabilityReport = {
  /** Extensions the engine advertised (e.g. via `project.show` extension). */
  extensions?: readonly string[];
};

const ALWAYS_SUPPORTED = new Set<EditorCapability>([]);

export function supports(cap: EditorCapability, report?: CapabilityReport | null): boolean {
  if (ALWAYS_SUPPORTED.has(cap)) return true;
  return report?.extensions?.includes(cap) ?? false;
}

export type UnsupportedReason = { capability: EditorCapability; reason: string };

const REASONS: Record<EditorCapability, string> = {
  "edit.expect_cursor": "Engine does not yet guard edits by revision cursor",
  "edit.batch": "Engine applies one action per revision; grouped undo needs an engine update",
  "edit.split": "Clip split is not yet a timeline action",
  "edit.slip": "Slip editing is not yet a timeline action",
  "edit.ripple_delete": "Range ripple-delete is not yet a timeline action",
  "media.waveform": "No waveform data endpoint on the engine yet",
  "edit.captions": "Caption-track editing is not yet on the timeline contract",
  "preview.frames": "No revision-bound preview host is connected",
};

export function unsupported(capability: EditorCapability): UnsupportedReason {
  return { capability, reason: REASONS[capability] };
}

// Compatibility shim for the in-flight editor refactor: builds a report from
// the engine-advertised extension ids.
export function reportCapabilities(extensions?: readonly string[]): CapabilityReport {
  return { extensions };
}
