import { Icon } from "./Icon";
import type { DaemonSnapshot } from "../lib/system";

export function StatusBar({ daemon }: { daemon: DaemonSnapshot }) {
  const connected = daemon.status === "connected";
  return (
    <footer className="status-bar" data-focus-zone tabIndex={0} aria-label="Project status">
      <span><span className={`status-led ${connected ? "status-led--success" : "status-led--warning"}`} aria-hidden="true" /> Daemon {connected ? "connected" : "offline"}</span>
      <span><Icon name="branch" /> Git not loaded</span>
      <span>Sessions not loaded</span>
      <span>Approvals not loaded</span>
      <span className="status-bar__spacer" />
      <span>Budget not loaded</span>
    </footer>
  );
}
