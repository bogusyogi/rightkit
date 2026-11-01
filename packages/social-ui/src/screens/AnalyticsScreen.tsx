import { useState } from "react";
import { EmptyState, Field, PageShell, SkeletonRows, Table, TableBody, TableCaption, TableCell, TableHead, TableHeaderCell, TableRow } from "@rightkit/react-shell/react";
import { ScopeSelect } from "../components/ScopeSelect";
import { LoadError, MockBanner } from "../components/Shared";
import { useAsync } from "../hooks/useAsync";
import {
  groupDigits,
  instantLabel,
  type AnalyticsObservation,
  type AnalyticsPost,
  type DataSource,
  type MetricImport,
  type OperatorScope,
} from "../data";
import { ANALYTICS_UNAVAILABLE, analyticsSourceText, platformName, postAgeText } from "./labels";

interface Props {
  source: DataSource;
  rev: number;
}

export function AnalyticsScreen({ source, rev }: Props) {
  const scopes = useAsync<OperatorScope[]>(() => source.listScopes(), [source, rev]);
  const [picked, setPicked] = useState<string | null>(null);

  const scopeList = scopes.value ?? [];
  const scopeId = scopeList.find((s) => s.id === picked)?.id ?? scopeList[0]?.id ?? null;

  return (
    <PageShell
      title="Analytics"
      subtitle="Read only. The numbers stored for each published post of one channel: the latest value of each metric with its age and source, the observation history of one post, and the manual imports. Nothing here pulls, imports, ranks, averages or compares numbers."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      {scopes.state.status === "error" ? (
        <LoadError message="Could not read the channels from the daemon." onRetry={scopes.reload} />
      ) : scopes.value === null ? (
        <SkeletonRows label="Loading channels" rows={2} />
      ) : scopeList.length === 0 || scopeId === null ? (
        <EmptyState title="No channels yet">
          <p>Analytics is kept per channel once one is set up.</p>
        </EmptyState>
      ) : (
        <>
          <ScopeSelect scopes={scopeList} value={scopeId} onChange={(id) => setPicked(id)} />
          {/* Keyed by scope so a switch never shows the previous channel's posts, history or imports. */}
          <AnalyticsView key={scopeId} source={source} rev={rev} scopeId={scopeId} />
        </>
      )}
    </PageShell>
  );
}

function AnalyticsView({ source, rev, scopeId }: { source: DataSource; rev: number; scopeId: string }) {
  const posts = useAsync<AnalyticsPost[]>(() => source.listAnalyticsPosts(scopeId), [source, rev, scopeId]);
  const imports = useAsync<MetricImport[]>(() => source.listMetricImports(scopeId), [source, rev, scopeId]);
  const [picked, setPicked] = useState<string | null>(null);

  if (posts.state.status === "error") {
    return <LoadError message="Could not read the published posts from the daemon." onRetry={posts.reload} />;
  }
  if (imports.state.status === "error") {
    return <LoadError message="Could not read the manual imports from the daemon." onRetry={imports.reload} />;
  }
  if (posts.state.status === "loading" || imports.state.status === "loading") {
    return <SkeletonRows label="Loading analytics" rows={3} />;
  }

  const postList = posts.state.value;
  const importList = imports.state.value;

  if (postList.length === 0 && importList.length === 0) {
    return (
      <EmptyState title="No numbers yet">
        <p>No numbers have been pulled or imported for this channel yet. This is not an error.</p>
      </EmptyState>
    );
  }

  const selected = postList.find((p) => p.jobId === picked) ?? postList[0];

  return (
    <>
      <p className="op-hint">
        Numbers are pulled on the server (the daemon), not from this screen. This screen pulls nothing and imports
        nothing. Each value is shown exactly as stored. A metric the platform does not provide says so, and is never
        shown as zero.
      </p>

      <section aria-labelledby="analytics-latest-title">
        <h2 id="analytics-latest-title" className="op-section-title">
          Published posts ({groupDigits(String(postList.length))})
        </h2>
        {postList.length === 0 ? (
          <p className="op-hint">No published post has a number stored yet.</p>
        ) : (
          <LatestTable posts={postList} />
        )}
      </section>

      {postList.length === 0 ? null : (
        <section aria-labelledby="analytics-history-title">
          <h2 id="analytics-history-title" className="op-section-title">
            Observation history
          </h2>
          <p className="op-hint">
            Every stored observation of the chosen post, oldest first. Each row names the pull or the import that stored
            it.
          </p>
          <Field label="Post">
            {({ id, "aria-describedby": describedBy }) => (
              <select
                id={id}
                aria-describedby={describedBy}
                className="op-select"
                value={selected.jobId}
                onChange={(e) => setPicked(e.target.value)}
              >
                {postList.map((post) => (
                  <option key={post.jobId} value={post.jobId}>
                    {platformName(post.platform)} post {post.remotePostId}
                  </option>
                ))}
              </select>
            )}
          </Field>
          {/* Keyed by post so a switch never shows the previous post's observations. */}
          <PostHistory key={selected.jobId} source={source} rev={rev} scopeId={scopeId} jobId={selected.jobId} />
        </section>
      )}

      <ImportsSection imports={importList} />
    </>
  );
}

