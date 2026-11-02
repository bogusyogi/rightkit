import type { JobCategory } from "./jobDisplay.js";

// Social uses the #a993d8 purple as the canonical tone; the older
// #a78bfa was a second, unrelated purple.
export const SOCIAL_HEX = "#a993d8";

const BASE_TONE: Record<JobCategory, string> = {
  media:
    "border-[color-mix(in_srgb,var(--accent)_40%,var(--line))] bg-[color-mix(in_srgb,var(--accent)_8%,transparent)] text-[var(--accent)]",
  social: `border-[color-mix(in_srgb,${SOCIAL_HEX}_40%,var(--line))] bg-[color-mix(in_srgb,${SOCIAL_HEX}_8%,transparent)] text-[${SOCIAL_HEX}]`,
  document:
    "border-[color-mix(in_srgb,#f59e0b_40%,var(--line))] bg-[color-mix(in_srgb,#f59e0b_8%,transparent)] text-[#f59e0b]",
  web: "border-[color-mix(in_srgb,#10b981_40%,var(--line))] bg-[color-mix(in_srgb,#10b981_8%,transparent)] text-[#10b981]",
  other:
    "border-[var(--line)] bg-[color-mix(in_srgb,var(--panel)_30%,transparent)] text-[var(--muted)]",
};

export const CATEGORY_TONE = BASE_TONE;

/** Border/background/text classes for a category header; unknown categories get none. */
export function categoryToneClass(category: string): string {
  return CATEGORY_TONE[category as JobCategory] ?? "";
}
