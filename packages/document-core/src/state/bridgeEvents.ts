import { isTauri as inTauri } from "@rightkit/platform-ui";
import { EVENTS } from "@rightkit/tauri-shell";

export type Unlisten = () => void | Promise<void>;

export async function onAppEvent(
  event: "open-file" | "file-changed" | "viewright:open-url" | "prepare-setup",
  cb: (path: string) => void,
): Promise<Unlisten> {
  if (!inTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  if (event === "open-file") {
    // The shell delivers OS-opened files (launch argv, second launch, macOS open-document) as one
    // event carrying already-brokered display paths; the app's logical `open-file` is one path.
    return listen<{ paths?: string[] }>(EVENTS.openPaths, (e) => {
      for (const path of e.payload?.paths ?? []) cb(path);
    });
  }
  const un = await listen<string>(event, (e) => cb(e.payload));
  return un;
}

export async function onContextViewSource(cb: () => void): Promise<Unlisten> {
  if (!inTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  const un = await listen("viewright:context-view-source", () => cb());
  return un;
}

export async function onContextOpenLinkTab(cb: () => void): Promise<Unlisten> {
  if (!inTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  const un = await listen("viewright:context-open-link-tab", () => cb());
  return un;
}

export async function onContextOpenLinkExternal(cb: () => void): Promise<Unlisten> {
  if (!inTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  const un = await listen("viewright:context-open-link-external", () => cb());
  return un;
}
