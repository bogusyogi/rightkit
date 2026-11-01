import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { CommandInfo } from "@rightkit/agent-ui-core/lib/daemon";
import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";
import { useComposerCompletions } from "./useComposerCompletions";

const commands: CommandInfo[] = [
  { name: "new", usage: "/new", description: "Start a session" },
  { name: "review", usage: "/review", description: "Review code" },
];
const skills: SkillItem[] = [{
  slug: "research", name: "Research", description: "Research first", trigger: "", enabled: true, valid: true,
  category: "tool", priority: 1, tags: [], pathAffinity: [], path: "research/SKILL.md", directory: "research", kind: "local",
}];

afterEach(cleanup);

describe("useComposerCompletions", () => {
  it("exposes slash command and skill stages from the controlled composer value", () => {
    const result: { current?: ReturnType<typeof useComposerCompletions> } = {};
    function Probe({ value }: { value: string }) { result.current = useComposerCompletions({ value, commands, skills }); return null; }
    const view = render(<Probe value="/re" />);
    expect(result.current?.state.mode).toBe("slash-command");
    expect(result.current?.items[0].command?.name).toBe("review");
    view.rerender(<Probe value="@skill res" />);
    expect(result.current?.state.mode).toBe("skill-mention");
    expect(result.current?.items[0].skill?.slug).toBe("research");
  });

  it("moves, closes, and reopens the selected completion", () => {
    const result: { current?: ReturnType<typeof useComposerCompletions> } = {};
    function Probe() { result.current = useComposerCompletions({ value: "/", commands, skills }); return null; }
    render(<Probe />);
    expect(result.current?.selectedIndex).toBe(0);
    act(() => result.current?.moveSelection(1));
    expect(result.current?.selectedIndex).toBe(1);
    act(() => result.current?.close());
    expect(result.current?.state.mode).toBe("off");
    act(() => result.current?.reopen());
    expect(result.current?.state.mode).toBe("slash-command");
  });
});
