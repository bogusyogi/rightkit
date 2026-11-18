import type { FocusEvent, ReactNode } from "react";
import { usePillAnimation, type PillAnimationOptions } from "./usePillAnimation.js";
import { usePillEvents, usePillEventSubscriptions, type PillLabels } from "./usePillEvents.js";
import { usePillGeometry } from "./usePillGeometry.js";

/** Mount once per native overlay; attach returned refs to persistent host elements. */
export function usePillOverlay(labels: PillLabels = {}, animationOptions: PillAnimationOptions = {}) {
  const events = usePillEvents(labels);
  const geometry = usePillGeometry(events);
  const animation = usePillAnimation(events.mode, animationOptions);
  usePillEventSubscriptions(events, { ...geometry, ...animation, labels });
  return {
    ...events,
    ...geometry,
    ...animation,
    focusProps: {
      onFocusCapture: () => {
        events.clearHoverExit();
        events.hoveredRef.current = true;
        events.setHovered(true);
      },
      onBlurCapture: (event: FocusEvent<HTMLDivElement>) => {
        if (event.currentTarget.contains(event.relatedTarget as Node | null)) return;
        events.clearHoverExit();
        events.hoveredRef.current = events.nativeHoveredRef.current;
        events.setHovered(events.nativeHoveredRef.current);
      },
    },
  };
}

export type PillOverlayController = ReturnType<typeof usePillOverlay>;
export interface PillOverlayProps {
  labels?: PillLabels;
  animation?: PillAnimationOptions;
  children: (controller: PillOverlayController) => ReactNode;
}

/** Host supplies markup, accessible names, visuals & intro presentation. */
export default function PillOverlay({ labels, animation, children }: PillOverlayProps) {
  return children(usePillOverlay(labels, animation));
}
