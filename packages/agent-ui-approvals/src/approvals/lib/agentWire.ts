// rightkit-agent-gateway 0.3.0 renamed the agent wire: an agent's `accounts` became `scopes` and an audit
// row's `account_id` became `scope`. These read either name, so the views work with hosts on both gateway
// versions and with @rightkit/mail-ui rows normalized or not.

/** An agent row on either wire. */
export interface AgentScopesWire {
  scopes?: readonly string[] | null;
  accounts?: readonly string[] | null;
}

/** An audit row on either wire. */
export interface AuditScopeWire {
  scope?: string | null;
  account_id?: string | null;
}

/** What the agent may act in: `scopes`, else the pre-0.3.0 `accounts`, else none. */
export function agentScopes(agent: AgentScopesWire): readonly string[] {
  return agent.scopes ?? agent.accounts ?? [];
}

/** The account or folder an audited call acted in: `scope`, else the pre-0.3.0 `account_id`, else null. */
export function auditScope(row: AuditScopeWire): string | null {
  return row.scope ?? row.account_id ?? null;
}
