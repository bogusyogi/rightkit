const SETTINGS_UPDATED_EVENT = "hr-settings-updated";

export type SettingsLoad<T> = Readonly<{
  generation: number;
  promise: Promise<T>;
}>;

export type SettingsPatchResult<T> =
  | { kind: "applied" }
  | { kind: "superseded" }
  | { kind: "failed"; error: unknown; reconciled: true }
  | { kind: "failed"; error: unknown; reconciled: false; fallback: T | null };

export type OptimisticSettingsRollback<T extends object> = Readonly<{
  key: keyof T;
  optimisticValue: unknown;
  hadPreviousValue: boolean;
  previousValue: unknown;
}>;

export function applyOptimisticSettingsPatch<T extends object>(
  current: T,
  key: keyof T,
  value: unknown,
): { settings: T; rollback: OptimisticSettingsRollback<T> } {
  const currentRecord = current as Record<PropertyKey, unknown>;
  return {
    settings: { ...current, [key]: value },
    rollback: {
      key,
      optimisticValue: value,
      hadPreviousValue: Object.getOwnPropertyDescriptor(current, key) !== undefined,
      previousValue: currentRecord[key],
    },
  };
}

export function rollbackOptimisticSettingsPatch<T extends object>(
  current: T,
  rollback: OptimisticSettingsRollback<T>,
): T {
  const currentRecord = current as Record<PropertyKey, unknown>;
  if (!Object.is(currentRecord[rollback.key], rollback.optimisticValue)) return current;

  const restored = { ...current } as T;
  const restoredRecord = restored as Record<PropertyKey, unknown>;
  if (rollback.hadPreviousValue) {
    restoredRecord[rollback.key] = rollback.previousValue;
  } else {
    Reflect.deleteProperty(restoredRecord, rollback.key);
  }
  return restored;
}

export function createSettingsCoordinator<T>() {
  let generation = 0;
  let nextPatchId = 0;
  let latestPatchId = 0;
  let inFlightLoad: SettingsLoad<T> | null = null;
  let patchTail: Promise<void> = Promise.resolve();
  let lastAuthoritative: T | null = null;

  function rememberAuthoritative(settings: T): T {
    lastAuthoritative = settings;
    return settings;
  }

  function load(loader: () => Promise<T>, fresh = false): SettingsLoad<T> {
    if (inFlightLoad && !fresh) return inFlightLoad;

    let promise: Promise<T>;
    try {
      promise = Promise.resolve(loader());
    } catch (error) {
      promise = Promise.reject(error);
    }

    const sharedLoad: SettingsLoad<T> = { generation, promise };
    inFlightLoad = sharedLoad;
    const clear = () => {
      if (inFlightLoad === sharedLoad) inFlightLoad = null;
    };
    void promise.then(clear, clear);
    return sharedLoad;
  }

  function publishUpdate(settings: T, publish: (settings: T) => void): void {
    rememberAuthoritative(settings);
    generation += 1;
    publish(settings);
  }

  function acceptLoad(sharedLoad: SettingsLoad<T>): boolean {
    return sharedLoad.generation === generation;
  }

  async function loadCurrent(loader: () => Promise<T>): Promise<T> {
    while (true) {
      const sharedLoad = load(loader);
      try {
        const settings = await sharedLoad.promise;
        if (acceptLoad(sharedLoad)) return rememberAuthoritative(settings);
      } catch (error) {
        if (acceptLoad(sharedLoad)) throw error;
      }
    }
  }

  async function runPatch(
    apply: () => Promise<T>,
    publish: (settings: T) => void,
    reconcile: () => Promise<T>,
  ): Promise<SettingsPatchResult<T>> {
    const patchId = ++nextPatchId;
    latestPatchId = patchId;
    generation += 1;
    const operation = patchTail.then(apply, apply);
    patchTail = operation.then(
      () => undefined,
      () => undefined,
    );

    try {
      const settings = await operation;
      rememberAuthoritative(settings);
      if (patchId !== latestPatchId) return { kind: "superseded" };
      publishUpdate(settings, publish);
      return { kind: "applied" };
    } catch (error) {
      if (patchId !== latestPatchId) return { kind: "superseded" };

      generation += 1;
      const authoritativeLoad = load(reconcile, true);
      try {
        const settings = await authoritativeLoad.promise;
        if (patchId !== latestPatchId) return { kind: "superseded" };
        if (!acceptLoad(authoritativeLoad)) {
          return { kind: "failed", error, reconciled: false, fallback: lastAuthoritative };
        }
        publishUpdate(settings, publish);
        return { kind: "failed", error, reconciled: true };
      } catch {
        return patchId === latestPatchId
          ? { kind: "failed", error, reconciled: false, fallback: lastAuthoritative }
          : { kind: "superseded" };
      }
    }
  }

  return {
    load,
    loadCurrent,
    acceptLoad,
    publishUpdate,
    runPatch,
  };
}

// Reference-equal gate for the hr-settings-updated fan-out: every published
// blob reaches all ~13 useSettings consumers, so a snapshot whose values are
// unchanged from the consumer's current one must not re-render it. Key
// comparison is Object.is per own key — patch values are primitives or
// treated-as-immutable payloads, so identity is the right comparison.
export function settingsSnapshotEqual<T extends object>(a: T, b: T): boolean {
  if (a === b) return true;
  const aKeys = Object.keys(a);
  const bKeys = Object.keys(b);
  if (aKeys.length !== bKeys.length) return false;
  const aRecord = a as Record<PropertyKey, unknown>;
  const bRecord = b as Record<PropertyKey, unknown>;
  return aKeys.every((key) => Object.is(aRecord[key], bRecord[key]));
}

export function subscribeToSettingsUpdates<T>(
  target: Pick<EventTarget, "addEventListener" | "removeEventListener">,
  consume: (settings: T) => void,
): () => void {
  const onUpdate = ((event: Event) => {
    const detail = (event as CustomEvent<T>).detail;
    if (detail !== undefined) consume(detail);
  }) as EventListener;
  target.addEventListener(SETTINGS_UPDATED_EVENT, onUpdate);
  return () => target.removeEventListener(SETTINGS_UPDATED_EVENT, onUpdate);
}

export function publishSettingsUpdate<T>(
  target: Pick<EventTarget, "dispatchEvent">,
  settings: T,
): void {
  target.dispatchEvent(new CustomEvent(SETTINGS_UPDATED_EVENT, { detail: settings }));
}
