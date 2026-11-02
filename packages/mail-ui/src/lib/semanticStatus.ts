// Plain-words mapping for the semantic search status (Settings line, search box).
import type { SemanticState } from "./ipc/semantic.js";
import { hostAppName } from "../appName.js";

export interface SemanticStatusView {
  state: SemanticState;
  error_code?: string | null;
  can_install?: boolean;
  indexed: number;
  total: number;
}

const COUNT = new Intl.NumberFormat("en-US");

/** One line for the Settings page. `null` while the first answer is loading. */
export function semanticStatusText(s: SemanticStatusView | null): string {
  if (!s) return "Checking status...";
  switch (s.state) {
    case "off":
      return "Off";
    case "runtime_missing":
      return "Not available on this computer yet";
    case "model_missing":
      return "Model not installed";
    case "indexing":
      return `Indexing ${COUNT.format(s.indexed)} of ${COUNT.format(s.total)}`;
    case "ready":
      return "Ready";
    case "error":
      return s.error_code
        ? `Indexing stopped (${s.error_code}). Restart ${hostAppName()} to try again.`
        : `Indexing stopped. Restart ${hostAppName()} to try again.`;
  }
}

/** The Install model button shows only when the model is missing and installable. */
export function showInstallModel(s: SemanticStatusView | null): boolean {
  return s?.state === "model_missing" && s.can_install === true;
}

/** The Meaning toggle in the search box shows only when search can run. */
export function meaningAvailable(s: SemanticStatusView | null): boolean {
  return s?.state === "ready";
}
