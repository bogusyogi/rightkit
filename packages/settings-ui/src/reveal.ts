// Reveal a settings row by its `data-setting-id`, even while its page is still mounting, and filter a host's
// searchable-setting table by platform, licence tier and developer mode. Imports nothing.

const FOCUSABLE = 'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

export interface RevealSettingOptions {
  /** Move focus to the row's first control. Default true; false scrolls and flashes only. */
  focus?: boolean;
  /** Class added to the row for `flashMs`. Default `"is-revealed"`. */
  flashClass?: string;
  /** Default 1800. */
  flashMs?: number;
  /** Poll window for a row that has not mounted yet. Default 2500. */
  timeoutMs?: number;
}

/**
 * Scrolls the row to the centre, flashes it and (by default) focuses its first control. A row inside a lazy
 * boundary may not exist yet, so the lookup repeats each frame until `timeoutMs`. Smooth scrolling is
 * skipped under reduced motion. A row with a `tabindex="-1"` element focuses that element.
 */
export function revealSettingWhenMounted(id: string, options: RevealSettingOptions = {}): void {
  const { focus = true, flashClass = "is-revealed", flashMs = 1800, timeoutMs = 2500 } = options;
  const deadline = performance.now() + timeoutMs;
  const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
  const attempt = () => {
    const el = document.querySelector<HTMLElement>(`[data-setting-id="${CSS.escape(id)}"]`);
    if (!el) {
      if (performance.now() < deadline) window.requestAnimationFrame(attempt);
      return;
    }
    el.scrollIntoView({ block: "center", behavior: reduced ? "auto" : "smooth" });
    el.classList.add(flashClass);
    window.setTimeout(() => el.classList.remove(flashClass), flashMs);
    const explicitFocusTarget = el.matches('[tabindex="-1"]') ? el : el.querySelector<HTMLElement>('[tabindex="-1"]');
    const control = explicitFocusTarget ?? (el.matches(FOCUSABLE) ? el : el.querySelector<HTMLElement>(FOCUSABLE));
    if (focus) control?.focus({ preventScroll: true });
  };
  window.requestAnimationFrame(attempt);
}

/** The fields `filterSettingEntries` reads. */
export interface SettingEntryScope<P extends string = string> {
  /** Platforms where the rendered control exists; absent means all. */
  platforms?: readonly P[];
  /** Licence state where the rendered control exists; absent means both. */
  licenseTier?: "free" | "pro";
  /** The row renders only while developer mode is on. */
  developerOnly?: boolean;
}

/** The entries whose rendered control exists for this platform, licence tier and developer mode. */
export function filterSettingEntries<P extends string, E extends SettingEntryScope<P>>(
  entries: readonly E[],
  scope: { platform: P; isPro: boolean; developerMode?: boolean },
): E[] {
  const { platform, isPro, developerMode = false } = scope;
  return entries.filter(
    (entry) =>
      (!entry.platforms || entry.platforms.includes(platform)) &&
      (!entry.licenseTier || (entry.licenseTier === "pro") === isPro) &&
      (!entry.developerOnly || developerMode),
  );
}
