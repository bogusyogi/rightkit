import type { CSSProperties, ReactElement } from "react";
import { formatChord, getPlatform, type Platform } from "./index.js";

export interface KbdProps {
  chord: string;
  platform?: Platform;
  gap?: number;
  className?: string;
  keyClassName?: string;
}

export function Kbd({
  chord,
  platform = getPlatform(),
  gap = 4,
  className = "rightkit-kbd-chord",
  keyClassName = "rightkit-kbd-key",
}: KbdProps): ReactElement {
  const keys = formatChord(chord, platform);
  return (
    <span
      className={className}
      aria-label={keys.join(" + ")}
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
