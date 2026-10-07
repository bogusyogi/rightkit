import { useEffect } from "react";
import type { ImageSize } from "./imageViewerState.js";
import { acquireResource, type MediaAdapters } from "../adapters.js";

interface UseDecodedImageSwapArgs {
  documentPath: string;
  displaySrc: string;
  shownSrc: string;
  imageSize: ImageSize | null;
  adapters: MediaAdapters;
  onDecodedSwap: (src: string, size: ImageSize) => void;
}

export function useDecodedImageSwap({
  documentPath,
  displaySrc,
  shownSrc,
  imageSize,
  adapters,
  onDecodedSwap,
}: UseDecodedImageSwapArgs) {
  // The browser's onLoad establishes that the image is actually resident.
  useEffect(() => {
    if (!documentPath || !shownSrc || !imageSize) return;
    return acquireResource(adapters, {
      kind: "image-decoded",
      documentPath,
      estimatedBytes: imageSize.width * imageSize.height * 4,
    }).release;
  }, [adapters, documentPath, imageSize, shownSrc]);

  useEffect(() => {
    if (!displaySrc || displaySrc === shownSrc) return;
    let alive = true;
    const pre = new Image();
    pre.src = displaySrc;
    const swap = () => {
      if (!alive) return;
      onDecodedSwap(displaySrc, {
        width: Math.max(1, pre.naturalWidth || 1),
        height: Math.max(1, pre.naturalHeight || 1),
      });
    };
    if (pre.decode) pre.decode().then(swap).catch(swap);
    else {
      pre.onload = swap;
      pre.onerror = swap;
    }
    return () => {
      alive = false;
    };
  }, [displaySrc, onDecodedSwap, shownSrc]);
}
