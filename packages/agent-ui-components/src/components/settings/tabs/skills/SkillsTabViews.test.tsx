// @vitest-environment happy-dom
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SkillsTabHeader } from "./SkillsTabViews";

function baseProps() {
  return {
    skillDirs: ["/repo/.claude/skills"],
    skillCount: 4,
    proposalCount: 1,
    trainingRunCount: 0,
    view: "installed" as const,
    onChangeView: vi.fn(),
    onRefresh: vi.fn(),
  };
}

// PA-07(b): at narrow pane widths the header packed the title row, the
// Installed/Proposals/Training tab segment, and Refresh into one non-wrapping
// flex row. The tab segment collided into the "N installed" pill and the
// browser truncated it mid-word ("insta…"). Fix: flex-wrap on the header so
// the action groups drop to their own line instead of colliding, shrink-0 +
// whitespace-nowrap on those groups so they render intact, and the title
// truncates instead of the pill.
describe("SkillsTabHeader narrow-width layout (PA-07b)", () => {
  it("wraps instead of colliding when the row can't fit", () => {
    const { container } = render(<SkillsTabHeader {...baseProps()} />);
    const header = container.firstElementChild as HTMLElement;
    expect(header.className).toContain("flex-wrap");
  });

  it("never lets the installed-count pill truncate mid-word", () => {
    render(<SkillsTabHeader {...baseProps()} skillCount={4} />);
    const pill = screen.getByTitle("4 installed");
    expect(pill.textContent).toBe("4 installed");
    expect(pill.className).toContain("shrink-0");
    expect(pill.className).not.toContain("truncate");
  });

  it("keeps the tab segment and Refresh button intact (shrink-0, no-wrap) so they can only wrap as whole units", () => {
    render(<SkillsTabHeader {...baseProps()} />);
    const installedTab = screen.getByRole("button", { name: /Installed/ });
    expect(installedTab.className).toContain("shrink-0");
    expect(installedTab.className).toContain("whitespace-nowrap");

    const refresh = screen.getByRole("button", { name: /Refresh/ });
    expect(refresh.className).toContain("shrink-0");
    expect(refresh.className).toContain("whitespace-nowrap");
  });

  it("truncates the long title instead of the pill when space is tight", () => {
    render(<SkillsTabHeader {...baseProps()} />);
    const title = screen.getByText("Composer skill library");
    expect(title.className).toContain("truncate");
    expect(title.className).toContain("min-w-0");
  });
});
