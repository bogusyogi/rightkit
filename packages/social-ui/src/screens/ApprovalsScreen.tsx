import { useEffect, useMemo, useState, type FormEvent } from "react";
import { Badge, Button, ConfirmDialog, Field, ModalShell, Notice, PageShell, SegmentedControl, SkeletonRows } from "@rightkit/react-shell/react";
import { AccountChip, LoadError, MockBanner, PublishProgressView } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import {
  accountOrdinals,
  awaitingDecision,
  charCount,
  formatCount,
  instantLabel,
  isApproved,
  mediaReady,
  shortHash,
  type Account,
  type DataSource,
  type PublishJob,
  type RejectReason,
} from "../data/index.js";
import { FORMAT_LABEL, MEDIA_STATUS_LABEL, PLATFORM_LABEL, REJECT_LABEL, STATE_LABEL, approvalText } from "./labels.js";

type Filter = "decision" | "approved" | "all";

const WIDE_RANGE = { fromUtc: "2026-01-01T00:00:00Z", toUtc: "2027-01-01T00:00:00Z" };
const REJECT_ORDER: RejectReason[] = ["wrong_content", "wrong_account", "timing", "media_issue", "other"];

interface Loaded {
  accounts: Account[];
  jobs: PublishJob[];
}

interface Props {
  source: DataSource;
  rev: number;
  /** Called after any write so the app re-reads counts and every screen. */
  onChanged: () => void;
  initialJobId: string | null;
}

/** Formats whole milliseconds as m:ss. Fractional seconds are dropped, so 59999 ms reads 0:59. */
function duration(ms: number | null): string {
  if (ms === null) return "still image";
  const total = Math.floor(ms / 1000);
  const m = Math.floor(total / 60);
  const s = String(total % 60).padStart(2, "0");
  return `${m}:${s}`;
}

/** Why approve is unavailable, in words. Null when approve can be pressed. */
function approveBlock(job: PublishJob): string | null {
  switch (job.approval.status) {
    case "approved":
      return `Already approved at revision ${job.approval.approvedRevision}. Any change to the caption, media or policy invalidates it.`;
    case "rejected":
      return "Rejected. Revise the brief to bring it back for approval.";
    case "none":
      return "No approval requested yet for this job.";
    default:
      break;
  }
  if (!mediaReady(job)) return "Media not ready on server, so approval is disabled until every item is ready.";
  return null;
}

