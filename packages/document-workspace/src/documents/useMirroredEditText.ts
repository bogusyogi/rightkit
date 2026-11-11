import { useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";

/**
 * Mirror an externally-owned text value into local editable state.
 *
 * `liveText` is the document's live unsaved edit (useEditText); `text` is the
 * base prop text. The mirror follows `liveText` whenever a live edit exists,
 * and follows `text` only when the prop changes while no live edit exists —
 * so a live local edit is never clobbered by a stale prop.
 *
 * When `sink` is supplied the mirrored value is routed to it instead of local
 * state (MarkdownSplitPane uses this to push external text into the document
 * core's `replaceFromExternal` while the core owns the rendered source).
 * Returns the mirrored text plus its setter for local edits.
 */
export function useMirroredEditText(
  liveText: string | undefined,
  text: string,
  sink?: (next: string) => void,
): [string, Dispatch<SetStateAction<string>>] {
  const [localText, setLocalText] = useState(() => liveText ?? text);
  const propTextRef = useRef(text);
  useEffect(() => {
    const next = liveText ?? text;
    if (sink) {
      sink(next);
      return;
    }
    if (liveText !== undefined || propTextRef.current !== text) setLocalText(next);
    propTextRef.current = text;
  }, [liveText, text, sink]);
  return [localText, setLocalText];
}
