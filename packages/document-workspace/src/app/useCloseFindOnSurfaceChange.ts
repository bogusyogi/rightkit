import { useEffect, useRef } from "react";

/**
 * Close the shared find bar whenever a document switches between its rendered surface and its raw
 * source.
 *
 * Both surfaces register a search provider into the SAME path-scoped slot — a CodeMirror "source"
 * provider for the raw text, a rendered provider for the preview/editor. Switching between them
 * therefore swaps the provider underneath an open find bar, silently retargeting the user's query
 * at completely different text with no visible cue that it happened. Closing find on the switch
 * makes the user reopen it against whichever surface they actually meant.
 *
 * `surfaceKey` is anything that identifies the current surface (a boolean like `showSource` is
 * typical); find closes on any change to it.
 */
export function useCloseFindOnSurfaceChange(
  surfaceKey: unknown,
  findOpen: boolean,
  onCloseFind?: () => void,
): void {
  const previousSurfaceRef = useRef(surfaceKey);
  useEffect(() => {
    const previous = previousSurfaceRef.current;
    previousSurfaceRef.current = surfaceKey;
    if (previous !== surfaceKey && findOpen) onCloseFind?.();
  }, [surfaceKey, findOpen, onCloseFind]);
}