export function ApprovalsScreen({ source, rev, onChanged, initialJobId }: Props) {
  const [filter, setFilter] = useState<Filter>("decision");
  const [selectedId, setSelectedId] = useState<string | null>(initialJobId);
  const [confirmApprove, setConfirmApprove] = useState(false);
  const [rejecting, setRejecting] = useState(false);
  const [notice, setNotice] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setNotice(null);
  }, [selectedId]);

  const list = useAsync<Loaded>(
    async () => {
      const [accounts, jobs] = await Promise.all([source.listAccounts(), source.listJobs(WIDE_RANGE)]);
      return { accounts, jobs };
    },
    [source, rev],
  );

  // The preview always reads the job fresh. Approve and reject use the revision on screen, so an
  // approval never rests on a stale snapshot.
  const preview = useAsync<PublishJob | null>(
    () => (selectedId ? source.getJob(selectedId) : Promise.resolve(null)),
    [source, selectedId, rev],
  );

  // list.value keeps the last good data during a reload, so the preview never blanks mid-approve.
  const accounts = list.value?.accounts ?? [];
  const allJobs = list.value?.jobs ?? [];
  const ordinals = useMemo(() => accountOrdinals(accounts), [accounts]);
  const accountById = useMemo(() => new Map(accounts.map((a) => [a.id, a])), [accounts]);

  const decisionCount = allJobs.filter(awaitingDecision).length;
  const filtered = allJobs.filter((job) => {
    if (filter === "decision") return awaitingDecision(job);
    if (filter === "approved") return isApproved(job);
    return true;
  });
  // Jobs whose account is not in the accounts list. They are listed on their own, not dropped.
  const orphans = filtered.filter((job) => !accountById.has(job.accountId));

  const job = preview.state.status === "ready" ? preview.state.value : null;
  const account = job ? accountById.get(job.accountId) ?? null : null;
  const ordinal = account ? ordinals.get(account.id) : undefined;
  const destination = account ? `${PLATFORM_LABEL[account.platform]} ${account.handle}` : "this account";

  async function approve(current: PublishJob) {
    setConfirmApprove(false);
    setBusy(true);
    const result = await source.approve(current.id, current.revision, current.accountId);
    setBusy(false);
    setNotice(
      result.ok
        ? { tone: "ok", text: `Approved revision ${current.revision} for ${destination}. It is scheduled.` }
        : { tone: "bad", text: result.error.message },
    );
    onChanged();
  }

  async function reject(current: PublishJob, code: RejectReason, note: string) {
    setRejecting(false);
    setBusy(true);
    const result = await source.reject(current.id, {
      revision: current.revision,
      accountId: current.accountId,
      code,
      note,
    });
    setBusy(false);
    setNotice(
      result.ok
        ? { tone: "ok", text: `Rejected for ${destination}. Reason: ${REJECT_LABEL[code]}. Revision ${result.value.revision}.` }
        : { tone: "bad", text: result.error.message },
    );
    onChanged();
  }

  async function simulateEdit(current: PublishJob) {
    if (!source.mock) return;
    const edited = `${current.caption}\n\nEdited on another device: one more line for the community.`;
    const result = await source.mock.simulateCaptionChange(current.id, edited);
    setNotice(
      result.ok
        ? { tone: "bad", text: "The caption changed on another device. Any approval for the old text is now invalid." }
        : { tone: "bad", text: result.error.message },
    );
    onChanged();
  }

  /** One job's row in the inbox. A job whose account is not in the accounts list also shows its raw account id. */
  function jobRow(row: PublishJob) {
    const approval = approvalText(row.approval);
    return (
      <li key={row.id}>
        <Button
          type="button"
          variant="secondary"
          className="op-row"
          aria-current={row.id === selectedId ? "true" : undefined}
          onClick={() => setSelectedId(row.id)}
        >
          <span className="op-row__time">{instantLabel(row.scheduledAtUtc, row.timeZone)}</span>
          <span className="op-row__meta">
            {FORMAT_LABEL[row.format]} · {STATE_LABEL[row.state].text}
            {accountById.has(row.accountId) ? null : (
              <>
                {" "}
                · account id <span className="op-mono">{row.accountId}</span>
              </>
            )}
          </span>
          <Badge tone={approval.tone}>{approval.text}</Badge>
        </Button>
      </li>
    );
  }

  const canApprove = job !== null && awaitingDecision(job) && mediaReady(job);
  const canReject = job !== null && (awaitingDecision(job) || isApproved(job));

  return (
    <PageShell
      title="Approvals"
      subtitle="The only place content becomes publishable. An approval binds one exact revision of one job."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      <div className="op-inbox__filters">
        <SegmentedControl<Filter>
          label="Show"
          value={filter}
          onChange={setFilter}
          options={[
            { value: "decision", label: `Needs decision (${decisionCount})` },
            { value: "approved", label: "Approved" },
            { value: "all", label: "All" },
          ]}
        />
      </div>

      <div className="op-inbox">
        <section className="op-inbox__list" aria-labelledby="inbox-list-title">
          <h2 id="inbox-list-title" className="op-section-title">Jobs</h2>
          {list.state.status === "error" ? (
            <LoadError message="Could not read the approval inbox from the daemon." onRetry={list.reload} />
          ) : list.state.status === "loading" && allJobs.length === 0 ? (
            <SkeletonRows label="Loading jobs" rows={4} />
          ) : filtered.length === 0 ? (
            <p className="op-hint">Nothing in this view.</p>
          ) : (
            <>
              {accounts.map((a) => {
                const rows = filtered.filter((j) => j.accountId === a.id);
                if (rows.length === 0) return null;
                return (
                  <section key={a.id} className="op-inbox__group" aria-label={`${ordinals.get(a.id)?.label ?? "Account"}, ${a.handle}`}>
                    <div className="op-inbox__group-head">
                      <AccountChip account={a} ordinal={ordinals.get(a.id)} />
                    </div>
                    <ul>{rows.map((row) => jobRow(row))}</ul>
                  </section>
                );
              })}
              {orphans.length > 0 ? (
                <section className="op-inbox__group" aria-label="Jobs whose account is not in the accounts list">
                  <div className="op-inbox__group-head">
                    <p className="op-hint">Account not found in the accounts list</p>
                  </div>
                  <ul>{orphans.map((row) => jobRow(row))}</ul>
                </section>
              ) : null}
            </>
          )}
        </section>

        <section className="op-inbox__preview" aria-label="Variant preview">
          {selectedId === null ? (
            <p className="op-hint">Select a job to see the exact variant that would publish.</p>
          ) : preview.state.status === "error" ? (
            <LoadError message="Could not load this job." onRetry={preview.reload} />
          ) : preview.state.status === "loading" ? (
            <SkeletonRows label="Fetching the job and its media by digest" rows={5} />
          ) : job !== null && account === null ? (
            <Notice tone="bad">
              Account not found in the accounts list (account id <span className="op-mono">{job.accountId}</span>). Nothing was
              changed.
            </Notice>
          ) : job === null || account === null || ordinal === undefined ? (
            <Notice tone="bad">This job is no longer available. Nothing was changed.</Notice>
          ) : (
            <article className="op-preview" aria-labelledby="preview-title">
              <header className={`op-preview__account op-band-${account.band}`}>
                <p className="op-kicker">Destination account</p>
                <h2 id="preview-title" className="op-preview__title">
                  {PLATFORM_LABEL[account.platform]}: {account.displayName} {account.handle}
                </h2>
                <p>
                  {ordinal.label} of {ordinal.of} on {PLATFORM_LABEL[account.platform]} · {account.venture.name} · id ending{" "}
                  {account.idSuffix}
                </p>
              </header>

              <section aria-labelledby="pv-when">
                <h3 id="pv-when" className="op-section-title">When</h3>
                <p>{instantLabel(job.scheduledAtUtc, job.timeZone)}</p>
                {job.dst.kind !== "none" ? <p>{job.dst.note}</p> : null}
              </section>

              {job.publish ? (
                <section aria-labelledby="pv-publish">
                  <h3 id="pv-publish" className="op-section-title">Daemon publish</h3>
                  <PublishProgressView progress={job.publish} timeZone={job.timeZone} />
                </section>
              ) : null}

              <section aria-labelledby="pv-caption">
                <h3 id="pv-caption" className="op-section-title">Caption as it will be posted</h3>
                <p className="op-count">
                  {formatCount(charCount(job.caption))} characters · fingerprint{" "}
                  <code>{job.captionDigest}</code>
                </p>
                <div className="op-caption" tabIndex={0} role="region" aria-label="Caption text">
                  {job.caption}
                </div>
              </section>

              <section aria-labelledby="pv-comment">
                <h3 id="pv-comment" className="op-section-title">First comment or link</h3>
                <p>{job.firstComment ?? "None"}</p>
              </section>

              <section aria-labelledby="pv-media">
                <h3 id="pv-media" className="op-section-title">Media</h3>
                <ul className="op-media">
                  {job.mediaItems.map((m, index) => (
                    <li key={`${m.hash}#${index}`}>
                      <span className="op-mono">{shortHash(m.hash)}</span> {m.kind === "video" ? "Video" : "Image"} ·{" "}
                      {duration(m.durationMs !== undefined ? m.durationMs : m.durationSeconds === null ? null : Math.round(m.durationSeconds * 1000))}
                      {m.dimensions ? ` · ${m.dimensions}` : ""} · {MEDIA_STATUS_LABEL[m.status]}
                      {m.status === "uploading" && m.uploadPercent !== null ? ` ${m.uploadPercent}%` : ""}
                      <details>
                        <summary>Full digest</summary>
                        <code className="op-digest">{m.hash}</code>
                      </details>
                    </li>
                  ))}
                </ul>
              </section>

              <section aria-labelledby="pv-settings">
                <h3 id="pv-settings" className="op-section-title">Settings</h3>
                <ul>
                  {job.settings.map((s, index) => (
                    <li key={`${s}#${index}`}>{s}</li>
                  ))}
                </ul>
              </section>

              <section aria-labelledby="pv-policy">
                <h3 id="pv-policy" className="op-section-title">Policy and revision</h3>
                <p>
                  Policy {job.policyRevision === null ? "revision not stated" : `r${job.policyRevision}`}. Approving revision {job.revision}. Job state: {STATE_LABEL[job.state].text}.
                </p>
                <p>
                  <Badge tone={approvalText(job.approval).tone}>{approvalText(job.approval).text}</Badge>
                </p>
                {job.approval.status === "invalidated" ? (
                  <p role="status" className="op-invalid">
                    The caption changed after revision {job.approval.approvedRevision} was approved. This job will not publish until
                    revision {job.revision} is approved.
                  </p>
                ) : null}
                {job.approval.status === "rejected" && job.approval.note ? <p>Note: {job.approval.note}</p> : null}
                {job.state === "outcome_unknown" ? (
                  <p role="alert" className="op-invalid">
                    The platform may have accepted this post. No retry until reconciled.
                  </p>
                ) : null}
              </section>

              <section className="op-decide" aria-labelledby="pv-decide">
                <h3 id="pv-decide" className="op-section-title">Decision</h3>
                {approveBlock(job) ? <p className="op-hint">{approveBlock(job)}</p> : null}
                <div className="op-decide__buttons">
                  <Button
                    variant="primary"
                    disabled={!canApprove || busy}
                    onClick={() => setConfirmApprove(true)}
                  >
                    {`Approve for ${account.handle} on ${PLATFORM_LABEL[account.platform]}`}
                  </Button>
                  <Button variant="danger" disabled={!canReject || busy} onClick={() => setRejecting(true)}>
                    {`Reject for ${account.handle} on ${PLATFORM_LABEL[account.platform]}`}
                  </Button>
                  {source.mock ? (
                    <Button variant="secondary" disabled={busy} onClick={() => simulateEdit(job)}>
                      Simulate an edit from another device
                    </Button>
                  ) : null}
                </div>
              </section>
            </article>
          )}

          <div aria-live="polite">
            {notice ? (
              <Notice tone={notice.tone} className="op-notice">{notice.text}</Notice>
            ) : null}
          </div>
        </section>
      </div>

      {confirmApprove && job && account && ordinal ? (
        <ConfirmDialog
          title={`Approve revision ${job.revision} for ${account.handle} on ${PLATFORM_LABEL[account.platform]}?`}
          description={
            <>
              <span className="op-dialog-line">
                Account: {account.displayName} ({account.handle}), {ordinal.label.toLowerCase()} of {ordinal.of} on{" "}
                {PLATFORM_LABEL[account.platform]}.
              </span>
              <span className="op-dialog-line">
                It publishes {instantLabel(job.scheduledAtUtc, job.timeZone)}.{" "}
                {job.policyRevision === null ? "Policy revision not stated." : `Policy r${job.policyRevision}.`}
              </span>
              <span className="op-dialog-line">
                Any later change to the caption, media or policy invalidates this approval.
              </span>
            </>
          }
          confirmLabel={`Approve for ${account.handle} on ${PLATFORM_LABEL[account.platform]}`}
          cancelLabel="Cancel"
          onConfirm={() => approve(job)}
          onCancel={() => setConfirmApprove(false)}
        />
      ) : null}

      {rejecting && job && account ? (
        <RejectDialog
          handle={account.handle}
          platform={PLATFORM_LABEL[account.platform]}
          onCancel={() => setRejecting(false)}
          onSubmit={(code, note) => reject(job, code, note)}
        />
      ) : null}
    </PageShell>
  );
}

