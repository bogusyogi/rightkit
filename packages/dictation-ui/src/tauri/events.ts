import { listen as tauriListen } from "@tauri-apps/api/event";
import type { Subscribe, Unsubscribe } from "../host.js";

/** Subscribes to one native event; resolves to the unsubscribe once registered. Receives the payload only. */
export type ListenFn = <T>(event: string, handler: (payload: T) => void) => Promise<Unsubscribe>;

export interface TauriClientOptions {
  /**
   * How native events reach this window. Default: Tauri's `listen`. An app that routes events to windows
   * through its own channel (a per-window bridge instead of the event plugin) passes its subscribe here.
   */
  listen?: ListenFn;
}

export const tauriListenFn: ListenFn = (event, handler) => tauriListen(event, (e) => handler(e.payload as never));

/** A typed `on` over a table from the client's event keys to native event names. */
export function eventSubscribe<M>(names: { [K in keyof M]: string }, listen: ListenFn): Subscribe<M> {
  return (event, handler) => listen(names[event], handler);
}

export function notMapped(what: string): Error {
  return new Error(`@rightkit/dictation-ui/tauri: ${what} is not mapped to a native call; supply it through the client options.`);
}
