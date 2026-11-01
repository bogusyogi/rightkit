// usePillAnimation — the pill's per-frame animation and recording-clock
// ownership, extracted from PillOverlay.
//
// Owns every path that mutates the DOM outside React's render:
//   • the rAF bar loop (recording integrates the live mic level into the
//     25-slot buffer; starting/processing run the mic-independent sweep)
//   • the 250 ms elapsed-timer text write
//   • the launch intro bloom + swell helpers (animatePillIntroBloom /
//     playPillIntroSwell), invoked by the contract-pinned `runIntro`
//     orchestration in usePillEvents.ts
//
// Direct-DOM discipline (do not regress): bar heights are written as
// el.style.height onto fixed <i> children whose JSX style prop is constant,
// and the timer writes textContent — neither goes through setState, so React
// never re-renders the overlay per frame during dictation.
//
// The refs returned here are shared with PillOverlay's contract-pinned
// `pill://event` listener, which owns the writes to `recStart`/`levelRef`
// (see test-delivery-clipboard-contract.mjs). The loops below own the reads.

import { useEffect, useRef, type RefObject } from "react";
import {
  BAR_BUFFER_SIZE,
  mmss,
  nextRecordingBuffer,
  processingBarPixelHeight,
  processingBuffer,
  processingDisplayBars,
  recordingBarPixelHeight,
  recordingDisplayBars,
  type Mode,
} from "./pillBars";

export interface PillAnimation {
  // Written by the view's pinned pill://event listener; read by the timer loop.
  recStart: RefObject<number | null>;
  // Written by the same listener on each `voice` level event; read by the rAF loop.
  levelRef: RefObject<number>;
  // Attach to the recording bar container <span className="hr-bars">.
  recBarsRef: RefObject<HTMLSpanElement | null>;
  // Attach to the starting/processing wave <span className="hr-wave">.
  procBarsRef: RefObject<HTMLSpanElement | null>;
  // Attach to the <span className="timer"> readout.
  timerRef: RefObject<HTMLSpanElement | null>;
}

export function usePillAnimation(mode: Mode): PillAnimation {
  // Live bar readout, ported 1:1 from the Swift pill (pillBars.ts). The buffer
  // lives in a ref and the rAF loop writes bar heights straight to the DOM —
  // routing per-frame updates through state re-rendered the whole overlay
  // ~60x/sec during dictation.
  const recStart = useRef<number | null>(null);
  const levelRef = useRef(0);
  const barsRef = useRef<number[]>(new Array(BAR_BUFFER_SIZE).fill(0));
  const recBarsRef = useRef<HTMLSpanElement | null>(null);
  const procBarsRef = useRef<HTMLSpanElement | null>(null);
  const timerRef = useRef<HTMLSpanElement | null>(null);

  // One rAF loop, alive only in the three states that show bars. Recording
  // integrates the live level; starting/processing sweep independently of it.
  useEffect(() => {
    if (mode !== "recording" && mode !== "processing" && mode !== "starting") {
      barsRef.current = new Array(BAR_BUFFER_SIZE).fill(0);
      return;
    }
    let raf = 0;
    const frame = (nowMs: number) => {
      const now = nowMs / 1000;
      barsRef.current =
        mode === "recording"
          ? nextRecordingBuffer(barsRef.current, levelRef.current, now)
          : processingBuffer(now);
      // Heights go straight to the DOM. The rendered <i> nodes carry a
      // constant style prop, so React's style diff never rewrites these.
      const container = mode === "recording" ? recBarsRef.current : procBarsRef.current;
      if (container) {
        const display =
          mode === "recording"
            ? recordingDisplayBars(barsRef.current)
            : processingDisplayBars(barsRef.current);
        const pixel = mode === "recording" ? recordingBarPixelHeight : processingBarPixelHeight;
        for (let index = 0; index < display.length; index += 1) {
          const el = container.children[index] as HTMLElement | undefined;
          if (el) el.style.height = `${pixel(display[index])}px`;
        }
      }
      raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);
    return () => cancelAnimationFrame(raf);
  }, [mode]);

  useEffect(() => {
    if (mode !== "recording") return;
    // The timer ticks at 4 Hz but only the visible text changes — writing
    // textContent keeps it off the React render path entirely.
    const id = window.setInterval(() => {
      if (recStart.current && timerRef.current) {
        timerRef.current.textContent = mmss(Math.floor((Date.now() - recStart.current) / 1000));
      }
    }, 250);
    return () => window.clearInterval(id);
  }, [mode]);

  return { recStart, levelRef, recBarsRef, procBarsRef, timerRef };
}

