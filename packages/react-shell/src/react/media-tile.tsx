import { useState, type FocusEvent, type HTMLAttributes, type ReactNode } from "react";
import { cx } from "./ui.js";

export interface MediaTileProps extends Omit<HTMLAttributes<HTMLDivElement>, "children" | "placeholder" | "onPointerEnter" | "onPointerLeave" | "onFocus" | "onBlur"> {
  /** Poster image. Without it the placeholder shows. */
  src?: string | null;
  /** Accessible description of the poster. Use an empty string when the tile is decorative. */
  alt: string;
  /** CSS aspect-ratio of the frame. Default `"16 / 9"`. */
  aspect?: string;
  /** `cover` fills the frame and crops; `contain` letterboxes on the `--rk-stage` colour. Default `cover`. */
  fit?: "cover" | "contain";
  /** Muted looping video shown over the poster while the tile is hovered or focused. Not mounted under reduced motion. */
  previewSrc?: string | null;
  /** Content at the bottom end of the frame, typically a `Badge`. */
  badge?: ReactNode;
  /** Content at the top start of the frame, typically a small icon button. */
  corner?: ReactNode;
  /** Shown in place of the poster when there is no `src`. */
  placeholder?: ReactNode;
  className?: string;
}

function prefersReducedMotion(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * A fixed-aspect media tile: poster, optional hover and focus video preview, and badge and corner slots. The root has
 * no inner padding and carries the shell's flush card surface. `rest` and `data-*` go to the root.
 */
export function MediaTile({ src, alt, aspect = "16 / 9", fit = "cover", previewSrc, badge, corner, placeholder, className, ...rest }: MediaTileProps) {
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const previewing = (hovered || focused) && Boolean(previewSrc) && !prefersReducedMotion();

  return (
    <div
      {...rest}
      className={cx("rk-card is-flush rk-media-tile", className)}
      data-fit={fit}
      onPointerEnter={() => setHovered(true)}
      onPointerLeave={() => setHovered(false)}
      onFocus={() => setFocused(true)}
      onBlur={(event: FocusEvent<HTMLDivElement>) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocused(false);
      }}
    >
      <div className="rk-media-tile__frame" style={{ aspectRatio: aspect }}>
        {src ? (
          <img className="rk-media-tile__img" src={src} alt={alt} loading="lazy" decoding="async" />
        ) : (
          <span className="rk-media-tile__placeholder">{placeholder}</span>
        )}
        {previewing && previewSrc ? (
          <video className="rk-media-tile__preview" src={previewSrc} muted loop autoPlay playsInline preload="none" aria-hidden="true" />
        ) : null}
        {corner ? <span className="rk-media-tile__corner">{corner}</span> : null}
        {badge ? <span className="rk-media-tile__badge">{badge}</span> : null}
      </div>
    </div>
  );
}
