import { createContext, useContext, type ReactNode } from "react";

/** One bundled font an app ships (id + display name + CSS font-family stack). */
export interface MediaFontOption {
  id: string;
  name: string;
  stack: string;
}

/** Bytes produced by an image export; the app decides where they persist. */
export interface ImageSavePayload {
  buffer: ArrayBuffer;
  extension: "png" | "jpeg" | "webp";
  mime: string;
  dataUrl?: string;
}

export interface ResourceRequest {
  kind: string;
  documentPath?: string;
  estimatedBytes?: number;
  /** True while the resource holds unsaved user edits. */
  dirty?: boolean;
  /** True when the resource's state survives release (e.g. kept in a host store). */
  stateExternalized?: boolean;
}

/**
 * What `acquireResource` may return: nothing, a plain release function
 * (original contract), or a handle whose `update` receives later changes to
 * `dirty` / `stateExternalized` so the host need not re-acquire.
 */
export interface ResourceHandle {
  release: () => void;
  update?: (next: Partial<ResourceRequest>) => void;
}
export type ResourceHandleResult = (() => void) | ResourceHandle | undefined;

/**
 * App-coupling boundary for @rightkit/media-ui. Every field is optional; the
 * components degrade cleanly without them (no sibling paging, no compat
 * conversion, no native fullscreen fallback, no-op resource tracking).
 *
 * Apps typically provide a `MediaAdaptersProvider` once near the root, or pass
 * `adapters` on each component prop.
 */
export interface MediaAdapters {
  /** Turn a media source (path, url, virtual route) into a playable src. */
  resolveSrc?: (source: string) => string;
  /**
   * Prepare a webview-compatible derivative for `id` and return its playable
   * path/URL. Called once after the native element fails to decode the source;
   * a thrown error becomes the player's explicit codec state. Pair with
   * `rightkit-playback`'s `prepare_media_playback` on Tauri apps.
   */
  preparePlayback?: (request: { id: string; requestId: string }) => Promise<string>;
  /** Cancel an in-flight `preparePlayback` by its requestId (e.g. on unmount). */
  cancelPlayback?: (requestId: string) => void | Promise<void>;
  /**
   * Native window fullscreen fallback for hosts whose webview rejects DOM
   * fullscreen (e.g. `appWindow.setFullscreen` on Tauri).
   */
  setFullscreen?: (on: boolean) => void | Promise<void>;
  /** Same-folder sibling image paths, naturally sorted; enables paging. */
  listImageSiblings?: (path: string) => Promise<string[]>;
  /** Installed system font family names for the image text panel. */
  listSystemFonts?: () => Promise<string[]>;
  /** App-bundled fonts offered in the image text panel. */
  bundledFonts?: MediaFontOption[];
  /**
   * Persist an exported image. Return the saved path/URL, or null when the
   * user cancelled. The package only renders the bytes; the app owns the
   * save dialog and the write.
   */
  saveImage?: (
    payload: ImageSavePayload,
    suggestedName: string,
    sourcePath: string,
  ) => Promise<string | null>;
  /** Print a rendered PNG data URL; resolve false when printing isn't possible. */
  printImage?: (dataUrl: string) => Promise<boolean>;
  /** Resource accounting hook for large decoded images/canvases. */
  acquireResource?: (desc: ResourceRequest) => ResourceHandleResult;
  /** User-facing notification (toast). Defaults to no-op. */
  notify?: (message: string, severity: "info" | "error") => void;
  /** Diagnostic logging hook. */
  logError?: (tag: string) => void;
  /**
   * Font id (from `bundledFonts`) used for newly added text layers. Falls back
   * to the first bundled font, then the package default.
   */
  defaultFontId?: string;
  /** Accent colour for interactive handles (default "#d9803f"). */
  accentColor?: string;
}

const MediaAdaptersContext = createContext<MediaAdapters>({});

export function MediaAdaptersProvider({
  adapters,
  children,
}: {
  adapters: MediaAdapters;
  children: ReactNode;
}) {
  return (
    <MediaAdaptersContext.Provider value={adapters}>
      {children}
    </MediaAdaptersContext.Provider>
  );
}

/** Resolve the effective adapters: per-component prop wins over context. */
export function useMediaAdapters(override?: MediaAdapters): MediaAdapters {
  const fromContext = useContext(MediaAdaptersContext);
  return override ?? fromContext;
}

/** Normalize any adapter return value into a handle with a callable release/update. */
export function normalizeResourceHandle(result: ResourceHandleResult): Required<ResourceHandle> {
  if (typeof result === "function") return { release: result, update: () => {} };
  return {
    release: result?.release ?? (() => {}),
    update: result?.update ?? (() => {}),
  };
}

export function acquireResource(
  adapters: MediaAdapters,
  desc: ResourceRequest,
): Required<ResourceHandle> {
  return normalizeResourceHandle(adapters.acquireResource?.(desc));
}
