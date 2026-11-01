import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(resolve(here, "useAppInteractionActions.ts"), "utf8");

describe("command action handling", () => {
  // /audit, /blueprint and /commit resolve to a `conduct_goal` action. The
  // switch had no case for it, so the command printed its opening line and the
  // orchestrator was never invoked from the UI — no task graph, no agents, no
  // error. Anything the daemon can return must be handled here.
  it("handles conduct_goal by invoking the orchestrator", () => {
    expect(source).toContain('case "conduct_goal"');
    expect(source).toMatch(/case "conduct_goal"[\s\S]{0,1200}orchestrate\(/);
  });

  it("still handles the other daemon command actions", () => {
    for (const kind of [
      "clear_chat",
      "compact_context",
      "list_sessions",
      "switch_session",
      "submit_message",
    ]) {
      expect(source).toContain(`case "${kind}"`);
    }
  });
});

describe("command input visibility", () => {
  // The command the user typed never round-trips through the transcript, so the
  // pane showed only the response with no record of the request.
  it("echoes the user's command into the transcript", () => {
    expect(source).toContain("appendLocalUser(input)");
  });

  // A static window during a multi-minute conductor run is indistinguishable
  // from a hang.
  it("marks the session working while orchestrating", () => {
    expect(source).toMatch(/case "conduct_goal"[\s\S]{0,1200}setIsWorking\(true\)/);
  });
});
