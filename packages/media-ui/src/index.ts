// @rightkit/media-ui — brand-neutral media player and image viewer/editor.
// Provenance: MediaPlayer merges ScrapeRight's playback player (compat-copy
// fallback, captions, native fullscreen) with GenRight's player (keyboard
// controls, single-active-player exclusivity, retry/proxy error actions).
// ImageViewer/ImageEditor are extracted from ViewRight's image document stack.
//
// Styling: import "@rightkit/media-ui/styles.css" once; theme via the
// --rk-media-* / --rk-iv-* custom properties (see styles.css :root defaults).

export {
  MediaAdaptersProvider,
  useMediaAdapters,
  type MediaAdapters,
  type MediaFontOption,
  type ImageSavePayload,
  type ResourceRequest,
} from "./adapters.js";

export {
  MediaPlayer,
  mediaClock,
  mediaErrorMessage,
  type MediaPlayerProps,
  type MediaTrack,
} from "./MediaPlayer.js";

export { ImageViewer, ImageEditor, type ImageViewerProps } from "./image/ImageViewer.js";
export type { ImageTool } from "./image/ImageViewerToolbar.js";
export {
  exportImageEdit,
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
  DEFAULT_IMAGE_FONT_ID,
} from "./image/imageFonts.js";
