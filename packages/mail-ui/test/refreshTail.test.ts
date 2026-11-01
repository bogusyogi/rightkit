import test from "node:test";
import assert from "node:assert/strict";
import {
  fetchRefreshTail,
  refreshAction,
  refreshAfterSyncAction,
  refreshStaticSettingsAction,
  type RefreshDeps,
} from "../src/store/actions/mail/refresh";

function baseFetchers() {
  return {
    hasGoogleClientId: async () => true,
    hasGoogleClientSecret: async () => true,
    listAccounts: async () => [{ id: "a@example.com" } as never],
    unreadCounts: async () => ({ total: 3, per_account: [] }),
    destructiveEnabled: async () => true,
    getImagePolicy: async () => false,
    getBlockTrackers: async () => true,
  };
}

test("fetchRefreshTail fires all seven calls concurrently, not sequentially", async () => {
  const order: string[] = [];
  const started: string[] = [];

  function tracked<T>(name: string, delayMs: number, value: T): () => Promise<T> {
    return () =>
      new Promise((resolve) => {
        started.push(name);
        setTimeout(() => {
          order.push(name);
          resolve(value);
        }, delayMs);
      });
  }

  const start = Date.now();
  const result = await fetchRefreshTail({
    hasGoogleClientId: tracked("clientId", 30, true),
    hasGoogleClientSecret: tracked("clientSecret", 30, true),
    listAccounts: tracked("accounts", 30, [{ id: "a@example.com" } as never]),
    unreadCounts: tracked("counts", 30, { total: 1, per_account: [] }),
    destructiveEnabled: tracked("destructive", 30, true),
    getImagePolicy: tracked("imagePolicy", 30, false),
    getBlockTrackers: tracked("blockTrackers", 30, true),
  });
  const elapsed = Date.now() - start;

  // All seven should have STARTED before any of them resolves if truly parallel.
  assert.equal(started.length, 7, "all seven fetchers should be invoked");
  // Sequential awaiting of seven 30ms calls would take ~210ms; parallel stays
  // near a single call's latency. Generous bound to avoid flakiness.
  assert.ok(elapsed < 150, `expected parallel execution (~30ms), took ${elapsed}ms`);
  assert.equal(result.clientIdSet, true);
  assert.equal(result.counts.total, 1);
});

test("fetchRefreshTail preserves fallback for destructiveEnabled/getImagePolicy on rejection", async () => {
  const fetchers = baseFetchers();
  const result = await fetchRefreshTail({
    ...fetchers,
    destructiveEnabled: async () => {
      throw new Error("boom");
    },
    getImagePolicy: async () => {
      throw new Error("boom");
    },
  });

  assert.equal(result.destructiveOn, false, "destructiveEnabled rejection falls back to false");
  assert.equal(result.imagePolicy, false, "getImagePolicy rejection falls back to the private choice (false)");
  // The other four calls still resolved normally alongside the two failures.
  assert.equal(result.clientIdSet, true);
  assert.equal(result.clientSecretSet, true);
  assert.equal(result.accs.length, 1);
  assert.equal(result.counts.total, 3);
});

test("fetchRefreshTail propagates a rejection from a call with no fallback", async () => {
  const fetchers = baseFetchers();
  await assert.rejects(
    () =>
      fetchRefreshTail({
        ...fetchers,
        listAccounts: async () => {
          throw new Error("accounts unavailable");
        },
      }),
    /accounts unavailable/,
  );
});

// Sync-refresh path: a sync event must refetch only accounts/counts/health and
// must never call the static-settings IPC wrappers (each is a DB read that
// would queue behind the sync it was just notified about).
function refreshHarness() {
  const calls: string[] = [];
  const rec = <T>(name: string, value: T) => async () => {
    calls.push(name);
    return value;
  };
  const deps: RefreshDeps = {
    appLockStatus: rec("appLockStatus", { state: "disabled" } as never),
    dbHealth: rec("dbHealth", { ok: true } as never),
    staticFetchers: {
      hasGoogleClientId: rec("hasGoogleClientId", true),
      hasGoogleClientSecret: rec("hasGoogleClientSecret", true),
      destructiveEnabled: rec("destructiveEnabled", false),
      getImagePolicy: rec("getImagePolicy", true),
      getBlockTrackers: rec("getBlockTrackers", true),
    },
    dynamicFetchers: {
      listAccounts: rec("listAccounts", [{ id: "a@example.com" } as never]),
      unreadCounts: rec("unreadCounts", { total: 2, per_account: [] }),
    },
  };
  const state = new Proxy({} as Record<string, unknown>, {
    get: (_t, key) => (typeof key === "string" ? () => {} : undefined),
  });
  const get = (() => state) as never;
  return { calls, deps, get };
}

const STATIC_CALLS = [
  "hasGoogleClientId",
  "hasGoogleClientSecret",
  "destructiveEnabled",
  "getImagePolicy",
  "getBlockTrackers",
];

test("refreshAfterSyncAction does not call any static-settings IPC wrapper", async () => {
  const { calls, deps, get } = refreshHarness();
  await refreshAfterSyncAction(get, deps);
  for (const name of STATIC_CALLS) {
    assert.ok(!calls.includes(name), `sync refresh must not call ${name}`);
  }
  assert.ok(calls.includes("listAccounts") && calls.includes("unreadCounts"));
  assert.ok(calls.includes("dbHealth") && calls.includes("appLockStatus"));
});

test("refreshStaticSettingsAction fetches only static settings", async () => {
  const { calls, deps, get } = refreshHarness();
  await refreshStaticSettingsAction(get, deps);
  for (const name of STATIC_CALLS) assert.ok(calls.includes(name), `expected ${name}`);
  assert.ok(!calls.includes("listAccounts") && !calls.includes("unreadCounts"));
  assert.ok(!calls.includes("dbHealth"));
});

test("refreshAction (startup/full) still fetches static settings and dynamic data", async () => {
  const { calls, deps, get } = refreshHarness();
  await refreshAction(get, deps);
  for (const name of [...STATIC_CALLS, "listAccounts", "unreadCounts", "dbHealth"]) {
    assert.ok(calls.includes(name), `expected ${name}`);
  }
});

