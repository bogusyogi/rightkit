import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { moveIndex } from "@rightkit/platform-ui/headless";
import { Button, Card, EmptyState, PageShell, SegmentedControl, SkeletonRows } from "@rightkit/react-shell/react";
import { AccountChip, JobBadges, LoadError, MockBanner, PublishProgressView } from "../components/Shared";
import { useAsync } from "../hooks/useAsync";
import {
  accountOrdinals,
  dateKeyAdd,
  dateKeyInZone,
  dayLabel,
  instantLabel,
  mediaReady,
  mondayOf,
  startOfDateUtc,
  timeInZone,
  weekDays,
  zoneAbbr,
  type Account,
  type DataSource,
  type PublishJob,
} from "../data";
import { FORMAT_LABEL, MEDIA_STATUS_LABEL } from "./labels";

type View = "day" | "week";

interface Loaded {
  accounts: Account[];
  jobs: PublishJob[];
}

interface Props {
  source: DataSource;
  /** Bumped by the app after any write, so the calendar re-reads. */
  rev: number;
  onOpenJob: (jobId: string) => void;
  onOpenApprovals: () => void;
}

const operatorZone = (() => {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
})();

function mediaSummary(job: PublishJob): string {
  if (mediaReady(job)) {
    const n = job.mediaItems.length;
    return `Media ready${n > 1 ? ` (${n} items)` : ""}`;
  }
  const uploading = job.mediaItems.find((m) => m.status === "uploading");
  if (uploading) return `Media uploading ${uploading.uploadPercent ?? 0}%`;
  if (job.mediaItems.some((m) => m.status === "missing")) return `Media ${MEDIA_STATUS_LABEL.missing.toLowerCase()}`;
  return "Media not ready";
}

