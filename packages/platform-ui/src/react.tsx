import { useEffect, useState, type CSSProperties, type ReactElement } from "react";
import { formatChord, getPlatform, loadPlatform, type Platform } from "./index.js";

export interface KbdProps {
  chord: string;
  platform?: Platform;
  gap?: number;
  className?: string;
  keyClassName?: string;
  ariaLabel?: string;
}

/**
 * The native OS is static for the lifetime of a mounted app. The first render uses the synchronous
 * platform; the native `plugin-os` value replaces it once it has loaded (a no-op when unavailable).
 */
export function usePlatform(): Platform {
  const [platform, setPlatform] = useState<Platform>(getPlatform);
  useEffect(() => {
    let live = true;
    void loadPlatform().then((next) => {
      if (live) setPlatform(next);
    });
    return () => {
      live = false;
    };
  }, []);
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
