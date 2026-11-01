import { isMac } from "../hooks/usePlatform";
import type React from "react";
import { useEffect, useState } from "react";
import {
  Article,
  BookOpenText,
  Camera,
  ChatCircleText,
  ClipboardText,
  MagicWand,
  Microphone,
  Mouse,
  Pause,
  Play,
  ShieldCheck,
  TerminalWindow,
} from "@phosphor-icons/react";
import {
  featureReelControlState,
  featureReelReducedMotionChanged,
  initialFeatureReelPlayback,
  setFeatureReelTransientPause,
  shouldFeatureReelAutoAdvance,
  toggleFeatureReelAutoAdvance,
} from "./featureReelState";

/** Background images for the reel slides. The host supplies its own artwork (setFeatureReelImages). */
export interface FeatureReelImages {
  cityEvening: string;
  galleryCorridor: string;
  haciendaTerrace: string;
  resortLounge: string;
  terraceStillLife: string;
  travelLinen: string;
  villaPool: string;
  whiteStudio: string;
}

let reelImages: Partial<FeatureReelImages> = {};

/** Sets the reel artwork; call once at startup. Slides without an image render on the plain background. */
export function setFeatureReelImages(images: Partial<FeatureReelImages>): void {
  reelImages = { ...images };
}


const reelIcon = "h-[17px] w-[17px] text-[var(--accent)]";

const FEATURES: Array<{
  icon: React.ReactNode;
  title: string;
  badge?: string;
  image: string;
  objectPosition?: string;
  desc: React.ReactNode;
}> = [
  {
    icon: <ShieldCheck className={reelIcon} weight="fill" aria-hidden="true" />,
    title: "On-device",
    get image() {
      return reelImages.villaPool ?? "";
    },
    objectPosition: "center 54%",
    desc: <>Fast dictation runs locally. Your audio never leaves your device.</>,
  },
  {
    icon: <Microphone className={reelIcon} aria-hidden="true" />,
    title: "Push-to-talk",
    get image() {
      return reelImages.haciendaTerrace ?? "";
    },
    objectPosition: "center 48%",
    desc: (
      <>
        Hold <code className="[font-family:var(--font-mono)] text-[var(--paper)]">{isMac() ? "Right Option" : "Right Alt"}</code> or{" "}
        <code className="[font-family:var(--font-mono)] text-[var(--paper)]">{isMac() ? "Control" : "Right Ctrl"}</code> to speak
        only when you mean it.
      </>
    ),
  },
  {
    icon: <Mouse className={reelIcon} aria-hidden="true" />,
    title: "Mouse capture",
    get image() {
      return reelImages.resortLounge ?? "";
    },
    objectPosition: "center 56%",
    desc: <>Start or stop recording from the mouse with the middle-click trigger.</>,
  },
  {
    icon: <TerminalWindow className={reelIcon} aria-hidden="true" />,
    title: "Voice commands",
    get image() {
      return reelImages.galleryCorridor ?? "";
    },
    objectPosition: "center 52%",
    desc: (
      <>
        Say <code className="[font-family:var(--font-mono)] text-white">copy</code>,{" "}
        <code className="[font-family:var(--font-mono)] text-white">paste</code>,{" "}
        <code className="[font-family:var(--font-mono)] text-white">select all</code>, or{" "}
        <code className="[font-family:var(--font-mono)] text-white">switch window</code>.
      </>
    ),
  },
  {
    icon: <Camera className={reelIcon} aria-hidden="true" />,
    title: "Screenshots",
    get image() {
      return reelImages.terraceStillLife ?? "";
    },
    objectPosition: "center 60%",
    desc: (
      <>
        Say <code className="[font-family:var(--font-mono)] text-white">screenshot</code> and save
        to the clipboard, or save to Pictures while keeping a clipboard copy.
      </>
    ),
  },
  {
    icon: <MagicWand className={reelIcon} aria-hidden="true" />,
    title: "AI polish",
    get image() {
      return reelImages.whiteStudio ?? "";
    },
    objectPosition: "center 58%",
    desc: <>Use your own supported provider key for smarter cleanup, with local fallback.</>,
  },
  {
    icon: <ChatCircleText className={reelIcon} aria-hidden="true" />,
    title: "Prompt mode",
    get image() {
      return reelImages.cityEvening ?? "";
    },
    objectPosition: "center 52%",
    desc: (
      <>
        End with <code className="[font-family:var(--font-mono)] text-white">prompt</code> to shape
        rough speech for AI chats and coding tools.
      </>
    ),
  },
  {
    icon: <Article className={reelIcon} aria-hidden="true" />,
    title: "Summaries",
    get image() {
      return reelImages.travelLinen ?? "";
    },
    objectPosition: "center 56%",
    desc: (
      <>
        End with <code className="[font-family:var(--font-mono)] text-white">summarize</code> to
        turn a messy capture into a cleaner brief.
      </>
    ),
  },
  {
    icon: <BookOpenText className={reelIcon} aria-hidden="true" />,
    title: "Personal memory",
    get image() {
      return reelImages.resortLounge ?? "";
    },
    objectPosition: "center 48%",
    desc: <>Vocabulary, snippets, and history keep names, phrases, and recent dictations close.</>,
  },
  {
    icon: <ClipboardText className={reelIcon} aria-hidden="true" />,
    title: "Mini clipboard",
    get image() {
      return reelImages.haciendaTerrace ?? "";
    },
    objectPosition: "center 54%",
    desc: <>Hover the pill after dictation to copy or reopen recent text without breaking flow.</>,
  },
];

