import { useState, type FormEvent } from "react";
import { Icon } from "./Icon";
import { describeHost, type SystemSnapshot } from "../lib/system";

export function ConnectingSurface() {
  return (
    <main className="startup-surface" aria-busy="true" aria-live="polite">
      <div className="maestro-mark" aria-hidden="true"><Icon name="spark" /></div>
      <div className="startup-copy">
        <p className="eyebrow">Maestro</p>
        <h1>Connecting to Maestro service…</h1>
        <p>Preparing the secure local workspace.</p>
      </div>
      <div className="progress-track" aria-hidden="true"><span /></div>
    </main>
  );
}

interface DaemonUnavailableSurfaceProps {
  error?: string;
  onRetry: () => void;
  snapshot?: SystemSnapshot;
}

export function DaemonUnavailableSurface({ error, onRetry, snapshot }: DaemonUnavailableSurfaceProps) {
  const detail = error ?? snapshot?.daemon.detail ?? "The local service did not respond.";

  return (
    <main className="startup-surface startup-surface--error" data-focus-zone tabIndex={-1}>
      <div className="maestro-mark maestro-mark--warning" aria-hidden="true"><Icon name="warning" /></div>
      <div className="startup-copy">
        <p className="eyebrow">Local service unavailable</p>
        <h1>Maestro could not connect to its service.</h1>
        <p>Your project and session data have not been opened. {detail}</p>
      </div>
      <div className="startup-actions">
        <button className="button button--primary" type="button" onClick={onRetry} autoFocus>Retry Connection</button>
        <details>
          <summary>Show safe details</summary>
          <dl className="detail-list">
            <div><dt>State</dt><dd>{snapshot?.daemon.status ?? "host command failed"}</dd></div>
            {snapshot ? <div><dt>Host</dt><dd>{describeHost(snapshot)}</dd></div> : null}
            <div><dt>Diagnostic</dt><dd>Foundation daemon connection is unavailable.</dd></div>
          </dl>
        </details>
      </div>
      <p className="startup-note">No agent or provider connection was attempted.</p>
    </main>
  );
}

interface StorageUnlockSurfaceProps {
  mode: "create" | "unlock";
  onUnlock: (passphrase: string) => Promise<void>;
}

export function StorageUnlockSurface({ mode, onUnlock }: StorageUnlockSurfaceProps) {
  const creating = mode === "create";
  const [passphrase, setPassphrase] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!passphrase) {
      setError("Enter a non-empty passphrase.");
      return;
    }
    if (creating && passphrase !== confirmation) {
      setError("The passphrases do not match.");
      return;
    }

    setError(null);
    setSubmitting(true);
    try {
      await onUnlock(passphrase);
    } catch {
      setError("The passphrase was not accepted. Check it and try again.");
      setPassphrase("");
      setConfirmation("");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <main className="startup-surface" data-focus-zone tabIndex={-1}>
      <div className="maestro-mark" aria-hidden="true"><Icon name="spark" /></div>
      <div className="startup-copy">
        <p className="eyebrow">Encrypted local storage</p>
        <h1>{creating ? "Protect Maestro with a passphrase" : "Unlock Maestro"}</h1>
        <p>
          {creating
            ? "Linux secure storage is unavailable. Create a passphrase to encrypt Maestro-owned project and session data."
            : "Enter the passphrase for Maestro-owned encrypted data. Vendor credentials remain managed by their CLIs."}
        </p>
      </div>
      <form className="unlock-form" onSubmit={(event) => void submit(event)}>
        <label>
          Passphrase
          <input
            autoComplete={creating ? "new-password" : "current-password"}
            autoFocus
            disabled={submitting}
            onChange={(event) => setPassphrase(event.currentTarget.value)}
            type="password"
            value={passphrase}
          />
        </label>
        {creating ? (
          <label>
            Confirm passphrase
            <input
              autoComplete="new-password"
              disabled={submitting}
              onChange={(event) => setConfirmation(event.currentTarget.value)}
              type="password"
              value={confirmation}
            />
          </label>
        ) : null}
        {error ? <p className="inline-error" role="alert">{error}</p> : null}
        <button className="button button--primary" disabled={submitting} type="submit">
          {submitting ? "Unlocking…" : creating ? "Create encrypted storage" : "Unlock"}
        </button>
      </form>
      <p className="startup-note">The passphrase is sent only to the local authenticated daemon and is not stored.</p>
    </main>
  );
}

interface StorageUnavailableSurfaceProps {
  onRetry: () => void;
  snapshot: SystemSnapshot;
}

export function StorageUnavailableSurface({ onRetry, snapshot }: StorageUnavailableSurfaceProps) {
  return (
    <main className="startup-surface startup-surface--error" data-focus-zone tabIndex={-1}>
      <div className="maestro-mark maestro-mark--warning" aria-hidden="true"><Icon name="warning" /></div>
      <div className="startup-copy">
        <p className="eyebrow">Encrypted storage unavailable</p>
        <h1>Maestro did not open your local data.</h1>
        <p>{snapshot.daemon.detail} No unencrypted fallback was created.</p>
      </div>
      <div className="startup-actions">
        <button className="button button--primary" onClick={onRetry} type="button">Retry</button>
        <details>
          <summary>Show safe details</summary>
          <dl className="detail-list">
            <div><dt>Storage</dt><dd>{snapshot.daemon.storageStatus}</dd></div>
            <div><dt>Host</dt><dd>{describeHost(snapshot)}</dd></div>
          </dl>
        </details>
      </div>
      <p className="startup-note">Preserve the Maestro data directory if recovery is required.</p>
    </main>
  );
}
