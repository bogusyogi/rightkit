import { useEffect, useMemo, useRef, useState } from "react";
import { Button, EmptyState, Notice, PageShell, SkeletonRows } from "@rightkit/react-shell/react";
import { AccountChip, LoadError, MockBanner, PermalinkCopy } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import { outcomeUnknownExplanation } from "./labels.js";
import {
  accountOrdinals,
  instantLabel,
  type AccountOrdinal,
  type Account,
  type DataSource,
  type Digest,
  type FailureItem,
  type FailureResolution,
  type ReconcileResult,
} from "../data/index.js";

interface Loaded {
  accounts: Account[];
  failures: FailureItem[];
  digest: Digest;
}

interface Props {
  source: DataSource;
  rev: number;
  onChanged: () => void;
}

const KIND_ORDER: Record<FailureItem["kind"], number> = { outcome_unknown: 0, dead_letter: 1, alert: 2 };

const TITLE: Record<FailureItem["kind"], string> = {
  outcome_unknown: "Outcome unknown",
  dead_letter: "Stopped after repeated failures",
  alert: "Alert",
};

const ALERT_TITLE: Record<NonNullable<FailureItem["alertKind"]>, string> = {
  disk_full: "Alert: server disk is nearly full",
  credentials_expired: "Alert: account credentials expired",
  quota_cooldown: "Alert: quota cooldown",
};

const RESOLUTION_LABEL: Record<FailureResolution, string> = {
  published_confirmed: "Confirmed published",
  cancelled_not_published: "Cancelled, not published",
  accepted_as_lost: "Accepted as lost",
  acknowledged: "Acknowledged",
};

function reconcileText(result: ReconcileResult, nowLabel: (iso: string) => string): string {
  return result.outcome === "found_on_platform"
    ? `Found on the platform. Checked ${nowLabel(result.checkedAtUtc)}.`
    : `Not found on the platform. Checked ${nowLabel(result.checkedAtUtc)}. Safe to retry: the post did not go out.`;
}

export function FailuresScreen({ source, rev, onChanged }: Props) {
  // A resolved card leaves the open list and unmounts, so its own notice would never be read.
  // The result is announced from here, in a region that stays mounted.
  const [resolvedNotice, setResolvedNotice] = useState<string | null>(null);
  const listRef = useRef<HTMLElement | null>(null);
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  // Set by a resolve, read once the reloaded list is on screen: the next failure row, or the heading when none remain.
  const pendingFocus = useRef<{ rowId: string | null } | null>(null);
  const { state, reload, value } = useAsync<Loaded>(
    async () => {
      const [accounts, failures, digest] = await Promise.all([
        source.listAccounts(),
        source.listFailures(),
        source.listDigest(),
      ]);
      return { accounts, failures, digest };
    },
    [source, rev],
  );

  const accounts = value?.accounts ?? [];
  const ordinals = useMemo(() => accountOrdinals(accounts), [accounts]);
  const open = (value?.failures ?? [])
    .filter((f) => f.status === "open")
    .sort((a, b) => KIND_ORDER[a.kind] - KIND_ORDER[b.kind] || b.atUtc.localeCompare(a.atUtc));
  const resolved = (value?.failures ?? []).filter((f) => f.status === "resolved");

  function handleResolved(id: string, text: string) {
    const index = open.findIndex((f) => f.id === id);
    const remaining = open.filter((f) => f.id !== id);
    const next = remaining[index] ?? remaining[index - 1];
    pendingFocus.current = { rowId: next ? next.id : null };
    setResolvedNotice(text);
  }

  // The resolved card has unmounted and took focus with it. Once the reloaded list renders, move focus to the target.
  useEffect(() => {
    const pending = pendingFocus.current;
    if (pending === null) return;
    const rows = listRef.current ? Array.from(listRef.current.querySelectorAll<HTMLElement>("[data-failure-id]")) : [];
    const row = pending.rowId === null ? undefined : rows.find((el) => el.dataset.failureId === pending.rowId);
    const target = row ?? headingRef.current;
    if (target === null) return;
    pendingFocus.current = null;
    target.focus();
  }, [value]);

  return (
    <PageShell
      title="Failures"
      subtitle="Everything that stopped or may have published wrongly, with the one next action for each."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {state.status === "error" ? (
        <LoadError message="Could not read failures and the digest from the daemon." onRetry={reload} />
      ) : value === null ? (
        <SkeletonRows label="Loading failures and the digest" rows={4} />
      ) : (
        <>
          <div aria-live="polite">
            {resolvedNotice ? <Notice tone="ok" className="op-notice">{resolvedNotice}</Notice> : null}
          </div>

          <section className="op-digest" aria-labelledby="digest-title">
            <h2 id="digest-title" className="op-section-title">Last 24 hours</h2>
            <ul className="op-digest__list">
              {value.digest.sentences.map((sentence) => (
                <li key={sentence}>{sentence}</li>
              ))}
            </ul>
          </section>

          <section ref={listRef} aria-labelledby="open-title">
            <h2 id="open-title" ref={headingRef} tabIndex={-1} className="op-section-title">
              Needs action ({open.length})
            </h2>
            {open.length === 0 ? (
              <EmptyState title="No open failures">
                <p>Nothing has stopped or may have published wrongly.</p>
              </EmptyState>
            ) : (
              <div className="op-failures">
                {open.map((item) => (
                  <FailureCard
                    key={item.id}
                    item={item}
                    account={accounts.find((a) => a.id === item.accountId) ?? null}
                    ordinal={item.accountId ? ordinals.get(item.accountId) : undefined}
                    source={source}
                    onChanged={onChanged}
                    onResolved={handleResolved}
                  />
                ))}
              </div>
            )}
          </section>

          {resolved.length > 0 ? (
            <details className="op-resolved">
              <summary>Resolved ({resolved.length})</summary>
              <ul>
                {resolved.map((item) => (
                  <li key={item.id}>
                    {item.resolution ? RESOLUTION_LABEL[item.resolution] : "Resolved"}: {item.summary}
                    {item.resolutionNote ? ` Note: ${item.resolutionNote}` : ""}
                  </li>
                ))}
              </ul>
            </details>
          ) : null}
        </>
      )}
    </PageShell>
  );
}

