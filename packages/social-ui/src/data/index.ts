// Public surface of the operator data layer. Screens import from here, never from mock/ or engine/.
import { isTauri } from "../lib/host";
import { EngineDataSource } from "./engine/engineDataSource";
import type { DataSource } from "./source";

export type * from "./types";
export type { DataSource, MockControls, DataError, DataErrorCode, EngineOnlyErrorCode, Result, UnmappedAccount } from "./source";
export { SOCIAL_PLATFORMS, isSocialPlatform } from "./platforms";
export { EngineDataSource } from "./engine/engineDataSource";
export { accountOrdinals, awaitingDecision, charCount, isApproved, jobsByAccount, mediaReady, shortHash } from "./derive";
export type { AccountOrdinal } from "./derive";
export * from "./time";
export { disclosureFound, firstSentence } from "./draftRules";
export { formatMicro, formatMilli, groupDigits, monthOfInstant } from "./money";

/**
 * The source the desktop runs on. A release build is the engine only: the mock, its JSON fixtures and the shared
 * data helpers load through a dynamic import behind import.meta.env.DEV || __RIGHTKIT_MOCK__ (RIGHTKIT_MOCK=1, UI QA only), which Vite folds to false, so they are not
 * in dist/ (right-release dist-scan, declared in right-release.config.mjs, forbids their chunk names and the markers
 * in src/data/release-forbid.txt). In dev the engine is used only inside the Tauri shell with
 * VITE_RIGHTKIT_DATA=engine; everything else is the mock, so the browser preview keeps its "Mock data" banner.
 */
export async function selectDataSource(): Promise<DataSource> {
  if (import.meta.env.DEV || (typeof __RIGHTKIT_MOCK__ === "boolean" && __RIGHTKIT_MOCK__)) {
    const { pickDevSource } = await import("./mock/devSource");
    return pickDevSource(isTauri(), () => new EngineDataSource());
  }
  return new EngineDataSource();
}
