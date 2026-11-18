import { useEffect, useRef, type RefObject } from "react";
import { BAR_BUFFER_SIZE, nextRecordingBuffer, processingBuffer, type Mode } from "./pillBars.js";

export interface PillAnimationFrame {
  mode: Mode;
  /** Normalized samples; host selects shape, count & pixel mapping. */
  levels: readonly number[];
  elapsedSeconds: number;
}
export interface PillAnimationOptions {
  /** Imperative host renderer; no React updates are required per frame. */
  onFrame?: (frame: PillAnimationFrame) => void;
}
export interface PillAnimation {
  recStart: RefObject<number | null>;
  levelRef: RefObject<number>;
}

export function usePillAnimation(mode: Mode, options: PillAnimationOptions = {}): PillAnimation {
  const recStart = useRef<number | null>(null);
  const levelRef = useRef(0);
  const barsRef = useRef<number[]>(new Array(BAR_BUFFER_SIZE).fill(0));
  const onFrameRef = useRef(options.onFrame);
  onFrameRef.current = options.onFrame;

  useEffect(() => {
    if (mode !== "recording" && mode !== "processing" && mode !== "starting") {
      barsRef.current = new Array(BAR_BUFFER_SIZE).fill(0);
      onFrameRef.current?.({ mode, levels: barsRef.current, elapsedSeconds: 0 });
      return;
    }
    let raf = 0;
    const frame = (nowMs: number) => {
      const now = nowMs / 1000;
      barsRef.current = mode === "recording"
        ? nextRecordingBuffer(barsRef.current, levelRef.current, now)
        : processingBuffer(now);
      onFrameRef.current?.({
        mode,
        levels: barsRef.current,
        elapsedSeconds: mode === "recording" && recStart.current !== null
          ? Math.max(0, (Date.now() - recStart.current) / 1000)
          : 0,
      });
      raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);
    return () => cancelAnimationFrame(raf);
  }, [mode]);

  return { recStart, levelRef };
}
