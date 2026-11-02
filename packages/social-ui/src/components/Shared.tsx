import { useState } from "react";
import { Badge, Button, Notice } from "@rightkit/react-shell/react";
import type { Account, AccountOrdinal, PublishJob, PublishProgress } from "../data/index.js";
import { instantLabel, timeInZone } from "../data/index.js";
import { shell } from "../lib/shell.js";
import { PLATFORM_LABEL, PUBLISH_STATE_LABEL, STATE_LABEL, approvalText } from "../screens/labels.js";

/** Shown on every screen while MockDataSource is active. Folds to nothing in a release build (Vite constant-folds the guard). */
export function MockBanner() {
  if (!(import.meta.env?.DEV || (typeof __RIGHTKIT_MOCK__ === "boolean" && __RIGHTKIT_MOCK__))) return null;
  return (
    <p role="status" className="op-banner">
      <strong>Mock data.</strong> Invented channels held in memory. Nothing here is live and nothing publishes.
    </p>
  );
}

/**
 * Account identity: stable colour band, a text label, platform and handle. The colour is a
 * decoration only; the words carry the meaning.
 */
export function AccountChip({
  account,
  ordinal,
  detail = false,
}: {
  account: Account;
  ordinal?: AccountOrdinal;
  detail?: boolean;
}) {
  return (
    <span className={`op-chip op-band-${account.band}`}>
      <span className="op-chip__name">
        {ordinal ? ordinal.label : "Account"}: {account.displayName}
      </span>
      <span className="op-chip__meta">
        {PLATFORM_LABEL[account.platform]} <span className="op-chip__handle">{account.handle}</span>
        {detail ? ` · id ending ${account.idSuffix}` : null}
      </span>
    </span>
  );
}

/** State, approval and flag labels for one job. Every flag is text, never colour alone. */
export function JobBadges({ job, compact = false }: { job: PublishJob; compact?: boolean }) {
  const state = STATE_LABEL[job.state];
  const approval = approvalText(job.approval);
  return (
    <span className="op-badges">
      <Badge tone={state.tone}>{state.text}</Badge>
      {compact ? null : <Badge tone={approval.tone}>{approval.text}</Badge>}
      {job.lateness ? (
        <Badge tone="warn">Late: media not ready by {timeInZone(job.lateness.mediaNotReadyBy, job.timeZone)}</Badge>
      ) : null}
      {job.conflict ? (
        <Badge tone="bad">Conflict: {job.conflict.minimumSpacingMinutes} min spacing</Badge>
      ) : null}
      {job.dst.kind === "ambiguous_fires_once" ? <Badge tone="accent">DST: fires once</Badge> : null}
      {job.dst.kind === "nonexistent_moved" ? <Badge tone="accent">DST: moved by account rule</Badge> : null}
    </span>
  );
}

/**
 * The daemon's publish state for one job. A waiting job shows when it resumes: a quota cooldown for
 * rate_limited, otherwise a retry wait. The platform link is shown as text with a Copy link button.
 * The link is never opened from here.
 */
/** A platform permalink as selectable text with a Copy link button. It is never an anchor, so it never opens. */
export function PermalinkCopy({ url }: { url: string }) {
  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");

  async function copyLink() {
    try {
      await shell.clipboard.write({ text: url });
      setCopy("copied");
    } catch {
      setCopy("failed");
    }
  }

  return (
    <div className="op-publish__link">
      <p>
        Platform link: <span className="op-mono">{url}</span>
      </p>
      <Button size="sm" variant="secondary" onClick={copyLink} aria-label={`Copy link to ${url}`}>
        Copy link
      </Button>
      <span role="status">
        {copy === "copied" ? "Link copied." : copy === "failed" ? "Copy failed. Select the link text and copy it." : ""}
      </span>
    </div>
  );
}

export function PublishProgressView({ progress, timeZone }: { progress: PublishProgress; timeZone: string }) {
  const label = PUBLISH_STATE_LABEL[progress.state];
  const link = progress.permalink;
  const waitKind = progress.errorCode === "rate_limited" ? "Quota cooldown" : "Retry wait";

  return (
    <section className="op-publish" aria-label="Publish status">
      <p>
        Daemon publish: <Badge tone={label.tone}>{label.text}</Badge>
      </p>
      {progress.state === "waiting" ? (
        <p>
          {progress.resumeAtUtc !== null
            ? `${waitKind}. Resumes ${instantLabel(progress.resumeAtUtc, timeZone)}.`
            : `${waitKind}. The resume time is not reported.`}
        </p>
      ) : null}
      {link !== null ? <PermalinkCopy url={link} /> : null}
    </section>
  );
}

export function LoadError({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <Notice
      tone="bad"
      action={
        <Button size="sm" variant="secondary" onClick={onRetry}>
          Retry
        </Button>
      }
    >
      {message}
    </Notice>
  );
}
