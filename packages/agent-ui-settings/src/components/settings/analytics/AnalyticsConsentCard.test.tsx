import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import AnalyticsConsentCard from "./AnalyticsConsentCard";
import { AnalyticsProjection, ANALYTICS_STORAGE_KEY } from "@rightkit/agent-ui-core/lib/analytics/analyticsProjection";

describe("AnalyticsConsentCard", () => {
  it("renders the privacy contract and starts enabled", () => {
    render(<AnalyticsConsentCard />);
    expect(screen.getByText("Share anonymous usage data")).toBeTruthy();
    expect(screen.getByText(/Prompts, project names and paths/)).toBeTruthy();
    expect(screen.getByRole("checkbox")).toBeChecked();
  });

  it("persists withdrawal and updates the injected analytics handle", () => {
    const setEnabled = vi.fn();
    const projection = new AnalyticsProjection({ track: vi.fn(), setEnabled }, true);
    const onChange = vi.fn();
    render(<AnalyticsConsentCard projection={projection} onChange={onChange} />);

    fireEvent.click(screen.getByRole("checkbox"));

    expect(screen.getByRole("checkbox")).not.toBeChecked();
    expect(projection.enabled).toBe(false);
    expect(setEnabled).toHaveBeenLastCalledWith(false);
    expect(onChange).toHaveBeenCalledWith(false);
    expect(localStorage.getItem(ANALYTICS_STORAGE_KEY)).toBe("0");
  });

  it("supports a persisted opt-in choice without a transport", () => {
    localStorage.setItem(ANALYTICS_STORAGE_KEY, "0");
    render(<AnalyticsConsentCard />);
    expect(screen.getByRole("checkbox")).not.toBeChecked();
  });
});
