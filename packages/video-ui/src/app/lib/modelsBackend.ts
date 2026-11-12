import type { ModelEntry, ModelsBackend } from "@rightkit/settings-ui";
import { errorMessage } from "../../lib/engine.js";
import { type ModelRow, modelsInstall, modelsStatus } from "../../lib/media.js";

const LABELS = {
  speech: { label: "Speech model", description: "Transcribes talking-head footage for cuts and captions." },
  face: { label: "Face model", description: "Tracks faces for vertical and square framing." },
} as const;
type Kind = keyof typeof LABELS;

function entry(id: Kind, row: ModelRow | undefined, installing: boolean): ModelEntry {
  const state: ModelEntry["state"] = installing || row?.in_progress
    ? "downloading"
    : row?.ready || row?.state === "ready"
      ? "installed"
      : row?.error && row.state !== "missing"
        ? "error"
        : "missing";
  return {
    id,
    ...LABELS[id],
    sizeBytes: row?.size_hint_bytes,
    state,
    // Signed packs live in the shared Orthic store and other apps use them.
    shared: true,
    error: state === "error" && row?.error ? { code: row.state ?? "error", message: row.error } : undefined,
  };
}

/** RightKit models port over the engine's signed-pack store (`models.status` / `models.install`). */
export function createModelsBackend(): ModelsBackend {
  const installing = new Set<Kind>();
  const listeners = new Set<(e: ModelEntry) => void>();
  // The engine rejects with a {code, message} object; the kits show `Error.message`.
  const asError = (e: unknown): never => {
    throw e instanceof Error ? e : new Error(errorMessage(e));
  };
  const list = async () => {
    const s = await modelsStatus().catch(asError);
    return (Object.keys(LABELS) as Kind[]).map((k) => entry(k, s[k], installing.has(k)));
  };
  const push = async () => {
    const all = await list().catch(() => []);
    for (const e of all) for (const l of listeners) l(e);
  };
  return {
    list,
    async install(id) {
      const k = id as Kind;
      installing.add(k);
      void push();
      try {
        await modelsInstall(k).catch(asError);
      } finally {
        installing.delete(k);
        void push();
      }
    },
    // The engine install is not resumable mid-file; cancel is a no-op the UI tolerates.
    async cancel() {},
    // settings-ui never offers Remove for shared entries; the port still requires the method.
    async remove() {
      throw new Error("Removing a signed model pack is not supported; it is shared with other Orthic Labs apps.");
    },
    async subscribe(handler) {
      listeners.add(handler);
      return () => listeners.delete(handler);
    },
  };
}
