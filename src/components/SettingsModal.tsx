import { useState, useEffect, useCallback } from "react";
import { invoke } from "../lib/invoke";
import { PopoutModal } from "./PopoutModal";
import type { SettingsResponse } from "../../shared/types";

interface SettingsModalProps {
  onClose: () => void;
  onSaved: (dir: string) => void;
}

type ConnectionMode = "local" | "remote";

function parseRemoteSpec(value: string): { host: string; path: string } | null {
  if (!value.startsWith("ssh://")) return null;
  const rest = value.slice("ssh://".length);
  const slash = rest.indexOf("/");
  if (slash <= 0) return null;
  return { host: rest.slice(0, slash), path: `/${rest.slice(slash + 1)}` };
}

function makeRemoteSpec(host: string, path: string): string {
  return `ssh://${host.trim()}/${path.trim().replace(/^\/+/, "")}`;
}

export function SettingsModal({ onClose, onSaved }: SettingsModalProps) {
  const [mode, setMode] = useState<ConnectionMode>("local");
  const [sessionsDir, setSessionsDir] = useState("");
  const [defaultDir, setDefaultDir] = useState("");
  const [remoteHost, setRemoteHost] = useState("");
  const [remoteDir, setRemoteDir] = useState("~/.codex/sessions");
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    invoke<SettingsResponse>("get_settings")
      .then((res) => {
        setDefaultDir(res.default_dir);
        const current = res.sessions_dir ?? res.default_dir;
        const remote = parseRemoteSpec(current);
        if (remote) {
          setMode("remote");
          setRemoteHost(remote.host);
          setRemoteDir(remote.path === "/~" ? "~" : remote.path.replace(/^\/~\//, "~/"));
        } else {
          setMode("local");
          setSessionsDir(current);
        }
      })
      .catch(console.error);
  }, []);

  const selectMode = useCallback((nextMode: ConnectionMode) => {
    setMode(nextMode);
    setError("");
  }, []);

  const handleSave = useCallback(async () => {
    setSaving(true);
    setError("");
    try {
      let target: string | null;
      if (mode === "remote") {
        if (!remoteHost.trim()) throw new Error("SSH host is required");
        if (!remoteDir.trim()) throw new Error("Remote sessions directory is required");
        target = makeRemoteSpec(remoteHost, remoteDir);
      } else {
        target = sessionsDir.trim() || null;
      }
      const res = await invoke<SettingsResponse>("set_sessions_dir", { path: target });
      onSaved(res.sessions_dir ?? res.default_dir);
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }, [mode, remoteHost, remoteDir, sessionsDir, onSaved, onClose]);

  const handleReset = useCallback(async () => {
    setSaving(true);
    setError("");
    try {
      const res = await invoke<SettingsResponse>("set_sessions_dir", { path: null });
      setMode("local");
      setSessionsDir(res.default_dir);
      onSaved(res.default_dir);
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }, [onSaved, onClose]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleSave();
      }
    },
    [handleSave],
  );

  return (
    <PopoutModal
      onClose={onClose}
      header={<span className="settings-modal__title">Settings</span>}
      initialWidth={560}
      initialHeight={390}
    >
      <div className="settings-modal">
        <div className="settings-modal__mode" role="group" aria-label="Session source">
          <button
            className={`settings-modal__mode-btn${mode === "local" ? " settings-modal__mode-btn--active" : ""}`}
            onClick={() => selectMode("local")}
            type="button"
          >
            Local
          </button>
          <button
            className={`settings-modal__mode-btn${mode === "remote" ? " settings-modal__mode-btn--active" : ""}`}
            onClick={() => selectMode("remote")}
            type="button"
          >
            SSH Remote
          </button>
        </div>

        {mode === "local" ? (
          <>
            <label className="settings-modal__label" htmlFor="sessions-dir">
              Sessions Directory
            </label>
            <input
              id="sessions-dir"
              className="settings-modal__input"
              type="text"
              value={sessionsDir}
              onChange={(e) => {
                setSessionsDir(e.target.value);
                setError("");
              }}
              onKeyDown={handleKeyDown}
              placeholder={defaultDir}
              spellCheck={false}
              autoFocus
            />
            <p className="settings-modal__hint">Default: {defaultDir}</p>
          </>
        ) : (
          <>
            <label className="settings-modal__label" htmlFor="remote-host">
              SSH Host
            </label>
            <input
              id="remote-host"
              className="settings-modal__input"
              type="text"
              value={remoteHost}
              onChange={(e) => {
                setRemoteHost(e.target.value);
                setError("");
              }}
              onKeyDown={handleKeyDown}
              placeholder="dev"
              spellCheck={false}
              autoFocus
            />
            <p className="settings-modal__hint">
              Uses an alias or host from your local ~/.ssh/config. SSH key authentication must work
              non-interactively.
            </p>

            <label className="settings-modal__label" htmlFor="remote-sessions-dir">
              Remote Sessions Directory
            </label>
            <input
              id="remote-sessions-dir"
              className="settings-modal__input"
              type="text"
              value={remoteDir}
              onChange={(e) => {
                setRemoteDir(e.target.value);
                setError("");
              }}
              onKeyDown={handleKeyDown}
              placeholder="~/.codex/sessions"
              spellCheck={false}
            />
            <p className="settings-modal__hint">
              The path must be absolute or start with ~/. No remote service needs to be installed.
            </p>
          </>
        )}

        {error && <p className="settings-modal__error">{error}</p>}
        <div className="settings-modal__actions">
          <button
            className="settings-modal__btn settings-modal__btn--secondary"
            onClick={handleReset}
            disabled={saving}
          >
            Use Local Default
          </button>
          <button
            className="settings-modal__btn settings-modal__btn--primary"
            onClick={handleSave}
            disabled={saving}
          >
            {saving
              ? mode === "remote"
                ? "Connecting…"
                : "Saving…"
              : mode === "remote"
                ? "Connect"
                : "Save"}
          </button>
        </div>
      </div>
    </PopoutModal>
  );
}
