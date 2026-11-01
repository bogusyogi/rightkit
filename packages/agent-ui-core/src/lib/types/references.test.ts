import { describe, expect, it } from "vitest";

import { createSkillReference, githubRepositoryIdentity, toReferenceMeta } from "./references";

describe("semantic reference helpers", () => {
  it("recognizes only GitHub repository roots", () => {
    expect(githubRepositoryIdentity("https://github.com/stablyai/orca.git?tab=readme")).toEqual({
      owner: "stablyai",
      repository: "orca",
      label: "stablyai/orca",
    });
    expect(githubRepositoryIdentity("https://github.com/stablyai/orca/issues")).toBeNull();
    expect(githubRepositoryIdentity("javascript://github.com/stablyai/orca")).toBeNull();
    expect(githubRepositoryIdentity("https://github.com.evil.test/stablyai/orca")).toBeNull();
  });

  it("creates stable, serializable skill metadata", () => {
    const first = createSkillReference(
      { slug: "blueprint", name: "Blueprint", path: "/skills/blueprint/SKILL.md" },
      "2026-07-21T00:00:00Z",
    );
    const second = createSkillReference(
      { slug: "blueprint", name: "Blueprint", path: "/skills/blueprint/SKILL.md" },
      "2026-07-21T00:01:00Z",
    );
    expect(first?.id).toBe("skill:blueprint");
    expect(second?.id).toBe(first?.id);
    expect(first?.destination).toBe("@blueprint");
    expect(first && toReferenceMeta(first)).not.toHaveProperty("previewUrl");
  });

  it("rejects malformed skill slugs instead of creating ambiguous references", () => {
    expect(createSkillReference({ slug: "../skill", name: "Unsafe" })).toBeNull();
    expect(createSkillReference({ slug: "a", name: "Too short" })).toBeNull();
  });
});