function FailureCard({
  item,
  account,
  ordinal,
  source,
  onChanged,
  onResolved,
}: {
  item: FailureItem;
  account: Account | null;
  ordinal: AccountOrdinal | undefined;
  source: DataSource;
  onChanged: () => void;
  onResolved: (id: string, text: string) => void;
}) {
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);
  const [reconcile, setReconcile] = useState<ReconcileResult | null>(item.reconcile);

  const tz = account?.timeZone ?? "UTC";
  const when = (iso: string) => instantLabel(iso, tz);
  const title = item.kind === "alert" && item.alertKind ? ALERT_TITLE[item.alertKind] : TITLE[item.kind];

  async function check() {
    if (!item.jobId) return;
    setBusy(true);
    const result = await source.reconcile(item.jobId);
    setBusy(false);
    if (result.ok) {
      setReconcile(result.value);
      setNotice({ tone: "ok", text: "The platform check finished. Choose the resolution that matches it." });
    } else {
      setNotice({ tone: "bad", text: result.error.message });
    }
  }

  async function resolve(resolution: FailureResolution) {
    setBusy(true);
    const result = await source.resolveFailure(item.id, resolution, note);
    setBusy(false);
    if (result.ok) {
      onResolved(item.id, `Recorded: ${RESOLUTION_LABEL[resolution]}.`);
      onChanged();
    } else {
      setNotice({ tone: "bad", text: result.error.message });
    }
  }

  const noteMissing = note.trim() === "";
  const reconciled = reconcile !== null;

  return (
    <article className="op-card op-failure" aria-labelledby={`f-${item.id}`} data-failure-id={item.id} tabIndex={-1}>
      <h3 id={`f-${item.id}`} className="op-failure__title">{title}</h3>
      {account ? <AccountChip account={account} ordinal={ordinal} detail /> : null}

      <dl className="op-dl">
        <dt>What happened</dt>
        <dd>{item.summary}</dd>
        <dt>When</dt>
        <dd>{when(item.atUtc)}</dd>
        {item.attempts !== null ? (
          <>
            <dt>Attempts</dt>
            <dd>{item.attempts}</dd>
          </>
        ) : null}
        <dt>Next action</dt>
        <dd><strong>{item.nextAction}</strong></dd>
      </dl>

      {item.kind === "outcome_unknown" ? (
        <div className="op-failure__check">
          <p>
            <strong>{outcomeUnknownExplanation()}</strong>
          </p>
          {reconcile === null ? (
            <p>Not checked yet. Checking reads the platform. It does not publish or retry.</p>
          ) : (
            <p>{reconcileText(reconcile, when)}</p>
          )}
          {reconcile !== null && reconcile.outcome === "found_on_platform" ? (
            <>
              <p>
                Platform id <code>{reconcile.remoteId}</code>
              </p>
              <PermalinkCopy url={reconcile.permalink} />
            </>
          ) : null}
          {!reconciled ? (
            <Button variant="secondary" size="sm" disabled={busy} onClick={check}>
              Check the platform
            </Button>
          ) : null}
        </div>
      ) : null}

      <div aria-live="polite">
        {notice ? (
          <Notice tone={notice.tone} className="op-notice">{notice.text}</Notice>
        ) : null}
      </div>

      {item.kind === "alert" ? (
        <div className="op-decide__buttons">
          <Button variant="secondary" disabled={busy} onClick={() => resolve("acknowledged")}>
            Acknowledge alert
          </Button>
          {item.alertKind === "credentials_expired" ? (
            <>
              <Button variant="secondary" disabled>
                Reconnect account
              </Button>
              <p className="op-hint">Reconnect opens the provider sign-in. It is not available in mock data.</p>
            </>
          ) : null}
        </div>
      ) : (
        <div className="op-failure__resolve">
          <label className="op-field-label" htmlFor={`note-${item.id}`}>
            Note (required to accept as lost)
          </label>
          <textarea
            id={`note-${item.id}`}
            className="op-textarea"
            rows={2}
            value={note}
            onChange={(e) => setNote(e.target.value)}
          />
          <div className="op-decide__buttons">
            {item.kind === "outcome_unknown" ? (
              <>
                <Button
                  variant="primary"
                  disabled={busy || reconcile?.outcome !== "found_on_platform"}
                  onClick={() => resolve("published_confirmed")}
                >
                  Mark published (platform confirmed)
                </Button>
                <Button
                  variant="secondary"
                  disabled={busy || reconcile?.outcome !== "not_found"}
                  onClick={() => resolve("cancelled_not_published")}
                >
                  Mark cancelled, not published
                </Button>
              </>
            ) : (
              <Button variant="secondary" disabled={busy} onClick={() => resolve("cancelled_not_published")}>
                Mark cancelled, not published
              </Button>
            )}
            <Button variant="danger" disabled={busy || noteMissing} onClick={() => resolve("accepted_as_lost")}>
              Accept as lost
            </Button>
          </div>
          {item.kind === "outcome_unknown" && !reconciled ? (
            <p className="op-hint">Publishing resolutions unlock after the platform check.</p>
          ) : null}
        </div>
      )}
    </article>
  );
}
