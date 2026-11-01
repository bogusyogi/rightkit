// Browser-QA mock for semantic search: a switch that persists for the session,
// a status that follows it, and an install that reports the "no source" error.

import { QA_THREADS } from "./qaMockData";
import type { InvokeArgs } from "./qaMockTypes";

let enabled = false;

export function qaSemanticInvoke(
  command: string,
  args?: InvokeArgs,
): { handled: boolean; value: unknown } {
  switch (command) {
    case "semantic_status":
      return {
        handled: true,
        value: { pro: true, enabled, embed_model_id: "embeddinggemma2-q4" },
      };
    case "set_semantic_enabled":
      enabled = Boolean(args?.enabled);
      return { handled: true, value: undefined };
    case "semantic_index_status":
      return {
        handled: true,
        value: {
          state: enabled ? "ready" : "off",
          error_code: null,
          can_install: false,
          indexed: enabled ? QA_THREADS.length : 0,
          total: QA_THREADS.length,
          accounts: [],
        },
      };
    case "semantic_search":
      return { handled: true, value: enabled ? QA_THREADS.slice(0, 2) : [] };
    case "install_semantic_model":
      throw new Error("No model download source is configured in this build.");
    default:
      return { handled: false, value: undefined };
  }
}
