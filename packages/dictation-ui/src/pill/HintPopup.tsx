import { useCallback, useEffect, useRef, useState } from "react";
import { dictationStorageKey } from "../host.js";
import { listenPill, pillClient } from "./pillHost.js";
import { usePillElementRef } from "./usePillElementRef.js";
import { usePillPopupGeometry, type PillPopupGeometryOptions } from "./usePillPopupGeometry.js";
export type ScreenshotDestination = "clipboard" | "disk";
export type HintView = { kind: string };
export interface HintPopupOptions extends PillPopupGeometryOptions {
  /** Host catalog supplies supported feature identifiers, never copy. */
  featureKinds: readonly string[];
}
type PillHintPayload = { kind?: string };

export function useHintPopup({ featureKinds, ...geometry }: HintPopupOptions) {
  const featureKindsRef = useRef(featureKinds);
  featureKindsRef.current = featureKinds;
  const [presentation, setPresentation] = useState(0);
  const [view, setView] = useState<HintView>({ kind: "hidden" });
  const [cardRef, card] = usePillElementRef<HTMLDivElement>();
  const hintHideTimer = useRef<number | null>(null);
  const clearTimers = useCallback(() => {
    if (hintHideTimer.current) window.clearTimeout(hintHideTimer.current);
    hintHideTimer.current = null;
  }, []);

  const dismiss = useCallback(() => {
    clearTimers();
    setView({ kind: "hidden" });
    void pillClient().dismissHint().catch(() => undefined);
  }, [clearTimers]);

  const saveScreenshotPreference = useCallback(
    async (destination: ScreenshotDestination) => {
      clearTimers();
      await pillClient().setScreenshotDestination(destination).catch(() => undefined);
      dismiss();
    },
    [clearTimers, dismiss],
  );

  const applyPayload = useCallback(
    (payload: PillHintPayload | null | undefined) => {
      if (!payload?.kind) return;
      clearTimers();
      setPresentation((value) => value + 1);
      if (payload.kind === "screenshot_prompt") {
        setView({ kind: "screenshot_prompt" });
        return;
      }
      if (payload.kind === "prompt_hint") {
        try {
          if (localStorage.getItem(dictationStorageKey("promptHintDisabled")) === "1") {
            dismiss();
            return;
          }
        } catch {
          // Best effort only.
        }
        setView({ kind: "prompt_hint" });
        hintHideTimer.current = window.setTimeout(() => dismiss(), 6200);
        return;
      }
      if (featureKindsRef.current.includes(payload.kind)) {
        setView({ kind: payload.kind });
        hintHideTimer.current = window.setTimeout(() => dismiss(), 8000);
      }
    },
    [clearTimers, dismiss],
  );

  const takePayload = useCallback(() => {
    void pillClient()
      .takeHintPayload()
      .then((payload) => {
        void pillClient().log("pill_hint_payload_take", `kind=${payload?.kind ?? "none"}`).catch(() => undefined);
        applyPayload(payload);
      })
      .catch(() => undefined);
  }, [applyPayload]);

  useEffect(() => {
    void pillClient().log("pill_hint_frontend_mount", `url=${window.location.href}`).catch(() => undefined);
  }, []);

  useEffect(() => {
    takePayload();
    const unlisten = listenPill("hintShow", () => {
      takePayload();
    });
    const onVisibility = () => {
      if (document.visibilityState !== "hidden") takePayload();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      void unlisten.then((fn) => fn()).catch(() => undefined);
    };
  }, [takePayload]);

  usePillPopupGeometry(
    card, view.kind !== "hidden", presentation,
    (width, height) => pillClient().resizeHint(width, height),
    () => pillClient().hintReady(), geometry,
  );

  useEffect(() => clearTimers, [clearTimers]);

  return { view, cardRef, dismiss, saveScreenshotPreference };
}

export type HintPopupController = ReturnType<typeof useHintPopup>;
export interface HintPopupProps extends HintPopupOptions {
  children: (controller: HintPopupController) => import("react").ReactNode;
}

/** Host supplies copy, layout & style; controller owns native presentation lifecycle. */
export default function HintPopup({ children, ...options }: HintPopupProps) {
  const controller = useHintPopup(options);
  return controller.view.kind === "hidden" ? null : children(controller);
}
