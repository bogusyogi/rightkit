import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ACCOUNT_HOME_MISMATCH,
  AccountsRequestError,
  accountForRateLimit,
  accountHomeConflict,
  addApiKeyAccount,
  buildHandoffSummary,
  describeAuthMethod,
  fetchAccountSelection,
  fetchSessionAuthMethod,
  groupPickerAccounts,
  parseAccountList,
  parseRateLimitEvent,
  parseSessionAuthMethod,
  putSessionAccount,
  removeApiKeyAccount,
} from "./accounts";

const models = [
  { id: "opus", label: "Opus" },
  { id: "sonnet", label: "Sonnet" },
];
const config = { url: "http://127.0.0.1:1/", token: "t" };

afterEach(() => vi.unstubAllGlobals());

describe("account picker grouping", () => {
  it("groups provider > account > models with kind and email", () => {
    const accounts = parseAccountList({
      accounts: [
        {
          id: "subscription/claude/work",
          kind: "subscription",
          provider: "claude",
          label: "Work",
          email: "w@x.com",
          organization: null,
          ready: true,
        },
        {
          id: "api_key/anthropic/key",
          kind: "api_key",
          provider: "anthropic",
          label: "Key",
          email: null,
          organization: "Org",
          ready: true,
        },
        { bad: true },
      ],
    });
    expect(accounts[1].organization).toBe("Org");
    const groups = groupPickerAccounts(accounts, models);
    expect(groups).toHaveLength(1);
    expect(groups[0].providerLabel).toBe("Claude");
    expect(groups[0].accounts.map((a) => a.heading)).toEqual([
      "Work · w@x.com (Subscription)",
      "Key (API key)",
    ]);
    expect(groups[0].accounts[0].models).toEqual(models);
  });
});

describe("typed account identity", () => {
  it("keeps label, kind and email separate for API configs, subscriptions and Codex", () => {
    const accounts = parseAccountList({
      accounts: [
        // Older engines stuffed the label into email for API keys; it must not surface as one.
        {
          id: "api_key/claude/team",
          kind: "api_key",
          provider: "claude",
          label: "Team",
          email: "API key team",
          ready: true,
        },
        { id: "api_key/claude/personal", kind: "api_key", provider: "claude", label: "Personal", ready: true },
        {
          id: "subscription/claude/work",
          kind: "subscription",
          provider: "claude",
          label: "Work",
          email: "w@x.com",
          ready: true,
        },
        { id: "subscription/codex/main", kind: "subscription", provider: "codex", label: "main", ready: true },
        { id: "mystery/claude/x", kind: "other", provider: "claude", label: "X", ready: true },
      ],
    });
    expect(accounts.map((a) => [a.label, a.kind, a.email])).toEqual([
      ["Team", "api_key", null],
      ["Personal", "api_key", null],
      ["Work", "subscription", "w@x.com"],
      ["main", "subscription", null],
    ]);
    const groups = groupPickerAccounts(accounts, models);
    expect(groups.map((g) => g.providerLabel)).toEqual(["Claude", "Codex"]);
    expect(groups[0].accounts.map((a) => a.heading)).toEqual([
      "Team (API key)",
      "Personal (API key)",
      "Work · w@x.com (Subscription)",
    ]);
    expect(groups[1].accounts[0].heading).toBe("main (Subscription)");
  });

  it("matches a rate-limit event by account id before any label", () => {
    const accounts = parseAccountList([
      { id: "api_key/claude/team", kind: "api_key", provider: "claude", label: "Team", ready: true },
      { id: "subscription/claude/team", kind: "subscription", provider: "claude", label: "Team", ready: true },
    ]);
    const event = parseRateLimitEvent({
      provider: "claude",
      account: "Team",
      accountId: "subscription/claude/team",
      status: "rejected",
    })!;
    expect(accountForRateLimit(accounts, event)?.kind).toBe("subscription");
    const apiEvent = parseRateLimitEvent({
      provider: "claude",
      account: "Team",
      accountKind: "api_key",
      email: "Team",
      status: "rejected",
    })!;
    expect(apiEvent.email).toBeNull();
  });
});

