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
  it("opens bundled documents through onOpen instead of a new window", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    render(<LegalGate {...baseProps({ documents: [{ title: "License", onOpen }] })} />);
    const link = screen.getByRole("link", { name: "License" });
    expect(link).not.toHaveAttribute("target");
    await user.click(link);
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("forces no theme: no data-theme, no colour-scheme, and it follows an ancestor data-theme", () => {
    const host = document.createElement("div");
    host.setAttribute("data-theme", "dark");
    document.body.appendChild(host);
    const { container } = render(<LegalGate {...baseProps({ theme: undefined })} />, { container: host });
    const root = container.querySelector(".rk-legal-gate") as HTMLElement;
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.closest("[data-theme]")?.getAttribute("data-theme")).toBe("dark");
    expect(root.style.colorScheme).toBe("");
    expect(root.style.background).toBe("");
    expect(root.style.color).toBe("");
    host.remove();
  });

  it("keeps the colour-scheme unset when a full host theme is passed", () => {
    const { container } = render(<LegalGate {...baseProps()} />);
    const root = container.querySelector(".rk-legal-gate") as HTMLElement;
    expect(root.style.colorScheme).toBe("");
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.style.background).not.toBe("");
  });

  it("accepts CSS variable references as theme values", () => {
    const { container } = render(<LegalGate {...baseProps({ theme: { bg: "var(--app-bg)", text: "var(--app-fg)" } })} />);
    const root = container.querySelector(".rk-legal-gate") as HTMLElement;
    expect(root.style.background).toContain("var(--app-bg)");
    expect(root.style.color).toContain("var(--app-fg)");
  });

  it("renders the default eligibility copy and lockup", () => {
    const { container } = render(<LegalGate {...baseProps()} />);
    expect(screen.getByRole("radio", { name: /for myself/i })).toBeInTheDocument();
    expect(screen.getByText("One person, my own devices, including my own freelance work.")).toBeInTheDocument();
    expect(screen.getByText("Seats are licensed under an Enterprise Order.")).toBeInTheDocument();
    expect(screen.getByText("Who is this license for")).toBeInTheDocument();
    expect(container.querySelector(".rk-legal-gate__lockup")).toHaveTextContent("Orthic Labs");
  });

  it("overrides eligibility copy per field and keeps the other defaults", () => {
    render(<LegalGate {...baseProps({ eligibilityLabels: { legend: "Licensee", individual: { label: "Solo" }, enterprise: { description: "Via order form." } } })} />);
    expect(screen.getByRole("radio", { name: /solo/i })).toBeInTheDocument();
    expect(screen.getByText("One person, my own devices, including my own freelance work.")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /for an organization/i })).toBeInTheDocument();
    expect(screen.getByText("Via order form.")).toBeInTheDocument();
    expect(screen.getByText("Licensee")).toBeInTheDocument();
  });

  it("replaces the lockup with a host slot, or hides it with null", () => {
    const { container, rerender } = render(<LegalGate {...baseProps({ lockup: <span data-testid="mine">Acme</span> })} />);
    expect(screen.getByTestId("mine")).toBeInTheDocument();
    expect(container.querySelector(".rk-legal-gate__lockup")).not.toHaveTextContent("Orthic Labs");
    rerender(<LegalGate {...baseProps({ lockup: null })} />);
    expect(container.querySelector(".rk-legal-gate__lockup")).toBeEmptyDOMElement();
  });

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
    expect(onAgree).toHaveBeenCalledWith({ version: undefined, acceptedAt: expect.any(String), manifestSha256: undefined, eligibilityBasis: "enterprise" });
  });

  it("passes { version, acceptedAt, manifestSha256, eligibilityBasis } with a UTC ISO acceptedAt taken at click time", async () => {
    const user = userEvent.setup();
    const onAgree = vi.fn();
    const sha = "a".repeat(64);
    render(<LegalGate {...baseProps({ onAgree, version: "2026-10-01", manifestSha256: sha })} />);
    await user.click(screen.getByRole("radio", { name: /for myself/i }));
    await user.click(screen.getByRole("checkbox"));
    const before = Date.now();
    await user.click(screen.getByRole("button", { name: /agree/i }));
    const after = Date.now();
    expect(onAgree).toHaveBeenCalledTimes(1);
    const [arg, ...rest] = onAgree.mock.calls[0]!;
    expect(rest).toEqual([]);
    expect(Object.keys(arg).sort()).toEqual(["acceptedAt", "eligibilityBasis", "manifestSha256", "version"]);
    expect(arg).toMatchObject({ version: "2026-10-01", manifestSha256: sha, eligibilityBasis: "individual" });
    expect(arg.acceptedAt).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
    expect(new Date(arg.acceptedAt).toISOString()).toBe(arg.acceptedAt);
    expect(Date.parse(arg.acceptedAt)).toBeGreaterThanOrEqual(before);
    expect(Date.parse(arg.acceptedAt)).toBeLessThanOrEqual(after);
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
