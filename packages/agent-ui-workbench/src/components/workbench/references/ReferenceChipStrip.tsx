import { memo } from "react";

import ReferenceChip from "./ReferenceChip";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

// Renders the typed-reference chip strip in two modes:
// - composer: horizontal compact thumbnails, tightly packed above the textarea.
// - transcript: wider cards below the user bubble showing kind/size/error/actions.
//
// Both modes share the same onRemove callback shape so the strip can be
// dropped into ComposerIsland or TranscriptBubble without per-site wiring.

export interface ReferenceChipStripProps {
  references: ReferenceObject[];
  onRemove?: (id: string) => void;
  onAction?: (id: string, action: "open" | "reveal" | "copy_path" | "preview") => void;
  /** When true, renders the compact composer layout. */
  compact?: boolean;
  ariaLabel?: string;
}

function ReferenceChipStripImpl({
  references,
  onRemove,
  onAction,
  compact = true,
  ariaLabel,
}: ReferenceChipStripProps) {
  if (!Array.isArray(references) || references.length === 0) return null;
  return (
    <ul
      aria-label={ariaLabel ?? "Attached references"}
      data-reference-strip="true"
      data-reference-strip-mode={compact ? "composer" : "transcript"}
      className={compact ? "flex flex-wrap gap-2" : "mt-2 flex flex-wrap gap-2"}
    >
      {references.map((ref) => (
        <li key={ref.id}>
          <ReferenceChip
            reference={ref}
            onRemove={onRemove}
            onAction={onAction}
            compact={compact}
          />
        </li>
      ))}
    </ul>
  );
}

export const ReferenceChipStrip = memo(ReferenceChipStripImpl);
export default ReferenceChipStrip;
