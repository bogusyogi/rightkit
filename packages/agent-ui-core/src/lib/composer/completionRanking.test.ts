import { describe, expect, it } from "vitest";
import type { CommandInfo } from "../daemon.js";
import type { SkillItem } from "../types.js";
import { rankComposerCompletions, rankMentionCompletions, rankSkillCompletions } from "./completionRanking.js";

const command = (name: string): CommandInfo => ({ name, usage: `/${name}`, description: `${name} command` });
const skill = (slug: string): SkillItem => ({
  slug, name: slug, description: `${slug} skill`, trigger: "", enabled: true, valid: true,
  category: "tool", priority: 0, tags: [], pathAffinity: [], path: `/skills/${slug}/SKILL.md`, directory: `/skills/${slug}`, kind: "local",
});

describe("completion ranking", () => {
  it("ranks exact and fuzzy slash command matches while retaining stable ties", () => {
    const result = rankComposerCompletions([command("new"), command("research"), command("review")], [], "research");
    expect(result.map((item) => item.command?.name)).toEqual(["research"]);
  });

  it("keeps typed skill searches complete and ranks bare browsing by usage", () => {
    const result = rankSkillCompletions([skill("docx"), skill("research"), skill("work")], "", {
      skillUsage: { research: { usage: 60 }, work: { usage: 172 } }, limit: 3,
    });
    expect(result.map((item) => item.skill?.slug)).toEqual(["work", "research", "docx"]);
    expect(rankSkillCompletions([skill("research"), skill("research-paper-writing")], "research").map((item) => item.skill?.slug)).toEqual(["research", "research-paper-writing"]);
  });

  it("does not duplicate a skill exposed by the command catalog", () => {
    const result = rankComposerCompletions([command("work")], [skill("work"), skill("other")], "");
    expect(result.filter((item) => item.skill?.slug === "work")).toHaveLength(0);
  });

  it("preserves workspace mention metadata and source order for ties", () => {
    const mentions = [{ kind: "file" as const, label: "App.tsx", detail: "src/App.tsx", insertText: "@App.tsx", path: "src/App.tsx" }];
    const result = rankMentionCompletions(mentions, "app");
    expect(result[0]).toMatchObject({ kind: "mention", label: "App.tsx", mention: mentions[0] });
  });
});
