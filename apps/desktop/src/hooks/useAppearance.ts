import { useEffect, useState } from "react";
import {
  readRememberedAppearance,
  rememberAppearance,
  resolveTheme,
  type ThemePreference,
  type UiScale,
} from "../lib/theme";

export interface AppearanceState {
  theme: ThemePreference;
  resolvedTheme: "light" | "dark";
  scale: UiScale;
  setTheme: (theme: ThemePreference) => void;
  setScale: (scale: UiScale) => void;
}

const DARK_QUERY = "(prefers-color-scheme: dark)";

export function useAppearance(): AppearanceState {
  const initial = readRememberedAppearance();
  const [theme, setTheme] = useState<ThemePreference>(initial.theme);
  const [scale, setScale] = useState<UiScale>(initial.scale);
  const [prefersDark, setPrefersDark] = useState(() => matchMedia(DARK_QUERY).matches);

  useEffect(() => {
    const media = matchMedia(DARK_QUERY);
    const update = (event: MediaQueryListEvent) => setPrefersDark(event.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);

  const resolvedTheme = resolveTheme(theme, prefersDark);

  useEffect(() => {
    rememberAppearance(theme, scale);
    document.documentElement.dataset.theme = resolvedTheme;
    document.documentElement.dataset.themePreference = theme;
    document.documentElement.style.setProperty("--ui-scale", String(scale / 100));
    document.documentElement.style.colorScheme = resolvedTheme;
  }, [resolvedTheme, scale, theme]);

  return { theme, resolvedTheme, scale, setTheme, setScale };
}
