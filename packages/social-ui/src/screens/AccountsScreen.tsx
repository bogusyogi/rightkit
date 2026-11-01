import { useMemo, useState } from "react";
import { Button, ConfirmDialog, EmptyState, Notice, PageShell, SkeletonRows, Toggle } from "@rightkit/react-shell/react";
import { AccountChip, LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import {
  SOCIAL_PLATFORMS,
  accountOrdinals,
  dayLabel,
  instantLabel,
  type Account,
  type DataSource,
  type SocialPlatform,
  type UnmappedAccount,
} from "../data/index.js";
import { ENTITLEMENT_LABEL, PLATFORM_LABEL, cooldownText, platformName, tokenText } from "./labels.js";
import { hostAppName } from "../appName.js";

interface Props {
  source: DataSource;
  rev: number;
  onChanged: () => void;
}

interface Loaded {
  accounts: Account[];
  /** Accounts the engine holds that this app cannot map yet. They are listed, never dropped. */
  unmapped: readonly UnmappedAccount[];
}

const SOURCE_LABEL = {
  numeric_retry_after: "numeric Retry-After header",
  dated_retry_after: "dated Retry-After header",
} as const;

/** Accounts grouped by venture, then platform in the engine's platform order. Order inside a group is the daemon's list order. */
function groupAccounts(accounts: readonly Account[]) {
  const ventures = new Map<string, { name: string; platforms: Map<SocialPlatform, Account[]> }>();
  for (const account of accounts) {
    const venture = ventures.get(account.venture.id) ?? { name: account.venture.name, platforms: new Map() };
    const list = venture.platforms.get(account.platform) ?? [];
    list.push(account);
    venture.platforms.set(account.platform, list);
    ventures.set(account.venture.id, venture);
  }
  return [...ventures.entries()].map(([id, v]) => ({
    id,
    name: v.name,
    platforms: SOCIAL_PLATFORMS.filter((p) => v.platforms.has(p)).map((p) => ({
      platform: p,
      accounts: v.platforms.get(p) ?? [],
    })),
  }));
}

/** Accounts whose platform or state this app does not show yet. Their readiness is not shown. */
function UnmappedAccounts({ rows }: { rows: readonly UnmappedAccount[] }) {
  return (
    <section className="op-venture" aria-labelledby="unmapped-accounts-title">
      <h2 id="unmapped-accounts-title" className="op-venture__title">
        Accounts this app does not show yet
      </h2>
      <p className="op-hint">
        The engine stores these accounts. Their readiness is not shown until this app knows their platform and state.
      </p>
      <div className="op-accounts">
        {rows.map((row, index) => (
          <article key={row.id} className="op-card op-account" aria-labelledby={`unmapped-${index}`}>
            <h3 id={`unmapped-${index}`} className="op-account__title">
              Account on {platformName(row.platform)}
            </h3>
            <p>
              Account id: <span className="op-mono">{row.id}</span>
            </p>
            <p>Reason: {row.reason}</p>
          </article>
        ))}
      </div>
    </section>
  );
}

export function AccountsScreen({ source, rev, onChanged }: Props) {
  const { state, reload } = useAsync<Loaded>(
    async () => ({
      accounts: await source.listAccounts(),
      unmapped: source.unmappedAccounts?.() ?? [],
    }),
    [source, rev],
  );
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);
  const [disconnecting, setDisconnecting] = useState<{ account: Account; label: string } | null>(null);

  const now = source.nowUtc();
  const loaded = state.status === "ready" ? state.value : null;
  const ordinals = useMemo(() => accountOrdinals(loaded?.accounts ?? []), [loaded]);

  async function setPaused(account: Account, paused: boolean, label: string) {
    setPendingId(account.id);
    const result = paused
      ? await source.pauseAccount(account.id, "Paused from desktop")
      : await source.resumeAccount(account.id);
    setPendingId(null);
    setNotice(
      result.ok
        ? {
            tone: "ok",
            text: paused ? `Publishing paused for ${label}. Its scheduled posts will not publish.` : `Publishing resumed for ${label}.`,
          }
        : { tone: "bad", text: result.error.message },
    );
    onChanged();
  }

  async function disconnect(account: Account, label: string) {
    setDisconnecting(null);
    setPendingId(account.id);
    const result = await source.disconnectAccount(account.id);
    setPendingId(null);
    if (!result.ok) {
      setNotice({ tone: "bad", text: result.error.message });
    } else {
      const { cancelledJobs, keptJobs } = result.value;
      const cancelled =
        cancelledJobs === 1 ? "1 queued or waiting post was cancelled." : `${cancelledJobs} queued or waiting posts were cancelled.`;
      const kept =
        keptJobs === 0
          ? ""
          : ` ${keptJobs} post${keptJobs === 1 ? " may" : "s may"} already be on the platform and ${keptJobs === 1 ? "was" : "were"} left for the daemon to check.`;
      setNotice({
        tone: "ok",
        text: `Disconnected ${label}. ${cancelled}${kept} The data ${hostAppName()} holds is not deleted; see the data deletion page to request that.`,
      });
    }
    onChanged();
  }

  return (
    <PageShell
      title="Accounts"
      subtitle="Readiness for each connected account. Token, entitlement and cooldown are shown separately and never merged."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      <div aria-live="polite">
        {notice ? (
          <Notice tone={notice.tone} className="op-notice">{notice.text}</Notice>
        ) : null}
      </div>

      {state.status === "error" ? (
        <LoadError message="Could not read accounts from the daemon." onRetry={reload} />
      ) : loaded === null ? (
        <SkeletonRows label="Loading accounts" rows={3} />
      ) : loaded.accounts.length === 0 && loaded.unmapped.length === 0 ? (
        <EmptyState title="No accounts connected">
          <p>Connect an account to start publishing. Connection runs through the provider sign-in in the browser.</p>
        </EmptyState>
      ) : (
        <>
          {groupAccounts(loaded.accounts).map((venture) => {
            return (
              <section key={venture.id} className="op-venture" aria-labelledby={`venture-${venture.id}`}>
                <h2 id={`venture-${venture.id}`} className="op-venture__title">
                  {venture.name}
                </h2>
                {venture.platforms.map((group) => (
                  <section key={group.platform} className="op-platform" aria-labelledby={`platform-${venture.id}-${group.platform}`}>
                    <h3 id={`platform-${venture.id}-${group.platform}`} className="op-platform__title">
                      {PLATFORM_LABEL[group.platform]}
                    </h3>
                    <div className="op-accounts">
                      {group.accounts.map((account) => {
                        const ordinal = ordinals.get(account.id);
                        const token = tokenText(account.readiness.token, now);
                        const ent = ENTITLEMENT_LABEL[account.readiness.entitlement];
                        const paused = account.readiness.paused;
                        const until = account.readiness.quotaCooldownUntil;
                        const label = `${ordinal?.label ?? "Account"} (${account.handle}, ${PLATFORM_LABEL[account.platform]})`;
                        const needsReconnect = account.readiness.token.status !== "valid";
                        return (
                          <article
                            key={account.id}
                            className={`op-card op-account op-band-${account.band}`}
                            aria-labelledby={`acc-${account.id}`}
                          >
                            <h4 id={`acc-${account.id}`} className="op-account__title">
                              <AccountChip account={account} ordinal={ordinal} detail />
                            </h4>
                            <dl className="op-dl">
                              <dt>Token</dt>
                              <dd>
                                <span className="op-mark">{token.mark}</span> {token.text}
                              </dd>

                              <dt>Re-authorisation</dt>
                              <dd>
                                {account.readiness.reauthNeededBy
                                  ? `Needed by ${dayLabel(account.readiness.reauthNeededBy)}. This platform issues no refresh, so the token cannot renew itself.`
                                  : "Not needed now"}
                              </dd>

                              <dt>Entitlement</dt>
                              <dd>
                                <strong>{ent.text}.</strong> {ent.hint}
                              </dd>

                              <dt>Quota cooldown</dt>
                              <dd>
                                {cooldownText(until, now)}
                                {until && cooldownText(until, now) !== "Cooldown has ended" ? (
                                  <>
                                    {" "}
                                    until {instantLabel(until, account.timeZone)}. Source: {SOURCE_LABEL[account.readiness.quotaCooldownSource ?? "numeric_retry_after"]}.
                                  </>
                                ) : null}
                              </dd>

                              <dt>Publishing</dt>
                              <dd>
                                {paused
                                  ? `Paused by ${paused.by} at ${instantLabel(paused.atUtc, account.timeZone)}. Reason: ${paused.reason}.`
                                  : "Running"}
                              </dd>
                            </dl>

                            <div className="op-account__actions">
                              <div className="op-pause">
                                <Toggle
                                  checked={paused !== null}
                                  disabled={pendingId !== null}
                                  label={`Pause publishing for ${account.displayName}, ${label}`}
                                  onChange={(next) => setPaused(account, next, label)}
                                />
                                <p className="op-pause__text">
                                  <strong>Pause publishing for {account.displayName}</strong>
                                  <br />
                                  {paused
                                    ? "Paused. Its scheduled posts will not publish until you resume."
                                    : "Running. Its scheduled posts publish at their time."}
                                </p>
                              </div>
                              {account.readiness.token.status !== "revoked" ? (
                                <>
                                  <Button
                                    variant="secondary"
                                    size="sm"
                                    disabled={pendingId !== null}
                                    onClick={() => setDisconnecting({ account, label })}
                                  >
                                    Disconnect {PLATFORM_LABEL[account.platform]}
                                  </Button>
                                  <p className="op-hint">
                                    Ends the connection and cancels this account's queued and waiting posts. It does not delete data.
                                  </p>
                                </>
                              ) : (
                                <p className="op-hint">Disconnected. Posts for this account are not published until it is connected again.</p>
                              )}
                              {needsReconnect ? (
                                <>
                                  <Button variant="secondary" size="sm" disabled>
                                    Reconnect
                                  </Button>
                                  <p className="op-hint">Reconnect opens the provider sign-in. It is not available in mock data.</p>
                                </>
                              ) : null}
                            </div>
                          </article>
                        );
                      })}
                    </div>
                  </section>
                ))}
              </section>
            );
          })}
          {loaded.unmapped.length > 0 ? <UnmappedAccounts rows={loaded.unmapped} /> : null}
        </>
      )}

      {disconnecting ? (
        <ConfirmDialog
          title={`Disconnect ${disconnecting.account.displayName} on ${PLATFORM_LABEL[disconnecting.account.platform]}?`}
          description={
            <>
              <span className="op-dialog-line">Account: {disconnecting.label}.</span>
              <span className="op-dialog-line">
                {hostAppName()} stops using this account. Its queued and waiting posts are cancelled and will not publish.
              </span>
              <span className="op-dialog-line">
                Posts already on the platform stay there, and the data {hostAppName()} holds is kept until you request deletion.
              </span>
            </>
          }
          confirmLabel={`Disconnect ${disconnecting.account.handle}`}
          cancelLabel="Cancel"
          onConfirm={() => disconnect(disconnecting.account, disconnecting.label)}
          onCancel={() => setDisconnecting(null)}
        />
      ) : null}
    </PageShell>
  );
}
