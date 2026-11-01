import { useState, type ReactNode } from "react";
import { Badge, Button, Card, IconButton, ModalShell, Notice, ProgressBar, cx } from "@rightkit/react-shell/react";
import { Icon } from "./icons.js";

export { Icon, type IconName } from "./icons.js";

export type Tone = "neutral" | "ok" | "warn" | "danger" | "info";

/**
 * Fixed-aspect media tile. The container is the shell Card (flush) and the badge is the shell Badge;
 * the media markup (poster, hover preview, corner action) stays here. `src` is an asset URL or
 * undefined while loading.
 */
export function Thumbnail({
  src,
  aspect = "16 / 9",
  badge,
  corner,
  contain,
  alt = "",
  video,
}: {
  src?: string;
  aspect?: string;
  badge?: string;
  corner?: ReactNode;
  contain?: boolean;
  alt?: string;
  /** When set, a muted looping preview plays while hovered. */
  video?: string;
}) {
  const [hover, setHover] = useState(false);
  return (
    <Card flush className="cr-thumb-card">
      <div
        className={cx("cr-thumb", contain && "cr-thumb--contain")}
        style={{ aspectRatio: aspect }}
        onMouseEnter={() => setHover(true)}
        onMouseLeave={() => setHover(false)}
      >
        {!src && <Icon name="film" width={22} height={22} />}
        {src && <img src={src} alt={alt} loading="lazy" draggable={false} />}
        {video && hover && <video src={video} autoPlay muted loop playsInline />}
        {corner && <div className="cr-thumb__corner">{corner}</div>}
        {badge && (
          <span className="cr-thumb__badge">
            <Badge tone="neutral">{badge}</Badge>
          </span>
        )}
      </div>
    </Card>
  );
}

export function ProgressRow({
  label,
  status,
  tone = "info",
  fraction,
  elapsed,
  error,
  onRetry,
}: {
  label: string;
  status: string;
  tone?: Tone;
  /** 0..1, or undefined for indeterminate while running; null hides the bar. */
  fraction?: number | null;
  elapsed?: string;
  error?: string;
  onRetry?: () => void;
}) {
  return (
    <div className="cr-progress">
      <span className="cr-progress__label">{label}</span>
      <Badge tone={error || tone === "danger" ? "bad" : tone === "info" ? "accent" : tone}>{status}</Badge>
      <span className="cr-progress__time">{elapsed ?? ""}</span>
      {fraction !== null && !error && (
        fraction === undefined ? (
          <div
            className="cr-progress__bar--indeterminate"
            role="progressbar"
            aria-label={label}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <i />
          </div>
        ) : (
          <ProgressBar
            value={fraction * 100}
            label={label}
            tone={tone === "danger" ? "bad" : tone === "ok" || tone === "warn" ? tone : "accent"}
          />
        )
      )}
      {error && (
        <Notice
          tone="bad"
          className="cr-progress__error"
          action={
            onRetry && (
              <Button size="sm" variant="ghost" onClick={onRetry}><Icon name={"rotate-ccw"} />Retry
              </Button>
            )
          }
        >
          <span className="cr-clamp-4" title={error}>{error}</span>
        </Notice>
      )}
    </div>
  );
}

export function Drawer({
  title,
  open,
  onClose,
  children,
  footer,
}: {
  title: string;
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
}) {
  // The shell's modal owns the scrim, Escape (topmost only), focus trap and focus restore;
  // the classes only dock it to the right edge below the title bar.
  if (!open) return null;
  return (
    <ModalShell
      label={title}
      ariaLabel={title}
      onClose={onClose}
      width={400}
      overlayClassName="cr-drawer-overlay"
      className="cr-drawer"
    >
      <div className="cr-drawer__head">
        <h2>{title}</h2>
        <IconButton label="Close" onClick={onClose}><Icon name={"x"} /></IconButton>
      </div>
      <div className="cr-drawer__body">{children}</div>
      {footer && <div className="cr-drawer__foot">{footer}</div>}
    </ModalShell>
  );
}

export function ErrorPanel({
  title,
  message,
  detail,
  actions,
}: {
  title: string;
  /** The actionable reason, always visible; `detail` stays collapsed. */
  message?: string;
  detail?: string;
  actions?: ReactNode;
}) {
  return (
    <Notice tone="bad" title={title} action={actions} className="cr-error">
      {(message || detail) && (
        <>
          {message && <span>{message}</span>}
          {detail && (
            <details>
              <summary>Details</summary>
              <pre>{detail}</pre>
            </details>
          )}
        </>
      )}
    </Notice>
  );
}

/** `mm:ss` under an hour, `h:mm:ss` above. */
export function timecode(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

export function fileSize(bytes: number): string {
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}
