// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented inbox events for the first channel, one channel with none, and the unmatched events. Recipient ids,
// sender ids, texts and event ids are fictional. Two texts look like markup and a link on purpose: the screens
// must show them as plain text, inert. No event here was received from a real platform.
import type { InboxEvent, InboxStats, UnreadableRow } from "../types";
import { SCOPE_ALPHA, SCOPE_GAMMA } from "./fixtures-optimise";

export interface FixtureInboxRow {
  event: InboxEvent;
  /** The raw item JSON, as the daemon would keep it. */
  rawJson: string;
}

function itemJson(item: Record<string, unknown>): string {
  return JSON.stringify(item, null, 2);
}

export const FIXTURE_INBOX: FixtureInboxRow[] = [
  {
    // Newest. Its text is markup, shown as text only.
    event: {
      readable: true,
      eventId: "inbox-fixture-0004",
      scopeId: SCOPE_ALPHA,
      unmatched: false,
      unmatchedReason: null,
      platform: "instagram",
      kind: "comment",
      recipientId: "fixture-ig-alpha",
      platformEventId: "fixture-comment-0004",
      senderId: "fixture-sender-0004",
      text: "<img src=x onerror=alert(1)>",
      eventTimeAsGiven: "2026-10-12T17:55:00+0000",
      oversize: false,
      receivedAtUtc: "2026-10-12T17:56:10Z",
      state: "open",
      stateBy: null,
      stateAtUtc: null,
      stateNote: "",
    },
    rawJson: itemJson({
      id: "fixture-comment-0004",
      from: { id: "fixture-sender-0004" },
      text: "<img src=x onerror=alert(1)>",
      created_time: "2026-10-12T17:55:00+0000",
    }),
  },
  {
    // A message whose text is a javascript: address, shown as plain text. A person handled it by hand.
    event: {
      readable: true,
      eventId: "inbox-fixture-0003",
      scopeId: SCOPE_ALPHA,
      unmatched: false,
      unmatchedReason: null,
      platform: "instagram",
      kind: "message",
      recipientId: "fixture-ig-alpha",
      platformEventId: "fixture-message-0003",
      senderId: "fixture-sender-0003",
      text: "Where is the link? javascript:void(0)",
      eventTimeAsGiven: "2026-10-12T09:12:00Z",
      oversize: false,
      receivedAtUtc: "2026-10-12T09:12:30Z",
      state: "handled",
      stateBy: "adrian",
      stateAtUtc: "2026-10-12T09:40:00Z",
      stateNote: "Answered by hand on the platform.",
    },
    rawJson: itemJson({
      sender: { id: "fixture-sender-0003" },
      message: { mid: "fixture-message-0003", text: "Where is the link? javascript:void(0)" },
      timestamp: 1791797520000,
    }),
  },
  {
    // A mention cut to its bound. The raw JSON keeps the whole item.
    event: {
      readable: true,
      eventId: "inbox-fixture-0002",
      scopeId: SCOPE_ALPHA,
      unmatched: false,
      unmatchedReason: null,
      platform: "instagram",
      kind: "mention",
      recipientId: "fixture-ig-alpha",
      platformEventId: "fixture-mention-0002",
      senderId: "fixture-sender-0002",
      text: "Fixture mention of the channel. The daemon cut this text to its bound",
      eventTimeAsGiven: "2026-10-11T18:40:00Z",
      oversize: true,
      receivedAtUtc: "2026-10-11T18:41:05Z",
      state: "ignored",
      stateBy: "adrian",
      stateAtUtc: "2026-10-11T19:00:00Z",
      stateNote: "Not a question. No reply needed.",
    },
    rawJson: itemJson({
      id: "fixture-mention-0002",
      from: { id: "fixture-sender-0002" },
      text: "Fixture mention of the channel. The daemon cut this text to its bound, and this copy keeps the whole item.",
    }),
  },
  {
    // An event with no text, sender or time: the empty fields are said so on the screen.
    event: {
      readable: true,
      eventId: "inbox-fixture-0001",
      scopeId: SCOPE_ALPHA,
      unmatched: false,
      unmatchedReason: null,
      platform: "instagram",
      kind: "other",
      recipientId: "fixture-ig-alpha",
      platformEventId: null,
      senderId: null,
      text: null,
      eventTimeAsGiven: null,
      oversize: false,
      receivedAtUtc: "2026-10-10T07:00:00Z",
      state: "open",
      stateBy: null,
      stateAtUtc: null,
      stateNote: "",
    },
    rawJson: itemJson({ id: "fixture-other-0001", field: "read" }),
  },
  {
    // Unmatched: no stored account has this recipient id.
    event: {
      readable: true,
      eventId: "inbox-fixture-0005",
      scopeId: null,
      unmatched: true,
      unmatchedReason: "no_account",
      platform: "facebook",
      kind: "comment",
      recipientId: "fixture-fb-unknown",
      platformEventId: "fixture-comment-0005",
      senderId: "fixture-sender-0005",
      text: "Fixture comment on a page App does not hold",
      eventTimeAsGiven: "2026-10-12T12:00:00Z",
      oversize: false,
      receivedAtUtc: "2026-10-12T12:00:20Z",
      state: "open",
      stateBy: null,
      stateAtUtc: null,
      stateNote: "",
    },
    rawJson: itemJson({ id: "fixture-comment-0005", from: { id: "fixture-sender-0005" } }),
  },
  {
    // Unmatched: two stored accounts share this handle, so the daemon refuses to guess.
    event: {
      readable: true,
      eventId: "inbox-fixture-0007",
      scopeId: null,
      unmatched: true,
      unmatchedReason: "ambiguous_account",
      platform: "instagram",
      kind: "mention",
      recipientId: "fixture-ig-shared",
      platformEventId: "fixture-mention-0007",
      senderId: "fixture-sender-0007",
      text: "Fixture mention of a handle that two channels hold",
      eventTimeAsGiven: "2026-10-11T08:00:00Z",
      oversize: false,
      receivedAtUtc: "2026-10-11T08:00:09Z",
      state: "open",
      stateBy: null,
      stateAtUtc: null,
      stateNote: "",
    },
    rawJson: itemJson({ id: "fixture-mention-0007", from: { id: "fixture-sender-0007" } }),
  },
  {
    // Channel GAMMA: one open mention.
    event: {
      readable: true,
      eventId: "inbox-fixture-0006",
      scopeId: SCOPE_GAMMA,
      unmatched: false,
      unmatchedReason: null,
      platform: "threads",
      kind: "mention",
      recipientId: "fixture-th-gamma",
      platformEventId: "fixture-mention-0006",
      senderId: "fixture-sender-0006",
      text: "Fixture gamma mention",
      eventTimeAsGiven: "2026-10-11T08:00:00Z",
      oversize: false,
      receivedAtUtc: "2026-10-11T08:00:05Z",
      state: "open",
      stateBy: null,
      stateAtUtc: null,
      stateNote: "",
    },
    rawJson: itemJson({ id: "fixture-mention-0006", from: { id: "fixture-sender-0006" } }),
  },
];

