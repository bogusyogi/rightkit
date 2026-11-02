// The account platforms the desktop shows. Ids are the engine's exact lowercase ids. Nothing here names a
// platform rule, limit or capability: those come from engine data at run time (see docs/design p9 section 22).
import type { SocialPlatform } from "./types.js";

/**
 * Every account platform the engine knows, in the engine's order: PLATFORMS in
 * crates/protocol/src/accounts.rs:22-33. Keep in step with the SocialPlatform union in types.ts.
 */
export const SOCIAL_PLATFORMS: readonly SocialPlatform[] = [
  "instagram",
  "facebook",
  "threads",
  "youtube",
  "tiktok",
  "x",
  "linkedin",
  "pinterest",
  "bluesky",
  "mastodon",
];

/** True when the id is an account platform the desktop shows. A draft-only id such as "reddit" is not one. */
export function isSocialPlatform(value: string): value is SocialPlatform {
  return (SOCIAL_PLATFORMS as readonly string[]).includes(value);
}

/** Draft-only platform ids the engine accepts beside the account vocabulary (drafts.rs, DRAFT_ONLY_PLATFORMS). */
export const DRAFT_ONLY_PLATFORMS: readonly string[] = ["reddit"];

/** True when the id is a platform the desktop shows: an account platform or a draft-only platform. */
export function isKnownPlatform(value: string): boolean {
  return isSocialPlatform(value) || DRAFT_ONLY_PLATFORMS.includes(value);
}
