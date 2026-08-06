import { Icon } from "./Icon";

interface ContextInspectorProps {
  drawer?: boolean;
  onClose?: () => void;
  open: boolean;
}

export function ContextInspector({ drawer = false, onClose, open }: ContextInspectorProps) {
  return (
    <aside aria-label="Context inspector" className={`context-inspector ${drawer ? "panel-drawer panel-drawer--right" : ""}`} data-open={open} data-focus-zone hidden={!open} tabIndex={-1}>
      <div className="panel-heading"><span>Session</span>{drawer ? <button className="icon-button icon-button--small" onClick={onClose} type="button" aria-label="Close context inspector"><Icon name="x" /></button> : <span className="support-badge">Foundation</span>}</div>
      <section className="inspector-section">
        <h2>Attention</h2>
        <div className="inspector-empty"><Icon name="info" /><span>No requests need your attention.</span></div>
      </section>
      <section className="inspector-section">
        <h2>Tools</h2>
        <dl className="inspector-stats">
          <div><dt>Running</dt><dd>Not loaded</dd></div>
          <div><dt>Completed</dt><dd>Not loaded</dd></div>
        </dl>
      </section>
      <section className="inspector-section">
        <h2>Session details</h2>
        <dl className="detail-list detail-list--compact">
          <div><dt>CLI</dt><dd>Not attached</dd></div>
          <div><dt>State</dt><dd><span className="state-dot" /> Not attached</dd></div>
          <div><dt>Transport</dt><dd>—</dd></div>
          <div><dt>Usage</dt><dd>—</dd></div>
        </dl>
      </section>
      <section className="inspector-section">
        <h2>Capabilities</h2>
        <button aria-describedby="capability-unavailable" className="capability-row" disabled type="button">
          <span>Exact TUI</span><span className="support-badge">Unavailable</span>
        </button>
        <button aria-describedby="capability-unavailable" className="capability-row" disabled type="button">
          <span>Session fork</span><span className="support-badge">Unavailable</span>
        </button>
        <p className="disabled-explanation" id="capability-unavailable" tabIndex={0}>Session capabilities become available with a supported CLI adapter.</p>
      </section>
    </aside>
  );
}
