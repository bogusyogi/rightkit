import { useState } from "react";
import { Badge, EmptyState, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect.js";
import { LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import {
  groupDigits,
  instantLabel,
  type DataSource,
  type OperatorScope,
  type PacingBlockedJob,
  type PacingJob,
  type PublishNextView,
  type PublishQuota,
  type UnreadableRow,
} from "../data/index.js";
import { PACING_REASON_LABEL, PUBLISH_STATE_LABEL, platformName } from "./labels.js";
import { hostAppName } from "../appName.js";

interface Props {
  source: DataSource;
  rev: number;
}

/** One account a job names. Its pacing is read per account, from publish.quota_get. */
interface AccountRef {
  /** The first job id of the account. A React key only; never shown or used as an attribute. */
  key: string;
  scopeId: string;
  platform: string;
  remoteAccount: string;
}

export function PacingScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Pacing"
      subtitle="Read only. What the daemon's publish pacing lets run now on one channel, every job that may not run with its reason and the earliest time it could, and each account's pacing as stored. Nothing here queues, runs, pauses or changes a limit."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Publish pacing is kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          <p className="op-hint">
            The default gap between posts and the default count per UTC day are {hostAppName()}&apos;s own caution. They are not
            platform limits. A limit is set on the server by a person, not here.
          </p>
          {/* Keyed by scope so a switch never shows the previous channel's jobs or pacing. */}
          <PacingView key={scopeId} source={source} rev={rev} scopeId={scopeId} scopes={scopeList} />
        </>
      )}
    </PageShell>
  );
}

