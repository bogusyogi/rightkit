import { useMemo, useState } from "react";
import { Badge, Button, Notice, PageShell, SegmentedControl, SkeletonRows } from "@rightkit/react-shell/react";
import { AccountChip, LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import {
  accountOrdinals,
  instantLabel,
  type Account,
  type AccountOrdinal,
  type DataSource,
  type ModerationAction,
  type ModerationCapability,
  type ThreadItem,
} from "../data/index.js";
import { PLATFORM_LABEL } from "./labels.js";

type Filter = "open" | "snoozed" | "resolved";

interface Loaded {
  accounts: Account[];
  threads: ThreadItem[];
  capabilities: ModerationCapability[];
}

interface Props {
  source: DataSource;
  rev: number;
  onChanged: () => void;
}

const ACTION_LABEL: Record<ModerationAction, string> = {
  hide_comment: "Hide comment",
  delete_comment: "Delete comment",
  block_author: "Block author",
};

const SNOOZE_HOURS = 4;
const SNOOZE_DAY_HOURS = 24;

function threadStatusText(thread: ThreadItem, tz: string): { text: string; tone: "neutral" | "ok" | "warn" } {
  if (thread.status === "resolved") return { text: "Resolved", tone: "ok" };
  if (thread.status === "snoozed" && thread.snoozedUntilUtc) {
    return { text: `Snoozed until ${instantLabel(thread.snoozedUntilUtc, tz)}`, tone: "warn" };
  }
  return { text: "Open", tone: "neutral" };
}

function autoReplyBadge(thread: ThreadItem): { text: string; tone: "accent" | "bad" } | null {
  if (!thread.autoReply) return null;
  return thread.autoReply.state === "matched_not_sent"
    ? { text: "Would reply, not sent", tone: "accent" }
    : { text: "Auto-reply unavailable", tone: "bad" };
}

export function ThreadsScreen({ source, rev, onChanged }: Props) {
  const { state, reload, value } = useAsync<Loaded>(
    async () => {
      const [accounts, threads, capabilities] = await Promise.all([
        source.listAccounts(),
        source.listThreads(),
        source.listModerationCapabilities(),
      ]);
      return { accounts, threads, capabilities };
    },
    [source, rev],
  );

  const [filter, setFilter] = useState<Filter>("open");
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const accounts = useMemo(() => value?.accounts ?? [], [value]);
  const ordinals = useMemo(() => accountOrdinals(accounts), [accounts]);
  const all = value?.threads ?? [];
  const counts = {
    open: all.filter((t) => t.status === "open").length,
    snoozed: all.filter((t) => t.status === "snoozed").length,
    resolved: all.filter((t) => t.status === "resolved").length,
  };

  const visible = all.filter((t) => t.status === filter);
  const groups = accounts
    .map((account) => {
      const mine = visible.filter((t) => t.accountId === account.id);
      const posts = new Map<string, { title: string; publishedUtc: string; threads: ThreadItem[] }>();
      for (const t of mine) {
        const key = `${t.postTitle}|${t.postPublishedUtc}`;
        const post = posts.get(key) ?? { title: t.postTitle, publishedUtc: t.postPublishedUtc, threads: [] };
        post.threads.push(t);
        posts.set(key, post);
      }
      return { account, posts: [...posts.values()] };
    })
    .filter((g) => g.posts.length > 0);

  const selected = selectedId ? all.find((t) => t.id === selectedId) ?? null : null;
  const selectedAccount = selected ? accounts.find((a) => a.id === selected.accountId) ?? null : null;

  return (
    <PageShell
      title="Threads"
      subtitle="Comments and messages on published posts. Read, resolve and snooze here. Replies are not sent from this screen."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      <div className="op-inbox__filters">
        <SegmentedControl<Filter>
          label="Show"
          value={filter}
          onChange={(f) => {
            setFilter(f);
            setSelectedId(null);
          }}
          options={[
            { value: "open", label: `Open (${counts.open})` },
            { value: "snoozed", label: `Snoozed (${counts.snoozed})` },
            { value: "resolved", label: `Resolved (${counts.resolved})` },
          ]}
        />
      </div>

      {state.status === "error" ? (
        <LoadError message="Could not read threads from the daemon." onRetry={reload} />
      ) : value === null ? (
        <SkeletonRows label="Loading threads" rows={4} />
      ) : (
        <div className="op-inbox">
          <section className="op-inbox__list" aria-labelledby="threads-list-title">
            <h2 id="threads-list-title" className="op-section-title">Threads</h2>
            {groups.length === 0 ? <p className="op-hint">No threads in this view.</p> : null}
            {groups.map((group) => (
              <section key={group.account.id} className="op-inbox__group" aria-label={group.account.handle}>
                <div className="op-inbox__group-head">
                  <AccountChip account={group.account} ordinal={ordinals.get(group.account.id)} />
                </div>
                {group.posts.map((post) => (
                  <div key={`${post.title}|${post.publishedUtc}`} className="op-thread-post">
                    <h3 className="op-thread-post__title">
                      {post.title}, published {instantLabel(post.publishedUtc, group.account.timeZone)}
                    </h3>
                    <ul>
                      {post.threads.map((thread) => {
                        const latest = thread.messages[thread.messages.length - 1];
                        const badge = autoReplyBadge(thread);
                        return (
                          <li key={thread.id}>
                            <Button
                              type="button"
                              variant="secondary"
                              className="op-row"
                              aria-current={thread.id === selectedId ? "true" : undefined}
                              onClick={() => setSelectedId(thread.id)}
                            >
                              <span className="op-row__time">
                                {thread.kind === "dm" ? "Direct message" : "Comment"} from {latest?.author ?? "unknown"}
                              </span>
                              <span className="op-row__meta">{latest?.text ?? ""}</span>
                              {badge ? <Badge tone={badge.tone}>{badge.text}</Badge> : null}
                            </Button>
                          </li>
                        );
                      })}
                    </ul>
                  </div>
                ))}
              </section>
            ))}
          </section>

          <section className="op-inbox__preview" aria-label="Thread detail">
            {selected && selectedAccount ? (
              <ThreadDetail
                key={selected.id}
                thread={selected}
                account={selectedAccount}
                ordinal={ordinals.get(selectedAccount.id)}
                capabilities={value.capabilities.filter((c) => c.platform === selectedAccount.platform)}
                nowUtc={source.nowUtc()}
                source={source}
                onChanged={onChanged}
              />
            ) : (
              <p className="op-hint">Select a thread to read it and see what an automated reply would do.</p>
            )}
          </section>
        </div>
      )}
    </PageShell>
  );
}

function ThreadDetail({
  thread,
  account,
  ordinal,
  capabilities,
  nowUtc,
  source,
  onChanged,
}: {
  thread: ThreadItem;
  account: Account;
  ordinal: AccountOrdinal | undefined;
  capabilities: ModerationCapability[];
  nowUtc: string;
  source: DataSource;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);

  const tz = account.timeZone;
  const status = threadStatusText(thread, tz);
  const closesMs = thread.replyWindow ? Date.parse(thread.replyWindow.closesUtc) : null;
  const windowOpen = closesMs !== null && closesMs > Date.parse(nowUtc);
  const kindLabel = thread.kind === "dm" ? "Direct message" : "Comment";
  const resolved = thread.status === "resolved";

  async function resolveThread() {
    setBusy(true);
    const result = await source.resolveThread(thread.id);
    setBusy(false);
    if (result.ok) {
      setNotice({ tone: "ok", text: "Thread resolved." });
      onChanged();
    } else {
      setNotice({ tone: "bad", text: result.error.message });
    }
  }

  async function snooze(hours: number) {
    setBusy(true);
    const until = new Date(Date.parse(nowUtc) + hours * 3_600_000).toISOString();
    const result = await source.snoozeThread(thread.id, until);
    setBusy(false);
    if (result.ok) {
      setNotice({ tone: "ok", text: `Snoozed until ${instantLabel(until, tz)}.` });
      onChanged();
    } else {
      setNotice({ tone: "bad", text: result.error.message });
    }
  }

  return (
    <article className={`op-preview op-band-${account.band}`} aria-labelledby="thread-title">
      <header className="op-preview__account">
        <p className="op-kicker">{kindLabel} on {thread.postTitle}</p>
        <h2 id="thread-title" className="op-preview__title">
          {PLATFORM_LABEL[account.platform]}: {account.displayName} {account.handle}
        </h2>
        <AccountChip account={account} ordinal={ordinal} detail />
      </header>

      <section aria-labelledby="th-status">
        <h3 id="th-status" className="op-section-title">Status</h3>
        <p>{status.text}</p>
      </section>

      <section aria-labelledby="th-window">
        <h3 id="th-window" className="op-section-title">Reply window</h3>
        {closesMs === null ? (
          <p>No platform reply window applies to this kind of thread.</p>
        ) : windowOpen ? (
          <p>Reply window closes {instantLabel(thread.replyWindow!.closesUtc, tz)}.</p>
        ) : (
          <p>Reply window closed on {instantLabel(thread.replyWindow!.closesUtc, tz)}. Replies are not possible now.</p>
        )}
        <Button variant="secondary" size="sm" disabled>
          Reply
        </Button>
        <p className="op-hint">Replying is not part of this screen yet. Replies are drafted on the desktop in a later step.</p>
      </section>

      <section aria-labelledby="th-messages">
        <h3 id="th-messages" className="op-section-title">Messages</h3>
        <ol className="op-messages">
          {thread.messages.map((m) => (
            <li key={m.id}>
              <strong>{m.fromOperator ? "You" : m.author}</strong>{" "}
              <time dateTime={m.sentAtUtc}>{instantLabel(m.sentAtUtc, tz)}</time>
              <p className="op-caption">{m.text}</p>
            </li>
          ))}
        </ol>
      </section>

      <section aria-labelledby="th-auto">
        <h3 id="th-auto" className="op-section-title">Automated reply</h3>
        {thread.autoReply === null ? (
          <p>No automated reply rule matches this thread.</p>
        ) : thread.autoReply.state === "matched_not_sent" ? (
          <>
            <p>
              Rule <strong>{thread.autoReply.ruleName}</strong> matches the keyword{" "}
              <code>{thread.autoReply.matchedKeyword}</code>. Not sent: this is a preview only.
            </p>
            <p className="op-caption">{thread.autoReply.draftReply}</p>
          </>
        ) : (
          <>
            <p role="status" className="op-invalid">
              Automated reply unavailable for rule <strong>{thread.autoReply.ruleName}</strong>.{" "}
              {thread.autoReply.unavailableReason}
            </p>
          </>
        )}
      </section>

      <section aria-labelledby="th-moderation">
        <h3 id="th-moderation" className="op-section-title">Moderation on {PLATFORM_LABEL[account.platform]}</h3>
        {capabilities.length === 0 ? (
          <p className="op-hint">No moderation capability is listed for {PLATFORM_LABEL[account.platform]} in this app.</p>
        ) : null}
        <ul className="op-moderation">
          {capabilities.map((c) => (
            <li key={c.action}>
              <strong>{ACTION_LABEL[c.action]}:</strong> {c.supported ? "Supported." : "Unavailable."} {c.reason}
            </li>
          ))}
        </ul>
        <p className="op-hint">Mock capability list. Each row is confirmed per platform before live use.</p>
      </section>

      <section className="op-decide" aria-labelledby="th-decide">
        <h3 id="th-decide" className="op-section-title">Decision</h3>
        <div className="op-decide__buttons">
          <Button variant="primary" disabled={busy || resolved} onClick={resolveThread}>
            Resolve thread
          </Button>
          <Button variant="secondary" disabled={busy || resolved} onClick={() => snooze(SNOOZE_HOURS)}>
            Snooze {SNOOZE_HOURS} hours
          </Button>
          <Button variant="secondary" disabled={busy || resolved} onClick={() => snooze(SNOOZE_DAY_HOURS)}>
            Snooze 24 hours
          </Button>
        </div>
        <div aria-live="polite">
          {notice ? (
            <Notice tone={notice.tone} className="op-notice">{notice.text}</Notice>
          ) : null}
        </div>
      </section>
    </article>
  );
}
