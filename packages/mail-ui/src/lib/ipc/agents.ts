import { invoke } from "./invoke.js";

// rightkit-agent-gateway 0.3.0 renamed the agent wire: an agent's `accounts` became `scopes`, an audit row's
// `account_id` became `scope`. Hosts on either gateway version must work, so every row read here is
// normalized: both names carry the same value whichever one the host sent.

export interface AgentRow {
  id: string;
  name: string;
  profile: string;
  /** What the agent may act in (accounts, folders). Gateway 0.3.0's name; filled from `accounts` for older hosts. */
  scopes: string[];
  /** The pre-0.3.0 name of `scopes`; always equal to it after normalization. */
  accounts: string[];
  created_at: number;
  revoked_at: number | null;
}

export interface AgentAuditRow {
  id: number;
  at: number;
  agent_name: string | null;
  /** The account or folder the call acted in. Gateway 0.3.0's name; filled from `account_id` for older hosts. */
  scope: string | null;
  /** The pre-0.3.0 name of `scope`; always equal to it after normalization. */
  account_id: string | null;
  method: string;
  outcome: string;
  detail: string | null;
}

/** An agent row from a host on either gateway wire (`scopes` or the older `accounts`), with both names set. */
export function normalizeAgentRow(raw: Omit<AgentRow, "scopes" | "accounts"> & { scopes?: string[] | null; accounts?: string[] | null }): AgentRow {
  const scopes = raw.scopes ?? raw.accounts ?? [];
  return { ...raw, scopes, accounts: scopes };
}

/** An audit row from a host on either gateway wire (`scope` or the older `account_id`), with both names set. */
export function normalizeAgentAuditRow(
  raw: Omit<AgentAuditRow, "scope" | "account_id"> & { scope?: string | null; account_id?: string | null },
): AgentAuditRow {
  const scope = raw.scope ?? raw.account_id ?? null;
  return { ...raw, scope, account_id: scope };
}

type RawAgentRow = Parameters<typeof normalizeAgentRow>[0];
type RawAgentAuditRow = Parameters<typeof normalizeAgentAuditRow>[0];

export function agentsList(): Promise<AgentRow[]> {
  return invoke<RawAgentRow[]>("agents_list").then((rows) => rows.map(normalizeAgentRow));
}

/** Pairs a read-only agent. The key goes to the credential store, never here. */
export function agentPair(name: string, accounts: string[]): Promise<AgentRow> {
  return invoke<RawAgentRow>("agent_pair", { name, accounts }).then(normalizeAgentRow);
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
  return invoke<RawAgentRow>("agent_set_profile", { id, profile }).then(normalizeAgentRow);
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
  return invoke<RawAgentAuditRow[]>("agent_audit_list", { limit, beforeId }).then((rows) => rows.map(normalizeAgentAuditRow));
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
