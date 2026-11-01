import { describe, expect, it } from "vitest";
import { formatMicro, formatMilli, groupDigits, monthOfInstant } from "./money";

describe("money text", () => {
  it("groups digits and keeps the sign", () => {
    expect(groupDigits("18450")).toBe("18,450");
    expect(groupDigits("-1234567")).toBe("-1,234,567");
  });
  it("formats micro and milli units exactly", () => {
    expect(formatMicro(2500000)).toBe("2.500000");
    expect(formatMicro(-400000)).toBe("-0.400000");
    expect(formatMilli(1500)).toBe("1.5");
    expect(formatMilli(250)).toBe("0.25");
    expect(formatMilli(4000)).toBe("4");
  });
  it("takes the month of an instant", () => {
    expect(monthOfInstant("2026-10-31T23:00:00Z")).toBe("2026-10");
  });
});
