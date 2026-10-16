import { describe, expect, it } from "vitest";
import { parse as parseToml } from "smol-toml";
import { findVersionRefs, replaceCargoVersion } from "../src/toml-edit.js";
import { cargoDependencies } from "../src/check.js";

const DOC = `# top comment
[package]
name = "app"
version = "0.1.0"   # the crate's own version, not a dependency

[dependencies]
serde = "1.0.100"  # keep me
anyhow = { version = "1", default-features = false, features = ["std"] } # trailing
tokio = { version = "1.2.3", features = [
  "rt",
  "macros", # why
] }
renamed = { package = "real-name", version = "0.3" }
dotted.version = "2.0.1"
dotted.features = ["a"]
local = { path = "../local", version = "0.1.0" }

[dependencies.complex]
version = "3.4.5"
optional = true

[dev-dependencies]
tempfile = '3.10'

[target.'cfg(windows)'.dependencies]
windows = { version = "0.62", features = ["Win32"] }

[workspace.dependencies]
serde_json = "1.0.150"

[profile.release]
version = "9.9.9"
`;

describe("findVersionRefs", () => {
  it("finds dependency versions in every declaration form and ignores everything else", () => {
    const refs = findVersionRefs(DOC);
    const byName = Object.fromEntries(refs.map((r) => [r.name, r.spec]));
    expect(byName).toEqual({
      serde: "1.0.100",
      anyhow: "1",
      tokio: "1.2.3",
      "real-name": "0.3",
      dotted: "2.0.1",
      local: "0.1.0",
      complex: "3.4.5",
      tempfile: "3.10",
      windows: "0.62",
      serde_json: "1.0.150",
    });
    expect(refs.find((r) => r.name === "real-name")?.keyName).toBe("renamed");
    for (const ref of refs) expect(DOC.slice(ref.start, ref.end)).toBe(ref.spec);
  });

  it("agrees with the check reader about which versions exist", () => {
    const fromCheck = cargoDependencies(parseToml(DOC) as Record<string, unknown>).filter((d) => d.spec !== null);
    const scanned = findVersionRefs(DOC);
    // check reports path dependencies as unknown; the scanner still sees their version text.
    const checkSpecs = fromCheck.map((d) => `${d.name}@${d.spec}`).sort();
    const scanSpecs = scanned.filter((r) => r.name !== "local").map((r) => `${r.name}@${r.spec}`).sort();
    expect(scanSpecs).toEqual(checkSpecs);
  });
});

describe("replaceCargoVersion", () => {
  it("changes only the version text and preserves comments, tables, features and layout", () => {
    const cases: Array<[string, string, string]> = [
      ["serde", "1.0.100", "1.0.229"],
      ["anyhow", "1", "1.4"],
      ["tokio", "1.2.3", "1.9.9"],
      ["real-name", "0.3", "0.3.9"],
      ["dotted", "2.0.1", "2.1.0"],
      ["complex", "3.4.5", "3.9.0"],
      ["tempfile", "3.10", "3.20"],
      ["windows", "0.62", "0.62.5"],
      ["serde_json", "1.0.150", "1.0.152"],
    ];
    for (const [name, oldSpec, newSpec] of cases) {
      const result = replaceCargoVersion(DOC, name, oldSpec, newSpec);
      expect(result.count, name).toBe(1);
      const ref = findVersionRefs(DOC).find((r) => r.name === name)!;
      expect(result.text, name).toBe(DOC.slice(0, ref.start) + newSpec + DOC.slice(ref.end));
      const after = findVersionRefs(result.text).find((r) => r.name === name)!;
      expect(after.spec).toBe(newSpec);
      expect(() => parseToml(result.text)).not.toThrow();
    }
  });

  it("does not touch the [package] version or other crates that share the version text", () => {
    const text = '[package]\nversion = "1.0.0"\n[dependencies]\na = "1.0.0"\nb = "1.0.0"\n';
    const result = replaceCargoVersion(text, "a", "1.0.0", "1.0.1");
    expect(result.text).toBe('[package]\nversion = "1.0.0"\n[dependencies]\na = "1.0.1"\nb = "1.0.0"\n');
  });

  it("replaces the same dependency in every table that declares it with the same text", () => {
    const text = '[dependencies]\nx = "1.2.3"\n[dev-dependencies]\nx = { version = "1.2.3" }\n';
    const result = replaceCargoVersion(text, "x", "1.2.3", "1.2.4");
    expect(result.count).toBe(2);
    expect(result.text).toBe('[dependencies]\nx = "1.2.4"\n[dev-dependencies]\nx = { version = "1.2.4" }\n');
  });

  it("reports zero matches when the version text differs, and rejects unsafe replacement text", () => {
    expect(replaceCargoVersion(DOC, "serde", "9.9.9", "1.0.0").count).toBe(0);
    expect(() => replaceCargoVersion(DOC, "serde", "1.0.100", '1"; evil = "x')).toThrow(/unsafe/);
  });

  it("handles CRLF files and multi-line strings elsewhere in the file", () => {
    const text = '[package]\r\ndescription = """\r\nmulti [dependencies] line\r\n"""\r\n\r\n[dependencies]\r\nserde = "1.0.1"\r\n';
    const result = replaceCargoVersion(text, "serde", "1.0.1", "1.0.2");
    expect(result.text).toBe(text.replace("1.0.1", "1.0.2"));
  });
});
