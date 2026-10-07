// @rightkit/media-ui — brand-neutral media player and image viewer/editor.
// Provenance: MediaPlayer merges ScrapeRight's playback player (compat-copy
// fallback, captions, native fullscreen) with GenRight's player (keyboard
// controls, single-active-player exclusivity, retry/proxy error actions).
// ImageViewer/ImageEditor are extracted from ViewRight's image document stack.
//
// Styling: import "@rightkit/media-ui/styles.css" once; theme via the
// --rk-media-* / --rk-iv-* custom properties (set them on :root or any ancestor; see styles.css).
// Image viewer/editor: import from "@rightkit/media-ui/image" (requires `fabric`).

export {
  MediaAdaptersProvider,
  useMediaAdapters,
  type MediaAdapters,
  type MediaFontOption,
  type ImageSavePayload,
  type ResourceRequest,
  type ResourceHandle,
  type ResourceHandleResult,
  normalizeResourceHandle,
} from "./adapters.js";

export {
  MediaPlayer,
  mediaClock,
  mediaErrorMessage,
  DEFAULT_MEDIA_PLAYER_LABELS,
  DEFAULT_MEDIA_SPEEDS,
  type MediaPlayerProps,
  type MediaPlayerLabels,
  type MediaPlayerErrorContext,
  type MediaTrack,
} from "./MediaPlayer.js";
export type { IconName } from "./icons.js";

// ImageViewer / ImageEditor live in "@rightkit/media-ui/image" (they need the
// optional `fabric` peer dependency); everything exported here is fabric-free.
export {
  exportImageEdit,
  scaleEditToSource,
  initialImageEdit,
  hasImageEdits,
  hasAdjustmentEdits,
  sameImageEdit,
  cssFilterForAdjustments,
  cropSourceSize,
  outputScaleForResize,
  exportDimensionsFor,
  validateExportCanvasSize,
  MAX_EXPORT_CANVAS_PIXELS,
  type ImageEdit,
  type ImageAdjustments,
  type CropRect,
  type TextLayer,
  type ExportResize,
} from "./image/imageEdit.js";
export {
  isBrowserImagePath,
  editableImageOutputForPath,
  imageMimeFromPath,
  mimeForOutputExtension,
  defaultImageSaveName,
  ensureImageOutputPath,
  canvasToArrayBuffer,
  type ImageSaveExtension,
  type ImageSaveFunction,
} from "./image/imageSave.js";
export {
  getImageEditSnapshot,
  setImageEditSnapshot,
  clearImageEditSnapshot,
  resetImageEditStoreForTests,
  type ImageEditSnapshot,
} from "./image/imageEditStore.js";
export {
  initialImageTransform,
  fitZoom,
  clampOffset,
  zoomAtPoint,
  fitTransform,
  actualSizeTransform,
  rotateTransform,
  canPan,
  MIN_IMAGE_ZOOM,
  MAX_IMAGE_ZOOM,
  type ImageSize,
  type ImageTransform,
  type ImageViewMode,
} from "./image/imageViewerState.js";
export { clampCrop, defaultCrop } from "./image/cropMath.js";
export {
  cssFontFamilyForImageText,
  fontCssForImageText,
  isBundledImageFontId,
  type ImageFontOptions,
  DEFAULT_IMAGE_FONT_ID,
} from "./image/imageFonts.js";
