import { memo, useCallback, useRef } from "react";

import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { IconName } from "@rightkit/agent-ui-core/components/Icon";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

// Single reference chip rendered in the composer chip strip and in the
// `TranscriptBubble` reference strip. Mirrors the keyboard affordances of the
// existing image chip in `ComposerIsland.tsx` (focus → Backspace removes) but
// routes through a typed ReferenceObject rather than the legacy
// `Attachment`-bytes index.

export interface ReferenceChipProps {
  reference: ReferenceObject;
  onRemove?: (id: string) => void;
  onAction?: (id: string, action: "open" | "reveal" | "copy_path" | "preview") => void;
  /** When true, the chip shows compact width (composer); when false, the
   * wider transcript strip layout. */
  compact?: boolean;
}

function iconForKind(kind: string): IconName {
  switch (kind) {
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

function humanSize(bytes: number | undefined): string | null {
  if (bytes === undefined || bytes === null || Number.isNaN(bytes)) return null;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function ReferenceChipImpl({ reference, onRemove, onAction, compact = true }: ReferenceChipProps) {
  const idAttr = `cr-ref-chip-${reference.id}`;
  const buttonRef = useRef<HTMLButtonElement | null>(null);

  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLFieldSetElement>) => {
      if ((event.key === "Backspace" || event.key === "Delete") && onRemove) {
        event.preventDefault();
        onRemove(reference.id);
      } else if (event.key === "Enter" && onAction) {
        event.preventDefault();
        onAction(reference.id, "open");
      }
    },
    [onRemove, onAction, reference.id],
  );

  const handleAction = useCallback(
    (action: "open" | "reveal" | "copy_path" | "preview") => () => {
      if (onAction) onAction(reference.id, action);
    },
    [onAction, reference.id],
  );

  const label = reference.filename ?? reference.destination;
  const size = humanSize(reference.sizeBytes);
  const mime = reference.mime;
  const showError = Boolean(reference.error);
  const supportsOpen = (reference.actions ?? []).includes("open");
  const supportsPreview = (reference.actions ?? []).includes("preview");

  if (reference.kind === "repository" || reference.kind === "skill") {
    return (
      <fieldset
        id={idAttr}
        aria-label={`Reference ${label}`}
        onKeyDown={handleKeyDown}
        data-reference-id={reference.id}
        data-reference-kind={reference.kind}
        className="inline-flex h-7 max-w-[240px] items-center gap-1.5 rounded-md border border-line bg-panel-2 px-2 text-accent focus:outline focus:outline-2 focus:outline-accent"
      >
        {reference.kind === "repository" && onAction ? (
          <button
            type="button"
            aria-label={`Open ${label}`}
            onClick={handleAction("open")}
            className="inline-flex min-w-0 flex-1 items-center gap-1.5 text-left"
          >
            <Icon name="github" size={15} />
            <span className="min-w-0 flex-1 truncate text-[12px] font-medium">{label}</span>
          </button>
        ) : (
          <>
            <Icon name={iconForKind(reference.kind)} size={15} />
            <span className="min-w-0 flex-1 truncate text-[12px] font-medium">{label}</span>
          </>
        )}
        {onRemove ? (
          <button
            ref={buttonRef}
            type="button"
            aria-label={`Remove ${label}`}
            title="Remove"
            onClick={() => onRemove(reference.id)}
            className="grid h-6 w-6 place-items-center rounded-full text-text-3 hover:bg-panel hover:text-text"
          >
            <Icon name="close" size={10} />
          </button>
        ) : null}
      </fieldset>
    );
  }

  return (
    <fieldset
      id={idAttr}
      aria-label={`Reference ${label}`}
      onKeyDown={handleKeyDown}
      data-reference-id={reference.id}
      data-reference-kind={reference.kind}
      className={
        compact
          ? "relative h-16 w-16 overflow-hidden rounded-lg border border-line bg-bg focus:outline focus:outline-2 focus:outline-accent"
          : "relative flex min-w-[160px] max-w-[260px] flex-col gap-1 overflow-hidden rounded-lg border border-line bg-bg p-2 focus:outline focus:outline-2 focus:outline-accent"
      }
    >
      {reference.kind === "image" && reference.previewUrl ? (
        <img
          src={reference.previewUrl}
          alt={label}
          className={compact ? "h-full w-full object-cover" : "h-16 w-full rounded-md object-cover"}
          loading="lazy"
        />
      ) : (
        <div
          className={
            compact
              ? "flex h-full w-full items-center justify-center bg-panel-2 text-text-2"
              : "flex h-16 w-full items-center justify-center rounded-md bg-panel-2 text-text-2"
          }
          aria-hidden="true"
        >
          <Icon name={iconForKind(reference.kind)} size={compact ? 20 : 18} />
        </div>
      )}
      {compact && onRemove ? (
        <button
          ref={buttonRef}
          type="button"
          aria-label={`Remove ${label}`}
          title="Remove"
          onClick={() => onRemove(reference.id)}
          className="absolute right-1 top-1 grid h-6 w-6 place-items-center rounded-full border border-line bg-panel text-text-3 hover:text-text"
        >
          <Icon name="close" size={10} />
        </button>
      ) : null}
      {!compact ? (
        <div className="flex flex-col gap-0.5">
          <span className="truncate font-mono text-[12px] text-text" title={label}>
            {label}
          </span>
          <div className="flex items-center gap-1.5 text-[11px] text-text-3">
            <Icon name={iconForKind(reference.kind)} size={10} />
            <span>{reference.kind}</span>
            {size ? <span aria-hidden="true">·</span> : null}
            {size ? <span>{size}</span> : null}
            {mime ? <span aria-hidden="true">·</span> : null}
            {mime ? <span className="truncate">{mime}</span> : null}
          </div>
          {showError ? (
            <div
              role="status"
              data-reference-error="true"
              className="mt-1 inline-flex items-center gap-1 rounded-md border border-danger/40 bg-danger/10 px-1.5 py-0.5 text-[11px] text-danger"
            >
              <Icon name="flag" size={9} />
              <span className="truncate">{reference.error}</span>
            </div>
          ) : null}
          <div className="mt-1 flex flex-wrap gap-1">
            {supportsOpen && onAction ? (
              <button
                type="button"
                aria-label={`Open ${label}`}
                onClick={handleAction("open")}
                className="inline-flex h-5 items-center gap-1 rounded-md border border-line bg-panel px-1.5 text-[10px] text-text-2 hover:bg-panel-2 hover:text-text"
              >
                <Icon name="link" size={9} />
                Open
              </button>
            ) : null}
            {supportsPreview && onAction ? (
              <button
                type="button"
                aria-label={`Preview ${label}`}
                onClick={handleAction("preview")}
                className="inline-flex h-5 items-center gap-1 rounded-md border border-line bg-panel px-1.5 text-[10px] text-text-2 hover:bg-panel-2 hover:text-text"
              >
                <Icon name="document" size={9} />
                Preview
              </button>
            ) : null}
            {onRemove ? (
              <button
                type="button"
                aria-label={`Remove ${label}`}
                onClick={() => onRemove(reference.id)}
                className="inline-flex h-5 items-center gap-1 rounded-md border border-line bg-panel px-1.5 text-[10px] text-text-2 hover:bg-panel-2 hover:text-text"
              >
                <Icon name="close" size={9} />
                Remove
              </button>
            ) : null}
          </div>
        </div>
      ) : null}
    </fieldset>
  );
}

export const ReferenceChip = memo(ReferenceChipImpl);
export default ReferenceChip;
