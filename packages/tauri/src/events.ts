/** Stops a listener. */
export type TauriUnlisten = () => void;

/** The webview's event listen (for example `@tauri-apps/api/event`'s `listen`). The handler gets an object whose `payload` is the event payload. */
export type TauriListen = <T>(
  event: string,
  handler: (event: { payload: T }) => void,
) => Promise<TauriUnlisten>;

/** Typed event subscription: resolves to an unsubscribe once the listener is registered. */
export type TauriSubscribe<Spec> = <K extends keyof Spec & string>(
  event: K,
  handler: (payload: Spec[K]) => void,
) => Promise<TauriUnlisten>;

/**
 * An event spec is an interface from the app's own event key to its payload type.
 *
 * ```ts
 * interface AppEvents {
 *   appState: AppState;
 *   wake: unknown;
 * }
 * ```
 */
export type TauriEventNames<Spec> = { [K in keyof Spec & string]: string };

export interface DefineTauriEventsConfig<Spec> {
  listen: TauriListen;
  /** The native event name for every key of `Spec`, once. */
  events: TauriEventNames<Spec>;
}

/**
 * Turns a declared event map into one typed subscribe function. The key is the app's name for the
 * event; the value is the native event name `listen` registers. `subscribe(key, handler)` hands the
 * handler the payload, not the event envelope. A key with no mapping rejects with an Error naming it.
 *
 * ```ts
 * const on = defineTauriEvents<AppEvents>({
 *   listen,
 *   events: { appState: 'app-state-changed', wake: 'wake-fired' },
 * });
 * const off = await on('appState', (state) => render(state));
 * ```
 */
export function defineTauriEvents<Spec>(config: DefineTauriEventsConfig<Spec>): TauriSubscribe<Spec> {
  const { listen, events } = config;
  return async <K extends keyof Spec & string>(
    event: K,
    handler: (payload: Spec[K]) => void,
  ): Promise<TauriUnlisten> => {
    const name = (events as Partial<Record<string, string>>)[event];
    if (typeof name !== 'string' || name.trim().length === 0) {
      throw new Error(`RightKit Tauri event ${String(event)} has no native event name`);
    }
    return listen<Spec[K]>(name, (message) => handler(message.payload));
  };
}

/** The native event names an `events` map listens to, sorted and unique. */
export function declaredTauriEventNames<Spec>(events: TauriEventNames<Spec>): string[] {
  return [...new Set(Object.values(events) as string[])].sort();
}