function RejectDialog({
  handle,
  platform,
  onSubmit,
  onCancel,
}: {
  handle: string;
  platform: string;
  onSubmit: (code: RejectReason, note: string) => void;
  onCancel: () => void;
}) {
  const [code, setCode] = useState<RejectReason | null>(null);
  const [note, setNote] = useState("");
  const [error, setError] = useState<string | null>(null);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (code === null) {
      setError("Choose a reason before rejecting.");
      return;
    }
    if (code === "other" && note.trim() === "") {
      setError("Add a note when the reason is Other.");
      return;
    }
    onSubmit(code, note.trim());
  }

  return (
    <ModalShell label={`Reject for ${handle} on ${platform}`} onClose={onCancel} width={480} role="alertdialog">
      <form className="op-form" onSubmit={submit} noValidate>
        <h2 className="op-section-title">{`Reject for ${handle} on ${platform}`}</h2>
        <fieldset className="op-fieldset" aria-describedby={error ? "reject-error" : undefined}>
          <legend>Reason</legend>
          {REJECT_ORDER.map((r) => (
            <label key={r} className="op-radio">
              <input
                type="radio"
                name="reject-reason"
                value={r}
                checked={code === r}
                onChange={() => {
                  setCode(r);
                  setError(null);
                }}
              />
              {REJECT_LABEL[r]}
            </label>
          ))}
        </fieldset>
        <Field label="Note (optional, required for Other)">
          {({ id }) => (
            <textarea
              id={id}
              className="op-textarea"
              rows={3}
              value={note}
              onChange={(e) => setNote(e.target.value)}
            />
          )}
        </Field>
        {error ? (
          <p id="reject-error" role="alert" className="op-error-inline">
            {error}
          </p>
        ) : null}
        <div className="op-dialog__actions">
          <Button type="button" variant="secondary" onClick={onCancel}>
            Cancel
          </Button>
          <Button type="submit" variant="danger">
            {`Reject for ${handle} on ${platform}`}
          </Button>
        </div>
      </form>
    </ModalShell>
  );
}
