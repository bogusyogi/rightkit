// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Account, SessionAuthMethod } from "@rightkit/agent-ui-core/lib/accounts/accounts";
import {
  AccountSwitchNotice,
  AuthMethodLine,
  SessionAuthMethodLine,
  SUBSCRIPTION_NOTICE_STORAGE_KEY,
  SubscriptionUsageNotice,
} from "./AccountNotices.js";

const account = (slug: string, label: string, kind: Account["kind"] = "subscription"): Account => ({
  id: `${kind}/claude/${slug}`,
  kind,
  provider: "claude",
  label,
  email: null,
  organization: null,
  ready: true,
});

beforeEach(() => {
  try {
    localStorage.clear();
  } catch {
    /* storage unavailable */
  }
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("SubscriptionUsageNotice (#348)", () => {
  it("renders once for a subscription account and stays dismissed", () => {
    const accounts = [account("work", "Work")];
    const first = render(<SubscriptionUsageNotice accounts={accounts} />);
    expect(screen.getByTestId("subscription-usage-notice").textContent).toContain("plan limits");
    fireEvent.click(screen.getByRole("button", { name: "Got it" }));
    expect(screen.queryByTestId("subscription-usage-notice")).toBeNull();
    expect(localStorage.getItem(SUBSCRIPTION_NOTICE_STORAGE_KEY)).toBe("1");
    first.unmount();
    render(<SubscriptionUsageNotice accounts={accounts} />);
    expect(screen.queryByTestId("subscription-usage-notice")).toBeNull();
  });

  it("does not show without a subscription account", () => {
    render(<SubscriptionUsageNotice accounts={[account("team", "Team", "api_key")]} />);
    expect(screen.queryByTestId("subscription-usage-notice")).toBeNull();
  });
});

describe("AuthMethodLine (#347, #344)", () => {
  const base: SessionAuthMethod = {
    provider: "claude",
    method: "ambient_login",
    accountId: null,
    label: null,
    strippedEnv: [],
    warnings: [],
  };

  it("shows the active method and the env override warning", () => {
    render(
      <AuthMethodLine
        auth={{
          ...base,
          method: "api_key_env",
          warnings: ["ANTHROPIC_API_KEY is set in the environment and overrides a subscription login."],
        }}
      />,
    );
    expect(screen.getByTestId("auth-method-line").textContent).toContain(
      "Auth: API key from the environment",
    );
    expect(screen.getByTestId("auth-warning").textContent).toContain("ANTHROPIC_API_KEY");
  });

  it("flags credential variables stripped under a managed profile without warning", () => {
    render(
      <AuthMethodLine
        auth={{
          ...base,
          method: "subscription",
          label: "Work",
          strippedEnv: ["ANTHROPIC_API_KEY"],
        }}
      />,
    );
    expect(screen.getByTestId("auth-method-line").textContent).toContain(
      "Subscription login · Work",
    );
    expect(screen.getByTestId("auth-env-stripped").textContent).toContain("ANTHROPIC_API_KEY");
    expect(screen.queryByTestId("auth-warning")).toBeNull();
  });

  it("loads the auth block for a Claude session", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(
        async () =>
          new Response(
            JSON.stringify({
              auth: { ...base, method: "api_key_env", warnings: ["ANTHROPIC_API_KEY overrides"] },
            }),
          ),
      ),
    );
    render(<SessionAuthMethodLine config={{ url: "http://127.0.0.1:1", token: "t" }} sessionId="s1" />);
    await waitFor(() => expect(screen.getByTestId("auth-warning")).toBeTruthy());
  });
});

describe("AccountSwitchNotice (#343)", () => {
  it("offers a new session on the chosen account and never switches by itself", () => {
    const onStart = vi.fn();
    const onDismiss = vi.fn();
    render(
      <AccountSwitchNotice
        conflict={{
          requestedAccountId: "subscription/claude/personal",
          currentAccountId: "subscription/claude/work",
        }}
        accounts={[account("work", "Work"), account("personal", "Personal")]}
        onStartNewSession={onStart}
        onDismiss={onDismiss}
      />,
    );
    expect(screen.getByTestId("account-switch-notice").textContent).toContain(
      "different config home",
    );
    fireEvent.click(screen.getByRole("button", { name: "Start a new session with Personal" }));
    expect(onStart).toHaveBeenCalledWith("subscription/claude/personal");
    fireEvent.click(screen.getByRole("button", { name: "Keep this account" }));
    expect(onDismiss).toHaveBeenCalled();
  });

  it("renders nothing without a conflict", () => {
    render(
      <AccountSwitchNotice conflict={null} accounts={[]} onStartNewSession={vi.fn()} onDismiss={vi.fn()} />,
    );
    expect(screen.queryByTestId("account-switch-notice")).toBeNull();
  });
});
