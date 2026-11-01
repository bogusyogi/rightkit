// tauriBridge — the Tauri IPC bridge. Every window (hub, pill, popups, onboarding) imports
// `invoke`/`listen` through `./tauri`, which re-exports this module unchanged.
//
// `scripts/lib/capability-event-discovery.mjs` walks the static import graph reachable from each
// overlay entry (PillOverlay.tsx, MiniClipboardPopup.tsx, HintPopup.tsx) and fails closed on ANY
// `import()` call in that graph; this module is on it, so it must never use a dynamic import.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

type RendererEvent<T> = { event: string; id: number; payload: T };
export type EventCallback<T> = (event: RendererEvent<T>) => void;
export type UnlistenFn = () => void;
export type RendererEventEnvelope<T> = { event: string; payload: T };

export const RENDERER_EVENT_BRIDGE = "heardright://renderer-event";

export type Args = Record<string, unknown> | undefined;

export function invoke<T = unknown>(command: string, args?: Args): Promise<T> {
  return tauriInvoke<T>(command, args);
}

export function listen<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
  const listener = ((customEvent: Event) => {
    const detail = (customEvent as CustomEvent<RendererEventEnvelope<T>>).detail;
    if (typeof detail !== "object" || detail === null || !("payload" in detail)) return;
    if (detail.event !== event) return;
    handler({ event, id: 0, payload: detail.payload });
  }) as EventListener;
  window.addEventListener(RENDERER_EVENT_BRIDGE, listener);
  return Promise.resolve(() => window.removeEventListener(RENDERER_EVENT_BRIDGE, listener));
}
