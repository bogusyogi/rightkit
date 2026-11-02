import { useState } from "react";
import { Badge, Button, EmptyState, Field, Input, Notice, PageShell, SkeletonRows } from "@rightkit/react-shell/react";
import { LoadError, MockBanner } from "../components/Shared.js";
import { useAsync } from "../hooks/useAsync.js";
import { shell } from "../lib/shell.js";
import { disclosureFound, type DataSource, type DraftKind, type DraftState, type ManualDraft } from "../data/index.js";
import { platformName } from "./labels.js";
import { hostAppName } from "../appName.js";

interface Props {
  source: DataSource;
  rev: number;
  onChanged: () => void;
  /** Disclosure phrases the caller supplies for approval. The engine has no built-in list. */
  disclosurePhrases: readonly string[];
}

type Notice = { tone: "ok" | "bad"; text: string } | null;

const SECTIONS: readonly { state: DraftState; title: string; empty: string }[] = [
  { state: "drafted", title: "Drafted, waiting for your approval", empty: "No drafts are waiting for approval." },
  { state: "approved", title: "Approved, to post by hand", empty: "No approved drafts are waiting for you to post." },
  { state: "posted_by_operator", title: "Posted by you", empty: "No drafts are recorded as posted." },
  { state: "skipped", title: "Skipped", empty: "No drafts are skipped." },
  { state: "stale", title: "Stale", empty: "No drafts are stale." },
];

const KIND_LABEL: Record<DraftKind, string> = { post: "Post", comment: "Comment", reply: "Reply" };

const STATE_BADGE: Record<DraftState, { text: string; tone: "neutral" | "accent" | "ok" | "warn" }> = {
  drafted: { text: "Drafted", tone: "neutral" },
  approved: { text: "Approved, post by hand", tone: "accent" },
  posted_by_operator: { text: "Posted by you", tone: "ok" },
  skipped: { text: "Skipped", tone: "neutral" },
  stale: { text: "Stale", tone: "warn" },
};

/** Identifiers in the engine contain slashes; DOM ids here use only letters, digits, "_" and "-". */
function domId(raw: string): string {
  return raw.replace(/[^A-Za-z0-9_-]/g, "_");
}

/** What the disclosure rule says, in words. The engine's gate at approval is the authority. */
function disclosureText(draft: ManualDraft, phrases: readonly string[]): string {
  if (!draft.disclosureRequired) return "Not required for this draft.";
  if (phrases.every((p) => p.trim() === "")) {
    return "Required. No disclosure phrases are set in this app, so approval will be refused until they are.";
  }
  return disclosureFound(draft.body, phrases)
    ? "Required. A disclosure phrase is in the first sentence. The engine checks again when you approve."
    : "Required. No disclosure phrase is in the first sentence yet, so approval will be refused.";
}

export function DraftsScreen({ source, rev, onChanged, disclosurePhrases }: Props) {
  const { state, reload, value } = useAsync<ManualDraft[]>(() => source.listDrafts(), [source, rev]);

  return (
    <PageShell
      title="Drafts"
      subtitle="Text you post by hand on each platform. Approve it, post it yourself, then record the link."
    >
      {source.kind === "mock" ? <MockBanner /> : null}

      <p className="op-draft__rule">
        <strong>{hostAppName()} never posts these.</strong> You post by hand and record the link.
      </p>

      {state.status === "error" ? (
        <LoadError message="Could not read drafts from the daemon." onRetry={reload} />
      ) : value === null ? (
        <SkeletonRows label="Loading drafts" rows={3} />
      ) : value.length === 0 ? (
        <EmptyState title="No drafts yet">
          <p>Drafts appear here once one is created.</p>
        </EmptyState>
      ) : (
        SECTIONS.map((section) => {
          const items = value.filter((draft) => draft.state === section.state);
          return (
            <section key={section.state} className="op-drafts-section" aria-labelledby={`drafts-${section.state}`}>
              <h2 id={`drafts-${section.state}`} className="op-section-title">
                {section.title} ({items.length})
              </h2>
              {items.length === 0 ? (
                <p className="op-hint">{section.empty}</p>
              ) : (
                <div className="op-drafts">
                  {items.map((draft) => (
                    <DraftCard
                      key={draft.id}
                      draft={draft}
                      disclosurePhrases={disclosurePhrases}
                      source={source}
                      onChanged={onChanged}
                    />
                  ))}
                </div>
              )}
            </section>
          );
        })
      )}
    </PageShell>
  );
}

