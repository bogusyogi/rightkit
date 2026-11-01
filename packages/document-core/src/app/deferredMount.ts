export function scheduleAfterPaint(
  run: () => void,
  requestFrame: (callback: FrameRequestCallback) => number = requestAnimationFrame,
  cancelFrame: (handle: number) => void = cancelAnimationFrame,
) {
  let secondFrame = 0;
  const firstFrame = requestFrame(() => {
    secondFrame = requestFrame(run);
  });

  return () => {
    cancelFrame(firstFrame);
    if (secondFrame) cancelFrame(secondFrame);
  };
}

interface PaintOrDelayOptions {
  maxDelayMs?: number;
  requestFrame?: (callback: FrameRequestCallback) => number;
  cancelFrame?: (handle: number) => void;
  setTimer?: (callback: () => void, delay: number) => number;
  clearTimer?: (handle: number) => void;
}

/**
 * Wait for two frames when painting normally, but do not deadlock lifecycle work when WebKit
 * suspends animation frames for a hidden window.
 */
export function scheduleAfterPaintOrDelay(
  run: () => void,
  {
    maxDelayMs = 100,
    requestFrame = requestAnimationFrame,
    cancelFrame = cancelAnimationFrame,
    setTimer = window.setTimeout,
    clearTimer = window.clearTimeout,
  }: PaintOrDelayOptions = {},
) {
  let settled = false;
  let timer = 0;
  let cancelFrames = () => {};

  const finish = () => {
    if (settled) return;
    settled = true;
    if (timer) clearTimer(timer);
    cancelFrames();
    run();
  };

  timer = setTimer(finish, maxDelayMs);
  cancelFrames = scheduleAfterPaint(finish, requestFrame, cancelFrame);
  if (settled) cancelFrames();

  return () => {
    if (settled) return;
    settled = true;
    if (timer) clearTimer(timer);
    cancelFrames();
  };
}
