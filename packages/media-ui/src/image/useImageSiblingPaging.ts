import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { MediaAdapters } from "../adapters.js";
import type { ImageTool } from "./ImageViewerToolbar.js";

// A horizontal swipe must accumulate this much before paging, and pages can't repeat faster than the
// cooldown, so one trackpad flick advances exactly one image instead of skipping several.
const SIBLING_WHEEL_THRESHOLD = 60;
const SIBLING_WHEEL_COOLDOWN_MS = 320;

interface UseImageSiblingPagingArgs {
  active: boolean;
  currentPath: string;
  tool: ImageTool;
  panning: boolean;
  adapters: MediaAdapters;
  onOpenSibling?: (currentPath: string, targetPath: string) => void;
}

export function useImageSiblingPaging({
  active,
  currentPath,
  tool,
  panning,
  adapters,
  onOpenSibling,
}: UseImageSiblingPagingArgs) {
  const [siblings, setSiblings] = useState<string[]>([]);
  const navAccumRef = useRef(0);
  const navCooldownRef = useRef(false);
  const listImageSiblings = adapters.listImageSiblings;

  useEffect(() => {
    if (!onOpenSibling || !listImageSiblings) {
      setSiblings([]);
      return;
    }
    let alive = true;
    void listImageSiblings(currentPath)
      .then((list) => {
        if (alive) setSiblings(list);
      })
      .catch(() => {
        if (alive) setSiblings([]);
      });
    return () => {
      alive = false;
    };
  }, [currentPath, onOpenSibling, listImageSiblings]);

  const siblingIndex = useMemo(() => siblings.indexOf(currentPath), [siblings, currentPath]);
  const canPrev = siblingIndex > 0;
  const canNext = siblingIndex >= 0 && siblingIndex < siblings.length - 1;
  const goSibling = useCallback(
    (delta: number) => {
      if (siblingIndex < 0) return;
      const next = siblingIndex + delta;
      if (next < 0 || next >= siblings.length) return;
      onOpenSibling?.(currentPath, siblings[next]);
    },
    [siblingIndex, siblings, currentPath, onOpenSibling],
  );

  const pageFromHorizontalWheel = useCallback(
    (horizontalDelta: number): boolean => {
      if (horizontalDelta === 0 || panning || tool !== "view" || siblings.length <= 1) return false;
      if (navCooldownRef.current) return true;
      navAccumRef.current += horizontalDelta;
      if (Math.abs(navAccumRef.current) < SIBLING_WHEEL_THRESHOLD) return true;
      goSibling(navAccumRef.current > 0 ? 1 : -1);
      navAccumRef.current = 0;
      navCooldownRef.current = true;
      window.setTimeout(() => {
        navCooldownRef.current = false;
      }, SIBLING_WHEEL_COOLDOWN_MS);
      return true;
    },
    [goSibling, panning, siblings.length, tool],
  );

  // Left/Right arrows page the folder when the image fits; when zoomed, the stage uses them to pan.
  useEffect(() => {
    if (!active || siblings.length < 2) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) return;
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      if (tool !== "view" || panning) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest?.("input, textarea, select, [contenteditable='true']")) return;
      event.preventDefault();
      goSibling(event.key === "ArrowLeft" ? -1 : 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, goSibling, panning, siblings.length, tool]);

  return {
    siblingIndex,
    siblingTotal: siblings.length,
    canPrev,
    canNext,
    goSibling,
    pageFromHorizontalWheel,
  };
}
