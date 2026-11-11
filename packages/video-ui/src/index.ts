// Video editing components over a host-supplied video engine and preview session.
// Install the host once (`installVideoHost`) before rendering.

// Host contract.
export {
  installVideoHost,
  videoHost,
  type VideoHost,
  type VideoEngineTransport,
  type VideoEngineStatus,
  type VideoEngineState,
  type VideoEngineHello,
  type VideoPlaybackHost,
  type PlaybackMeta,
} from "./host.js";

// Editor.
export { TimelineEditorView, type PreviewHost } from "./editor/TimelineEditorView.js";
export { Timeline, type PreviewSeek } from "./editor/Timeline.js";
export { Inspector } from "./editor/Inspector.js";
export { ColourInspector } from "./editor/ColourInspector.js";
export { CaptionInspector } from "./editor/CaptionInspector.js";
export { TranscriptPanel } from "./editor/TranscriptPanel.js";
export { ChangesPanel } from "./editor/ChangesPanel.js";
export { EditSession, StaleEditError, EngineEditError, type EngineCall, type EditResponse, type RevisionInfo } from "./editor/adapter.js";
export type { EditAction } from "./editor/actions.js";
export * as actions from "./editor/actions.js";
export * from "./editor/model.js";
export * from "./editor/capabilities.js";
export * from "./editor/keys.js";
export * from "./editor/gesture.js";
export * from "./editor/transcript.js";

// Preview.
export { NativePlayer, StagePlayer, useTimelinePreview, type PreviewHost as PlayerPreviewHost } from "./components/Player.js";
export { RangePreview } from "./components/RangePreview.js";

// Typed client of the video engine, over the host.
export {
  engineCall,
  engineStatus,
  engineDoctor,
  engineBusy,
  onEngineEvent,
  revealPath,
  errorCode,
  errorMessage,
  type EngineState,
  type EngineHello,
  type EngineStatus,
  type DoctorCheck,
  type DoctorReport,
  type EngineCallError,
} from "./lib/engine.js";
export * from "./lib/media.js";
export {
  TICKS_PER_SECOND,
  secondsToTicks,
  ticksToSeconds,
  titleLabel,
  projectShow,
  appendClip,
  projectInit,
  fpsValue,
  formatSeconds,
  baseName,
  newClipId,
  type SourceInfo,
  type VideoClip,
  type AudioClip,
  type TitleTemplate,
  type OverlayClip,
  type Timeline as ProjectTimeline,
  type ProjectShow,
} from "./lib/project.js";
export * from "./lib/progress.js";
export { assetUrl } from "./lib/asset.js";
export { mediaSourceId, useMediaPlaybackId, isPlayable, mediaKind } from "./lib/mediaAdapters.js";

// Hooks.
export { useEngine, subscribeDoctor, getDoctor } from "./hooks/useEngine.js";
export { useHdrToSdr, HDR_COLOR_PRESETS, HDR_UNAVAILABLE_NOTICE } from "./hooks/useHdrToSdr.js";

// Small UI pieces the components share.
export * from "./ui/index.js";
