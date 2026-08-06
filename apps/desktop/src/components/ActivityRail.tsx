import type { Activity } from "../hooks/useWindowLayout";
import { Icon, type IconName } from "./Icon";

interface ActivityRailProps {
  activity: Activity;
  onOpenSettings: () => void;
  onSelect: (activity: Activity) => void;
}

const items: ReadonlyArray<{ id: Activity; icon: IconName; label: string }> = [
  { id: "sessions", icon: "agents", label: "Sessions" },
  { id: "files", icon: "files", label: "Files" },
  { id: "search", icon: "search", label: "Search" },
  { id: "git", icon: "git", label: "Git" },
];

export function ActivityRail({ activity, onOpenSettings, onSelect }: ActivityRailProps) {
  return (
    <nav className="activity-rail" aria-label="Project navigation" data-focus-zone tabIndex={-1}>
      {items.map((item) => (
        <button
          aria-label={item.label}
          aria-pressed={activity === item.id}
          className={activity === item.id ? "is-active" : ""}
          key={item.id}
          onClick={() => onSelect(item.id)}
          title={item.label}
          type="button"
        >
          <Icon name={item.icon} />
        </button>
      ))}
      <div className="activity-rail__spacer" />
      <button aria-label="Settings" onClick={onOpenSettings} title="Settings" type="button"><Icon name="settings" /></button>
    </nav>
  );
}