/** The latest value of each metric of each post. A post with no metric says so in its own row. */
function LatestTable({ posts }: { posts: AnalyticsPost[] }) {
  return (
    <div className="op-table-scroll">
      <Table aria-labelledby="analytics-latest-title">
        <TableCaption>Latest stored value of each metric, by published post</TableCaption>
        <TableHead>
          <TableRow>
            <TableHeaderCell>Post</TableHeaderCell>
            <TableHeaderCell>Metric</TableHeaderCell>
            <TableHeaderCell>Window</TableHeaderCell>
            <TableHeaderCell align="end">
              Latest value
            </TableHeaderCell>
            <TableHeaderCell>Observed (UTC)</TableHeaderCell>
            <TableHeaderCell>Age of the post at that observation</TableHeaderCell>
            <TableHeaderCell>Source</TableHeaderCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {posts.map((post) => (
            <PostRows key={`post:${post.jobId}`} post={post} />
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

function PostRows({ post }: { post: AnalyticsPost }) {
  const head = (
    <TableCell rowHeader rowSpan={Math.max(post.metrics.length, 1)}>
      <div>
        {platformName(post.platform)}, post <span className="op-mono">{post.remotePostId}</span>
      </div>
      <div>Published (UTC): {instantLabel(post.publishedAtUtc, "UTC")}</div>
    </TableCell>
  );

  if (post.metrics.length === 0) {
    return (
      <TableRow>
        {head}
        <TableCell colSpan={6}>No number is stored for this post yet.</TableCell>
      </TableRow>
    );
  }

  return (
    <>
      {post.metrics.map((metric, index) => (
        <TableRow key={`metric:${metric.metric}:${metric.window ?? "no window"}:${metric.source}`}>
          {index === 0 ? head : null}
          <TableCell className="op-mono">{metric.metric}</TableCell>
          <TableCell>{metric.window ?? "No window"}</TableCell>
          {metric.state === "value" ? (
            <TableCell numeric className="op-mono">{metric.value}</TableCell>
          ) : (
            <TableCell>{ANALYTICS_UNAVAILABLE}</TableCell>
          )}
          <TableCell className="op-mono">{instantLabel(metric.observedAtUtc, "UTC")}</TableCell>
          <TableCell className="op-mono">{postAgeText(metric.postAgeSeconds)}</TableCell>
          <TableCell>{analyticsSourceText(metric.source, metric.importId)}</TableCell>
        </TableRow>
      ))}
    </>
  );
}

/** Every stored observation of one post, oldest first, with the pull or import that stored it. */
function PostHistory({
  source,
  rev,
  scopeId,
  jobId,
}: {
  source: DataSource;
  rev: number;
  scopeId: string;
  jobId: string;
}) {
  const history = useAsync<AnalyticsObservation[]>(
    () => source.listAnalyticsObservations(scopeId, jobId),
    [source, rev, scopeId, jobId],
  );

  if (history.state.status === "error") {
    return <LoadError message="Could not read the observation history from the daemon." onRetry={history.reload} />;
  }
  if (history.state.status === "loading") {
    return <SkeletonRows label="Loading the observation history" rows={3} />;
  }

  const rows = history.state.value;
  if (rows.length === 0) return <p className="op-hint">No observation is stored for this post.</p>;

  return (
    <div className="op-table-scroll">
      <Table aria-labelledby="analytics-history-title">
        <TableCaption>Every stored observation of this post, oldest first</TableCaption>
        <TableHead>
          <TableRow>
            <TableHeaderCell>Observed (UTC)</TableHeaderCell>
            <TableHeaderCell>Metric</TableHeaderCell>
            <TableHeaderCell>Window</TableHeaderCell>
            <TableHeaderCell align="end">
              Value
            </TableHeaderCell>
            <TableHeaderCell>Source</TableHeaderCell>
            <TableHeaderCell>Pull or import</TableHeaderCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {rows.map((row, index) => (
            <TableRow key={`observation:${index}`}>
              <TableCell rowHeader className="op-mono">
                {instantLabel(row.observedAtUtc, "UTC")}
              </TableCell>
              <TableCell className="op-mono">{row.metric}</TableCell>
              <TableCell>{row.window ?? "No window"}</TableCell>
              {row.state === "value" ? (
                <TableCell numeric className="op-mono">{row.value}</TableCell>
              ) : (
                <TableCell>{ANALYTICS_UNAVAILABLE}</TableCell>
              )}
              <TableCell>{analyticsSourceText(row.source, row.importId)}</TableCell>
              <TableCell className="op-mono">
                {row.importId !== null ? `Import ${row.importId}` : row.pullId !== null ? `Pull ${row.pullId}` : "Not stated"}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

function ImportsSection({ imports }: { imports: MetricImport[] }) {
  return (
    <section aria-labelledby="analytics-imports-title">
      <h2 id="analytics-imports-title" className="op-section-title">
        Manual imports ({groupDigits(String(imports.length))})
      </h2>
      <p className="op-hint">
        A manual import is a file the channel owner exported by hand and the daemon read. Its values appear in the tables
        above, with the import id. This screen imports nothing.
      </p>
      {imports.length === 0 ? (
        <p className="op-hint">No manual import is stored for this channel.</p>
      ) : (
        <div className="op-table-scroll">
          <Table aria-labelledby="analytics-imports-title">
            <TableCaption>Manual imports in this channel, by import id</TableCaption>
            <TableHead>
              <TableRow>
                <TableHeaderCell>Import id</TableHeaderCell>
                <TableHeaderCell>Platform</TableHeaderCell>
                <TableHeaderCell align="end">
                  Rows
                </TableHeaderCell>
                <TableHeaderCell align="end">
                  Values
                </TableHeaderCell>
                <TableHeaderCell>Source note</TableHeaderCell>
                <TableHeaderCell>Imported (UTC)</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {imports.map((record) => (
                <TableRow key={`import:${record.importId}`}>
                  <TableCell rowHeader className="op-mono">
                    {record.importId}
                  </TableCell>
                  <TableCell>{platformName(record.platform)}</TableCell>
                  <TableCell numeric>{groupDigits(String(record.rowsWritten))}</TableCell>
                  <TableCell numeric>{groupDigits(String(record.observationsWritten))}</TableCell>
                  <TableCell>{record.sourceNote}</TableCell>
                  <TableCell className="op-mono">{instantLabel(record.createdAtUtc, "UTC")}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </section>
  );
}
