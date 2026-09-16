import { useState, useEffect, useCallback } from "react";
import { invoke } from "../lib/invoke";
import { PopoutModal } from "./PopoutModal";
import { AcceptedClients } from "./AcceptedClients";
import type { ApiClient, SettingsResponse } from "../../shared/types";

interface SettingsModalProps {
  onClose: () => void;
  onSaved: (dir: string) => void;
}

export function SettingsModal({ onClose, onSaved }: SettingsModalProps) {
  const [sessionsDir, setSessionsDir] = useState("");
  const [defaultDir, setDefaultDir] = useState("");
  const [allowedOrigins, setAllowedOrigins] = useState<string[]>([]);
  const [clients, setClients] = useState<ApiClient[]>([]);
  const [authEnabled, setAuthEnabled] = useState(true);
  const [authSource, setAuthSource] = useState("file");
  const [newOrigin, setNewOrigin] = useState("");
  const [error, setError] = useState("");
  const [originError, setOriginError] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    invoke<SettingsResponse>("get_settings")
      .then((res) => {
        setDefaultDir(res.default_dir);
        setSessionsDir(res.sessions_dir ?? res.default_dir);
        setAllowedOrigins(res.allowed_origins);
        setClients(res.clients);
        setAuthEnabled(res.api_auth_enabled);
        setAuthSource(res.api_auth_source);
      })
      .catch(console.error);
  }, []);

  const handleSave = useCallback(async () => {
    setSaving(true);
    setError("");
    try {
      const trimmed = sessionsDir.trim();
      await invoke<SettingsResponse>("set_sessions_dir", { path: trimmed || null });
      onSaved(trimmed || defaultDir);
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }, [sessionsDir, defaultDir, onSaved, onClose]);

  const handleReset = useCallback(async () => {
    setSaving(true);
    setError("");
    try {
      const res = await invoke<SettingsResponse>("set_sessions_dir", { path: null });
      setSessionsDir(res.default_dir);
      onSaved(res.default_dir);
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }, [onSaved, onClose]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void handleSave();
      }
    },
    [handleSave],
  );

  // Origins persist as soon as they change rather than on Save, so the running
  // server picks them up immediately — the same as every other setting.
  const persistOrigins = useCallback(async (origins: string[]) => {
    setOriginError("");
    try {
      const res = await invoke<SettingsResponse>("set_allowed_origins", { origins });
      setAllowedOrigins(res.allowed_origins);
      return true;
    } catch (err) {
      setOriginError(String(err));
      return false;
    }
  }, []);

  const handleAddOrigin = useCallback(async () => {
    const trimmed = newOrigin.trim();
    if (!trimmed) return;
    const ok = await persistOrigins([...allowedOrigins, trimmed]);
    if (ok) setNewOrigin("");
  }, [newOrigin, allowedOrigins, persistOrigins]);

  const handleRemoveOrigin = useCallback(
    (origin: string) => {
      void persistOrigins(allowedOrigins.filter((o) => o !== origin));
    },
    [allowedOrigins, persistOrigins],
  );

  const handleOriginKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void handleAddOrigin();
      }
    },
    [handleAddOrigin],
  );

  return (
    <PopoutModal
      onClose={onClose}
      header={<span className="settings-modal__title">Settings</span>}
      initialWidth={560}
      initialHeight={560}
    >
      <div className="settings-modal">
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
        {error && <p className="settings-modal__error">{error}</p>}

        <label className="settings-modal__label" htmlFor="new-origin">
          Allowed Origins
        </label>
        <p className="settings-modal__hint">
          Websites allowed to call the local API from a browser. The desktop app and the same-origin
          web UI never need an entry here.
        </p>
        {allowedOrigins.length > 0 && (
          <ul className="settings-modal__origins">
            {allowedOrigins.map((origin) => (
              <li key={origin} className="settings-modal__origin">
                <span className="settings-modal__origin-value">{origin}</span>
                <button
                  className="settings-modal__origin-remove"
                  onClick={() => handleRemoveOrigin(origin)}
                  aria-label={`Remove ${origin}`}
                  title={`Remove ${origin}`}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="settings-modal__origin-add">
          <input
            id="new-origin"
            className="settings-modal__input"
            type="text"
            value={newOrigin}
            onChange={(e) => {
              setNewOrigin(e.target.value);
              setOriginError("");
            }}
            onKeyDown={handleOriginKeyDown}
            placeholder="https://example.com:8080"
            spellCheck={false}
          />
          <button
            className="settings-modal__btn"
            onClick={() => void handleAddOrigin()}
            disabled={!newOrigin.trim()}
          >
            Add
          </button>
        </div>
        {originError && <p className="settings-modal__error">{originError}</p>}

        <AcceptedClients
          clients={clients}
          authEnabled={authEnabled}
          authSource={authSource}
          onClientsChanged={setClients}
        />

        <div className="settings-modal__actions">
          <button
            className="settings-modal__btn settings-modal__btn--secondary"
            onClick={handleReset}
            disabled={saving}
          >
            Reset to Default
          </button>
          <button
            className="settings-modal__btn settings-modal__btn--primary"
            onClick={handleSave}
            disabled={saving}
          >
            Save
          </button>
        </div>
      </div>
    </PopoutModal>
  );
}
