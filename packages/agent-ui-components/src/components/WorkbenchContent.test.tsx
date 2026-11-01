// @vitest-environment node
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(fileURLToPath(new URL("./WorkbenchContent.tsx", import.meta.url)), "utf8");

describe("WorkbenchContent shell wiring", () => {
  it("keeps the clarification prompt on the live pending-question path", () => {
    expect(source).toContain('import QuestionPrompt from "@rightkit/agent-ui-workbench/components/workbench/QuestionPrompt"');
    expect(source).toContain("key={props.pendingQuestion.questionId}");
    expect(source).toContain("props.pendingQuestion.questions");
    expect(source).toContain("onSubmit={props.onAnswerQuestion}");
    expect(source.match(/<QuestionPrompt/g)).toHaveLength(1);
  });

  it("renders one away summary surface so the transcript does not duplicate it", () => {
    expect(source.match(/<AwaySummaryCard/g)).toHaveLength(1);
  });

  it("keeps the conversation landmark stable for the docked workspace shell", () => {
    expect(source).toContain('id="main-conversation"');
    expect(source).toContain('aria-label="Conversation"');
    expect(source).toContain('className="cr-transcript-scroll min-h-0 w-full flex-1 overflow-y-auto"');
  });
});
