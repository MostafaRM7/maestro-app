import { UI_SCALES, type ThemePreference, type UiScale } from "../lib/theme";

interface AppearanceControlsProps {
  scale: UiScale;
  setScale: (scale: UiScale) => void;
  setTheme: (theme: ThemePreference) => void;
  theme: ThemePreference;
}

export function AppearanceControls({ scale, setScale, setTheme, theme }: AppearanceControlsProps) {
  return (
    <div className="appearance-controls" aria-label="Appearance controls">
      <label>
        <span>Theme</span>
        <select
          aria-label="Theme"
          value={theme}
          onChange={(event) => setTheme(event.target.value as ThemePreference)}
        >
          <option value="system">System</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
      </label>
      <label>
        <span>UI scale</span>
        <select
          aria-label="UI scale"
          value={scale}
          onChange={(event) => setScale(Number(event.target.value) as UiScale)}
        >
          {UI_SCALES.map((option) => (
            <option key={option} value={option}>{option}%</option>
          ))}
        </select>
      </label>
    </div>
  );
}
