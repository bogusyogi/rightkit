export type RoomEvent = { seq: number; seat_id: string; phase: string; kind: string; body: string };
export type RoomReceipt = {
  kind: "file_line" | "artifact" | "measurement" | "web" | "review_scope";
  locator: string;
  line?: number | null;
  digest?: string | null;
};
export type RoomFinding = {
  finding_id: string;
  claim: string;
  severity: string;
  status: "Open" | "ExitProposed" | "Recontested" | "Folded" | "Refuted";
  disposition_id?: string | null;
};
export type RoomDisposition = {
  disposition_id: string;
  finding_id: string;
  status: "Proposed" | "Accepted" | "Recontested" | "Ruled";
  receipt_refs: RoomReceipt[];
};
export type RoomSeat = {
  seat_id: string;
  name: string;
  model: string;
  role: string;
  budget: { max_messages: number; max_wakes: number; max_context_bytes: number };
  usage: { sent_messages: number; wakes: number; context_bytes: number };
};
export type RoomSnapshot = {
  room_id: string;
  phase: string;
  sealed: boolean;
  seats: RoomSeat[];
  findings: RoomFinding[];
  dispositions: RoomDisposition[];
  events: RoomEvent[];
  escalation_rate: number;
};

export const FINDING_GROUPS: Array<{ status: RoomFinding["status"]; label: string }> = [
  { status: "Open", label: "Open" },
  { status: "ExitProposed", label: "Exit proposed" },
  { status: "Recontested", label: "Re-contested" },
  { status: "Folded", label: "Folded" },
  { status: "Refuted", label: "Refuted" },
];

export function isTerminalFinding(finding: RoomFinding) {
  return finding.status === "Folded" || finding.status === "Refuted";
}

export function receiptAnchor(receipt: RoomReceipt, index: number) {
  return `room-receipt-${receipt.kind}-${index}-${receipt.locator.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
}

export function isCouncilJoinLink(value: string) {
  try {
    const url = new URL(value);
    return (
      url.protocol === "http:" &&
      url.hostname === "127.0.0.1" &&
      Boolean(url.port) &&
      !url.username &&
      !url.password &&
      !url.search &&
      !url.hash &&
      /^\/join\/[a-zA-Z0-9_-]{1,256}$/.test(url.pathname)
    );
  } catch {
    return false;
  }
}
