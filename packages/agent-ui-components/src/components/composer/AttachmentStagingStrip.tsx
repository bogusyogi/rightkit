import type { StagedAttachment } from "@rightkit/agent-ui-hooks/hooks/useAttachmentStaging";

export interface AttachmentStagingStripProps {
  attachments: readonly StagedAttachment[];
  onRemove?: (id: string) => void;
  onRetry?: (attachment: StagedAttachment) => void;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function iconFor(attachment: StagedAttachment): string {
  if (attachment.kind === "folder") return "▰";
  if (attachment.kind === "image") return "▧";
  if (attachment.kind === "audio") return "♫";
  if (attachment.kind === "video") return "▶";
  return "⌑";
}

/**
 * Port of Hermes' attachment-pill presentation. It is metadata-first: image
 * bytes are shown only while available, upload/error/lost states stay
 * actionable, and remove is owned by the composer integration.
 */
export function AttachmentStagingStrip({
  attachments,
  onRemove,
  onRetry,
}: AttachmentStagingStripProps) {
  if (!attachments.length) return null;
  return (
    <section
      data-testid="attachment-staging-strip"
      aria-label="Staged attachments"
      className="flex max-w-full flex-wrap gap-1.5 px-1 pt-1"
    >
      {attachments.map((attachment) => {
        const unavailable = attachment.state === "error" || attachment.state === "lost";
        return (
          <div
            key={attachment.id}
            data-attachment-id={attachment.id}
            data-attachment-state={attachment.state}
            className={`group relative flex min-w-0 max-w-60 items-center gap-2 rounded-lg border px-2 py-1.5 text-left text-xs ${
              unavailable ? "border-danger/50 bg-danger/10" : "border-line bg-panel-2"
            }`}
          >
            <span
              className="grid h-7 w-7 shrink-0 place-items-center overflow-hidden rounded-md border border-line bg-bg text-text-3"
              aria-hidden="true"
            >
              {attachment.previewUrl && attachment.kind === "image" && attachment.state === "ready" ? (
                <img src={attachment.previewUrl} alt="" className="h-full w-full object-cover" />
              ) : (
                iconFor(attachment)
              )}
            </span>
            <span className="min-w-0">
              <span className="block truncate font-medium text-text" title={attachment.name}>
                {attachment.name}
              </span>
              <span className={unavailable ? "block truncate text-danger" : "block text-text-3"}>
                {attachment.error ?? (attachment.state === "uploading" ? "Staging…" : formatBytes(attachment.size))}
              </span>
            </span>
            {onRetry && unavailable ? (
              <button
                type="button"
                aria-label={`Retry ${attachment.name}`}
                className="shrink-0 rounded px-1 text-accent hover:bg-panel"
                onClick={() => onRetry(attachment)}
              >
                Retry
              </button>
            ) : null}
            {onRemove ? (
              <button
                type="button"
                aria-label={`Remove ${attachment.name}`}
                className="absolute -right-2 -top-2 grid h-6 w-6 place-items-center rounded-full border border-line bg-panel text-text-3 hover:text-text"
                onClick={() => onRemove(attachment.id)}
              >
                ×
              </button>
            ) : null}
          </div>
        );
      })}
    </section>
  );
}

export default AttachmentStagingStrip;
