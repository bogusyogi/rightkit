import type { SettingsBackend, SettingsSnapshot, SettingsValues } from "@rightkit/settings-ui";

/** The native document as the store holds it: JSON text (null when empty) and committed revision. */
export interface SettingsDocumentState {
  raw: string | null;
  revision: number;
}

/** The ports the backend needs. `read` / `write` are the native settings document commands and
 *  reject on failure; the cache is the synchronous boot copy in localStorage. */
export interface SettingsBackendDeps {
  read(): Promise<SettingsDocumentState>;
  /** Commit `raw`. With `merge` it is a top-level patch applied inside the store's write lock
   *  (`null` removes a key), otherwise it replaces the document. `expectedRevision` makes it a
   *  compare-and-swap on the committed revision and rejects with code `write_conflict` when stale.
   *  Resolves the committed document, or null when there is no native store (browser QA). */
  write(
    raw: string,
    options: { expectedRevision?: number; merge: boolean },
  ): Promise<{ raw: string; revision: number } | null>;
  /** Defaults, normalisation and clamps over a raw JSON payload (the app schema lives in settings.tsx). */
  parse(raw: string): SettingsValues;
  readCache(): string | null;
  writeCache(values: SettingsValues): void;
}

/** Thrown for a stale `expectedRevision`; `code` is the one the settings store retries on. */
export class SettingsRevisionConflict extends Error {
  readonly code = "revision_conflict";
}

function isNativeConflict(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as { code?: unknown }).code === "write_conflict"
  );
}

/**
 * `SettingsBackend` over the host app native settings commands (`rightkit-app-settings` Document).
 *
 * - The native document is authoritative and so is its revision. `load()` reads both; the first
 *   read is memoised and seeds the document from the boot cache only when the store holds nothing,
 *   so a localStorage boot copy is carried into the store. Later `load()` calls re-read the
 *   store, which is how the settings store picks up another writer's commit after a conflict.
 * - `patch` is applied by the store itself (`Document::patch`) under its write lock, with the
 *   caller's `expectedRevision` as compare-and-swap. A stale revision rejects with
 *   `revision_conflict` and nothing is written, so a second window or process can never be
 *   overwritten by a stale merge. Every write waits for the first read; if that read failed, writes
 *   are refused and the error reaches the caller (no silent second store, no write over unread data).
 * - The boot cache is written only after a native commit.
 */
export function createSettingsBackend(
  deps: SettingsBackendDeps,
  initial?: SettingsValues,
): SettingsBackend {
  let current: SettingsValues | null = initial ?? null;
  let revision = 0;
  let hydration: Promise<void> | null = null;
  let hydrated = false;

  const adopt = (raw: string, committedRevision: number) => {
    current = deps.parse(raw);
    revision = committedRevision;
    deps.writeCache(current);
  };

  const hydrate = (): Promise<void> => {
    hydration ??= (async () => {
      const state = await deps.read();
      if (state.raw != null) {
        adopt(state.raw, state.revision);
        return;
      }
      const cached = deps.readCache();
      current = deps.parse(cached ?? "{}");
      revision = state.revision;
      if (cached != null) {
        // Seed an empty store only: expecting the revision just read, so a document another writer
        // created in between is not replaced.
        const written = await deps.write(JSON.stringify(current), {
          expectedRevision: state.revision,
          merge: false,
        });
        if (written) adopt(written.raw, written.revision);
      }
      deps.writeCache(current);
    })()
      .then(() => {
        hydrated = true;
      })
      .catch((error: unknown) => {
        hydration = null; // a later load or write retries the read
        throw error;
      });
    return hydration;
  };

  const snapshot = (): SettingsSnapshot => ({ values: current ?? {}, revision });

  return {
    async load() {
      const wasHydrated = hydrated;
      await hydrate();
      if (wasHydrated) {
        const state = await deps.read();
        if (state.raw != null) {
          adopt(state.raw, state.revision);
          }
      }
      return snapshot();
    },
    async patch(patch, expectedRevision) {
      await hydrate();
      let written: { raw: string; revision: number } | null;
      try {
        written = await deps.write(JSON.stringify(patch), { expectedRevision, merge: true });
      } catch (error) {
        if (isNativeConflict(error)) {
          throw new SettingsRevisionConflict("settings changed elsewhere");
        }
        throw error;
      }
      if (written) {
        adopt(written.raw, written.revision);
      } else {
        // No native store (browser QA): merge locally.
        const next: SettingsValues = { ...current };
        for (const [key, value] of Object.entries(patch)) {
          if (value === null) delete next[key];
          else next[key] = value;
        }
        current = next;
        revision += 1;
        deps.writeCache(next);
      }
      return snapshot();
    },
  };
}