export default function FeatureReel({
  className = "",
  rounded = true,
}: {
  className?: string;
  // Onboarding wants a centered, shadowed card; Home wants a full-bleed band
  // flush with the page edges, so the radius isn't baked in unconditionally.
  rounded?: boolean;
}) {
  const [feat, setFeat] = useState(0);
  const [playback, setPlayback] = useState(() => {
    const media = typeof window === "undefined" ? null : window.matchMedia?.("(prefers-reduced-motion: reduce)");
    return initialFeatureReelPlayback(media?.matches ?? false);
  });
  const { autoAdvanceEnabled, pausedByHover, pausedByFocus } = playback;
  const effectiveAuto = shouldFeatureReelAutoAdvance(
    autoAdvanceEnabled,
    pausedByHover,
    pausedByFocus,
  );
  const controlState = featureReelControlState(autoAdvanceEnabled);
  const f = FEATURES[feat];

  useEffect(() => {
    const media = window.matchMedia?.("(prefers-reduced-motion: reduce)");
    if (!media) return;
    const onReducedMotionChange = (event: MediaQueryListEvent) => {
      setPlayback((current) => featureReelReducedMotionChanged(current, event.matches));
    };
    media.addEventListener("change", onReducedMotionChange);
    return () => media.removeEventListener("change", onReducedMotionChange);
  }, []);

  useEffect(() => {
    if (!effectiveAuto) return;
    const id = window.setInterval(
      () => setFeat((current) => (current + 1) % FEATURES.length),
      4200,
    );
    return () => window.clearInterval(id);
  }, [effectiveAuto]);

  // Only the first frame loads eagerly (the rendered <img>). The rest warm
  // the browser's image cache during idle time so mount doesn't fetch/decode
  // eight PNGs up front, while an auto-advance tick still never waits on a
  // network/decode round trip in practice (the "blank strip" symptom).
  useEffect(() => {
    let cancelled = false;
    const preloadRest = () => {
      if (cancelled) return;
      const firstImage = FEATURES[0]?.image;
      for (const item of FEATURES) {
        if (item.image === firstImage) continue;
        const img = new Image();
        img.src = item.image;
      }
    };
    let cancel: () => void;
    if (typeof window.requestIdleCallback === "function") {
      const id = window.requestIdleCallback(preloadRest, { timeout: 2000 });
      cancel = () => window.cancelIdleCallback(id);
    } else {
      const id = window.setTimeout(preloadRest, 1200);
      cancel = () => window.clearTimeout(id);
    }
    return () => {
      cancelled = true;
      cancel();
    };
  }, []);

  return (
    <section
      aria-roledescription="carousel"
      aria-label="Feature highlights"
      className={`relative h-[148px] overflow-hidden ${rounded ? "rounded-[var(--radius-lg)]" : ""} ${className}`}
      // Pointer hover and focus within the reel pause only the current interval;
      // they do not change the explicit Pause/Resume control state.
      onMouseEnter={() =>
        setPlayback((current) => setFeatureReelTransientPause(current, "hover", true))
      }
      onMouseLeave={() =>
        setPlayback((current) => setFeatureReelTransientPause(current, "hover", false))
      }
      onFocus={() =>
        setPlayback((current) => setFeatureReelTransientPause(current, "focus", true))
      }
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
          setPlayback((current) => setFeatureReelTransientPause(current, "focus", false));
        }
      }}
    >
      <img
        src={f.image}
        alt=""
        aria-hidden="true"
        // No `key` here: this element is never unmounted between reel
        // frames. Every image is preloaded above, so a bare `src` swap is
        // an instant repaint (no load gap, no blank strip). A remount on
        // every tick would briefly leave nothing painted while the new node
        // re-fetches and re-decodes an image the browser had already cached.
        className="absolute inset-0 h-full w-full object-cover"
        style={{ objectPosition: f.objectPosition ?? "center" }}
      />
      <div className="absolute inset-0 bg-[rgba(15,12,10,0.34)]" />
      <div
        // No `key` here either, for the same reason: remounting this box
        // every tick replayed `hr-reel-fade`'s `opacity: 0 -> 1` entrance
        // animation from scratch, and any interruption mid-replay (a
        // background tab throttling rAF, a reduced-motion toggle firing
        // between frames, etc.) could leave the caption stuck at the
        // animation's 0% opacity keyframe. Keeping this node mounted lets
        // the fade play once on first mount and then hold at opacity 1;
        // content updates via normal React reconciliation.
        className="hr-reel-fade absolute left-[18px] top-[16px] max-w-[64%] rounded-[var(--radius-sm)] bg-[rgba(15,12,10,0.82)] px-[12px] py-[10px]"
      >
        <div className="mb-[6px] flex items-center gap-[8px]">
          {f.icon}
          <span className="[font-family:var(--font-display)] text-[length:var(--size-xl)] uppercase leading-none tracking-[0.02em] text-[var(--paper)]">
            {f.title}
          </span>
          {f.badge ? (
            <span className="rounded-full border border-[color-mix(in_srgb,var(--accent),transparent_50%)] px-[8px] py-[1px] text-[length:var(--size-xs)] font-medium text-[var(--accent)]">
              {f.badge}
            </span>
          ) : null}
        </div>
        <div className="text-[length:var(--size-sm)] text-[var(--paper)]">{f.desc}</div>
      </div>
      <div className="absolute right-[16px] top-[14px] flex items-center gap-[8px]">
        <button
          type="button"
          aria-label={controlState.label}
          aria-pressed={controlState.paused}
          onClick={() => setPlayback(toggleFeatureReelAutoAdvance)}
          className="grid h-[28px] w-[28px] place-items-center rounded-full text-[var(--paper)]/80 transition-colors duration-150 hover:text-[var(--accent)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
        >
          {autoAdvanceEnabled ? (
            <Pause className="h-[12px] w-[12px]" weight="fill" aria-hidden="true" />
          ) : (
            <Play className="h-[12px] w-[12px]" weight="fill" aria-hidden="true" />
          )}
        </button>
        <div className="flex gap-[8px]">
          {FEATURES.map((item, i) => (
            <button
              key={item.title}
              type="button"
              aria-label={`Show ${item.title}`}
              className="h-[7px] rounded-full transition-[transform,background-color] duration-200"
              style={{
                width: 7,
                // transform, not width: no layout work per frame.
                transform: i === feat ? "scaleX(2.57)" : "none",
                background: i === feat ? "var(--accent)" : "color-mix(in srgb, var(--paper) 32%, transparent)",
              }}
              onClick={() => setFeat(i)}
            />
          ))}
        </div>
      </div>
    </section>
  );
}
