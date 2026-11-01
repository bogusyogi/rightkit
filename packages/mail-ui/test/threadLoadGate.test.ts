import test from "node:test";
import assert from "node:assert/strict";
import { claimThreadLoad, type ThreadLoadInputs } from "../src/lib/threadLoadGate";
import {
  isInitialRefreshSettled,
  subscribeInitialRefreshSettled,
} from "../src/lib/initialRefresh";
import {
  refreshAction,
  type RefreshDeps,
} from "../src/store/actions/mail/refresh";

const unlockedStart: ThreadLoadInputs = {
  appUnlocked: true,
  categoryPending: false,
  hasQuery: false,
  refreshSettled: false,
  labelsSettled: false,
};

// Each step is one run of the load effect (a dependency changed). Returns how
// many list_threads calls the effect issued across the steps.
function runEffects(steps: Partial<ThreadLoadInputs>[], gate = { startupLoaded: false }) {
  let loads = 0;
  for (const step of steps) {
    if (claimThreadLoad(gate, { ...unlockedStart, ...step })) loads += 1;
  }
  return { loads, gate };
}

test("nothing loads before the initial refresh settles", () => {
  // Mount, then the account and labels arrive while the refresh is still open.
  const { loads } = runEffects([
    {},
    { labelsSettled: true },
    { labelsSettled: true, categoryPending: false },
  ]);
  assert.equal(loads, 0, "no load while refreshSettled is false");

  // Refresh settles but startup labels are still in flight.
  const awaitingLabels = runEffects([{ refreshSettled: true }]);
  assert.equal(awaitingLabels.loads, 0, "no load while labels are unresolved");
});

test("exactly one load at startup, after refresh and labels settle", () => {
  // Gmail with categories: the Inbox waits for a category to be picked, then loads once.
  const { loads, gate } = runEffects([
    {},
    { refreshSettled: true },
    { refreshSettled: true, labelsSettled: true, categoryPending: true },
    { refreshSettled: true, labelsSettled: true, categoryPending: false },
  ]);
  assert.equal(loads, 1);
  assert.equal(gate.startupLoaded, true);

  // Account without categories: labels settle and the load fires once.
  const plain = runEffects([
    {},
    { refreshSettled: true },
    { refreshSettled: true, labelsSettled: true },
  ]);
  assert.equal(plain.loads, 1);
});

test("a later account or label change loads again as before", () => {
  const started = runEffects([
    { refreshSettled: true, labelsSettled: true },
  ]);
  assert.equal(started.loads, 1);

  // Account switch and label change are ordinary triggers after startup.
  const accountSwitch = claimThreadLoad(started.gate, {
    ...unlockedStart,
    refreshSettled: true,
    labelsSettled: true,
  });
  const labelChange = claimThreadLoad(started.gate, {
    ...unlockedStart,
    refreshSettled: true,
    labelsSettled: true,
  });
  assert.equal(accountSwitch, true);
  assert.equal(labelChange, true);

  // Later triggers still respect the category and search guards.
  assert.equal(
    claimThreadLoad(started.gate, { ...unlockedStart, refreshSettled: true, labelsSettled: true, categoryPending: true }),
    false,
  );
  assert.equal(
    claimThreadLoad(started.gate, { ...unlockedStart, refreshSettled: true, labelsSettled: true, hasQuery: true }),
    false,
  );
});

test("a locked app does not load, even when everything else has settled", () => {
  const locked = runEffects([
    { appUnlocked: false, refreshSettled: true, labelsSettled: true },
    { appUnlocked: false, refreshSettled: true, labelsSettled: true, categoryPending: false },
  ]);
  assert.equal(locked.loads, 0);
  assert.equal(locked.gate.startupLoaded, false);
});

// Refresh signal. This file's module state persists across its tests, so the
// locked refresh runs before the unlocked one.
const storeHarness = () => {
  const state = new Proxy({} as Record<string, unknown>, {
    get: (_t, key) => (typeof key === "string" ? () => {} : undefined),
  });
  return (() => state) as never;
};

const baseDeps = (lockState: string): RefreshDeps => ({
  appLockStatus: async () => ({ state: lockState } as never),
  dbHealth: async () => ({ ok: true } as never),
  staticFetchers: {
    hasGoogleClientId: async () => true,
    hasGoogleClientSecret: async () => true,
    destructiveEnabled: async () => false,
    getImagePolicy: async () => true,
    getBlockTrackers: async () => true,
  },
  dynamicFetchers: {
    listAccounts: async () => [],
    unreadCounts: async () => ({ total: 0, per_account: [] }),
  },
});

test("a locked full refresh leaves the initial-refresh signal unset", async () => {
  await refreshAction(storeHarness(), baseDeps("locked"));
  assert.equal(isInitialRefreshSettled(), false);
});

test("an unlocked full refresh settles the signal once and notifies subscribers", async () => {
  let notified = 0;
  const unsubscribe = subscribeInitialRefreshSettled(() => {
    notified += 1;
  });

  await refreshAction(storeHarness(), baseDeps("unlocked"));
  assert.equal(isInitialRefreshSettled(), true);
  assert.equal(notified, 1);

  await refreshAction(storeHarness(), baseDeps("unlocked"));
  assert.equal(notified, 1, "the signal fires only on the first settle");

  unsubscribe();
});
