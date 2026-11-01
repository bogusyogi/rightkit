import { invoke } from "./invoke.js";
import type { Thread } from "./mail.js";

export type SemanticState =
  | "off"
  | "model_missing"
  | "runtime_missing"
  | "indexing"
  | "ready"
  | "error";

export interface SemanticAccountProgress {
  account_id: string;
  indexed: number;
  total: number;
  done: boolean;
}

/** Index state for the Settings line and the search box. Never message content. */
export interface SemanticIndexStatus {
  state: SemanticState;
  /** Short code, set only when state is "error". */
  error_code: string | null;
  /** A model install source exists in this build. */
  can_install: boolean;
  indexed: number;
  total: number;
  accounts: SemanticAccountProgress[];
}

export interface SemanticSwitch {
  pro: boolean;
  enabled: boolean;
  embed_model_id: string;
}

export function semanticIndexStatus(): Promise<SemanticIndexStatus> {
  return invoke<SemanticIndexStatus>("semantic_index_status");
}

export function semanticSwitch(): Promise<SemanticSwitch> {
  return invoke<SemanticSwitch>("semantic_status");
}

/** Rejects with a ready-to-show sentence when Pro is missing. */
export function setSemanticEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_semantic_enabled", { enabled });
}

/** Rejects with a ready-to-show sentence when no install source exists. */
export function installSemanticModel(): Promise<unknown> {
  return invoke<unknown>("install_semantic_model");
}

export function semanticSearch(
  query: string,
  accountId: string | null = null,
  label: string | null = null,
  category: string | null = null,
): Promise<Thread[]> {
  return invoke<Thread[]>("semantic_search", {
    query,
    accountId,
    label,
    category,
  });
}
