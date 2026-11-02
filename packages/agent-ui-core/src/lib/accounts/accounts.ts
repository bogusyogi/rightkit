import type { DaemonConfig } from "../types.js";

/**
 * Typed adapters for the engine's account API (issue #34). Wire shapes follow
 * engine/bins/coderight/src/api_accounts.rs and the accountRateLimited stream
 * event in engine/crates/protocol/src/event/protocol_event.rs.
 *   GET    /accounts                               -> { accounts: AccountView[] }
 *   POST   /accounts/api-key {label,provider,apiKey}
 *   DELETE /accounts/api-key/{provider}/{slug}
 *   GET    /accounts/selection?sessionId=&project= -> { accountId, source, account, unavailable }
 *   PUT    /accounts/session/{sessionId} {accountId|null}
 *   PUT    /accounts/project-default {project, accountId|null}
 */
export type AccountKind = "subscription" | "api_key";

export interface Account {
  /** `subscription/<provider>/<slug>` or `api_key/<provider>/<slug>`. */
  id: string;
  kind: AccountKind;
  provider: string;
  /** What the user named the account (profile or API-config label). Always present. */
  label: string;
  /** Only when the vendor CLI reported one. Never set for API-key configs. */
  email: string | null;
  organization: string | null;
  /** provider + email + org UUID identity key (#341). Null when the CLI exposes no identity. */
  fingerprint?: string | null;
  /** Subscription: signed in. API key: a key is stored. */
  ready: boolean;
}

export type RateLimitStatus = "allowed" | "warning" | "rejected";

export interface RateLimitEvent {
  sessionId: string | null;
  provider: string;
  /** Account display label as the engine reports it (label, else email, else profile id). */
  account: string;
  /** Stable account id when the engine reports it. */
  accountId: string | null;
  accountKind: AccountKind | null;
  email: string | null;
  organization: string | null;
  status: RateLimitStatus;
  rateLimitType: string | null;
  /** Epoch milliseconds when the cap lifts, or null when unknown. */
  resetsAt: number | null;
  /** 0..1 when reported. */
  utilization: number | null;
}

export interface AccountSelection {
  accountId: string | null;
  source: "session" | "project" | "none";
  account: Account | null;
  /** The selected account no longer exists; the user must choose again. */
  unavailable: boolean;
}

const str = (v: unknown): string | null => (typeof v === "string" && v.trim() ? v : null);

function parseKind(v: unknown): AccountKind | null {
  return v === "subscription" || v === "api_key" ? v : null;
}

function kindFromId(id: string): AccountKind | null {
  const head = id.split("/")[0];
  return head === "subscription" || head === "api_key" ? head : null;
}

export function parseAccount(raw: unknown): Account | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const id = str(r.id);
  if (!id) return null;
  const kind = parseKind(r.kind) ?? kindFromId(id);
  if (!kind) return null;
  return {
    id,
    kind,
    provider: str(r.provider) ?? "claude",
    label: str(r.label) ?? str(r.email) ?? id,
    // An API-key config has no mailbox; ignore anything an older engine put here.
    email: kind === "api_key" ? null : str(r.email),
    organization: str(r.organization),
    fingerprint: str(r.fingerprint),
    ready: r.ready === true,
  };
}

export function parseAccountList(raw: unknown): Account[] {
  const list = Array.isArray(raw)
    ? raw
    : raw && typeof raw === "object" && Array.isArray((raw as { accounts?: unknown }).accounts)
      ? (raw as { accounts: unknown[] }).accounts
      : [];
  return list.flatMap((item) => {
    const account = parseAccount(item);
    return account ? [account] : [];
  });
}

