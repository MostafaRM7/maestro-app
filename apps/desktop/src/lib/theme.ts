export const THEMES = ["system", "light", "dark"] as const;
export type ThemePreference = (typeof THEMES)[number];

export const UI_SCALES = [80, 90, 100, 110, 125, 150, 175, 200] as const;
export type UiScale = (typeof UI_SCALES)[number];

export function isThemePreference(value: string | null): value is ThemePreference {
  return THEMES.some((theme) => theme === value);
}

export function isUiScale(value: number): value is UiScale {
  return UI_SCALES.some((scale) => scale === value);
}

export interface AppearancePreference {
  scale: UiScale;
  theme: ThemePreference;
}

let rememberedAppearance: AppearancePreference = { scale: 100, theme: "system" };

export function readRememberedAppearance(): AppearancePreference {
  return { ...rememberedAppearance };
}

export function rememberAppearance(theme: ThemePreference, scale: UiScale): void {
  rememberedAppearance = { scale, theme };
}

export function resolveTheme(theme: ThemePreference, prefersDark: boolean): "light" | "dark" {
  return theme === "system" ? (prefersDark ? "dark" : "light") : theme;
}