describe("codex accounts mirror the claude shape", () => {
  it("lists subscription and API-key Codex accounts under one Codex group and matches events by id", () => {
    const accounts = parseAccountList([
      { id: "subscription/codex/personal", kind: "subscription", provider: "codex", label: "Personal", ready: true },
      { id: "api_key/codex/team", kind: "api_key", provider: "codex", label: "Team", email: "Team", ready: true },
      { id: "subscription/claude/personal", kind: "subscription", provider: "claude", label: "Personal", ready: true },
    ]);
    const groups = groupPickerAccounts(accounts, models);
    const codex = groups.find((g) => g.provider === "codex")!;
    expect(codex.providerLabel).toBe("Codex");
    expect(codex.accounts.map((a) => a.heading)).toEqual([
      "Personal (Subscription)",
      "Team (API key)",
    ]);
    expect(accounts[1].email).toBeNull();
    const event = parseRateLimitEvent({
      provider: "codex",
      account: "Personal",
      accountId: "subscription/codex/personal",
      status: "rejected",
    })!;
    expect(accountForRateLimit(accounts, event)?.id).toBe("subscription/codex/personal");
  });
});

describe("accountRateLimited events", () => {
  const raw = {
    $kind: "accountRateLimited",
    sessionId: "s1",
    provider: "claude",
    account: "w@x.com",
    accountId: "subscription/claude/work",
    accountKind: "subscription",
    email: "w@x.com",
    organization: "Acme",
    status: "rejected",
    rateLimitType: "five_hour",
    resetsAt: 1_800_000_000,
    utilization: 1,
  };

  it("normalizes the engine event (resetsAt is unix seconds)", () => {
    expect(parseRateLimitEvent(raw)).toEqual({
      sessionId: "s1",
      provider: "claude",
      account: "w@x.com",
      accountId: "subscription/claude/work",
      accountKind: "subscription",
      email: "w@x.com",
      organization: "Acme",
      status: "rejected",
      rateLimitType: "five_hour",
      resetsAt: 1_800_000_000_000,
      utilization: 1,
    });
    expect(parseRateLimitEvent({ ...raw, status: "allowed_warning" })?.status).toBe("warning");
    expect(parseRateLimitEvent({})).toBeNull();
  });

  it("matches the event's account label to a listed account", () => {
    const accounts = parseAccountList([
      {
        id: "subscription/claude/work",
        kind: "subscription",
        provider: "claude",
        label: "Work",
        email: "w@x.com",
        ready: true,
      },
      {
        id: "subscription/claude/spare",
        kind: "subscription",
        provider: "claude",
        label: "spare",
        ready: true,
      },
    ]);
    const event = parseRateLimitEvent(raw)!;
    expect(accountForRateLimit(accounts, event)?.id).toBe("subscription/claude/work");
    expect(accountForRateLimit(accounts, { ...event, accountId: null, account: "spare" })?.id).toBe(
      "subscription/claude/spare",
    );
    expect(accountForRateLimit(accounts, { ...event, accountId: null, account: "ghost" })).toBeNull();
  });
});

describe("daemon account routes", () => {
  it("sends the API key once and never returns it", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          account: {
            id: "api_key/openai/team",
            kind: "api_key",
            provider: "openai",
            label: "Team",
            ready: true,
          },
        }),
        { status: 201 },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const account = await addApiKeyAccount(config, {
      label: "Team",
      provider: "openai",
      apiKey: "sk-SECRET",
    });
    expect(JSON.stringify(account)).not.toContain("SECRET");
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://127.0.0.1:1/accounts/api-key");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body)).toEqual({
      label: "Team",
      provider: "openai",
      apiKey: "sk-SECRET",
    });
  });

  it("does not leak the key in a failure message", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockImplementation(
          async () => new Response(JSON.stringify({ error: "bad label" }), { status: 400 }),
        ),
    );
    const error = await addApiKeyAccount(config, {
      label: "",
      provider: "openai",
      apiKey: "sk-SECRET",
    }).catch((e: Error) => e);
    expect((error as Error).message).toContain("bad label");
    expect((error as Error).message).not.toContain("SECRET");
  });

  it("calls the selection, session, and delete routes", async () => {
    const fetchMock = vi.fn().mockImplementation(
      async () =>
        new Response(
          JSON.stringify({
            accountId: "subscription/claude/work",
            source: "session",
            account: null,
            unavailable: false,
          }),
        ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const selection = await fetchAccountSelection(config, { sessionId: "s 1", project: "/p" });
    expect(selection.source).toBe("session");
    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://127.0.0.1:1/accounts/selection?sessionId=s+1&project=%2Fp",
    );
    await putSessionAccount(config, "s1", "subscription/claude/work");
    expect(fetchMock.mock.calls[1][0]).toBe("http://127.0.0.1:1/accounts/session/s1");
    expect(fetchMock.mock.calls[1][1].method).toBe("PUT");
    await removeApiKeyAccount(config, "api_key/openai/team");
    expect(fetchMock.mock.calls[2][0]).toBe("http://127.0.0.1:1/accounts/api-key/openai/team");
    expect(fetchMock.mock.calls[2][1].method).toBe("DELETE");
    await expect(removeApiKeyAccount(config, "subscription/claude/work")).rejects.toThrow();
  });
});

