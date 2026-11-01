import { useCallback, useEffect, useRef, useState } from "react";
import { onEngineEvent } from "../lib/engine.js";
import { beginProgress, initialProgress, type ProgressState, reduceFrame, seedFromStatus } from "../lib/progress.js";

export type { ProgressState } from "../lib/progress.js";

/** Progress of the one engine call the UI is currently following (by tag, then frame id). */
export function useProgress() {
  const [progress, setProgress] = useState<ProgressState>(initialProgress);
  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;
    void onEngineEvent((frame) => setProgress((s) => reduceFrame(s, frame))).then((fn) => {
      if (cancelled) fn();
      else off = fn;
    });
    return () => {
      cancelled = true;
      off?.();
    };
  }, []);
  const seq = useRef(0);
  const begin = useCallback((kind: string, assembleId: string | null = null) => {
    seq.current += 1;
    const tag = `${kind}-${Date.now().toString(36)}-${seq.current}`;
    setProgress(beginProgress(tag, assembleId));
    return tag;
  }, []);
  const patch = useCallback((p: Partial<ProgressState>) => setProgress((s) => ({ ...s, ...p })), []);
  const seed = useCallback(
    (status: Parameters<typeof seedFromStatus>[1]) => setProgress(seedFromStatus(initialProgress, status)),
    [],
  );
  const reset = useCallback(() => setProgress(initialProgress), []);
  return { progress, begin, patch, seed, reset };
}
