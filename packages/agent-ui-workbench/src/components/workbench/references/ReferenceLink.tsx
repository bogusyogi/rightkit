import { memo } from "react";

import Icon from "@rightkit/agent-ui-core/components/Icon";
import { openExternal } from "@rightkit/agent-ui-core/lib/secureStore";
import type { IconName } from "@rightkit/agent-ui-core/components/Icon";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

// Renders an actionable inline reference link — used by `MarkdownText.tsx`
// when a known reference's destination (or filename) appears in the rendered
// transcript text. Falls back to a plain anchor for `external_link`s via
// `openExternal`, and to inert styled spans for unsafe schemes (javascript:,
// data: URLs whose MIME is not image/*).
//
// Identity is `ReferenceObject.id` — the MarkdownText registry keys on it
// so the same rendered text never collapses two distinct references into one.

export interface ReferenceLinkProps {
  reference: ReferenceObject;
  /** Optional display label override; defaults to the filename / destination. */
  label?: string;
  /** Optional action handler; when absent, external_link defaults to
   * `openExternal` via `secureStore`, while non-link kinds render inert. */
  onAction?: (id: string, action: "open" | "reveal" | "copy_path" | "preview") => void;
}

function isSafeLinkDestination(reference: ReferenceObject): boolean {
  if (reference.kind !== "external_link" && reference.kind !== "repository") return true;
  try {
    const protocol = new URL(reference.destination).protocol;
    return reference.kind === "repository"
      ? protocol === "https:" || protocol === "http:"
      : protocol === "https:" || protocol === "http:" || protocol === "mailto:";
  } catch {
    return false;
  }
}

function iconForReference(reference: ReferenceObject): IconName {
  switch (reference.kind) {
    case "image":
      return "image";
    case "external_link":
      return "link";
    case "workspace_file":
      return "file";
    case "generated_document":
      return "document";
    case "artifact":
      return "package";
    case "change_set":
      return "diff";
    case "repository":
      return "github";
    case "skill":
      return "skill";
    default:
      return "attachment";
  }
}

function ReferenceLinkImpl({ reference, label, onAction }: ReferenceLinkProps) {
  const display = label ?? reference.filename ?? reference.destination;
  const unsafe = !isSafeLinkDestination(reference);

  if (reference.error) {
    return (
      <span
        data-reference-link="true"
        data-reference-id={reference.id}
        data-reference-error="true"
        className="inline-flex items-center gap-1 rounded-md border border-danger/40 bg-danger/10 px-1 py-0.5 font-mono text-[0.92em] text-danger"
        title={reference.error}
      >
        <Icon name="flag" size={11} />
        {display}
      </span>
    );
  }

  if (unsafe) {
    return (
      <span
        data-reference-link="true"
        data-reference-id={reference.id}
        data-reference-unsafe="true"
        className="inline-flex items-center gap-1 rounded-md border border-danger/40 bg-danger/10 px-1 py-0.5 font-mono text-[0.92em] text-danger"
        title={`Unsafe scheme blocked: ${reference.destination}`}
      >
        <Icon name="flag" size={11} />
        {display}
      </span>
    );
  }

  // External links route through secureStore's openExternal — preserves the
  // existing convention for opening http(s) URLs in the user's browser.
  if ((reference.kind === "external_link" || reference.kind === "repository") && !onAction) {
    return (
      <a
        data-reference-link="true"
        data-reference-id={reference.id}
        data-reference-kind={reference.kind}
        href={reference.destination}
        title={reference.destination}
        className={
          reference.kind === "repository"
            ? "inline-flex items-center gap-1.5 rounded-md px-0.5 font-medium text-accent hover:underline"
            : "inline-flex items-center gap-1 text-accent underline decoration-line-2 hover:decoration-accent break-words cursor-pointer"
        }
        rel="noopener noreferrer"
        onClick={(event) => {
          event.preventDefault();
          void openExternal(reference.destination);
        }}
      >
        <Icon name={iconForReference(reference)} size={11} />
        {display}
      </a>
    );
  }

  if (reference.kind === "skill") {
    return (
      <span
        data-reference-link="true"
        data-reference-id={reference.id}
        data-reference-kind="skill"
        data-rich-link="skill"
        className="inline-flex items-center gap-1.5 rounded-md px-0.5 font-medium text-accent"
        title={`Skill: ${display}`}
      >
        <Icon name="skill" size={15} />
        {display}
      </span>
    );
  }

  // Non-link kinds render as actionable buttons. The host application wires
  // these to the appropriate workspace resolver / preview / reveal handler.
  const supportsOpen = (reference.actions ?? []).includes("open");
  const supportsPreview = (reference.actions ?? []).includes("preview");
  const action = supportsPreview ? "preview" : supportsOpen ? "open" : null;

  if (onAction && action) {
    return (
      <button
        type="button"
        data-reference-link="true"
        data-reference-id={reference.id}
        data-reference-kind={reference.kind}
        onClick={() => onAction(reference.id, action)}
        className="inline-flex items-center gap-1 rounded-md border border-line bg-panel px-1.5 py-0.5 text-[0.92em] text-text-2 hover:bg-panel-2 hover:text-text"
      >
        <Icon name={iconForReference(reference)} size={11} />
        {display}
      </button>
    );
  }

  // Last-resort inert span: a known reference with no resolvable handler.
  return (
    <span
      data-reference-link="true"
      data-reference-id={reference.id}
      data-reference-kind={reference.kind}
      className="inline-flex items-center gap-1 rounded-md border border-line bg-panel-2 px-1.5 py-0.5 font-mono text-[0.92em] text-text-3"
      title={reference.destination}
    >
      <Icon name={iconForReference(reference)} size={11} />
      {display}
    </span>
  );
}

export const ReferenceLink = memo(ReferenceLinkImpl);
export default ReferenceLink;
