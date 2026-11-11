// Host configuration for the system views (Storage, Cleanup, Duplicates, Drive health, Apps, Activity) and the
// graph helpers.
//
// The package holds no IPC and no product: every call goes through the typed clients the host supplies
// (`SystemClient`, `AppsClient`), every native event through `subscribe` (`SystemEventMap`), and the product name and
// storage-key prefix come from here. Call `configureSystemHost` once per window, before rendering.
import type {
  AppEntry,
  AppUpdate,
  AppsClient,
  LeftoversEvent,
  SystemClient,
  UpdateJob,
  UpdateReport,
} from "./api.js";
import { watchVisibility } from "./visible.js";

export type Unsubscribe = () => void;

/** One `apps-icon` event. A null data URL means the app has no usable icon. */
export interface AppIconEvent {
  path: string;
  data_url: string | null;
}

/** The native events the views listen to. The host maps each to its own event. */
export interface SystemEventMap {
  /** The disk index changed under these folders (`stale`: anything may have changed). */
  "storage-updated": { folders: string[]; stale: boolean };
  "apps-row": AppEntry;
  "apps-inventory": AppEntry[];
  "apps-update-row": AppUpdate;
  "apps-updates-done": UpdateReport;
  "apps-icon": AppIconEvent;
  "apps-update-job": UpdateJob;
  "apps-leftovers": LeftoversEvent;
  /** The host's window was hidden (false) or shown (true); polling pauses while hidden. */
  visibility: boolean;
}

/** Typed event subscription: resolves to an unsubscribe once the host has registered the listener. */
export type Subscribe = <K extends keyof SystemEventMap>(
  event: K,
  handler: (event: { payload: SystemEventMap[K] }) => void,
) => Promise<Unsubscribe>;

export interface SystemHost {
  client: SystemClient;
  apps: AppsClient;
  subscribe: Subscribe;
  /** The product name in user-facing copy ("What <appName> did and saw"). */
  appName: string;
  /** Prefix of the local-storage keys the views keep their selection under, e.g. "myapp.hub.". */
  storagePrefix: string;
}

let current: SystemHost | null = null;

export function configureSystemHost(host: SystemHost): void {
  current = host;
  watchVisibility(host.subscribe);
}

export function systemHost(): SystemHost {
  if (!current) throw new Error("@rightkit/system-ui: call configureSystemHost({ client, apps, subscribe, appName, storagePrefix }) before rendering");
  return current;
}

/** The configured product name. */
export const appName = (): string => systemHost().appName;
