// Public surface of the operator data layer. Screens import from here, never from mock/ or engine/.
import { isTauri } from "../lib/host.js";
import { EngineDataSource } from "./engine/engineDataSource.js";
import type { DataSource } from "./source.js";

export type * from "./types.js";
export type { DataSource, MockControls, DataError, DataErrorCode, EngineOnlyErrorCode, Result, UnmappedAccount } from "./source.js";
export { SOCIAL_PLATFORMS, isSocialPlatform } from "./platforms.js";
export { EngineDataSource } from "./engine/engineDataSource.js";
export { accountOrdinals, awaitingDecision, charCount, isApproved, jobsByAccount, mediaReady, shortHash } from "./derive.js";
export type { AccountOrdinal } from "./derive.js";
export * from "./time.js";
export { disclosureFound, firstSentence } from "./draftRules.js";
export { formatMicro, formatMilli, groupDigits, monthOfInstant } from "./money.js";

/**
 * The source the desktop runs on. A release build is the engine only: the mock, its JSON fixtures and the shared
 * data helpers load through a dynamic import behind import.meta.env?.DEV || __RIGHTKIT_MOCK__ (RIGHTKIT_MOCK=1, UI QA only), which Vite folds to false, so they are not
 * in dist/ (right-release dist-scan, declared in right-release.config.mjs, forbids their chunk names and the markers
 * in src/data/release-forbid.txt). In dev the engine is used only inside the Tauri shell with
 * VITE_RIGHTKIT_DATA=engine; everything else is the mock, so the browser preview keeps its "Mock data" banner.
 */
export async function selectDataSource(): Promise<DataSource> {
  if (import.meta.env?.DEV || (typeof __RIGHTKIT_MOCK__ === "boolean" && __RIGHTKIT_MOCK__)) {
    const { pickDevSource } = await import("./mock/devSource.js");
    return pickDevSource(isTauri(), () => new EngineDataSource());
  }
  return new EngineDataSource();
}
