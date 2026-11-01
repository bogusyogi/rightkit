import { invoke } from "./invoke.js";

export interface AgentRow {
  id: string;
  name: string;
  profile: string;
  accounts: string[];
  created_at: number;
  revoked_at: number | null;
}

export interface AgentAuditRow {
  id: number;
  at: number;
  agent_name: string | null;
  account_id: string | null;
  method: string;
  outcome: string;
  detail: string | null;
}

export function agentsList(): Promise<AgentRow[]> {
  return invoke<AgentRow[]>("agents_list");
}

/** Pairs a read-only agent. The key goes to the credential store, never here. */
export function agentPair(name: string, accounts: string[]): Promise<AgentRow> {
  return invoke<AgentRow>("agent_pair", { name, accounts });
}

export function agentRevoke(id: string): Promise<void> {
  return invoke<void>("agent_revoke", { id });
}

/** Profiles Settings can set. `send_with_approval` still needs the user to approve each message. */
export type AgentProfile = "read_only" | "draft" | "send_with_approval";

export function agentSetProfile(
  id: string,
  profile: AgentProfile,
): Promise<AgentRow> {
  return invoke<AgentRow>("agent_set_profile", { id, profile });
}

/** Whether agent-made drafts get the Gmail label "Agent draft" (off by default). */
export function agentLabelDrafts(): Promise<boolean> {
  return invoke<boolean>("agent_label_drafts");
}

export function agentSetLabelDrafts(enabled: boolean): Promise<void> {
  return invoke<void>("agent_set_label_drafts", { enabled });
}

/** Newest first, at most 200 rows. `beforeId` is the oldest id already shown. */
export function agentAuditList(
  limit?: number,
  beforeId?: number,
): Promise<AgentAuditRow[]> {
  return invoke<AgentAuditRow[]>("agent_audit_list", { limit, beforeId });
}

/** Deletes the files agents exported with `attachment`. Returns how many were removed. */
export function agentClearExports(): Promise<number> {
  return invoke<number>("agent_clear_exports");
}

export interface AgentImportedFile {
  /** The name the agent attaches the file by, such as `attachment-3.pdf`. */
  name: string;
  size: number;
  /** The type can run code: it is kept as `.blocked` and cannot be attached. */
  extension_blocked: boolean;
}

/**
 * Gives an agent a file the user chose: the app copies the bytes (base64) into
 * that agent's export folder. No path is sent; the agent never names one.
 */
export function agentImportFile(
  agentId: string,
  name: string,
  dataB64: string,
): Promise<AgentImportedFile> {
  return invoke<AgentImportedFile>("agent_import_file", {
    agentId,
    name,
    dataB64,
  });
}