function PacingView({
  source,
  rev,
  scopeId,
  scopes,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  scopes: readonly OperatorScope[];
}) {
  const view = useAsync<PublishNextView>(() => source.listPublishNext(scopeId), [source, rev, scopeId]);

  if (view.state.status === "error") {
    return <LoadError message="Could not read the publish pacing from the daemon." onRetry={view.reload} />;
  }
  if (view.state.status === "loading") {
    return <SkeletonRows label="Loading the publish pacing" rows={3} />;
  }

  const data = view.state.value;
  const total = data.ready.length + data.blocked.length + data.inProgress.length;
  if (total === 0) {
    return (
      <EmptyState title="Nothing is queued">
        <p>Nothing is queued for this channel.</p>
      </EmptyState>
    );
  }

  return (
    <>
      <p role="status" aria-live="polite" className="op-hint">
        Decided at {instantLabel(data.nowUtc, "UTC")} by the daemon&apos;s clock.
      </p>

      <section aria-labelledby="pacing-ready-title">
        <h2 id="pacing-ready-title" className="op-section-title">
          May run now ({groupDigits(String(data.readyTotal))})
        </h2>
        {data.ready.length === 0 ? (
          <p className="op-hint">No job may run now on this channel.</p>
        ) : (
          <div className="op-table-scroll">
            <Table aria-labelledby="pacing-ready-title">
              <TableCaption>
                Jobs that may run now, at most one per account, earliest scheduled time first
              </TableCaption>
              <TableHead>
                <TableRow>
                  <TableHeaderCell>Platform and remote account</TableHeaderCell>
                  <TableHeaderCell>Channel account</TableHeaderCell>
                  <TableHeaderCell>Job</TableHeaderCell>
                  <TableHeaderCell>Calendar entry</TableHeaderCell>
                  <TableHeaderCell>State</TableHeaderCell>
                  <TableHeaderCell>Phase</TableHeaderCell>
                  <TableHeaderCell>Scheduled (UTC)</TableHeaderCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {data.ready.map((row, index) =>
                  row.readable === true ? (
                    <JobRow key={`ready:${row.jobId}`} job={row} scopes={scopes} />
                  ) : (
                    <UnreadableJobRow key={`ready-unreadable:${index}`} row={row} colSpan={7} />
                  ),
                )}
              </TableBody>
            </Table>
          </div>
        )}
      </section>

      <section aria-labelledby="pacing-blocked-title">
        <h2 id="pacing-blocked-title" className="op-section-title">
          May not run now ({groupDigits(String(data.blocked.length))})
        </h2>
        <p className="op-hint">
          Where a job waits behind another job on its account, the earliest time shown is a lower bound, not a promise.
          A job with no time given needs a person or a check first.
        </p>
        {data.blocked.length === 0 ? (
          <p className="op-hint">No job is held back.</p>
        ) : (
          <div className="op-table-scroll">
            <Table aria-labelledby="pacing-blocked-title">
              <TableCaption>
                Jobs that may not run now, with the reason in words, the other reasons, and the earliest run time
              </TableCaption>
              <TableHead>
                <TableRow>
                  <TableHeaderCell>Platform and remote account</TableHeaderCell>
                  <TableHeaderCell>Channel account</TableHeaderCell>
                  <TableHeaderCell>Job</TableHeaderCell>
                  <TableHeaderCell>State</TableHeaderCell>
                  <TableHeaderCell>Reason</TableHeaderCell>
                  <TableHeaderCell>Other reasons that apply</TableHeaderCell>
                  <TableHeaderCell>Earliest run (UTC)</TableHeaderCell>
                  <TableHeaderCell>Detail, as given</TableHeaderCell>
                  <TableHeaderCell>Behind job</TableHeaderCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {data.blocked.map((row, index) =>
                  row.readable === true ? (
                    <BlockedRow key={`blocked:${row.jobId}`} job={row} scopes={scopes} />
                  ) : (
                    <UnreadableJobRow key={`blocked-unreadable:${index}`} row={row} colSpan={9} />
                  ),
                )}
              </TableBody>
            </Table>
          </div>
        )}
      </section>

      <section aria-labelledby="pacing-progress-title">
        <h2 id="pacing-progress-title" className="op-section-title">
          In progress ({groupDigits(String(data.inProgress.length))})
        </h2>
        <p className="op-hint">
          A job already past its create is in progress. Pacing does not hold it back; the daemon continues it.
        </p>
        {data.inProgress.length === 0 ? (
          <p className="op-hint">No job is in progress on this channel.</p>
        ) : (
          <div className="op-table-scroll">
            <Table aria-labelledby="pacing-progress-title">
              <TableCaption>Jobs in progress on this channel</TableCaption>
              <TableHead>
                <TableRow>
                  <TableHeaderCell>Platform and remote account</TableHeaderCell>
                  <TableHeaderCell>Channel account</TableHeaderCell>
                  <TableHeaderCell>Job</TableHeaderCell>
                  <TableHeaderCell>Calendar entry</TableHeaderCell>
                  <TableHeaderCell>State</TableHeaderCell>
                  <TableHeaderCell>Phase</TableHeaderCell>
                  <TableHeaderCell>Scheduled (UTC)</TableHeaderCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {data.inProgress.map((row, index) =>
                  row.readable === true ? (
                    <JobRow key={`progress:${row.jobId}`} job={row} scopes={scopes} />
                  ) : (
                    <UnreadableJobRow key={`progress-unreadable:${index}`} row={row} colSpan={7} />
                  ),
                )}
              </TableBody>
            </Table>
          </div>
        )}
      </section>

      <AccountsSection accounts={accountsOf(data)} source={source} rev={rev} scopes={scopes} nowUtc={data.nowUtc} />
    </>
  );
}

function scopeLabelOf(scopes: readonly OperatorScope[], scopeId: string): string {
  return scopes.find((s) => s.id === scopeId)?.label ?? "Not in the channel list";
}