/** Parses an accountRateLimited stream event (camelCase fields, resetsAt in unix seconds). */
export function parseRateLimitEvent(raw: unknown): RateLimitEvent | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const account = str(r.account);
  if (!account) return null;
  const s = str(r.status);
  const status: RateLimitStatus =
    s === "rejected"
      ? "rejected"
      : s === "allowed_warning" || s === "warning"
        ? "warning"
        : "allowed";
  let resetsAt: number | null = null;
  if (typeof r.resetsAt === "number" && Number.isFinite(r.resetsAt)) {
    // The engine sends unix seconds; tolerate milliseconds.
    resetsAt = r.resetsAt < 1e12 ? r.resetsAt * 1000 : r.resetsAt;
  } else if (typeof r.resetsAt === "string") {
    const t = Date.parse(r.resetsAt);
    resetsAt = Number.isNaN(t) ? null : t;
  }
  const utilization =
    typeof r.utilization === "number" && Number.isFinite(r.utilization) ? r.utilization : null;
  const accountId = str(r.accountId);
  const accountKind = parseKind(r.accountKind) ?? (accountId ? kindFromId(accountId) : null);
  return {
    sessionId: str(r.sessionId),
    provider: str(r.provider) ?? "claude",
    account,
    accountId,
    accountKind,
    email: accountKind === "api_key" ? null : str(r.email),
    organization: str(r.organization),
    status,
    rateLimitType: str(r.rateLimitType),
    resetsAt,
    utilization,
  };
}

/** The listed account an event refers to. Events carry a label (email or profile id), not an id. */
export function accountForRateLimit(accounts: Account[], event: RateLimitEvent): Account | null {
  if (event.accountId) {
    const byId = accounts.find((a) => a.id === event.accountId);
    if (byId) return byId;
  }
  const provider = event.provider.toLowerCase();
  return (
    accounts.find(
      (a) =>
        a.provider.toLowerCase() === provider &&
        (a.label === event.account ||
          a.email === event.account ||
          a.id.endsWith(`/${event.account}`)),
    ) ?? null
  );
}

const baseUrl = (config: DaemonConfig) => config.url.replace(/\/+$/, "");

/** Failed account API call. `code` is the engine's machine-readable error code, when it sent one. */
export class AccountsRequestError extends Error {
  readonly status: number;
  readonly code: string | null;
  readonly body: Record<string, unknown> | null;

  constructor(
    message: string,
    status: number,
    code: string | null,
    body: Record<string, unknown> | null,
  ) {
    super(message);
    this.name = "AccountsRequestError";
    this.status = status;
    this.code = code;
    this.body = body;
  }
}

export const ACCOUNT_HOME_MISMATCH = "account_home_mismatch";

/** Same-home rule (#343): the chat already runs under an account with a different config home. */
export interface AccountHomeConflict {
  requestedAccountId: string | null;
  currentAccountId: string | null;
}

export function accountHomeConflict(error: unknown): AccountHomeConflict | null {
  if (!(error instanceof AccountsRequestError) || error.code !== ACCOUNT_HOME_MISMATCH) return null;
  return {
    requestedAccountId: str(error.body?.requestedAccountId),
    currentAccountId: str(error.body?.currentAccountId),
  };
}