// The launch bloom's animation mechanics. The trigger/latch lives in
// usePillEvents.ts's `runIntro` (contract-pinned: see
// test-20260726-release-regressions.mjs); this owns only the animation.
//
// Bloom is driven by JS (rAF + inline style), NOT a CSS @keyframes animation.
// WHY: the CSS bloom worked on macOS (WKWebView) but never rendered on Windows
// (WebView2/Chromium) — a transparent always-on-top overlay + prefers-reduced-
// motion (Windows "Animation effects: Off") + injected-keyframe cascade all
// conspired to no-op it, even with !important. rAF + inline style sidesteps
// every one of those: it ignores the reduced-motion media query, mutating
// inline style forces a WebView2 repaint, and there's no cascade to lose. The
// curve mirrors the old hrIntroBloom (ember ring + blue inner glow + coffee→
// deep-blue bg, peaking at ~45%). Essential "it came alive" feedback (WCAG
// 2.3.3 exempt), so it runs regardless of the OS reduce-motion setting.
//
// `setIntro` is the caller's intro-state setter: the bloom ends it from inside
// the animation itself, never from a competing timer (contract: bloom state
// must end from the animation, and must hand off through the exact steady
// idle pixels).
export function animatePillIntroBloom(
  pillRef: RefObject<HTMLDivElement | null>,
  setIntro: (intro: boolean) => void,
): void {
  const el = pillRef.current;
  if (!el) return;
  const DUR = 2600;
  const PEAK = 0.45;
  const smooth = (t: number) => t * t * (3 - 2 * t); // smoothstep ease
  const start = performance.now();
  const mix = (a: number, b: number, k: number) => Math.round(a + (b - a) * k);
  const frame = (now: number) => {
    if (pillRef.current !== el) return; // element gone / re-rendered
    const p = Math.min(1, (now - start) / DUR);
    // Intensity 0→1 by PEAK, then 1→0 by the end — eased both ways.
    const k = p < PEAK ? smooth(p / PEAK) : smooth(1 - (p - PEAK) / (1 - PEAK));
    el.style.background = `rgb(${mix(0x08, 0x0c, k)},${mix(0x08, 0x16, k)},${mix(0x08, 0x26, k)})`;
    // Drive the SAME ::after rim used by idle. The old element-level
    // box-shadow stacked with ::after, then flashed when it was removed.
    el.style.setProperty("--rim", (0.5 + 0.5 * k).toFixed(3));
    el.style.setProperty("--intro-blur", `${(9 + 15 * k).toFixed(2)}px`);
    el.style.setProperty("--intro-spread", `${(1 + 4 * k).toFixed(2)}px`);
    el.style.setProperty("--intro-glow", `rgba(90,160,255,${(0.55 + 0.28 * k).toFixed(3)})`);
    if (p < 1) {
      requestAnimationFrame(frame);
    } else {
      // End on the exact idle values, then release the custom properties.
      // Both sides of the handoff now render through the same ::after layer.
      el.style.background = "#080808";
      el.style.setProperty("--rim", "0.5");
      el.style.setProperty("--intro-blur", "9px");
      el.style.setProperty("--intro-spread", "1px");
      el.style.setProperty("--intro-glow", "rgba(90,160,255,.55)");
      setIntro(false);
      requestAnimationFrame(() =>
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            if (pillRef.current !== el) return;
            el.style.background = "";
            el.style.removeProperty("--rim");
            el.style.removeProperty("--intro-blur");
            el.style.removeProperty("--intro-spread");
            el.style.removeProperty("--intro-glow");
          }),
        ),
      );
    }
  };
  requestAnimationFrame(frame);
}

// The launch bloom's audible half: a slow C5→G5 sine swell matching the 2.6s
// visual curve. Failure to open an audio context must never suppress the
// bloom, so this stays a self-contained try.
export function playPillIntroSwell(): void {
  try {
    const ctx = new (
      window.AudioContext ||
      (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext
    )();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    const t = ctx.currentTime;
    osc.type = "sine";
    // Slow swell matching the 2.6s bloom: gentle rise to the peak, long fade.
    osc.frequency.setValueAtTime(523.25, t); // C5
    osc.frequency.exponentialRampToValueAtTime(783.99, t + 1.1); // → G5 over the swell
    gain.gain.setValueAtTime(0.012, t);
    gain.gain.exponentialRampToValueAtTime(0.05, t + 0.18);
    gain.gain.exponentialRampToValueAtTime(0.0001, t + 2.5); // fade with the bloom
    osc.connect(gain).connect(ctx.destination);
    osc.start();
    osc.stop(t + 2.55);
    osc.onended = () => void ctx.close().catch(() => undefined);
  } catch {
    /* no audio context — bloom still plays */
  }
}
