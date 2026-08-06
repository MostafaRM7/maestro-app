import { describe, expect, it } from "vitest";
import {
  isThemePreference,
  isUiScale,
  readRememberedAppearance,
  rememberAppearance,
  resolveTheme,
} from "./theme";

describe("appearance preferences", () => {
  it("rejects unsupported theme and scale values", () => {
    expect(isThemePreference("invalid")).toBe(false);
    expect(isUiScale(105)).toBe(false);
  });

  it("retains appearance only in process memory", () => {
    const original = readRememberedAppearance();
    rememberAppearance("dark", 125);
    expect(readRememberedAppearance()).toEqual({ scale: 125, theme: "dark" });
    rememberAppearance(original.theme, original.scale);
  });

  it("tracks the operating system only for the system preference", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
  });
});
