import { describe, expect, it } from "vitest";
import { decideBump, parseRequirement } from "../src/bump.js";

describe("decideBump", () => {
  it("applies patch and minor bumps and keeps prefix and precision", () => {
    expect(decideBump("1.0.100", "1.0.210")).toMatchObject({ action: "apply", newRequirement: "1.0.210", fullVersion: "1.0.210" });
    expect(decideBump("^1.2.3", "1.5.0")).toMatchObject({ action: "apply", newRequirement: "^1.5.0" });
    expect(decideBump("~1.2.3", "1.2.9")).toMatchObject({ action: "apply", newRequirement: "~1.2.9" });
    expect(decideBump("1.2", "1.5.7")).toMatchObject({ action: "apply", newRequirement: "1.5", fullVersion: "1.5.7" });
    expect(decideBump("0.40.1", "0.40.2")).toMatchObject({ action: "apply", newRequirement: "0.40.2" });
    expect(decideBump(">=1.0.0", "1.4.0")).toMatchObject({ action: "apply", newRequirement: ">=1.4.0" });
  });

  it("reports major bumps as needs-migration", () => {
    expect(decideBump("1.9.9", "2.0.0").action).toBe("needs-migration");
    expect(decideBump("^4.1.0", "5.0.1").action).toBe("needs-migration");
    expect(decideBump("1", "2.0.0").action).toBe("needs-migration");
  });

  it("reports a minor bump at 0.x as needs-migration, and a 0.0.x patch bump too", () => {
    expect(decideBump("0.3.1", "0.4.0").action).toBe("needs-migration");
    expect(decideBump("0.3", "0.4.2").action).toBe("needs-migration");
    expect(decideBump("^0.62.2", "0.100.0").action).toBe("needs-migration");
    expect(decideBump("0.0.3", "0.0.4").action).toBe("needs-migration");
  });

  it("skips what it cannot rewrite", () => {
    expect(decideBump("1.x", "1.5.0").action).toBe("skip");
    expect(decideBump("1.0.0-beta.1", "1.0.1").action).toBe("skip");
    expect(decideBump("1.0.0", "1.1.0-rc.1").action).toBe("skip");
    expect(decideBump("1.0.0", "1.1.8+spec-1.1.0").action).toBe("skip");
  });

  it("parseRequirement splits prefix and numeric parts", () => {
    expect(parseRequirement("^1.2")).toMatchObject({ prefix: "^", parts: [1, 2] });
    expect(parseRequirement("v3")).toMatchObject({ prefix: "v", parts: [3] });
    expect(parseRequirement("1.2.3.4")).toBeNull();
  });
});