async function accountsRequest(
  config: DaemonConfig,
  method: string,
  path: string,
  body?: unknown,
): Promise<unknown> {
  const headers: Record<string, string> = {};
  if (config.token) headers.authorization = `Bearer ${config.token}`;
  if (body !== undefined) headers["content-type"] = "application/json";
  const response = await fetch(`${baseUrl(config)}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    // Never echo the request body: it may carry an API key.
    let detail = "";
    let parsedBody: Record<string, unknown> | null = null;
    try {
      const parsed = (await response.json()) as Record<string, unknown>;
      parsedBody = parsed && typeof parsed === "object" ? parsed : null;
      detail = typeof parsed.error === "string" ? `: ${parsed.error}` : "";
    } catch {
      /* no body */
    }
    throw new AccountsRequestError(
      `${method} ${path.split("?")[0]} failed (${response.status})${detail}`,
      response.status,
      str(parsedBody?.code),
      parsedBody,
    );
  }
  try {
    return await response.json();
  } catch {
    return null;
  }
}

export async function fetchAccounts(config: DaemonConfig): Promise<Account[]> {
  return parseAccountList(await accountsRequest(config, "GET", "/accounts"));
}

/** The key is sent once and never returned: the response carries only the account row. */
export async function addApiKeyAccount(
  config: DaemonConfig,
  input: { label: string; provider: string; apiKey: string },
): Promise<Account | null> {
  const raw = await accountsRequest(config, "POST", "/accounts/api-key", input);
  return parseAccount((raw as { account?: unknown } | null)?.account);
}

/** Account ids are `api_key/<provider>/<slug>`; the route takes provider and slug. */
export async function removeApiKeyAccount(config: DaemonConfig, accountId: string): Promise<void> {
  const [kind, provider, slug] = accountId.split("/");
  if (kind !== "api_key" || !provider || !slug) throw new Error("Not an API-key account id");
  await accountsRequest(
    config,
    "DELETE",
    `/accounts/api-key/${encodeURIComponent(provider)}/${encodeURIComponent(slug)}`,
  );
}

export async function fetchAccountSelection(
  config: DaemonConfig,
  query: { sessionId?: string | null; project?: string | null },
): Promise<AccountSelection> {
  const params = new URLSearchParams();
  if (query.sessionId) params.set("sessionId", query.sessionId);
  if (query.project) params.set("project", query.project);
  const qs = params.toString();
  const raw = (await accountsRequest(
    config,
    "GET",
    `/accounts/selection${qs ? `?${qs}` : ""}`,
  )) as Record<string, unknown> | null;
  const source = raw?.source === "session" || raw?.source === "project" ? raw.source : "none";
  return {
    accountId: str(raw?.accountId),
    source,
    account: parseAccount(raw?.account),
    unavailable: raw?.unavailable === true,
  };
}

/**
 * `threadStarted: false` tells the engine the chat has not run yet, which lifts the same-home rule
 * (#343). Omit it for a chat that has history: a switch to an account with a different config home
 * is then refused with {@link ACCOUNT_HOME_MISMATCH}.
 */
export async function putSessionAccount(
  config: DaemonConfig,
  sessionId: string,
  accountId: string | null,
  options: { threadStarted?: boolean } = {},
): Promise<void> {
  await accountsRequest(config, "PUT", `/accounts/session/${encodeURIComponent(sessionId)}`, {
    accountId,
    ...(options.threadStarted === undefined ? {} : { threadStarted: options.threadStarted }),
  });
}

/** How a session authenticates (#347), as the engine's driver status route reports it. */
export type AuthMethod =
  | "subscription"
  | "api_key"
  | "api_key_env"
  | "oauth_token_env"
  | "ambient_login"
  | "unavailable";

export interface SessionAuthMethod {
  provider: string;
  method: AuthMethod;
  accountId: string | null;
  label: string | null;
  /** Credential variables removed from the child environment under a managed profile (#344). */
  strippedEnv: string[];
  /** Includes the warning that ANTHROPIC_API_KEY in the environment overrides a subscription. */
  warnings: string[];
}

const AUTH_METHODS: AuthMethod[] = [
  "subscription",
  "api_key",
  "api_key_env",
  "oauth_token_env",
  "ambient_login",
  "unavailable",
];

export function parseSessionAuthMethod(raw: unknown): SessionAuthMethod | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const method = AUTH_METHODS.find((m) => m === r.method);
  if (!method) return null;
  const strings = (v: unknown): string[] =>
    Array.isArray(v) ? v.filter((x): x is string => typeof x === "string" && x.length > 0) : [];
  return {
    provider: str(r.provider) ?? "claude",
    method,
    accountId: str(r.accountId),
    label: str(r.label),
    strippedEnv: strings(r.strippedEnv),
    warnings: strings(r.warnings),
  };
}

/** The session's active auth method, from the driver status route's `auth` block (#347). */
export async function fetchSessionAuthMethod(
  config: DaemonConfig,
  sessionId: string,
): Promise<SessionAuthMethod | null> {
  const raw = (await accountsRequest(
    config,
    "GET",
    `/sessions/${encodeURIComponent(sessionId)}/driver/status`,
  )) as { auth?: unknown } | null;
  return parseSessionAuthMethod(raw?.auth);
}

const AUTH_METHOD_TEXT: Record<AuthMethod, string> = {
  subscription: "Subscription login",
  api_key: "API key account",
  api_key_env: "API key from the environment",
  oauth_token_env: "OAuth token from the environment",
  ambient_login: "The CLI's own login",
  unavailable: "Unavailable",
};

/** One line for the UI: "Subscription login · Work". */
export function describeAuthMethod(auth: SessionAuthMethod): string {
  return [AUTH_METHOD_TEXT[auth.method], auth.label].filter(Boolean).join(" · ");
}

export async function putProjectDefaultAccount(
  config: DaemonConfig,
  project: string,
  accountId: string | null,
): Promise<void> {
  await accountsRequest(config, "PUT", "/accounts/project-default", { project, accountId });
}

/** Plain-text handoff for "Continue on account Y": recent turns, newest last. */
export function buildHandoffSummary(
  messages: { $kind: string; content?: unknown }[],
  cappedLabel: string,
  limit = 8,
): string {
  const turns = messages
    .filter((m) => (m.$kind === "user" || m.$kind === "assistant") && typeof m.content === "string")
    .slice(-limit)
    .map((m) => {
      const text = (m.content as string).trim().replace(/\s+/g, " ");
      const clipped = text.length > 600 ? `${text.slice(0, 600)}...` : text;
      return `${m.$kind === "user" ? "User" : "Assistant"}: ${clipped}`;
    });
  return [
    `Continuing from a previous session whose account (${cappedLabel}) hit its usage limit.`,
    turns.length ? "Recent conversation:" : "No prior conversation was recorded.",
    ...turns,
    "Pick up where it left off.",
  ].join("\n");
}

export const accountKindLabel = (kind: AccountKind): string =>
  kind === "subscription" ? "Subscription" : "API key";

export interface PickerAccountGroup {
  account: Account;
  heading: string;
  models: { id: string; label: string }[];
}
export interface PickerProviderGroup {
  provider: string;
  providerLabel: string;
  accounts: PickerAccountGroup[];
}

/** API keys for Anthropic belong with the Claude accounts. */
export const isClaudeProvider = (p: string): boolean =>
  p.toLowerCase() === "claude" || p.toLowerCase() === "anthropic";

const providerLabel = (p: string) => {
  const key = p.toLowerCase();
  return key === "claude" ? "Claude" : key === "codex" ? "Codex" : p;
};

/** provider > account > models. Accounts keep input order; providers sort by name. */
export function groupPickerAccounts(
  accounts: Account[],
  models: { id: string; label: string }[],
): PickerProviderGroup[] {
  const byProvider = new Map<string, PickerAccountGroup[]>();
  for (const account of accounts) {
    const key = isClaudeProvider(account.provider) ? "claude" : account.provider.toLowerCase();
    const name = [
      account.label,
      account.email && account.email !== account.label ? account.email : null,
    ]
      .filter(Boolean)
      .join(" · ");
    const list = byProvider.get(key) ?? [];
    list.push({ account, heading: `${name} (${accountKindLabel(account.kind)})`, models });
    byProvider.set(key, list);
  }
  return [...byProvider.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([provider, accts]) => ({
      provider,
      providerLabel: providerLabel(provider),
      accounts: accts,
    }));
}

export function formatCapUntil(resetsAt: number | null, now = Date.now()): string {
  if (resetsAt === null) return "later";
  const d = new Date(resetsAt);
  const sameDay = d.toDateString() === new Date(now).toDateString();
  const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return sameDay ? time : `${d.toLocaleDateString([], { month: "short", day: "numeric" })} ${time}`;
}