function JobRow({ job, scopes }: { job: PacingJob; scopes: readonly OperatorScope[] }) {
  const state = PUBLISH_STATE_LABEL[job.state];
  return (
    <TableRow>
      <TableCell rowHeader>
        <div>{platformName(job.platform)}</div>
        <div className="op-mono">{job.remoteAccount}</div>
      </TableCell>
      <TableCell>{scopeLabelOf(scopes, job.scopeId)}</TableCell>
      <TableCell className="op-mono">{job.jobId}</TableCell>
      <TableCell className="op-mono">{job.entryId}</TableCell>
      <TableCell>
        <Badge tone={state.tone}>{state.text}</Badge>
      </TableCell>
      <TableCell className="op-mono">{job.phase}</TableCell>
      <TableCell className="op-mono">{instantLabel(job.scheduledAtUtc, "UTC")}</TableCell>
    </TableRow>
  );
}

function BlockedRow({ job, scopes }: { job: PacingBlockedJob; scopes: readonly OperatorScope[] }) {
  const state = PUBLISH_STATE_LABEL[job.state];
  const others = job.reasons.filter((reason) => reason !== job.reason);
  return (
    <TableRow>
      <TableCell rowHeader>
        <div>{platformName(job.platform)}</div>
        <div className="op-mono">{job.remoteAccount}</div>
      </TableCell>
      <TableCell>{scopeLabelOf(scopes, job.scopeId)}</TableCell>
      <TableCell className="op-mono">{job.jobId}</TableCell>
      <TableCell>
        <Badge tone={state.tone}>{state.text}</Badge>
      </TableCell>
      <TableCell>{PACING_REASON_LABEL[job.reason]}</TableCell>
      <TableCell>{others.length === 0 ? "None" : others.map((reason) => PACING_REASON_LABEL[reason]).join("; ")}</TableCell>
      <TableCell className="op-mono">{job.untilUtc === null ? "Not given" : instantLabel(job.untilUtc, "UTC")}</TableCell>
      <TableCell>{job.detail}</TableCell>
      <TableCell className="op-mono">{job.behindJobId ?? "None"}</TableCell>
    </TableRow>
  );
}

/** A row the desktop cannot read: shown with its reason and its raw JSON, never dropped. */
function UnreadableJobRow({ row, colSpan }: { row: UnreadableRow; colSpan: number }) {
  return (
    <TableRow>
      <TableCell colSpan={colSpan}>
        <p>This row could not be read: {row.reason}. It is kept here with its raw JSON.</p>
        <pre className="op-raw" tabIndex={0} aria-label="Raw JSON of a row that could not be read">
          {row.rawJson}
        </pre>
      </TableCell>
    </TableRow>
  );
}

/** Each distinct account the jobs name, once, in the order the daemon lists its jobs. */
function accountsOf(view: PublishNextView): AccountRef[] {
  const rows: (PacingJob | UnreadableRow)[] = [...view.ready, ...view.inProgress, ...view.blocked];
  const accounts = new Map<string, AccountRef>();
  for (const row of rows) {
    if (row.readable !== true) continue;
    const id = JSON.stringify([row.scopeId, row.platform, row.remoteAccount]);
    if (!accounts.has(id)) {
      accounts.set(id, {
        key: row.jobId,
        scopeId: row.scopeId,
        platform: row.platform,
        remoteAccount: row.remoteAccount,
      });
    }
  }
  return [...accounts.values()];
}

function AccountsSection({
  accounts,
  source,
  rev,
  scopes,
  nowUtc,
}: {
  accounts: AccountRef[];
  source: DataSource;
  rev: number;
  scopes: readonly OperatorScope[];
  nowUtc: string;
}) {
  return (
    <section aria-labelledby="pacing-accounts-title">
      <h2 id="pacing-accounts-title" className="op-section-title">
        Pacing per account ({groupDigits(String(accounts.length))})
      </h2>
      <p className="op-hint">
        Each account shows the limits in force. &quot;Defaults&quot; means no limit was stored for it, so {hostAppName()}&apos;s own
        caution applies. &quot;Stored&quot; means a person set it on the server. This screen changes nothing.
      </p>
      <p className="op-hint">
        An account with no queued, waiting or running job is not listed here. The daemon names an account&apos;s remote id
        only in its jobs.
      </p>
      {accounts.length === 0 ? <p className="op-hint">No account has a job to show pacing for.</p> : null}
      {accounts.map((account) => (
        <AccountPacing
          key={account.key}
          source={source}
          rev={rev}
          account={account}
          scopes={scopes}
          nowUtc={nowUtc}
        />
      ))}
    </section>
  );
}

