import { describe, expect, it } from "vitest";
import {
  FINDING_GROUPS,
  isCouncilJoinLink,
  isTerminalFinding,
  receiptAnchor,
  type RoomFinding,
} from "./reviewRoom.js";

describe("Review Room protocol projection", () => {
  it("uses the frozen five-state finding vocabulary", () => {
    expect(FINDING_GROUPS.map((group) => group.status)).toEqual([
      "Open",
      "ExitProposed",
      "Recontested",
      "Folded",
      "Refuted",
    ]);
  });

  it("never renders an open or proposed finding as terminal", () => {
    const finding = (status: RoomFinding["status"]): RoomFinding => ({
      finding_id: "f1",
      claim: "claim",
      severity: "medium",
      status,
    });
    expect(isTerminalFinding(finding("Open"))).toBe(false);
    expect(isTerminalFinding(finding("ExitProposed"))).toBe(false);
    expect(isTerminalFinding(finding("Recontested"))).toBe(false);
    expect(isTerminalFinding(finding("Folded"))).toBe(true);
    expect(isTerminalFinding(finding("Refuted"))).toBe(true);
  });

  it("creates stable local receipt anchors without exposing a file URI", () => {
    const anchor = receiptAnchor({ kind: "artifact", locator: "docs/plan.md", digest: "abc" }, 0);
    expect(anchor).toBe("room-receipt-artifact-0-docs-plan-md");
    expect(anchor).not.toContain("file:");
  });

  it("accepts only the private loopback join-link shape", () => {
    expect(isCouncilJoinLink("http://127.0.0.1:43119/join/private-capability")).toBe(true);
    expect(isCouncilJoinLink("http://127.0.0.1:43119/pair")).toBe(false);
    expect(isCouncilJoinLink("http://example.com:43119/join/private-capability")).toBe(false);
    expect(isCouncilJoinLink("http://127.0.0.1:43119/join/private-capability?copy=true")).toBe(
      false,
    );
  });
});
