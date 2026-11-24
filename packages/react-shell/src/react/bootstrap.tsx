// Window-kind bootstrap for an app whose one bundle serves several windows (a hub, a splash, native overlays),
// picked by a query parameter. Imports `react-dom/client`, so it is not in `./headless`.
import { StrictMode, type ReactNode } from "react";
import { createRoot } from "react-dom/client";

/** One window kind: what it renders and what runs before. */
export interface WindowView {
  /**
   * Runs before `render`, before any lazy import it triggers (stamp a dataset, migrate state). Synchronous: the
   * render starts after it returns. Errors propagate to the caller of {@link bootstrapWindows}.
   */
  prepare?: () => void;
  /**
   * Returns the element to mount; usually `import("./Page").then(...)` so a window that does not show this view
   * never parses its code. Wrap it in the host's error boundary here.
   */
  render: () => ReactNode | Promise<ReactNode>;
  /** Mount inside `StrictMode`. Default false. */
  strictMode?: boolean;
}

/** Options of {@link bootstrapWindows}. */
export interface BootstrapWindowsOptions {
  /** The window kind to run, or null to use the hub. Default: the `viewParam` query parameter of `window.location`. */
  view?: string | null;
  /** Query parameter that names the window kind. Default `view`. */
  viewParam?: string;
  /** Element to mount into, or its id. Default the element with id `root`. */
  root?: HTMLElement | string;
  /** Runs first, once, for every window kind that mounts, before `prepare` (platform stamp, crash capture). */
  before?: () => void;
  /**
   * Window kinds with no webview (native overlays on this platform): nothing mounts and nothing falls through
   * to the hub. Checked before `views`.
   */
  nativeViews?: readonly string[];
  /** Window kinds that mount their own view, by name. */
  views?: Readonly<Record<string, WindowView>>;
  /** The view for any other window kind, and for none. */
  hub: WindowView;
}

/**
 * Picks the window kind from the URL and mounts its view. Order: `before`; a native kind returns without
 * mounting (`before` has already run, so crash capture covers native-kind loads too); a named view; else the hub.
 * Resolves after the mount call.
 */
export async function bootstrapWindows(options: BootstrapWindowsOptions): Promise<void> {
  const { viewParam = "view", nativeViews = [], views = {}, hub } = options;
  options.before?.();
  const view = options.view !== undefined ? options.view : new URLSearchParams(window.location.search).get(viewParam);
  if (view !== null && nativeViews.includes(view)) return;
  const picked = view !== null && Object.prototype.hasOwnProperty.call(views, view) ? views[view] : hub;
  const rootOption = options.root ?? "root";
  const root = typeof rootOption === "string" ? document.getElementById(rootOption) : rootOption;
  if (!root) throw new Error(`bootstrapWindows: no element "${String(rootOption)}"`);
  picked.prepare?.();
  const element = await picked.render();
  createRoot(root).render(picked.strictMode ? <StrictMode>{element}</StrictMode> : element);
}
