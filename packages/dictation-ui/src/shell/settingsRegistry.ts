// Settings search registry: one entry per user-facing setting row rendered with a `data-setting-id` attribute, merged by
// the host into its command palette; choosing one navigates to the page and reveals the row (`revealSetting`). The entries are
// the host's data (page keys, labels, keywords); this module holds the filtering and the focus and reveal behaviour.
// Ids must match the `data-setting-id` values rendered by the pages.

export type ShellPlatform = "macos" | "ios" | "windows" | "linux";

export interface SettingEntry<Page extends string = string, Section extends string = string> {
  /** Matches the row's `data-setting-id` attribute, e.g. `settings.input-device`. */
  id: string;
  label: string;
  keywords: string[];
  page: Page;
  /** Group name shown as the palette hint: "Settings › Audio". */
  group: string;
  /** Platforms where its rendered control exists. */
  platforms?: readonly ShellPlatform[];
  /** License state where its rendered control exists. */
  licenseTier?: "free" | "pro";
  /** Settings tab that holds the row (`page: "settings"` only); absent for rows in the title bar. */
  section?: Section;
  /** The row renders only while developer mode is on. */
  developerOnly?: boolean;
}

/** The filters over a host's registry entries; the tab, search and palette all read the same list. */
export function createSettingsRegistry<Page extends string, Section extends string>(
  entries: readonly SettingEntry<Page, Section>[],
) {
  function settingsForPlatform(
    platform: ShellPlatform,
    isPro: boolean,
    developerMode = false,
  ): SettingEntry<Page, Section>[] {
    return entries.filter(
      (entry) =>
        (!entry.platforms || entry.platforms.includes(platform)) &&
        (!entry.licenseTier || (entry.licenseTier === "pro") === isPro) &&
        (!entry.developerOnly || developerMode),
    );
  }

  /** The searchable rows of one Settings tab, from the registry so the tab, search and palette agree. */
  function settingsEntriesForSection(
    section: Section,
    platform: ShellPlatform,
    developerMode: boolean,
  ): { id: string; label: string; keywords: string[] }[] {
    return settingsForPlatform(platform, false, developerMode)
      .filter((entry) => entry.page === "settings" && entry.section === section)
      .map(({ id, label, keywords }) => ({ id, label, keywords }));
  }

  /** The Settings tab that holds a row, or undefined for rows that sit outside any tab. */
  function settingsSectionFor(id: string): Section | undefined {
    return entries.find((entry) => entry.page === "settings" && entry.id === id)?.section;
  }

  return { entries, settingsForPlatform, settingsEntriesForSection, settingsSectionFor };
}

const FOCUSABLE = 'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

const FLASH_MS = 1800;
const REVEAL_TIMEOUT_MS = 2500;

// Page switches may suspend while their destination mounts. Focus its title-bar
// heading once the page is available so keyboard users land on visible destination context.
export function focusPageDestination(expectedTab?: string): void {
  const deadline = performance.now() + REVEAL_TIMEOUT_MS;
  const attempt = () => {
    const page = expectedTab
      ? document.querySelector<HTMLElement>(`[data-page="${CSS.escape(expectedTab)}"]`)
      : document.querySelector<HTMLElement>("[data-page]");
    // The page title lives in the title bar, so wait for the page body and focus that heading.
    const heading = page ? document.querySelector<HTMLElement>(".rk-top h1") : null;
    const control = expectedTab ? null : page?.querySelector<HTMLElement>(FOCUSABLE) ?? null;
    const target = heading ?? control;
    if (target) {
      if (target === heading) target.tabIndex = -1;
      target.focus({ preventScroll: true });
      return;
    }
    if (performance.now() < deadline) window.requestAnimationFrame(attempt);
  };
  window.requestAnimationFrame(attempt);
}

// Native dialogs restore focus only while their opener remains usable. A
// removed launcher falls back to current page rather than losing focus.
export function focusConnectedOrPageDestination(opener: HTMLElement | null): void {
  window.requestAnimationFrame(() => {
    if (opener?.isConnected && opener.matches(FOCUSABLE)) {
      opener.focus({ preventScroll: true });
      return;
    }
    focusPageDestination();
  });
}

// After a palette-driven page switch the target page may still be inside its
// lazy Suspense boundary, so poll each frame until the row mounts (bounded).
// Highlights briefly (the CSS animation is stripped under reduced motion, in
// which case the static ring below remains for the same duration) and moves
// focus to the row's first control.
// `focus: false` scrolls and flashes without moving focus, for route aliases
// that land on a section rather than a specific control.
export function revealSetting(id: string, { focus = true }: { focus?: boolean } = {}): void {
  const deadline = performance.now() + REVEAL_TIMEOUT_MS;
  const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
  const attempt = () => {
    const el = document.querySelector<HTMLElement>(`[data-setting-id="${CSS.escape(id)}"]`);
    if (!el) {
      if (performance.now() < deadline) window.requestAnimationFrame(attempt);
      return;
    }
    el.scrollIntoView({ block: "center", behavior: reduced ? "auto" : "smooth" });
    el.classList.add("hr-setting-flash");
    window.setTimeout(() => el.classList.remove("hr-setting-flash"), FLASH_MS);
    const explicitFocusTarget = el.matches('[tabindex="-1"]')
      ? el
      : el.querySelector<HTMLElement>('[tabindex="-1"]');
    const control = explicitFocusTarget ?? (el.matches(FOCUSABLE) ? el : el.querySelector<HTMLElement>(FOCUSABLE));
    if (focus) control?.focus({ preventScroll: true });
  };
  window.requestAnimationFrame(attempt);
}
