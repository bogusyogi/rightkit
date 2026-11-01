import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import ObservabilityRuntimePanel from "./ObservabilityRuntimePanel.js";
import type { MiddlewareRegistrySnapshot, RuntimeObservation, RuntimeSpan } from "@rightkit/agent-ui-core/lib/types";

const emptyMiddleware: MiddlewareRegistrySnapshot = {
  middleware: [],
  spans: [],
  observations: [],
};

describe("ObservabilityRuntimePanel", () => {
  it("shows the empty state when there are no spans, middleware, or observations", () => {
    const html = renderToStaticMarkup(
      <ObservabilityRuntimePanel middleware={emptyMiddleware} spans={[]} observations={[]} />,
    );

    expect(html).toContain("No spans yet");
    expect(html).toContain("No middleware or observations");
  });

  it("renders each span with its observation count and status", () => {
    const spans: RuntimeSpan[] = [
      {
        id: "span-1",
        sessionId: "s1",
        phase: "preToolCall",
        status: "ok",
        startedAt: "2026-07-01T00:00:00Z",
        attributes: { tool: "shell.run" },
      },
    ];
    const observations: RuntimeObservation[] = [
      {
        id: "obs-1",
        spanId: "span-1",
        sessionId: "s1",
        severity: "info",
        message: "middleware allowed the call",
        payload: {},
        createdAt: "2026-07-01T00:00:01Z",
      },
    ];

    const html = renderToStaticMarkup(
      <ObservabilityRuntimePanel
        middleware={emptyMiddleware}
        spans={spans}
        observations={observations}
      />,
    );

    expect(html).toContain("root span");
    expect(html).toContain("session s1");
    expect(html).toContain("1 observation");
    expect(html).toContain("middleware allowed the call");
  });

  it("shows middleware definitions with their enabled state", () => {
    const middleware: MiddlewareRegistrySnapshot = {
      middleware: [
        {
          id: "mw-1",
          title: "Sandbox guard",
          source: "builtIn",
          phase: "preToolCall",
          effect: "deny",
          tool: "shell.run",
          enabled: true,
          order: 0,
          description: "Blocks shell commands outside the sandbox.",
        },
      ],
      spans: [],
      observations: [],
    };

    const html = renderToStaticMarkup(
      <ObservabilityRuntimePanel middleware={middleware} spans={[]} observations={[]} />,
    );

    expect(html).toContain("Sandbox guard");
    expect(html).toContain("enabled");
  });
});
