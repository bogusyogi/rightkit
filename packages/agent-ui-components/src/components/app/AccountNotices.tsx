import { useEffect, useState } from "react";
import {
  describeAuthMethod,
  fetchSessionAuthMethod,
  type Account,
  type AccountHomeConflict,
  type SessionAuthMethod,
} from "@rightkit/agent-ui-core/lib/accounts/accounts";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";
import { hostAppName } from "../../appName";

/** The auth method of a vendor session, with the env-override warning (#347, #344). */
export function AuthMethodLine({
  auth,
}: {
  auth: SessionAuthMethod | null;
}) {
  if (!auth) return null;
  const warn = auth.warnings.length > 0;
  return (
    <div
      data-testid="auth-method-line"
      data-auth-method={auth.method}
      className={`flex flex-col gap-0.5 rounded-md border px-2 py-1 text-[11px] ${warn ? "border-warning/40 bg-warning/10 text-warning" : "border-line text-text-3"}`}
    >
      <span>{`Auth: ${describeAuthMethod(auth)}`}</span>
      {auth.strippedEnv.length > 0 ? (
        <span data-testid="auth-env-stripped">
          {`Ignored in the environment so the account login is used: ${auth.strippedEnv.join(", ")}`}
        </span>
      ) : null}
      {auth.warnings.map((warning) => (
        <span key={warning} role="alert" data-testid="auth-warning">
          {warning}
        </span>
      ))}
    </div>
  );
}

/** Loads the session's auth method once per session and shows it. Silent when unavailable. */
export function SessionAuthMethodLine({
  config,
  sessionId,
}: {
  config: DaemonConfig | null;
  sessionId: string;
}) {
  const [auth, setAuth] = useState<SessionAuthMethod | null>(null);
  useEffect(() => {
    setAuth(null);
    if (!config) return;
    let cancelled = false;
    void fetchSessionAuthMethod(config, sessionId)
      .then((value) => {
        if (!cancelled) setAuth(value);
      })
      .catch(() => {
        /* no auth block for this session: show nothing */
      });
    return () => {
      cancelled = true;
    };
  }, [config, sessionId]);
  return <AuthMethodLine auth={auth} />;
}

/**
 * Same-home rule (#343): the chat cannot move to an account with a different config home, so offer
 * a new session on the chosen account (seeded with a handoff). Never switches by itself.
 */
export function AccountSwitchNotice({
  conflict,
  accounts,
  onStartNewSession,
  onDismiss,
}: {
  conflict: AccountHomeConflict | null;
  accounts: Account[];
  onStartNewSession: (accountId: string) => void;
  onDismiss: () => void;
}) {
  if (!conflict?.requestedAccountId) return null;
  const requestedId = conflict.requestedAccountId;
  const requested = accounts.find((a) => a.id === requestedId);
  const name = requested?.label ?? requestedId;
  return (
    <div
      role="alert"
      data-testid="account-switch-notice"
      className="flex flex-wrap items-center gap-2 rounded-md border border-warning/40 bg-warning/10 p-2 text-[12px] text-warning"
    >
      <span>{`This chat keeps its account because ${name} uses a different config home.`}</span>
      <button
        type="button"
        onClick={() => onStartNewSession(requestedId)}
        className="rounded border border-line px-2 py-0.5 text-text hover:bg-panel-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
      >
        {`Start a new session with ${name}`}
      </button>
      <button
        type="button"
        onClick={onDismiss}
        className="rounded px-2 py-0.5 text-text-3 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
      >
        Keep this account
      </button>
    </div>
  );
}

export const SUBSCRIPTION_NOTICE_STORAGE_KEY = "cr.accounts.subscriptionNoticeDismissed.v1";

export function readSubscriptionNoticeDismissed(): boolean {
  try {
    return localStorage.getItem(SUBSCRIPTION_NOTICE_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

function writeSubscriptionNoticeDismissed() {
  try {
    localStorage.setItem(SUBSCRIPTION_NOTICE_STORAGE_KEY, "1");
  } catch {
    /* best effort: the notice may show again next launch */
  }
}

/**
 * First-run notice (#348): subscription use draws on the user's own plan limits. Renders once,
 * only when at least one subscription account exists, and stays dismissed once dismissed.
 */
export function SubscriptionUsageNotice({ accounts }: { accounts: Account[] }) {
  const [dismissed, setDismissed] = useState(readSubscriptionNoticeDismissed);
  const hasSubscription = accounts.some((a) => a.kind === "subscription");
  if (dismissed || !hasSubscription) return null;
  return (
    <div
      role="status"
      data-testid="subscription-usage-notice"
      className="flex items-center gap-2 rounded-md border border-line bg-panel p-2 text-[12px] text-text-2"
    >
      <span className="min-w-0 flex-1">
        Chats on a subscription account use that account&apos;s own plan limits. {hostAppName()} never
        sees your login; the official CLI handles sign-in.
      </span>
      <button
        type="button"
        onClick={() => {
          writeSubscriptionNoticeDismissed();
          setDismissed(true);
        }}
        className="shrink-0 rounded border border-line px-2 py-0.5 text-text hover:bg-panel-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
      >
        Got it
      </button>
    </div>
  );
}
