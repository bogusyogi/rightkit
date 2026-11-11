// usePillGeometry — the pill's measured-geometry inputs, native hitbox/size
// reporting, and window-surface bootstrap, extracted from PillOverlay.
//
// Owns:
//   • the transparent-window surface setup + mount log
//   • the wrap/pill DOM refs the rest of the pill measures and attaches
//   • the idle-hover shell's --idle-collapse-sx scale ratio, measured from the
//     rendered dock and published before paint
//   • reportPillHitbox (compact anchor at rest, full capsule while hovered)
//   • the ResizeObserver auto-size push (rAF-coalesced integer-change report)
//   • the mode/hover/status/preparing geometry-settle re-report fan-out
//     ([48, 140, 260]ms) so aux windows follow status collapse
//
// The reporting bodies stay contract-pinned verbatim (scripts/
// test-pill-hover-contract.mjs pins the compact-anchor selection and
// assert-release-contracts.mjs pins the `Math.ceil` size push plus the
// [48, 140, 260]ms re-report cadence + dependency list).

import { useCallback, useEffect, useLayoutEffect, useRef, type RefObject } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { listenPill, pillClient } from "./pillHost.js";
import type { Mode } from "./pillBars.js";

// MUST equal `.hr-pill.idle` width in pillStyles.ts. This is the footprint the
// idle hover shell expands OUT of; if the two drift the expand starts from the
// wrong size.
const IDLE_COLLAPSED_WIDTH_PX = 64;

export interface PillGeometry {
  // Attach to the .hr-wrap element — the content box the ResizeObserver
  // reports to `pill_resize` so the native window hugs it.
  wrapRef: RefObject<HTMLDivElement | null>;
  // Attach to the persistent .hr-pill capsule — the element hitbox reports,
  // the intro bloom, and the shell-ratio measurement all read.
  pillRef: RefObject<HTMLDivElement | null>;
}

export function usePillGeometry({
  mode,
  hovered,
  statusLabel,
  modelPreparing,
  modeRef,
  hoveredRef,
}: {
  mode: Mode;
  hovered: boolean;
  statusLabel: string;
  modelPreparing: boolean;
  // Mirrors owned by usePillEvents: modeRef is read imperatively inside the
  // resize push log; hoveredRef selects the compact-vs-expanded hitbox surface.
  modeRef: RefObject<Mode>;
  hoveredRef: RefObject<boolean>;
}): PillGeometry {
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const pillRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    document.body.style.margin = "0";
    document.body.style.overflow = "hidden";
    void pillClient().log("pill_frontend_mount", `url=${window.location.href} platform=${getPlatform()}`).catch(() => undefined);
  }, []);

  // The idle hover shell (.hr-pill.idle.hover::before) expands with a scale
  // keyframe, so it needs the ratio of the COLLAPSED capsule to the dock it is
  // growing into. The dock is `width:auto` — its width is whatever the mounted
  // buttons measure — so that ratio cannot be a CSS literal (the old hardcoded
  // .5161 assumed Swift's fixed 124px dock and made the shell start far too
  // narrow). Measure the real dock and publish the ratio before paint, so the
  // animation's very first frame already uses it.
  useLayoutEffect(() => {
    const pill = pillRef.current;
    if (!pill) return;
    if (mode !== "idle" || !hovered) {
      pill.style.removeProperty("--idle-collapse-sx");
      return;
    }
    const width = pill.getBoundingClientRect().width;
    if (width < 1) return;
    // Clamp: a ratio >1 would shrink inward instead of expanding outward.
    const ratio = Math.min(1, IDLE_COLLAPSED_WIDTH_PX / width);
    pill.style.setProperty("--idle-collapse-sx", ratio.toFixed(4));
  }, [mode, hovered]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: pillRef/hoveredRef are stable refs read imperatively on each call.
  const reportPillHitbox = useCallback((reason = "frame") => {
    const pill = pillRef.current;
    if (!pill) return;
    const compactAnchor = pill.querySelector<HTMLElement>(".hr-hover-anchor") ?? pill;
    const hoverSurface = hoveredRef.current ? pill : compactAnchor;
    const r = hoverSurface.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return;
    const payload = {
      x: r.left,
      y: r.top,
      width: r.width,
      height: r.height,
    };
    void pillClient().setHitbox(payload).catch((error) => {
      void pillClient().log("pill_hitbox_error", `reason=${reason} error=${String(error).slice(0, 220)}`).catch(() => undefined);
    });
  }, []);

  // Window auto-size: report the pill's content box to Rust so the NSPanel hugs
  // it (slim idle → tall when the clipboard opens). rAF-coalesced; only fires on
  // an integer change to avoid resize chatter.
  // biome-ignore lint/correctness/useExhaustiveDependencies: wrapRef/modeRef are stable refs measured/read imperatively.
  useEffect(() => {
    const el = wrapRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let raf = 0;
    let lastW = 0;
    let lastH = 0;
    const push = () => {
      raf = 0;
      const r = el.getBoundingClientRect();
      const w = Math.ceil(r.width);
      const h = Math.ceil(r.height);
      if (w === lastW && h === lastH) return;
      lastW = w;
      lastH = h;
      void pillClient().log("pill_frontend_resize", `wrap=${r.width.toFixed(1)}x${r.height.toFixed(1)} send=${w}x${h} mode=${modeRef.current}`).catch(() => undefined);
      void pillClient().resize(w, h).catch(() => undefined);
      reportPillHitbox("resize");
    };
    const ro = new ResizeObserver(() => {
      if (!raf) raf = requestAnimationFrame(push);
    });
    ro.observe(el);
    return () => {
      ro.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [reportPillHitbox]);

  // Mode/hover/status/preparing transitions resize or reshape the pill, and
  // an open aux window (clipboard/hint) is anchored to the pill's hitbox — so
  // it must re-report ACROSS the CSS settle window (not just at the end) or the
  // popup lags the shrinking pill during a status->idle collapse. Contract:
  // assert-release-contracts "aux windows follow status collapse".
  // biome-ignore lint/correctness/useExhaustiveDependencies: visual state changes must republish native pill geometry.
  useEffect(() => {
    const rafs: number[] = [];
    const timers: number[] = [];
    const queue = (reason: string) => {
      rafs.push(
        requestAnimationFrame(() => {
          reportPillHitbox(reason);
          rafs.push(requestAnimationFrame(() => reportPillHitbox(`${reason}:settled`)));
        }),
      );
    };
    queue("mode");
    for (const delay of [48, 140, 260]) {
      timers.push(window.setTimeout(() => queue(`mode+${delay}`), delay));
    }
    return () => {
      for (const raf of rafs) cancelAnimationFrame(raf);
      for (const timer of timers) window.clearTimeout(timer);
    };
  }, [mode, hovered, statusLabel, modelPreparing, reportPillHitbox]);

  return { wrapRef, pillRef };
}
