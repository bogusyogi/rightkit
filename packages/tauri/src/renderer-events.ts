import type { TauriListen, TauriUnlisten } from './events.js';

/** One event delivered to a renderer listener. `id` is always `0`: the native bridge does not number events. */
export interface RendererEvent<T> {
  event: string;
  id: number;
  payload: T;
}

/** The handler a renderer listener receives: the event name it asked for, and the payload. */
export type RendererEventCallback<T> = (event: RendererEvent<T>) => void;

/** The `detail` shape the native bridge dispatches on the window: the event name and its payload. */
export interface RendererEventEnvelope<T> {
  event: string;
  payload: T;
}

export interface RendererEventBridgeConfig {
  /**
   * The window `CustomEvent` name the native renderer-event bridge dispatches on, for example the app's
   * `<app-scheme>://renderer-event`. The app passes its own name; this package has no default.
   */
  bridgeEvent: string;
}

export interface RendererEventBridge {
  /**
   * Subscribes to one native event name. The handler runs only for envelopes whose `event` equals `event`
   * and that carry a `payload`; anything else on the bridge is ignored. Resolves to the unsubscribe function.
   */
  listen<T>(event: string, handler: RendererEventCallback<T>): Promise<TauriUnlisten>;
}

/**
 * The renderer side of the native event bridge: a window listener on `bridgeEvent`, filtered by event name.
 * It needs no Tauri import and no dynamic import, so it can sit in a statically walked overlay graph.
 * The host passes its bridge's window event name in `bridgeEvent`.
 */
export function createRendererEventBridge({ bridgeEvent }: RendererEventBridgeConfig): RendererEventBridge {
  return {
    listen<T>(event: string, handler: RendererEventCallback<T>): Promise<TauriUnlisten> {
      const listener = ((customEvent: Event) => {
        const detail = (customEvent as CustomEvent<RendererEventEnvelope<T>>).detail;
        if (typeof detail !== 'object' || detail === null || !('payload' in detail)) return;
        if (detail.event !== event) return;
        handler({ event, id: 0, payload: detail.payload });
      }) as EventListener;
      window.addEventListener(bridgeEvent, listener);
      return Promise.resolve(() => window.removeEventListener(bridgeEvent, listener));
    },
  };
}

/**
 * Wraps a `listen` so that each event name it is asked for is remapped through `routes` to the name the
 * native side actually emits. A name with no route resolves to a no-op unlisten and is never delivered, so an
 * event the app did not declare cannot reach its handler. The payload passes through unchanged.
 *
 * ```ts
 * const listen = mapListenRoutes({
 *   listen: bridge.listen,
 *   routes: { 'app://license-changed': 'license-changed' },
 * });
 * ```
 */
export function mapListenRoutes(config: { listen: TauriListen; routes: Readonly<Record<string, string>> }): TauriListen {
  const { listen, routes } = config;
  return <T>(event: string, handler: (message: { payload: T }) => void): Promise<TauriUnlisten> => {
    if (!Object.hasOwn(routes, event)) return Promise.resolve(() => undefined);
    const target = routes[event] as string;
    return listen<T>(target, (message) => handler({ payload: message.payload }));
  };
}