describe("handoff summary", () => {
  it("carries recent turns and the capped account", () => {
    const text = buildHandoffSummary(
      [
        { $kind: "user", content: "fix the bug" },
        { $kind: "system", content: "ignored" },
        { $kind: "assistant", content: "done" },
      ],
      "Work",
    );
    expect(text).toContain("Work");
    expect(text).toContain("User: fix the bug");
    expect(text).toContain("Assistant: done");
    expect(text).not.toContain("ignored");
  });
});

describe("account identity fingerprint (#341)", () => {
  it("keeps the engine fingerprint so two orgs under one email stay distinct", () => {
    const accounts = parseAccountList({
      accounts: [
        { id: "subscription/claude/a", kind: "subscription", provider: "claude", label: "A", email: "x@y.io", fingerprint: "aaaa1111aaaa1111", ready: true },
        { id: "subscription/claude/b", kind: "subscription", provider: "claude", label: "B", email: "x@y.io", fingerprint: "bbbb2222bbbb2222", ready: true },
        { id: "api_key/claude/k", kind: "api_key", provider: "claude", label: "K", ready: true },
      ],
    });
    expect(accounts[0].email).toBe(accounts[1].email);
    expect(accounts[0].fingerprint).not.toBe(accounts[1].fingerprint);
    expect(accounts[2].fingerprint).toBeNull();
  });
});

describe("same-home rule (#343)", () => {
  it("surfaces the engine refusal as a typed conflict, not a silent switch", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(
        async () =>
          new Response(
            JSON.stringify({
              error: "different config home",
              code: ACCOUNT_HOME_MISMATCH,
              action: "new_session",
              currentAccountId: "subscription/claude/work",
              requestedAccountId: "subscription/claude/personal",
            }),
            { status: 409 },
          ),
      ),
    );
    const error = await putSessionAccount(config, "s1", "subscription/claude/personal").catch(
      (e: unknown) => e,
    );
    expect(error).toBeInstanceOf(AccountsRequestError);
    expect(accountHomeConflict(error)).toEqual({
      requestedAccountId: "subscription/claude/personal",
      currentAccountId: "subscription/claude/work",
    });
    expect(accountHomeConflict(new Error("boom"))).toBeNull();
  });

  it("sends threadStarted only when the caller knows it", async () => {
    const fetchMock = vi.fn().mockImplementation(async () => new Response("{}"));
    vi.stubGlobal("fetch", fetchMock);
    await putSessionAccount(config, "s1", "subscription/claude/work");
    await putSessionAccount(config, "s1", "subscription/claude/work", { threadStarted: false });
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
      accountId: "subscription/claude/work",
    });
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({
      accountId: "subscription/claude/work",
      threadStarted: false,
    });
  });
});

describe("active auth method (#347)", () => {
  it("parses the driver status auth block and warns about an env API key", async () => {
    const auth = {
      provider: "claude",
      method: "api_key_env",
      accountId: null,
      label: null,
      strippedEnv: [],
      warnings: ["ANTHROPIC_API_KEY is set in the environment and overrides a subscription login."],
    };
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(async () => new Response(JSON.stringify({ auth }))),
    );
    const parsed = await fetchSessionAuthMethod(config, "s 1");
    expect(parsed?.method).toBe("api_key_env");
    expect(parsed?.warnings[0]).toContain("ANTHROPIC_API_KEY");
    expect(describeAuthMethod(parsed!)).toBe("API key from the environment");
  });

  it("describes a subscription with its label and ignores unknown methods", () => {
    const parsed = parseSessionAuthMethod({
      provider: "claude",
      method: "subscription",
      label: "Work",
      strippedEnv: ["ANTHROPIC_API_KEY", 3],
      warnings: [],
    });
    expect(describeAuthMethod(parsed!)).toBe("Subscription login · Work");
    expect(parsed?.strippedEnv).toEqual(["ANTHROPIC_API_KEY"]);
    expect(parseSessionAuthMethod({ method: "mystery" })).toBeNull();
    expect(parseSessionAuthMethod(null)).toBeNull();
  });
});
