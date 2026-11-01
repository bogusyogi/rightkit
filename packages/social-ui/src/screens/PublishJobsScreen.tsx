import { useState } from "react";
import { Badge, Button, EmptyState, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect.js";
import { LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import { shell } from "../lib/shell.js";
import {
  groupDigits,
  instantLabel,
  type DataSource,
  type OperatorScope,
  type PublishJobRecord,
  type PublishJobResolution,
} from "../data/index.js";
import { PUBLISH_RESOLUTION_LABEL, PUBLISH_STATE_LABEL, platformName } from "./labels.js";
import { hostAppName } from "../appName.js";

interface Props {
  source: DataSource;
  rev: number;
}

type CopyFn = (text: string, what: string) => Promise<void>;

/** The plain explanation shown above every outcome_unknown job. The wording is fixed. */
const OUTCOME_UNKNOWN_TEXT =
  `The platform may or may not have received the post. ${hostAppName()} will not send it again by itself. A person must look at the account on the platform, then record what they saw with the publish.resolve_unknown command on the server. This screen cannot change a job.`;

export function PublishJobsScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Publish jobs"
      subtitle="Read only. Every publish job of one channel, newest first: its platform, calendar entry, state, phase, attempts, next attempt, remote post and last error. Jobs whose outcome is unknown are listed first. Nothing here queues, steps, checks, cancels or resolves a job."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Publish jobs are kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's jobs. */}
          <PublishJobsView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function PublishJobsView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const jobs = useAsync<PublishJobRecord[]>(() => source.listPublishJobs(scopeId), [source, rev, scopeId]);
  const [notice, setNotice] = useState("");

  const copy: CopyFn = async (text, what) => {
    try {
      await shell.clipboard.write({ text });
      setNotice(`Copied ${what}.`);
    } catch {
      setNotice(`Copying ${what} failed. Select the text and copy it by hand.`);
    }
  };

  if (jobs.state.status === "error") {
    return <LoadError message="Could not read the publish jobs from the daemon." onRetry={jobs.reload} />;
  }
  if (jobs.state.status === "loading") {
    return <SkeletonRows label="Loading the publish jobs" rows={3} />;
  }

  const rows = jobs.state.value;
  if (rows.length === 0) {
    return (
      <EmptyState title="No publish jobs yet">
        <p>Nothing has been queued for publishing on this channel yet.</p>
      </EmptyState>
    );
  }

  const unknown = rows.filter((job) => job.state === "outcome_unknown");
  const others = rows.filter((job) => job.state !== "outcome_unknown");

  return (
    <>
      <p className="op-hint">
        The daemon publishes; this screen shows only what it stored. Each value is shown exactly as stored. Nothing is
        sent to a platform from here.
      </p>
      <p role="status" aria-live="polite" className="op-hint">
        {notice}
      </p>

      <section aria-labelledby="publish-unknown-title">
        <h2 id="publish-unknown-title" className="op-section-title">
          Outcome unknown ({groupDigits(String(unknown.length))})
        </h2>
        <p className="op-hint">{OUTCOME_UNKNOWN_TEXT}</p>
        {unknown.length === 0 ? (
          <p className="op-hint">No job has an unknown outcome.</p>
        ) : (
          <JobTable
            caption="Publish jobs whose outcome is unknown, newest first"
            labelledBy="publish-unknown-title"
            rows={unknown}
            copy={copy}
          />
        )}
      </section>

      <section aria-labelledby="publish-other-title">
        <h2 id="publish-other-title" className="op-section-title">
          Other publish jobs ({groupDigits(String(others.length))})
        </h2>
        {others.length === 0 ? (
          <p className="op-hint">No other publish job.</p>
        ) : (
          <JobTable
            caption="Publish jobs of this channel, newest first"
            labelledBy="publish-other-title"
            rows={others}
            copy={copy}
          />
        )}
      </section>
    </>
  );
}

function JobTable({
  caption,
  labelledBy,
  rows,
  copy,
}: {
  caption: string;
  labelledBy: string;
  rows: PublishJobRecord[];
  copy: CopyFn;
}) {
  return (
    <div className="op-table-scroll">
      <Table aria-labelledby={labelledBy}>
        <TableCaption>{caption}</TableCaption>
        <TableHead>
          <TableRow>
            <TableHeaderCell>Job and platform</TableHeaderCell>
            <TableHeaderCell>Calendar entry</TableHeaderCell>
            <TableHeaderCell>State</TableHeaderCell>
            <TableHeaderCell>Phase</TableHeaderCell>
            <TableHeaderCell align="end">
              Attempts
            </TableHeaderCell>
            <TableHeaderCell>Next attempt (UTC)</TableHeaderCell>
            <TableHeaderCell>Remote post and link</TableHeaderCell>
            <TableHeaderCell>Last error</TableHeaderCell>
            <TableHeaderCell>Resolved by a person</TableHeaderCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {rows.map((job) => (
            <JobRow key={`job:${job.jobId}`} job={job} copy={copy} />
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

function JobRow({ job, copy }: { job: PublishJobRecord; copy: CopyFn }) {
  const state = PUBLISH_STATE_LABEL[job.state];
  const what = `${platformName(job.platform)} job ${job.jobId}`;
  const link = job.permalink;

  return (
    <TableRow>
      <TableCell rowHeader>
        <div>{platformName(job.platform)}</div>
        <div className="op-mono">{job.jobId}</div>
      </TableCell>
      <TableCell>
        <div className="op-mono">{job.entryId}</div>
        <div>Entry revision {job.entryRevision}</div>
      </TableCell>
      <TableCell>
        <Badge tone={state.tone}>{state.text}</Badge>
      </TableCell>
      <TableCell className="op-mono">{job.phase}</TableCell>
      <TableCell numeric className="op-mono">{job.attempts}</TableCell>
      <TableCell className="op-mono">{nextAttemptText(job)}</TableCell>
      <TableCell>
        {job.remotePostId === null ? (
          <div>No remote post id.</div>
        ) : (
          <div>
            Post id: <span className="op-mono">{job.remotePostId}</span>
          </div>
        )}
        {link === null ? (
          <div>No platform link.</div>
        ) : (
          <>
            <div className="op-mono">{link}</div>
            <Button
              size="sm"
              variant="secondary"
              aria-label={`Copy the permalink of ${what}`}
              onClick={() => copy(link, `the permalink of ${what}`)}
            >
              Copy
            </Button>
          </>
        )}
      </TableCell>
      <TableCell>
        {job.errorCode === null ? (
          "No error recorded"
        ) : (
          <>
            <div className="op-mono">{job.errorCode}</div>
            <div>{job.errorMessage ?? "No message given."}</div>
          </>
        )}
      </TableCell>
      <TableCell>{job.resolution === null ? "Not resolved by a person" : <ResolutionView resolution={job.resolution} />}</TableCell>
    </TableRow>
  );
}

/** Who resolved an unknown job, when, what they decided, their note, and whether the remote id was typed by hand. */
function ResolutionView({ resolution }: { resolution: PublishJobResolution }) {
  return (
    <div>
      <div>
        Recorded by {resolution.resolvedBy} at {instantLabel(resolution.resolvedAtUtc, "UTC")}
      </div>
      <div>Decision: {PUBLISH_RESOLUTION_LABEL[resolution.decision]}</div>
      <div>Note: {resolution.note}</div>
      {resolution.remoteIdEnteredByHand ? (
        <div>
          <span className="op-mark">Entered by hand</span> The remote id was typed by a person. The platform did not
          return it.
        </div>
      ) : null}
    </div>
  );
}

/** The next attempt only while the job waits. Any other state says it is not waiting. */
function nextAttemptText(job: PublishJobRecord): string {
  if (job.state !== "waiting") return "Not waiting";
  return job.nextAttemptAtUtc === null
    ? "Waiting. The next attempt time is not reported."
    : instantLabel(job.nextAttemptAtUtc, "UTC");
}
