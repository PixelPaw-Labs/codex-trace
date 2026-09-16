import { useCallback, useState } from "react";
import { invoke } from "../lib/invoke";
import type { ApiClient, IssuedCredential } from "../../shared/types";

interface AcceptedClientsProps {
  clients: ApiClient[];
  /** False under `CODEXTRACE_API_AUTH=off` — nothing can be issued. */
  authEnabled: boolean;
  /** `"file"`, `"ephemeral"`, or `"disabled"`. */
  authSource: string;
  onClientsChanged: (clients: ApiClient[]) => void;
}

/**
 * Unix seconds as a named month, e.g. "8 Sep 2026". A numeric-only format
 * renders as two different dates depending on the reader's locale.
 */
function formatIssued(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
  });
}

/** A destructive action that needs a second click to go through. */
type PendingAction = { kind: "reissue" | "revoke"; id: string } | null;

export function AcceptedClients({
  clients,
  authEnabled,
  authSource,
  onClientsChanged,
}: AcceptedClientsProps) {
  const [newName, setNewName] = useState("");
  const [error, setError] = useState("");
  const [issued, setIssued] = useState<IssuedCredential | null>(null);
  const [pending, setPending] = useState<PendingAction>(null);
  const [copied, setCopied] = useState(false);

  const refresh = useCallback(async () => {
    const next = await invoke<ApiClient[]>("list_clients");
    onClientsChanged(next);
  }, [onClientsChanged]);

  const run = useCallback(
    async (work: () => Promise<void>) => {
      setError("");
      try {
        await work();
        await refresh();
      } catch (err) {
        setError(String(err));
      }
    },
    [refresh],
  );

  const handleAdd = useCallback(() => {
    const name = newName.trim();
    if (!name) return;
    void run(async () => {
      const result = await invoke<IssuedCredential>("register_client", { name });
      setIssued(result);
      setCopied(false);
      setNewName("");
    });
  }, [newName, run]);

  const handleReissue = useCallback(
    (id: string) => {
      void run(async () => {
        const result = await invoke<IssuedCredential>("reissue_client", { id });
        setIssued(result);
        setCopied(false);
        setPending(null);
      });
    },
    [run],
  );

  const handleRevoke = useCallback(
    (id: string) => {
      void run(async () => {
        await invoke<ApiClient>("revoke_client", { id });
        setPending(null);
      });
    },
    [run],
  );

  const handleCopy = useCallback(() => {
    if (!issued) return;
    navigator.clipboard
      .writeText(issued.credential)
      .then(() => setCopied(true))
      .catch(() => setError("could not copy to the clipboard"));
  }, [issued]);

  const isPending = (kind: "reissue" | "revoke", id: string) =>
    pending?.kind === kind && pending.id === id;

  if (!authEnabled) {
    return (
      <>
        <span className="settings-modal__label">Accepted Clients</span>
        <p className="settings-modal__hint">
          Client verification is off (CODEXTRACE_API_AUTH=off), so any local process can call the
          API. Unset that variable and restart to turn it back on.
        </p>
      </>
    );
  }

  return (
    <>
      <span className="settings-modal__label">Accepted Clients</span>
      <p className="settings-modal__hint">
        Every caller of the local API is a registered client with its own signed credential (a JWT).
        Send it as an <code>X-CodexTrace-Token</code> header or <code>Authorization: Bearer</code>.
        Revoking or reissuing one client never affects another.
      </p>
      {authSource === "ephemeral" && (
        <p className="settings-modal__error">
          The config directory is unusable, so this run uses a one-off signing key. Credentials
          issued now stop working when codex-trace exits.
        </p>
      )}

      <ul className="settings-modal__clients">
        {clients.map((client) => (
          <li key={client.id} className="settings-modal__client">
            <span className="settings-modal__client-name">
              {client.name}
              {client.builtin && <span className="settings-modal__client-tag">built-in</span>}
              {client.revoked_at != null && (
                <span className="settings-modal__client-tag settings-modal__client-tag--revoked">
                  revoked
                </span>
              )}
            </span>
            <span className="settings-modal__client-issued">
              Issued {formatIssued(client.issued_at)}
            </span>
            <button
              className="settings-modal__btn"
              onClick={() =>
                isPending("reissue", client.id)
                  ? handleReissue(client.id)
                  : setPending({ kind: "reissue", id: client.id })
              }
            >
              {isPending("reissue", client.id) ? "Confirm reissue" : "Reissue"}
            </button>
            <button
              className="settings-modal__btn"
              onClick={() =>
                isPending("revoke", client.id)
                  ? handleRevoke(client.id)
                  : setPending({ kind: "revoke", id: client.id })
              }
              disabled={client.revoked_at != null}
            >
              {isPending("revoke", client.id) ? "Confirm revoke" : "Revoke"}
            </button>
          </li>
        ))}
      </ul>

      <div className="settings-modal__origin-add">
        <input
          className="settings-modal__input"
          type="text"
          value={newName}
          onChange={(e) => {
            setNewName(e.target.value);
            setError("");
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              handleAdd();
            }
          }}
          placeholder="my-script"
          aria-label="New client name"
          spellCheck={false}
        />
        <button className="settings-modal__btn" onClick={handleAdd} disabled={!newName.trim()}>
          Add client
        </button>
      </div>

      {issued && (
        <div className="settings-modal__credential">
          <p className="settings-modal__hint">
            Credential for <strong>{issued.client.name}</strong>. This is the only time it is shown
            — copy it now.
          </p>
          <code className="settings-modal__credential-value">{issued.credential}</code>
          <div className="settings-modal__origin-add">
            <button className="settings-modal__btn" onClick={handleCopy}>
              {copied ? "Copied" : "Copy"}
            </button>
            <button className="settings-modal__btn" onClick={() => setIssued(null)}>
              Done
            </button>
          </div>
        </div>
      )}

      {error && <p className="settings-modal__error">{error}</p>}
    </>
  );
}
