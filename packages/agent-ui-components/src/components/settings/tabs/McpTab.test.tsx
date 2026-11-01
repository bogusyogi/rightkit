// @vitest-environment happy-dom
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import McpTab from "./McpTab";
import type { McpServerModeStatus, McpServerRow } from "@rightkit/agent-ui-core/lib/types";
import type { McpDraft } from "../types";

function baseDraft(overrides: Partial<McpDraft> = {}): McpDraft {
  return {
    name: "",
    scope: "workspace",
    type: "stdio",
    trustPreset: "local_process",
    channelAllowlist: "",
    allowMutating: false,
    command: "",
    args: "",
    url: "",
    ...overrides,
  };
}

function makeServer(overrides: Partial<McpServerRow> = {}): McpServerRow {
  return {
    name: "github",
    scope: "workspace",
    type: "stdio",
    config: { type: "stdio", command: "npx", args: ["-y", "@modelcontextprotocol/server-github"] },
    trust: { preset: "local_process", inferred: false },
    channelPermissions: { allowedChannels: ["desktop"], allowMutating: false },
    policyDiagnostics: [],
    ...overrides,
  } as McpServerRow;
}

function baseProps() {
  return {
    servers: [] as McpServerRow[],
    probeResults: {},
    serverModeStatus: null,
    userConfigPath: "/Users/adrian/.coderight/mcp.json",
    workspaceConfigPath: "/repo/.mcp.json",
    draft: baseDraft(),
    pendingId: null,
    error: null,
    workspaceRoot: "/repo",
    onDraft: vi.fn(),
    onSave: vi.fn(),
    onRemove: vi.fn(),
    onProbe: vi.fn(),
    onRefresh: vi.fn(),
  };
}

// PA-07(a): at narrow pane widths the fixed 4-column [1fr 140px 120px 160px]
// template forced the Trust select past the right edge because the first
// column defaulted to `1fr` (== minmax(auto,1fr)) and blew out on the input's
// intrinsic min-content width. The fix stacks 2x2 below @2xl and switches the
// first track to minmax(0,1fr) so the row can actually shrink to its container.
describe("McpTab narrow-width layout (PA-07a)", () => {
  it("keeps the Trust field inside a container-safe, shrinkable grid", () => {
    const { container } = render(<McpTab {...baseProps()} />);
    const fieldGrid = container.querySelector(".grid.grid-cols-2");
    expect(fieldGrid).toBeTruthy();
    expect(fieldGrid?.className).toContain("@2xl:grid-cols-[minmax(0,1fr)_140px_120px_160px]");

    // The Trust field is a shrinkable wrapper around RightKit's Select (class rk-select).
    const trustField = screen.getByText("Trust").parentElement;
    expect(trustField).toBeTruthy();
    expect(trustField?.className).toContain("min-w-0");

    const trustSelect = screen.getByDisplayValue("Local process");
    expect(trustSelect.className).toContain("rk-select");
  });

  it("marks the outer section as a container so the responsive grid tracks its real width", () => {
    const { container } = render(<McpTab {...baseProps()} />);
    const section = container.querySelector("section");
    expect(section?.className).toContain("@container");
  });

  it("exposes the full target path via title when the path text truncates", () => {
    const longPath = "/Users/adrian/very/deeply/nested/workspace/path/.mcp.json";
    render(<McpTab {...baseProps()} workspaceConfigPath={longPath} />);
    const path = screen.getByText(longPath);
    expect(path.getAttribute("title")).toBe(longPath);
    expect(path.className).toContain("truncate");
  });

  it("exposes full trust and channel detail via title on the server row", () => {
    const server = makeServer({
      trust: { preset: "remote_unreviewed", inferred: true },
      channelPermissions: { allowedChannels: ["desktop", "cli", "telegram"], allowMutating: true },
    });
    render(<McpTab {...baseProps()} servers={[server]} />);

    const trustLine = screen.getByText(/trust: remote unreviewed inferred/);
    expect(trustLine.getAttribute("title")).toBe("trust: remote unreviewed inferred");

    const channelsLine = screen.getByText(/channels:/);
    expect(channelsLine.getAttribute("title")).toContain("desktop, cli, telegram");
    expect(channelsLine.getAttribute("title")).toContain("mutating allowed");
  });
});

function baseServerModeStatus(overrides: Partial<McpServerModeStatus> = {}): McpServerModeStatus {
  return {
    command: "mcp",
    readOnlyCommand: "mcp --read-only",
    mutatingCommand: "mcp --allow-mutating",
    defaultMode: "read_only",
    mutatingRequiresOptIn: true,
    readOnlyToolCount: 0,
    mutatingToolCount: 2,
    totalToolCount: 2,
    readOnlyTools: [],
    mutatingTools: ["write_file", "run_command"],
    ...overrides,
  };
}

describe("McpTab status card tone (PA-06a)", () => {
  it("renders a warn server-mode card in amber, not the command-accent red", () => {
    const { container } = render(
      <McpTab {...baseProps()} serverModeStatus={baseServerModeStatus({ readOnlyToolCount: 0 })} />,
    );
    expect(screen.getByText("Read-only tools")).toBeTruthy();
    expect(container.innerHTML).toContain("border-warning/40 bg-warning/10");
    expect(container.innerHTML).not.toContain("border-accent/40 bg-accent/10");
  });
});

describe("McpTab server-type badge tone (PA-06b)", () => {
  it("renders the transport-type tag in a neutral tone, not success green", () => {
    const { container } = render(<McpTab {...baseProps()} servers={[makeServer()]} />);
    const typeTag = screen.getByText("stdio", { selector: "code" });
    expect(typeTag.className).toContain("text-text-3");
    expect(typeTag.className).not.toContain("text-accent-2");
    expect(container.innerHTML).toContain("stdio");
  });
});
