import { useState, type CSSProperties, type ReactElement } from "react";
import { formatChord, getPlatform, type Platform } from "./index.js";

export interface KbdProps {
  chord: string;
  platform?: Platform;
  gap?: number;
  className?: string;
  keyClassName?: string;
  ariaLabel?: string;
}

/** The native OS is static for the lifetime of a mounted app. */
export function usePlatform(): Platform {
  const [platform] = useState<Platform>(getPlatform);
  return platform;
}

export function Kbd({
  chord,
  platform = getPlatform(),
  gap = 4,
  className = "rightkit-kbd-chord",
  keyClassName = "rightkit-kbd-key",
  ariaLabel,
}: KbdProps): ReactElement {
  const keys = formatChord(chord, platform);
  return (
    <span
      className={className}
      aria-label={ariaLabel ?? keys.join(" + ")}
      style={{ "--rightkit-kbd-gap": `${gap}px` } as CSSProperties}
    >
      {keys.map((key, index) => (
        <kbd key={`${key}-${index}`} className={keyClassName}>
          {key}
        </kbd>
      ))}
    </span>
  );
}