function DraftCard({
  draft,
  disclosurePhrases,
  source,
  onChanged,
}: {
  draft: ManualDraft;
  disclosurePhrases: readonly string[];
  source: DataSource;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<Notice>(null);
  const [linkOpen, setLinkOpen] = useState(false);
  const [postedUrl, setPostedUrl] = useState("");
  const [skipReason, setSkipReason] = useState("");

  const id = domId(draft.id);
  const platform = platformName(draft.platform);
  const badge = STATE_BADGE[draft.state];
  const canSkip = draft.state === "drafted" || draft.state === "approved";

  async function copyBody() {
    try {
      await shell.clipboard.write({ text: draft.body });
      setNotice({ tone: "ok", text: "Copied. Paste it on the platform, post it there, then record the link here." });
    } catch {
      setNotice({ tone: "bad", text: "Copying is not available here. Select the text above and copy it by hand." });
    }
  }

  async function approve() {
    setBusy(true);
    const result = await source.approveDraft(draft.id, draft.revision, disclosurePhrases);
    setBusy(false);
    if (result.ok) {
      setNotice({ tone: "ok", text: "Approved. Post it by hand, then record the link." });
      onChanged();
    } else {
      setNotice({ tone: "bad", text: result.error.message });
    }
  }

  async function recordPost() {
    const url = postedUrl.trim();
    if (!/^https:\/\//i.test(url)) {
      setNotice({ tone: "bad", text: "The link must start with https://. Nothing was recorded." });
      return;
    }
    setBusy(true);
    const result = await source.markDraftPosted(draft.id, draft.revision, url);
    setBusy(false);
    if (result.ok) {
      setNotice({ tone: "ok", text: "Recorded. This draft is now marked as posted by you." });
      onChanged();
    } else {
      setNotice({ tone: "bad", text: result.error.message });
    }
  }

  async function skip() {
    setBusy(true);
    const result = await source.skipDraft(draft.id, draft.revision, skipReason);
    setBusy(false);
    if (result.ok) {
      setNotice({ tone: "ok", text: "Skipped." });
      onChanged();
    } else {
      setNotice({ tone: "bad", text: result.error.message });
    }
  }

  return (
    <article className="op-card op-draft" aria-labelledby={`draft-${id}`}>
      <h3 id={`draft-${id}`} className="op-draft__title">
        {platform}: {KIND_LABEL[draft.kind]}
        {draft.title ? `, ${draft.title}` : ""}
      </h3>
      <div className="op-badges">
        <Badge tone={badge.tone}>{badge.text}</Badge>
      </div>

      <dl className="op-dl">
        <dt>Account</dt>
        <dd>{draft.channelLabel}</dd>
        <dt>Target</dt>
        <dd>{draft.target ?? "None: a new post"}</dd>
        <dt>Disclosure</dt>
        <dd>{disclosureText(draft, disclosurePhrases)}</dd>
        <dt>Length</dt>
        <dd>{draft.bodyChars} characters</dd>
        {draft.sourceNote ? (
          <>
            <dt>Why this draft exists</dt>
            <dd>{draft.sourceNote}</dd>
          </>
        ) : null}
        {draft.postedUrl ? (
          <>
            <dt>Link you recorded</dt>
            <dd>
              {draft.postedUrl}
              {draft.postedAtUtc ? ` (recorded ${draft.postedAtUtc})` : ""}
            </dd>
          </>
        ) : null}
        {draft.skipReason ? (
          <>
            <dt>Skip reason</dt>
            <dd>{draft.skipReason}</dd>
          </>
        ) : null}
      </dl>

      <section aria-labelledby={`draft-text-${id}`}>
        <h4 id={`draft-text-${id}`} className="op-section-title">
          Text
        </h4>
        <div className="op-caption">{draft.body}</div>
      </section>

      <div className="op-decide__buttons">
        <Button variant="secondary" size="sm" onClick={copyBody}>
          Copy text
        </Button>
        {draft.state === "drafted" ? (
          <Button variant="primary" disabled={busy} onClick={approve}>
            Approve
          </Button>
        ) : null}
        {draft.state === "approved" ? (
          <Button
            variant="primary"
            disabled={busy}
            aria-expanded={linkOpen}
            onClick={() => setLinkOpen((open) => !open)}
          >
            I posted this
          </Button>
        ) : null}
      </div>

      {draft.state === "approved" && linkOpen ? (
        <div className="op-draft__form">
          <Field label="Link to your post" note="Paste the full https address of the post you made by hand.">
            {({ id: fieldId, "aria-describedby": describedBy }) => (
              <Input
                id={fieldId}
                aria-describedby={describedBy}
                type="url"
                inputMode="url"
                placeholder="https://"
                value={postedUrl}
                onChange={(e) => setPostedUrl(e.target.value)}
              />
            )}
          </Field>
          <div className="op-decide__buttons">
            <Button variant="primary" disabled={busy || postedUrl.trim() === ""} onClick={recordPost}>
              Record posted link
            </Button>
          </div>
        </div>
      ) : null}

      {canSkip ? (
        <div className="op-draft__form">
          <Field label="Reason to skip (required)">
            {({ id: fieldId, "aria-describedby": describedBy }) => (
              <Input
                id={fieldId}
                aria-describedby={describedBy}
                value={skipReason}
                onChange={(e) => setSkipReason(e.target.value)}
              />
            )}
          </Field>
          <div className="op-decide__buttons">
            <Button variant="secondary" disabled={busy || skipReason.trim() === ""} onClick={skip}>
              Skip draft
            </Button>
          </div>
        </div>
      ) : null}

      {notice ? (
        <Notice tone={notice.tone} className="op-notice">{notice.text}</Notice>
      ) : null}
    </article>
  );
}
