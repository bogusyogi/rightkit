// @vitest-environment happy-dom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import ApiKeyAccounts from "./ApiKeyAccounts";

afterEach(() => vi.unstubAllGlobals());

describe("ApiKeyAccounts", () => {
  it("uses a password field, sends the key once, and clears it", async () => {
    const created = {
      account: {
        id: "api_key/openai/team",
        kind: "api_key",
        provider: "openai",
        label: "Team",
        ready: true,
      },
    };
    const fetchMock = vi.fn().mockImplementation(async (_url: string, init?: RequestInit) =>
      init?.method === "POST"
        ? new Response(JSON.stringify(created), { status: 201 })
        : new Response(JSON.stringify({ accounts: [] })),
    );
    vi.stubGlobal("fetch", fetchMock);
    render(<ApiKeyAccounts config={{ url: "http://127.0.0.1:1", token: "t" }} />);
    const key = screen.getByLabelText("API key") as HTMLInputElement;
    expect(key.type).toBe("password");
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Team" } });
    fireEvent.change(key, { target: { value: "sk-SECRET" } });
    fireEvent.click(screen.getByRole("button", { name: "Add API key" }));
    await waitFor(() => {
      const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
      expect(post).toBeTruthy();
      expect(JSON.parse(post![1].body).apiKey).toBe("sk-SECRET");
    });
    expect(key.value).toBe("");
    expect(document.body.textContent).not.toContain("sk-SECRET");
  });
});
