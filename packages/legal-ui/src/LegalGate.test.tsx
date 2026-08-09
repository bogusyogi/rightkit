import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LegalGate, type LegalGateProps, type LegalGateTheme } from "./LegalGate";

afterEach(() => {
  cleanup();
});

const theme: LegalGateTheme = {
  bg: "#F7F3EC",
  surface: "#FFFFFF",
  border: "#E4DCDA",
  text: "#272322",
  muted: "#686160",
  accent: "#FF5630",
  accentContrast: "#FFFFFF",
  fontDisplay: "Tanker, sans-serif",
  fontBody: "'Hanken Grotesk', sans-serif",
};

function baseProps(overrides: Partial<LegalGateProps> = {}): LegalGateProps {
  return {
    theme,
    appName: "HeardRight",
    trustLine: "HeardRight transcribes and listens for its wake word on your device.",
    documents: [
      { title: "License", url: "https://heardright.app/legal/license/" },
      { title: "End User License Agreement", url: "https://heardright.app/legal/eula/" },
    ],
    agreementStatement:
      "I have read and agree to the HeardRight License, End User License Agreement, Acceptable Use Policy, and Product Schedule.",
    variant: "first-run",
    onAgree: vi.fn(),
    onQuit: vi.fn(),
    ...overrides,
  };
}

