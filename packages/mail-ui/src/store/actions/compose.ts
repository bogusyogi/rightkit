// Compose store actions: signature resolution, alias/contact-group warm-up, and
// opening the compose drawer for a new message, reply, reply-all, or forward.
import { htmlToText } from "../../lib/html";
import {
  getSignature,
  listAliases,
  warmPeopleCache,
  listContactGroups,
  type ThreadMessage,
} from "../../lib/ipc";
import type { AppState, ComposeState } from "../types";
import type { StoreGet } from "../mk";

type ComposeActions = Pick<
  AppState,
  "sigFor" | "loadAliases" | "openCompose" | "openComposeTo" | "openReply" | "openForward"
>;

function quoteBody(m: ThreadMessage): string {
  const when = new Date(m.internal_date).toLocaleString();
  const who = m.from_name || m.from_addr;
  const text = (m.is_html ? htmlToText(m.body) : m.body).trim();
  const quoted = text
    .split("\n")
    .map((l) => `> ${l}`)
    .join("\n");
  return `On ${when}, ${who} wrote:\n${quoted}`;
}

function baseCompose(accountId: string, overrides: Partial<ComposeState>): ComposeState {
  return {
    accountId,
    from: accountId,
    to: "",
    cc: "",
    bcc: "",
    subject: "",
    body: "",
    threadId: null,
    inReplyTo: null,
    showCc: false,
    nonce: Date.now(),
    attachments: [],
    gmailDraftId: null,
    ...overrides,
  };
}

type SignatureState = Pick<AppState, "activeAccount" | "signature" | "accountSigs" | "aliasSigs">;

export async function resolveComposeSignature(
  state: SignatureState,
  from: string,
  accountId: string,
  fetchSignature: (accountId: string) => Promise<string> = getSignature,
): Promise<string> {
  const cached =
    state.aliasSigs[from] ??
    state.accountSigs[from] ??
    state.accountSigs[accountId];
  if (cached !== undefined) return cached;
  if (accountId === state.activeAccount) return state.signature;
  return fetchSignature(accountId);
}

export function composeActions(get: StoreGet): ComposeActions {
  const signatureLoads = new Map<string, Promise<string>>();

  const loadAccountSignature = (accountId: string): Promise<string> => {
    const existing = get().accountSigs[accountId];
    if (existing !== undefined) return Promise.resolve(existing);

    const inFlight = signatureLoads.get(accountId);
    if (inFlight) return inFlight;

    const promise = getSignature(accountId)
      .then((sig) => {
        const s = get();
        s.setAccountSigs((prev) => ({ ...prev, [accountId]: sig }));
        if (accountId === get().activeAccount) s.setSignatureState(sig);
        return sig;
      })
      .catch(() => {
        const s = get();
        s.setAccountSigs((prev) => ({ ...prev, [accountId]: "" }));
        if (accountId === get().activeAccount) s.setSignatureState("");
        return "";
      })
      .finally(() => signatureLoads.delete(accountId));

    signatureLoads.set(accountId, promise);
    return promise;
  };

  return {
    sigFor: (from, accountId) => {
      const s = get();
      const targetAccount = accountId || s.activeAccount || from;
      return (
        s.aliasSigs[from] ??
        s.accountSigs[from] ??
        s.accountSigs[targetAccount] ??
        (targetAccount === s.activeAccount ? s.signature : "")
      );
    },
    loadAliases: (accountId) => {
      const { activeAccount, setAliases, setContactGroups } = get();
      const target = accountId || activeAccount;
      if (!target) return;
      listAliases(target).then(setAliases).catch(() => setAliases([]));
      warmPeopleCache(target).catch(() => {});
      listContactGroups(target).then(setContactGroups).catch(() => setContactGroups([]));
      loadAccountSignature(target).catch(() => {});
    },
    openCompose: () => {
      const s = get();
      if (!s.activeAccount) return;
      s.loadAliases();
      const from = s.defaultAlias || s.activeAccount;
      const sig = s.sigFor(from, s.activeAccount);
      s.setCompose(baseCompose(s.activeAccount, {
        from,
        body: sig ? `\n\n-- \n${sig}` : "",
      }));
    },
    openComposeTo: (to, subject, body) => {
      const s = get();
      if (!s.activeAccount) return;
      s.loadAliases();
      const sigv = s.sigFor(s.activeAccount, s.activeAccount);
      const sig = sigv ? `\n\n-- \n${sigv}` : "";
      s.setCompose(baseCompose(s.activeAccount, {
        to,
        subject,
        body: body ? `${body}${sig}` : sig,
      }));
    },
    openReply: async (m, all) => {
      const s = get();
      if (!s.openThread) return;
      const thread = s.openThread;
      const accountId = thread.account_id;
      s.loadAliases(accountId);
      const to = (m.reply_to && m.reply_to.trim()) || m.from_addr;
      const subj = /^re:/i.test(thread.subject)
        ? thread.subject
        : `Re: ${thread.subject}`;
      const cc = all ? [m.to_addrs, m.cc_addrs].filter((x) => x && x.trim()).join(", ") : "";
      const sigv = await resolveComposeSignature(get(), accountId, accountId, loadAccountSignature);
      const currentThread = get().openThread;
      if (!currentThread || currentThread.thread_id !== thread.thread_id || currentThread.account_id !== accountId) {
        return;
      }
      const sig = sigv ? `-- \n${sigv}\n` : "";
      get().setCompose(baseCompose(accountId, {
        to,
        cc,
        subject: subj,
        body: `\n\n${sig}\n${quoteBody(m)}`,
        threadId: thread.thread_id,
        inReplyTo: m.message_id,
        showCc: !!cc,
      }));
    },
    openForward: async (m) => {
      const s = get();
      if (!s.openThread) return;
      const thread = s.openThread;
      const accountId = thread.account_id;
      s.loadAliases(accountId);
      const subj = /^fwd:/i.test(thread.subject)
        ? thread.subject
        : `Fwd: ${thread.subject}`;
      const text = (m.is_html ? htmlToText(m.body) : m.body).trim();
      const hdr =
        `---------- Forwarded message ----------\n` +
        `From: ${m.from_name} <${m.from_addr}>\n` +
        `Date: ${new Date(m.internal_date).toLocaleString()}\n` +
        `Subject: ${thread.subject}\n` +
        `To: ${m.to_addrs}\n\n`;
      const sigv = await resolveComposeSignature(get(), accountId, accountId, loadAccountSignature);
      const currentThread = get().openThread;
      if (!currentThread || currentThread.thread_id !== thread.thread_id || currentThread.account_id !== accountId) {
        return;
      }
      const sig = sigv ? `-- \n${sigv}\n\n` : "";
      get().setCompose(baseCompose(accountId, {
        subject: subj,
        body: `\n\n${sig}${hdr}${text}`,
      }));
    },
  };
}
