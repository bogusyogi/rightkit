export type DialogKind = "confirm" | "prompt";

export interface DialogConfig {
  kind: DialogKind;
  message: string;
  initial?: string;
}

let showHandler: ((cfg: DialogConfig) => Promise<string | null>) | null = null;

export function setDialogHandler(handler: ((cfg: DialogConfig) => Promise<string | null>) | null) {
  showHandler = handler;
}

/** Call from anywhere (store actions, hooks, components). Returns the user's
 *  input (prompt) or "ok" (confirm), or null if cancelled/dismissed. */
export function showDialog(cfg: DialogConfig): Promise<string | null> {
  if (showHandler) return showHandler(cfg);
  // Fallback before the provider mounts (rare: only during very early render).
  if (cfg.kind === "confirm") return Promise.resolve(window.confirm(cfg.message) ? "ok" : null);
  return Promise.resolve(window.prompt(cfg.message, cfg.initial ?? ""));
}