describe("LegalGate", () => {
  it("keeps Agree disabled until a basis is chosen and the checkbox is ticked", async () => {
    const user = userEvent.setup();
    render(<LegalGate {...baseProps()} />);

    const agree = screen.getByRole("button", { name: /agree/i });
    expect(agree).toBeDisabled();

    await user.click(screen.getByRole("radio", { name: /for myself/i }));
    expect(agree).toBeDisabled();

    await user.click(screen.getByRole("checkbox"));
    expect(agree).toBeEnabled();
  });

  it("does not enable Agree from the checkbox alone", async () => {
    const user = userEvent.setup();
    render(<LegalGate {...baseProps()} />);

    await user.click(screen.getByRole("checkbox"));
    expect(screen.getByRole("button", { name: /agree/i })).toBeDisabled();
  });

  it("renders the checkbox unticked by default", () => {
    render(<LegalGate {...baseProps()} />);
    expect(screen.getByRole("checkbox")).not.toBeChecked();
  });

  it("passes the selected eligibility basis to onAgree", async () => {
    const user = userEvent.setup();
    const onAgree = vi.fn();
    render(<LegalGate {...baseProps({ onAgree })} />);

    await user.click(screen.getByRole("radio", { name: /for an organization/i }));
    await user.click(screen.getByRole("checkbox"));
    await user.click(screen.getByRole("button", { name: /agree/i }));

    expect(onAgree).toHaveBeenCalledTimes(1);
    expect(onAgree).toHaveBeenCalledWith({ eligibilityBasis: "enterprise" });
  });

  it("fires onQuit when Quit is clicked, independent of agreement state", async () => {
    const user = userEvent.setup();
    const onQuit = vi.fn();
    const onAgree = vi.fn();
    render(<LegalGate {...baseProps({ onQuit, onAgree })} />);

    await user.click(screen.getByRole("button", { name: /quit/i }));
    expect(onQuit).toHaveBeenCalledTimes(1);
    expect(onAgree).not.toHaveBeenCalled();
  });

  it("renders the re-acceptance headline instead of onboarding framing", () => {
    render(<LegalGate {...baseProps({ variant: "re-acceptance" })} />);
    expect(screen.getByText(/terms below have changed/i)).toBeInTheDocument();
  });

  it("does not render the re-acceptance statement on first-run", () => {
    render(<LegalGate {...baseProps({ variant: "first-run" })} />);
    expect(screen.queryByText(/terms below have changed/i)).not.toBeInTheDocument();
  });

  const ACCENT_RGB = "rgb(255, 86, 48)"; // theme.accent #FF5630, as jsdom normalizes it

  it("uses theme.accent as the Agree button's background", () => {
    render(<LegalGate {...baseProps()} />);
    const agree = screen.getByRole("button", { name: /agree/i });
    expect(agree.style.background).toBe(ACCENT_RGB);
  });

  it("colors only the wordmark suffix and the Agree button with the accent", () => {
    render(<LegalGate {...baseProps()} />);
    const heading = screen.getByRole("heading", { level: 1 });
    const accentSpan = heading.querySelector("span");
    expect(accentSpan?.textContent).toBe("Right");
    expect(accentSpan?.style.color).toBe(ACCENT_RGB);
  });

  it("associates radio and checkbox labels via real <label for>", () => {
    render(<LegalGate {...baseProps()} />);
    const individual = screen.getByRole("radio", { name: /for myself/i });
    const enterprise = screen.getByRole("radio", { name: /for an organization/i });
    const checkbox = screen.getByRole("checkbox");
    expect(individual.id).toBeTruthy();
    expect(enterprise.id).toBeTruthy();
    expect(checkbox.id).toBeTruthy();
  });

  it("describes the form via aria-describedby pointing at the trust line", () => {
    render(<LegalGate {...baseProps()} />);
    const form = screen.getByText(/transcribes and listens/i).closest("form");
    const describedBy = form?.getAttribute("aria-describedby");
    expect(describedBy).toBeTruthy();
    expect(document.getElementById(describedBy!)?.textContent).toMatch(/transcribes and listens/i);
  });

  it("centers ONLY the Orthic Labs lockup; the app wordmark stays left-aligned", () => {
    const { container } = render(<LegalGate {...baseProps()} />);
    // The lockup is a centered flex row (mark left, wordmark right) and lives
    // outside the form as page chrome — the publisher is not form content.
    const lockup = container.querySelector<HTMLElement>(".rk-legal-gate__lockup");
    expect(lockup).toBeTruthy();
    expect(lockup!.style.justifyContent).toBe("center");
    expect(lockup!.style.alignItems).toBe("center");
    expect(lockup!.closest("form")).toBeNull();

    const wordmark = screen.getByRole("heading", { level: 1 });
    expect(wordmark.style.textAlign).toBe("left");
    expect(lockup!.contains(wordmark)).toBe(false);
  });

  it("renders an override logo in the lockup when parentLogoSrc is given", () => {
    const { container } = render(<LegalGate {...baseProps({ parentLogoSrc: "/orthic-mark.svg" })} />);
    const img = container.querySelector(".rk-legal-gate__lockup img");
    expect(img).toBeTruthy();
    expect(img!.getAttribute("src")).toBe("/orthic-mark.svg");
    expect(img!.getAttribute("aria-hidden")).toBe("true");
  });

  it("inlines the Orthic mark when parentLogoSrc is absent", () => {
    const { container } = render(<LegalGate {...baseProps()} />);
    expect(container.querySelector(".rk-legal-gate__lockup img")).toBeNull();
    // The mark is inlined, not absent — a bare "ORTHIC LABS" with no mark is a defect.
    expect(container.querySelector(".rk-legal-gate__lockup svg")).toBeTruthy();
  });

  it("registers a Tanker @font-face when wordmarkFontUrl is provided", () => {
    render(<LegalGate {...baseProps({ wordmarkFontUrl: "/fonts/Tanker-400.woff2" })} />);
    const style = document.head.querySelector('style[data-rightkit-legal-gate-font="/fonts/Tanker-400.woff2"]');
    expect(style).toBeTruthy();
    expect(style!.textContent).toContain('font-family: "Tanker"');
    expect(style!.textContent).toContain("/fonts/Tanker-400.woff2");
  });
  it("shows pending state", async () => {
    const { act, waitFor } = await import("@testing-library/react");
    const user = userEvent.setup();
    let resolveAccept!: () => void;
    const onAgree = vi.fn(() => new Promise<void>((resolve) => { resolveAccept = resolve; }));
    render(<LegalGate {...baseProps({ onAgree })} />);
    await user.click(screen.getByRole("radio", { name: /for myself/i }));
    await user.click(screen.getByRole("checkbox"));
    await user.click(screen.getByRole("button", { name: /agree/i }));
    const submitting = screen.getByRole("button", { name: "Saving…" });
    expect(submitting).toBeDisabled();
    expect(submitting).toHaveAttribute("aria-busy", "true");
    await act(async () => resolveAccept());
    await waitFor(() => expect(screen.getByRole("button", { name: /agree/i })).toBeEnabled());
  });

  it("shows rejection & permits retry", async () => {
    const user = userEvent.setup();
    const onAgree = vi.fn().mockRejectedValueOnce(new Error("network down"));
    render(<LegalGate {...baseProps({ onAgree })} />);
    await user.click(screen.getByRole("radio", { name: /for myself/i }));
    await user.click(screen.getByRole("checkbox"));
    const agree = screen.getByRole("button", { name: /agree/i });
    await user.click(agree);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Couldn’t save your acceptance. Try again.");
    onAgree.mockResolvedValueOnce(undefined);
    await user.click(screen.getByRole("button", { name: /agree/i }));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(onAgree).toHaveBeenCalledTimes(2);
  });

  it("contains synchronous throw", async () => {
    const user = userEvent.setup();
    const onAgree = vi.fn(() => {
      throw new Error("boom");
    });
    render(<LegalGate {...baseProps({ onAgree })} />);

    await user.click(screen.getByRole("radio", { name: /for myself/i }));
    await user.click(screen.getByRole("checkbox"));
    await user.click(screen.getByRole("button", { name: /agree/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Couldn’t save your acceptance. Try again.");
    expect(screen.getByRole("button", { name: /agree/i })).toBeEnabled();
  });
});
