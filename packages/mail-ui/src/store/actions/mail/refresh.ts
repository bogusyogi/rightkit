import type { Account } from "../../../lib/ipc/system";
import type { UnreadCounts } from "../../../lib/ipc/mail";
import {
  appLockStatus,
  dbHealth,
  destructiveEnabled,
  getBlockTrackers,
  getImagePolicy,
  hasGoogleClientId,
  hasGoogleClientSecret,
  listAccounts,
  unreadCounts,
} from "../../../lib/ipc";
import type { StoreGet } from "../../mk";
import { settleSettingWrites } from "../../../lib/settingsWriteQueue";
import { markInitialRefreshSettled } from "../../../lib/initialRefresh";

/** Settings that a mail sync can never change. They are fetched at startup and
 *  again only when Settings changes them / the Settings view closes — never on
 *  a sync-complete refresh (each one is a DB read that would queue behind the
 *  sync it was just notified about). */
export type StaticSettings = {
  clientIdSet: boolean;
  clientSecretSet: boolean;
  destructiveOn: boolean;
  imagePolicy: boolean;
  blockTrackers: boolean;
};

export type StaticSettingsFetchers = {
  hasGoogleClientId: () => Promise<boolean>;
  hasGoogleClientSecret: () => Promise<boolean>;
  destructiveEnabled: () => Promise<boolean>;
  getImagePolicy: () => Promise<boolean>;
  getBlockTrackers: () => Promise<boolean>;
};

/** What a sync can change: accounts and unread counts. */
export type DynamicTail = {
  accs: Account[];
  counts: UnreadCounts;
};

export type DynamicTailFetchers = {
  listAccounts: () => Promise<Account[]>;
  unreadCounts: () => Promise<UnreadCounts>;
};

export type RefreshTail = StaticSettings & DynamicTail;

export type RefreshTailFetchers = StaticSettingsFetchers & DynamicTailFetchers;

/** Static-settings half of the refresh. Per-call .catch fallbacks preserved;
 *  clientId/clientSecret rejections propagate as before. */
export async function fetchStaticSettings(f: StaticSettingsFetchers): Promise<StaticSettings> {
  const [clientIdSet, clientSecretSet, destructiveOn, imagePolicy, blockTrackers] =
    await Promise.all([
      f.hasGoogleClientId(),
      f.hasGoogleClientSecret(),
      f.destructiveEnabled().catch(() => false),
      // A read error is not "no row": fail to the private choice (do not load remote images).
      f.getImagePolicy().catch(() => false),
      f.getBlockTrackers().catch(() => true),
    ]);
  return { clientIdSet, clientSecretSet, destructiveOn, imagePolicy, blockTrackers };
}

/** Sync-sensitive half of the refresh: accounts + unread counts only. */
export async function fetchDynamicTail(f: DynamicTailFetchers): Promise<DynamicTail> {
  const [accs, counts] = await Promise.all([f.listAccounts(), f.unreadCounts()]);
  return { accs, counts };
}

/** The independent tail of a full refresh — none of these seven calls depend on
 *  each other's result, so fire them together instead of awaiting one at a
 *  time. Per-call .catch fallbacks are preserved so one rejection doesn't sink
 *  the others; a rejection with no fallback (clientId/clientSecret/accounts/
 *  counts) propagates so the caller's try/catch can surface it, exactly as it
 *  did when these were awaited sequentially. Pulled out as a pure function (DI
 *  over the fetchers) so it's testable without a Tauri runtime. */
export async function fetchRefreshTail(fetchers: RefreshTailFetchers): Promise<RefreshTail> {
  const [settings, dynamic] = await Promise.all([
    fetchStaticSettings(fetchers),
    fetchDynamicTail(fetchers),
  ]);
  return { ...settings, ...dynamic };
}

export type RefreshDeps = {
  appLockStatus: typeof appLockStatus;
  dbHealth: typeof dbHealth;
  staticFetchers: StaticSettingsFetchers;
  dynamicFetchers: DynamicTailFetchers;
};

const defaultDeps: RefreshDeps = {
  appLockStatus,
  dbHealth,
  staticFetchers: {
    hasGoogleClientId,
    hasGoogleClientSecret,
    destructiveEnabled,
    getImagePolicy,
    getBlockTrackers,
  },
  dynamicFetchers: { listAccounts, unreadCounts },
};

type RefreshScope = { staticSettings: boolean; dynamic: boolean };

type StoreState = ReturnType<StoreGet>;

function applyStaticSettings(s: StoreState, v: StaticSettings) {
  s.setClientIdSet(v.clientIdSet);
  s.setClientSecretSet(v.clientSecretSet);
  s.setDestructiveOn(v.destructiveOn);
  s.setImagePolicyState(v.imagePolicy);
  s.setBlockTrackersState(v.blockTrackers);
}

async function runRefresh(get: StoreGet, scope: RefreshScope, deps: RefreshDeps) {
  const s = get();
  let unlocked = false;
  try {
    // Never read static settings while a save is still in flight: the read
    // would return the old committed value and overwrite the user's change.
    if (scope.staticSettings) await settleSettingWrites();
    // appLockStatus must stay ahead of the data fetches: a locked app must not
    // fire list/count reads. It is an in-memory read on the backend, so it
    // adds no DB wait; dbHealth is independent and rides in the parallel batch.
    const lock = await deps.appLockStatus().catch(() => null);
    if (lock) s.setAppLock(lock);
    if (lock && lock.state !== "disabled" && lock.state !== "unlocked") {
      if (scope.dynamic) {
        s.setHealth(await deps.dbHealth());
        s.setAccounts([]);
        s.setThreads([]);
        s.setCounts({ total: 0, per_account: [] });
      }
      return;
    }
    unlocked = true;
    const [health, settings, dynamic] = await Promise.all([
      scope.dynamic ? deps.dbHealth() : Promise.resolve(null),
      scope.staticSettings ? fetchStaticSettings(deps.staticFetchers) : Promise.resolve(null),
      scope.dynamic ? fetchDynamicTail(deps.dynamicFetchers) : Promise.resolve(null),
    ]);
    if (health) s.setHealth(health);
    if (settings) applyStaticSettings(s, settings);
    if (dynamic) {
      s.setAccounts(dynamic.accs);
      s.setCounts(dynamic.counts);
      s.setActiveAccount((prev) =>
        prev && dynamic.accs.some((a) => a.id === prev)
          ? prev
          : (dynamic.accs[0]?.id ?? null),
      );
    }
  } catch (e) {
    s.setErr(String(e));
  } finally {
    // Marked even when the batch failed, so a broken account read cannot leave
    // the inbox unloaded for the whole session. A locked refresh never marks it.
    if (unlocked && scope.dynamic) markInitialRefreshSettled();
  }
}

/** Full refresh: startup, unlock, post-mutation. Fetches everything. */
export async function refreshAction(get: StoreGet, deps: RefreshDeps = defaultDeps) {
  return runRefresh(get, { staticSettings: true, dynamic: true }, deps);
}

/** Sync-complete refresh: accounts, unread counts, health only. Never touches
 *  the static-settings IPC wrappers. */
export async function refreshAfterSyncAction(get: StoreGet, deps: RefreshDeps = defaultDeps) {
  return runRefresh(get, { staticSettings: false, dynamic: true }, deps);
}

/** Re-read the static settings (Settings view closed / a setting changed). */
export async function refreshStaticSettingsAction(get: StoreGet, deps: RefreshDeps = defaultDeps) {
  return runRefresh(get, { staticSettings: true, dynamic: false }, deps);
}