/** One account's pacing. Its own read: a failed read shows for this account only, and the other accounts still show. */
function AccountPacing({
  source,
  rev,
  account,
  scopes,
  nowUtc,
}: {
  source: DataSource;
  rev: number;
  account: AccountRef;
  scopes: readonly OperatorScope[];
  nowUtc: string;
}) {
  const quota = useAsync<PublishQuota>(
    () => source.getPublishQuota(account.scopeId, account.platform, account.remoteAccount),
    [source, rev, account.scopeId, account.platform, account.remoteAccount],
  );
  const name = `${platformName(account.platform)} account ${account.remoteAccount}`;

  if (quota.state.status === "error") {
    return <LoadError message={`Could not read the pacing of the ${name} from the daemon.`} onRetry={quota.reload} />;
  }
  if (quota.state.status === "loading") {
    return <SkeletonRows label={`Loading the pacing of the ${name}`} rows={2} />;
  }

  const q = quota.state.value;
  const now = Date.parse(nowUtc);
  const limitsText = q.optedIn ? "Stored by a person on the server" : `${hostAppName()} defaults: no limit stored`;

  return (
    <div>
      <p className="op-hint">Channel account: {scopeLabelOf(scopes, account.scopeId)}</p>
      <div className="op-table-scroll">
        <Table aria-label={`Pacing of the ${name}, as stored`}>
          <TableCaption>Pacing of the {name}, as stored</TableCaption>
          <TableBody>
            <TableRow>
              <TableCell rowHeader>Limits in force</TableCell>
              <TableCell>{limitsText}</TableCell>
            </TableRow>
            <TableRow>
              <TableCell rowHeader>Gap between two creates</TableCell>
              <TableCell className="op-mono">
                {q.minGapSecs} seconds
                {q.optedIn ? `. ${hostAppName()} default: ${q.defaultMinGapSecs} seconds.` : " (the default)"}
              </TableCell>
            </TableRow>
            <TableRow>
              <TableCell rowHeader>Creates per UTC day</TableCell>
              <TableCell className="op-mono">
                {q.maxPerDay}
                {q.optedIn ? `. ${hostAppName()} default: ${q.defaultMaxPerDay}.` : " (the default)"}
              </TableCell>
            </TableRow>
            <TableRow>
              <TableCell rowHeader>Paused until</TableCell>
              <TableCell>
                {q.pausedUntilUtc === null
                  ? "Not paused"
                  : `${instantLabel(q.pausedUntilUtc, "UTC")}, ${Date.parse(q.pausedUntilUtc) > now ? "in force" : "ended"}`}
              </TableCell>
            </TableRow>
            <TableRow>
              <TableCell rowHeader>Last create (UTC)</TableCell>
              <TableCell className="op-mono">
                {q.lastCreateAtUtc === null ? "None recorded" : instantLabel(q.lastCreateAtUtc, "UTC")}
              </TableCell>
            </TableRow>
            <TableRow>
              <TableCell rowHeader>Creates today (UTC day)</TableCell>
              <TableCell className="op-mono">
                {q.createsToday}. The count restarts {instantLabel(q.dayResetsAtUtc, "UTC")}.
              </TableCell>
            </TableRow>
            <TableRow>
              <TableCell rowHeader>Platform wait</TableCell>
              <TableCell>
                {q.rateLimitedUntilUtc === null
                  ? "None recorded"
                  : `Until ${instantLabel(q.rateLimitedUntilUtc, "UTC")}, ${Date.parse(q.rateLimitedUntilUtc) > now ? "in force" : "ended"}`}
                {q.rateLimitedAtUtc === null ? null : ` The signal was seen ${instantLabel(q.rateLimitedAtUtc, "UTC")}.`}
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </div>
    </div>
  );
}
