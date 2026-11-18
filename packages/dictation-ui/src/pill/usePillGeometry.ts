import { useCallback, useEffect, type RefObject } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { pillClient } from "./pillHost.js";
import { usePillElementRef, type PillElementRef } from "./usePillElementRef.js";
import type { Mode } from "./pillBars.js";

export interface PillGeometry {
  wrapRef: PillElementRef<HTMLDivElement>;
  compactAnchorRef: PillElementRef<HTMLDivElement>;
  pillRef: PillElementRef<HTMLDivElement>;
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
  modeRef: RefObject<Mode>;
  hoveredRef: RefObject<boolean>;
}): PillGeometry {
  const [wrapRef, wrapElement] = usePillElementRef<HTMLDivElement>();
  const [pillRef, pillElement] = usePillElementRef<HTMLDivElement>();
  const [compactAnchorRef, compactElement] = usePillElementRef<HTMLDivElement>();

  useEffect(() => {
    void pillClient().log("pill_frontend_mount", `url=${window.location.href} platform=${getPlatform()}`).catch(() => undefined);
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: pillRef/hoveredRef are stable refs read imperatively on each call.
  const reportPillHitbox = useCallback((reason = "frame") => {
    const pill = pillRef.current;
    if (!pill) return;
    const compactAnchor = compactAnchorRef.current ?? pill;
    const hoverSurface = hoveredRef.current ? pill : compactAnchor;
    const r = hoverSurface.getBoundingClientRect();
    if (!Number.isFinite(r.width) || !Number.isFinite(r.height) || r.width < 1 || r.height < 1) return;
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

  // biome-ignore lint/correctness/useExhaustiveDependencies: wrapRef/modeRef are stable refs measured/read imperatively.
  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    let raf = 0;
    let lastW = 0;
    let lastH = 0;
    const push = () => {
      raf = 0;
      const r = el.getBoundingClientRect();
      const w = Math.ceil(r.width);
      const h = Math.ceil(r.height);
      if (!Number.isFinite(w) || !Number.isFinite(h) || w < 1 || h < 1) return;
      if (w === lastW && h === lastH) return;
      lastW = w;
      lastH = h;
      void pillClient().log("pill_frontend_resize", `wrap=${r.width.toFixed(1)}x${r.height.toFixed(1)} send=${w}x${h} mode=${modeRef.current}`).catch(() => undefined);
      void pillClient().resize(w, h).catch(() => undefined);
      reportPillHitbox("resize");
    };
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => {
      if (!raf) raf = requestAnimationFrame(push);
    });
    ro?.observe(el);
    push();
    return () => {
      ro?.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [reportPillHitbox, wrapElement]);

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
  }, [mode, hovered, statusLabel, modelPreparing, reportPillHitbox, pillElement, compactElement]);

  return { wrapRef, pillRef, compactAnchorRef };
}
