import { describe, expect, it } from "vitest";

import { routeTargetFromToken, sessionContextDrift } from "./sessionContextDrift";

const SESS_A = "sess-a";
const SESS_B = "sess-b";

// the host app route token carries path, search, and hash in separate segments.
const sessionPath = (id: string) => `/session/${encodeURIComponent(id)}`;
const routeToken = (pathname: string, search = "", hash = "") => `${pathname}:${search}:${hash}`;

const sessionArgs = (overrides: Partial<Parameters<typeof sessionContextDrift>[0]> = {}) => ({
  startRouteToken: routeToken(sessionPath(SESS_A)),
  nowRouteToken: routeToken(sessionPath(SESS_A)),
  startSelectedStoredId: SESS_A,
  nowSelectedStoredId: SESS_A,
  submitTargetStoredId: SESS_A,
  ...overrides,
});

describe("routeTargetFromToken", () => {
  it("maps session, settings, and new-chat paths", () => {
    expect(routeTargetFromToken(routeToken(sessionPath(SESS_A)))).toBe(SESS_A);
    expect(routeTargetFromToken(routeToken("/settings"))).toBeNull();
    expect(routeTargetFromToken(routeToken("/new-chat"))).toBe("__new__");
  });

  it("ignores search and hash because only the pathname selects the chat", () => {
    expect(routeTargetFromToken(routeToken(sessionPath(SESS_A), "?pane=runtime", "#reply"))).toBe(SESS_A);
  });

  it("accepts a bare pathname and decodes session ids", () => {
    expect(routeTargetFromToken(sessionPath("session:a"))).toBe("session:a");
    expect(routeTargetFromToken("/sessions/sess-b")).toBe(SESS_B);
  });

  it("fails closed for malformed session URL encoding", () => {
    expect(routeTargetFromToken("/session/%E0%A4%A")).toBeNull();
  });
});

describe("sessionContextDrift", () => {
  it("does not drift on search/hash-only route churn", () => {
    expect(
      sessionContextDrift(
        sessionArgs({ nowRouteToken: routeToken(sessionPath(SESS_A), "?pane=runtime", "#reply") }),
      ),
    ).toBeNull();
  });

  it("does not drift on a selection null-reset during reconnect", () => {
    expect(sessionContextDrift(sessionArgs({ nowSelectedStoredId: null }))).toBeNull();
  });

  it("drifts when selection moves to another non-null stored session", () => {
    expect(sessionContextDrift(sessionArgs({ nowSelectedStoredId: SESS_B }))).toBe(
      "selection:sess-a->sess-b",
    );
  });

  it("drifts when the routed session changes", () => {
    expect(
      sessionContextDrift(sessionArgs({ nowRouteToken: routeToken(sessionPath(SESS_B)) })),
    ).toBe("route:sess-a->sess-b");
  });

  it("drifts when an existing chat moves to new-chat mid-submit", () => {
    expect(
      sessionContextDrift(sessionArgs({ nowRouteToken: routeToken("/new-chat") })),
    ).toBe("route:sess-a->__new__");
  });

  it("does not drift when the route moves to a non-chat path", () => {
    expect(sessionContextDrift(sessionArgs({ nowRouteToken: routeToken("/settings") }))).toBeNull();
  });

  it("does not drift when create/resume re-homes route and selection to its target", () => {
    expect(
      sessionContextDrift(
        sessionArgs({
          startRouteToken: routeToken("/new-chat"),
          nowRouteToken: routeToken(sessionPath(SESS_A)),
          startSelectedStoredId: null,
          nowSelectedStoredId: SESS_A,
        }),
      ),
    ).toBeNull();
  });

  it("drifts when a targetless new-chat draft switches to an existing chat", () => {
    expect(
      sessionContextDrift(
        sessionArgs({
          startRouteToken: routeToken("/new-chat"),
          nowRouteToken: routeToken(sessionPath(SESS_B)),
          startSelectedStoredId: null,
          nowSelectedStoredId: SESS_B,
          submitTargetStoredId: null,
        }),
      ),
    ).toBe("route:__new__->sess-b");
  });

  it("omits the composer prong for non-composer submits", () => {
    expect(sessionContextDrift(sessionArgs())).toBeNull();
  });

  it("accepts a composer scope matching the resolved lineage target", () => {
    expect(
      sessionContextDrift(
        sessionArgs({ composerScope: SESS_A, submitTargetComposerScope: SESS_A }),
      ),
    ).toBeNull();
  });

  it("reports composer drift before route or selection drift", () => {
    expect(
      sessionContextDrift(
        sessionArgs({
          nowRouteToken: routeToken(sessionPath(SESS_B)),
          nowSelectedStoredId: SESS_B,
          composerScope: SESS_B,
          submitTargetComposerScope: SESS_A,
        }),
      ),
    ).toBe("composer:sess-b->sess-a");
  });

  it("does not drift when compression rotates the live tip but keeps the lineage root", () => {
    expect(
      sessionContextDrift(
        sessionArgs({
          startRouteToken: routeToken(sessionPath("stored-tip-after-compression")),
          nowRouteToken: routeToken(sessionPath("stored-tip-after-compression")),
          startSelectedStoredId: "stored-tip-after-compression",
          nowSelectedStoredId: "stored-tip-after-compression",
          submitTargetStoredId: "stored-tip-after-compression",
          composerScope: "stored-root",
          submitTargetComposerScope: "stored-root",
        }),
      ),
    ).toBeNull();
  });
});