/** A row the desktop cannot read: a kind it does not show yet. It stays in the unmatched list, with its raw JSON. */
export const FIXTURE_UNMATCHED_UNREADABLE: UnreadableRow[] = [
  {
    readable: false,
    reason: 'inbox kind "story_reply" is not shown by the desktop yet',
    rawJson: itemJson({ event_id: "inbox-fixture-0008", kind: "story_reply", recipient_id: "fixture-ig-unknown" }),
  },
];

/** The counts that match the rows above. The whole-store totals include the unreadable row. */
export const FIXTURE_INBOX_STATS: InboxStats = {
  totalEvents: 8,
  unmatchedEvents: 3,
  deliveriesParsed: 6,
  deliveriesUnparseable: 1,
  refusedTotal: 2,
  groups: [
    { scopeId: SCOPE_ALPHA, kind: "comment", state: "open", count: 1 },
    { scopeId: SCOPE_ALPHA, kind: "message", state: "handled", count: 1 },
    { scopeId: SCOPE_ALPHA, kind: "mention", state: "ignored", count: 1 },
    { scopeId: SCOPE_ALPHA, kind: "other", state: "open", count: 1 },
    { scopeId: SCOPE_GAMMA, kind: "mention", state: "open", count: 1 },
    { scopeId: null, kind: "comment", state: "open", count: 1 },
    { scopeId: null, kind: "mention", state: "open", count: 1 },
    { scopeId: null, kind: "story_reply", state: "open", count: 1 },
  ],
};
