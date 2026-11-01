import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AttachmentStagingStrip } from "./AttachmentStagingStrip";
import type { StagedAttachment } from "@rightkit/agent-ui-hooks/hooks/useAttachmentStaging";

const image: StagedAttachment = {
  id: "a1",
  name: "screen.png",
  size: 2048,
  mimeType: "image/png",
  kind: "image",
  state: "ready",
  origin: "local",
  previewUrl: "data:image/png;base64,AA==",
};

describe("AttachmentStagingStrip", () => {
  it("renders metadata, preview, and a removable lost attachment", () => {
    const onRemove = vi.fn();
    render(<AttachmentStagingStrip attachments={[image, { ...image, id: "a2", name: "lost.png", state: "lost", error: "re-attach to resend" }]} onRemove={onRemove} />);
    expect(screen.getByTestId("attachment-staging-strip")).toBeTruthy();
    expect(screen.getByAltText("")).toBeTruthy();
    expect(screen.getByText("re-attach to resend")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Remove lost.png" })).toBeTruthy();
  });

  it("renders nothing for an empty staging list", () => {
    const { container } = render(<AttachmentStagingStrip attachments={[]} />);
    expect(container.firstChild).toBeNull();
  });
});
