const FALLBACK_DELAY_MS = 100;

type PendingFrame = { timer: number };

let users = 0;
let restoreRequested = false;
let originalRequest: typeof window.requestAnimationFrame | null = null;
let originalCancel: typeof window.cancelAnimationFrame | null = null;
const pending = new Map<number, PendingFrame>();

function maybeRestore() {
  if (!restoreRequested || users > 0 || pending.size > 0 || !originalRequest || !originalCancel) {
    return;
  }
  window.requestAnimationFrame = originalRequest;
  window.cancelAnimationFrame = originalCancel;
  originalRequest = null;
  originalCancel = null;
  restoreRequested = false;
}

function install() {
  if (originalRequest || originalCancel) return;

  originalRequest = window.requestAnimationFrame;
  originalCancel = window.cancelAnimationFrame;
  const request = originalRequest;
  const cancel = originalCancel;

  window.requestAnimationFrame = (callback: FrameRequestCallback) => {
    let frame = 0;
    const timer = window.setTimeout(() => {
      if (!pending.delete(frame)) return;
      cancel.call(window, frame);
      maybeRestore();
      callback(performance.now());
    }, FALLBACK_DELAY_MS);

    frame = request.call(window, (timestamp) => {
      if (!pending.delete(frame)) return;
      window.clearTimeout(timer);
      maybeRestore();
      callback(timestamp);
    });
    pending.set(frame, { timer });
    return frame;
  };

  window.cancelAnimationFrame = (frame: number) => {
    const entry = pending.get(frame);
    if (entry) {
      pending.delete(frame);
      window.clearTimeout(entry.timer);
    }
    cancel.call(window, frame);
    maybeRestore();
  };
}

/** Keep pdf.js display rendering moving when WKWebView pauses background animation frames.
 *
 * Wrapper exists only while at least one PDF display render is active. Foreground frames retain
 * native timing; 100 ms timer wins only when WebKit does not deliver a frame.
 */
export function acquirePdfRenderKeepalive(): () => void {
  install();
  users += 1;
  let released = false;

  return () => {
    if (released) return;
    released = true;
    users -= 1;
    if (users === 0) restoreRequested = true;
    maybeRestore();
  };
}
