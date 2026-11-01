import {
  accountForRateLimit,
  formatCapUntil,
  type Account,
  type RateLimitEvent,
} from "@rightkit/agent-ui-core/lib/accounts/accounts";

export interface RateLimitBannerProps {
  event: RateLimitEvent | null;
  accounts: Account[];
  now?: number;
  /** Manual only: starts a new session on the chosen account with a handoff. */
  onContinueOnAccount: (accountId: string) => void;
}

/** Shown only when the session's account is rejected. Never switches automatically. */
export default function RateLimitBanner({
  event,
  accounts,
  now,
  onContinueOnAccount,
}: RateLimitBannerProps) {
  if (!event || event.status !== "rejected") return null;
  const capped = accountForRateLimit(accounts, event);
  const name = capped?.label ?? event.account;
  const others = accounts.filter((a) => a.id !== capped?.id && a.ready);
  return (
    <div
      role="alert"
      data-testid="rate-limit-banner"
      className="flex flex-wrap items-center gap-2 rounded-md border border-warning/40 bg-warning/10 p-2 text-[12px] text-warning"
    >
      <span>{`Account ${name} capped until ${formatCapUntil(event.resetsAt, now)}`}</span>
      {others.map((account) => (
        <button
          key={account.id}
          type="button"
          onClick={() => onContinueOnAccount(account.id)}
          className="rounded border border-line px-2 py-0.5 text-text hover:bg-panel-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          {`Continue on account ${account.label}`}
        </button>
      ))}
    </div>
  );
}