export function CalendarScreen({ source, rev, onOpenJob, onOpenApprovals }: Props) {
  const today = dateKeyInZone(source.nowUtc(), operatorZone);
  const [anchor, setAnchor] = useState(today);
  const [view, setView] = useState<View>("week");
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const days = useMemo(
    () => (view === "week" ? weekDays(mondayOf(anchor)) : [anchor]),
    [anchor, view],
  );
  const first = days[0];
  const last = days[days.length - 1];
  // One day of slack each side, because a job is bucketed by its account's local date.
  const fromUtc = startOfDateUtc(dateKeyAdd(first, -1));
  const toUtc = startOfDateUtc(dateKeyAdd(last, 2));

  const { state, reload } = useAsync<Loaded>(
    async () => {
      const [accounts, jobs] = await Promise.all([
        source.listAccounts(),
        source.listJobs({ fromUtc, toUtc }),
      ]);
      return { accounts, jobs };
    },
    [source, fromUtc, toUtc, rev],
  );

  // Keep the last good data on screen while a new range loads, so arrow-key moves do not flash.
  const lastGood = useRef<Loaded | null>(null);
  useEffect(() => {
    if (state.status === "ready") lastGood.current = state.value;
  }, [state]);
  const data = state.status === "ready" ? state.value : lastGood.current;

  const containerRef = useRef<HTMLDivElement>(null);
  const pendingFocus = useRef(false);

  useEffect(() => {
    if (!pendingFocus.current || !containerRef.current) return;
    const firstEntry = containerRef.current.querySelector<HTMLElement>("[data-entry]");
    const target = firstEntry ?? containerRef.current.querySelector<HTMLElement>("[data-day-heading]");
    if (target) {
      target.focus();
      pendingFocus.current = false;
    }
  });

  const ordinals = useMemo(() => accountOrdinals(data?.accounts ?? []), [data]);
  const accountById = useMemo(() => new Map((data?.accounts ?? []).map((a) => [a.id, a])), [data]);

  const jobsByDay = useMemo(() => {
    const map = new Map<string, PublishJob[]>();
    for (const day of days) map.set(day, []);
    for (const job of data?.jobs ?? []) {
      const key = dateKeyInZone(job.scheduledAtUtc, job.timeZone);
      const list = map.get(key);
      if (list) list.push(job);
    }
    for (const list of map.values()) list.sort((a, b) => a.scheduledAtUtc.localeCompare(b.scheduledAtUtc));
    return map;
  }, [data, days]);

  const visibleCount = [...jobsByDay.values()].reduce((n, list) => n + list.length, 0);
  const selected = selectedId ? data?.jobs.find((j) => j.id === selectedId) ?? null : null;

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const target = event.target as HTMLElement;
    if (!target.matches("[data-entry]") || !containerRef.current) return;
    const entries = Array.from(containerRef.current.querySelectorAll<HTMLElement>("[data-entry]"));
    const day = target.dataset.day ?? "";
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp": {
        event.preventDefault();
        const sameDay = entries.filter((el) => el.dataset.day === day);
        sameDay[moveIndex(sameDay.indexOf(target), sameDay.length, event.key === "ArrowDown" ? "next" : "prev", false)]?.focus();
        return;
      }
      case "ArrowLeft":
      case "ArrowRight": {
        event.preventDefault();
        const dir = event.key === "ArrowRight" ? 1 : -1;
        if (view === "day") {
          pendingFocus.current = true;
          setAnchor((a) => dateKeyAdd(a, dir));
          setSelectedId(null);
          return;
        }
        // The next day that has an entry, in the arrow's direction: owner moveIndex skips the empty days.
        const index = days.indexOf(day);
        const hasEntry = (i: number) => entries.some((el) => el.dataset.day === days[i]);
        const k = moveIndex(index, days.length, dir > 0 ? "next" : "prev", false, (i) => !hasEntry(i));
        if (k !== index) entries.find((el) => el.dataset.day === days[k])?.focus();
        return;
      }
      case "Home":
      case "End": {
        event.preventDefault();
        const sameDay = entries.filter((el) => el.dataset.day === day);
        sameDay[moveIndex(0, sameDay.length, event.key === "Home" ? "first" : "last", false)]?.focus();
        return;
      }
      default:
        return;
    }
  }

  const rangeLabel =
    view === "week" ? `Week of ${dayLabel(first)} to ${dayLabel(last)}` : dayLabel(anchor);

  // A plain render function, not a nested component: a nested component would remount on every
  // render and drop keyboard focus.
  function renderEntry(job: PublishJob, day: string) {
    const account = accountById.get(job.accountId);
    return (
      <Button
        type="button"
        data-entry
        data-day={day}
        variant="secondary"
        className="op-entry"
        aria-current={job.id === selectedId ? "true" : undefined}
        onClick={() => setSelectedId(job.id)}
      >
        {account ? (
          <AccountChip account={account} ordinal={ordinals.get(account.id)} />
        ) : (
          <span>
            Account not found in the accounts list: <span className="op-mono">{job.accountId}</span>
          </span>
        )}
        <span className="op-entry__line">
          <time dateTime={job.scheduledAtUtc}>{timeInZone(job.scheduledAtUtc, job.timeZone)}</time>{" "}
          {zoneAbbr(job.scheduledAtUtc, job.timeZone)} · {FORMAT_LABEL[job.format]}
        </span>
        <JobBadges job={job} compact />
        <span className="op-entry__media">{mediaSummary(job)}</span>
      </Button>
    );
  }

  return (
    <PageShell
      title="Calendar"
      subtitle="Scheduled and media-ready posts for every account. Times are each account's local time."
      actions={
        <div className="op-toolbar">
          <Button size="sm" variant="secondary" onClick={() => setAnchor((a) => dateKeyAdd(a, view === "week" ? -7 : -1))}>
            {view === "week" ? "Previous week" : "Previous day"}
          </Button>
          <Button size="sm" variant="secondary" onClick={() => setAnchor(today)}>
            Today
          </Button>
          <Button size="sm" variant="secondary" onClick={() => setAnchor((a) => dateKeyAdd(a, view === "week" ? 7 : 1))}>
            {view === "week" ? "Next week" : "Next day"}
          </Button>
          <SegmentedControl<View>
            label="View"
            value={view}
            onChange={(v) => setView(v)}
            options={[
              { value: "day", label: "Day" },
              { value: "week", label: "Week" },
            ]}
          />
        </div>
      }
    >
      {source.kind === "mock" ? <MockBanner /> : null}
      <p className="op-range" aria-live="polite">{rangeLabel}</p>
      <p className="op-hint">
        Arrow keys move between entries. Up and Down stay inside one day; Left and Right move between days.
      </p>

      {state.status === "error" ? (
        <LoadError message="Could not read the calendar from the daemon." onRetry={reload} />
      ) : data === null ? (
        <SkeletonRows label="Loading the calendar" rows={4} />
      ) : (
        <>
          {state.status === "loading" ? <p role="status" className="op-hint">Updating</p> : null}
          {visibleCount === 0 ? (
            <EmptyState title="Nothing scheduled for these days">
              <p>Accounts publish only approved, scheduled posts. Drafts waiting for a decision are in Approvals.</p>
              <Button variant="secondary" onClick={onOpenApprovals}>
                Open approvals
              </Button>
            </EmptyState>
          ) : null}

          <div className="op-week-scroll" ref={containerRef} onKeyDown={onKeyDown}>
            <div className={view === "week" ? "op-week" : "op-day-list"}>
              {days.map((day) => {
                const entries = jobsByDay.get(day) ?? [];
                const isToday = day === today;
                return (
                  <section key={day} className="op-day" aria-labelledby={`day-${day}`}>
                    <h3 id={`day-${day}`} data-day-heading tabIndex={-1} className="op-day__heading">
                      {dayLabel(day)}
                      {isToday ? <span className="op-today"> (today)</span> : null}
                    </h3>
                    {entries.length === 0 ? (
                      <p className="op-day__empty">Nothing scheduled</p>
                    ) : (
                      <ul className="op-day__list">
                        {entries.map((job) => (
                          <li key={job.id}>{renderEntry(job, day)}</li>
                        ))}
                      </ul>
                    )}
                  </section>
                );
              })}
            </div>
          </div>

          {selected ? (
            <Card as="section" className="op-detail">
              <h2 className="op-detail__title">
                {accountById.get(selected.accountId)?.displayName ??
                  `Account not found in the accounts list (${selected.accountId})`}
                : {instantLabel(selected.scheduledAtUtc, selected.timeZone)}
              </h2>
              <JobBadges job={selected} />
              {selected.publish ? <PublishProgressView progress={selected.publish} timeZone={selected.timeZone} /> : null}
              {selected.dst.kind !== "none" ? <p>{selected.dst.note}</p> : null}
              {selected.state === "outcome_unknown" ? (
                <p role="alert">The platform may have accepted this post. No retry until reconciled.</p>
              ) : null}
              {selected.lateness ? (
                <p>
                  Late: media was not ready by {instantLabel(selected.lateness.mediaNotReadyBy, selected.timeZone)}. Daemon policy:{" "}
                  {selected.lateness.policy === "publish_next_slot" ? "publish at the next slot" : "held for review"}.
                </p>
              ) : null}
              {selected.conflict ? (
                <p>This account has another job within {selected.conflict.minimumSpacingMinutes} minutes. Review both before approving.</p>
              ) : null}
              <div>
                <Button variant="primary" onClick={() => onOpenJob(selected.id)}>
                  Open in approvals
                </Button>
              </div>
            </Card>
          ) : (
            <p className="op-hint">Select an entry to see its details.</p>
          )}
        </>
      )}
    </PageShell>
  );
}
