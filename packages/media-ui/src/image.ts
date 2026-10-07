// @rightkit/media-ui/image — fabric-backed image viewer/editor.
// Kept out of the root entry so player-only apps never install `fabric`
// (whose optional native `canvas` dependency forces a build approval).
export { ImageViewer, ImageEditor, type ImageViewerProps } from "./image/ImageViewer.js";
export type { ImageTool } from "./image/ImageViewerToolbar.js";
