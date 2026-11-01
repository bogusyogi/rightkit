import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { openThreadAction } from "../src/store/actions/mail/openThread";
import type { Thread, ThreadMessage } from "../src/lib/ipc";
import type { AppState } from "../src/store";

const oldMessage: ThreadMessage = {
  message_id: "old-msg",
  from_name: "Old Sender",
  from_addr: "old@example.com",
  to_addrs: "me@example.com",
  cc_addrs: "",
  reply_to: null,
  list_unsubscribe: null,
  internal_date: 1,
  subject: "Old subject",
  body: "old body that must not remain visible",
  is_html: false,
  unread: false,
  has_attachments: false,
  auth: null,
};

const nextThread: Thread = {
  account_id: "acct",
  account_color: "#000000",
  account_name: "Account",
  thread_id: "next-thread",
  from_name: "Next Sender",
  subject: "Next subject",
  snippet: "Next snippet",
  unread: false,
  starred: false,
  last_message_at: 2,
  labels: ["INBOX"],
  has_attachments: false,
  attachment_names: [],
  important: false,
  message_count: 1,
  participants: ["Next Sender"],
};

function createOpeningState() {
  const state: Record<string, unknown> = {
    imagePolicy: true,
    senderImageTrust: {},
    domainImageTrust: {},
    expandAll: false,
    openThread: {
      account_id: "acct",
      thread_id: "old-thread",
      subject: "Old subject",
    },
    threadMsgs: [oldMessage],
    threads: [nextThread],
    results: [],
    openLabels: ["IMPORTANT"],
    openStarred: true,
  };

  const set = (key: string) => (value: unknown) => {
    state[key] =
      typeof value === "function"
        ? (value as (prev: unknown) => unknown)(state[key])
        : value;
  };

  Object.assign(state, {
    setShowImages: set("showImages"),
    setMoreOpenIdx: set("moreOpenIdx"),
    setLabelPickerOpen: set("labelPickerOpen"),
    setMsgAttach: set("msgAttach"),
    setInlineMap: set("inlineMap"),
    setRemoteImgMap: set("remoteImgMap"),
    setExpandedMsgs: set("expandedMsgs"),
    setDetailMsgs: set("detailMsgs"),
    setCustomSnoozeOpen: set("customSnoozeOpen"),
    setAiSummary: set("aiSummary"),
    setTrustReport: set("trustReport"),
    setOpenThread: set("openThread"),
    setThreadMsgs: set("threadMsgs"),
    setOpenLabels: set("openLabels"),
    setOpenStarred: set("openStarred"),
    setErr: set("err"),
    setThreads: set("threads"),
    setResults: set("results"),
    setCounts: set("counts"),
  });

  return {
    state,
    get: () => state as unknown as AppState,
  };
}

test("openThreadAction clears previous messages before loading the next thread", () => {
  const { state, get } = createOpeningState();

  void openThreadAction(get, nextThread);

  assert.deepEqual(state.threadMsgs, []);
  assert.deepEqual(state.openLabels, []);
  assert.equal(state.openStarred, false);
  assert.deepEqual(state.openThread, {
    account_id: "acct",
    thread_id: "next-thread",
    subject: "Next subject",
  });
});

test("ReaderView keys messages by opened thread and message id", () => {
  const source = readFileSync(resolve("src/views/ReaderView.tsx"), "utf8");

  assert.match(
    source,
    /key=\{`\$\{openThread\.account_id\}:\$\{openThread\.thread_id\}:\$\{threadMsgs\[i\]\.message_id\}`\}/,
  );
});
