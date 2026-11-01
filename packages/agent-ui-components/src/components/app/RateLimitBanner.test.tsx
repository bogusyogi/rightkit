import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Account, RateLimitEvent } from "@rightkit/agent-ui-core/lib/accounts/accounts";
import AccountModelPicker from "./AccountModelPicker";
import RateLimitBanner from "./RateLimitBanner";

const acct = (slug: string, label: string, ready = true): Account => ({
  id: `subscription/claude/${slug}`,
  label,
  kind: "subscription",
  provider: "claude",
  email: null,
  organization: null,
  ready,
});
const accounts = [acct("a", "Work"), acct("b", "Personal")];
const event = (over: Partial<RateLimitEvent> = {}): RateLimitEvent => ({
  sessionId: "s1",
  provider: "claude",
  account: "Work",
  accountId: null,
  accountKind: null,
  email: null,
  organization: null,
  status: "rejected",
  rateLimitType: null,
  resetsAt: null,
  utilization: 1,
  ...over,
});

describe("RateLimitBanner", () => {
  it("shows capped text and a manual continue action for other accounts", () => {
    const html = renderToStaticMarkup(
      <RateLimitBanner accounts={accounts} event={event()} onContinueOnAccount={() => {}} />,
    );
    expect(html).toContain("Account Work capped until later");
    expect(html).toContain("Continue on account Personal");
    expect(html).not.toContain("Continue on account Work");
    expect(html).toContain('role="alert"');
  });

  it("names the capped API config by its label and matches by account id", () => {
    const api: Account = {
      id: "api_key/claude/team",
      label: "Team",
      kind: "api_key",
      provider: "claude",
      email: null,
      organization: null,
      ready: true,
    };
    const html = renderToStaticMarkup(
      <RateLimitBanner
        accounts={[...accounts, api]}
        event={event({ account: "something else", accountId: api.id, accountKind: "api_key" })}
        onContinueOnAccount={() => {}}
      />,
    );
    expect(html).toContain("Account Team capped");
    expect(html).not.toContain("Continue on account Team");
    expect(html).toContain("Continue on account Work");
  });

  it("does not offer accounts that are not ready", () => {
    const html = renderToStaticMarkup(
      <RateLimitBanner
        accounts={[...accounts, acct("c", "Spare", false)]}
        event={event()}
        onContinueOnAccount={() => {}}
      />,
    );
    expect(html).toContain("Continue on account Personal");
    expect(html).not.toContain("Continue on account Spare");
  });

  it("renders nothing unless rejected", () => {
    const html = renderToStaticMarkup(
      <RateLimitBanner
        accounts={accounts}
        event={event({ status: "warning", utilization: 0.9 })}
        onContinueOnAccount={() => {}}
      />,
    );
    expect(html).toBe("");
  });
});

describe("AccountModelPicker", () => {
  it("marks the selected account/model pair", () => {
    const html = renderToStaticMarkup(
      <AccountModelPicker
        accounts={accounts}
        models={[{ id: "opus", label: "Opus" }]}
        selectedAccountId="subscription/claude/b"
        selectedModelId="opus"
        onSelect={() => {}}
      />,
    );
    expect(html).toContain('aria-label="Opus on Personal" aria-pressed="true"');
    expect(html).toContain('aria-label="Opus on Work" aria-pressed="false"');
  });
});
