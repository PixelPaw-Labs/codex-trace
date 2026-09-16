//! Registry of accepted API clients.
//!
//! Every caller of the HTTP API is a registered *client* with a stable id and a
//! display name. Clients authenticate with an HS256 credential (see
//! `crate::jwt`) whose `sub` is their id. Revocation and reissue are recorded
//! here, so a credential is only as good as the registry says:
//!
//! - **revoked** clients are rejected outright;
//! - **reissue** bumps `issued_at`, so every credential minted before it
//!   (`iat < issued_at`) is dead, without rotating anybody else's.
//!
//! One client is built in and registered on first start: `web-ui` (the browser
//! frontend). Its credential is written to `clients/web-ui.jwt` in the config
//! dir so the Vite dev server can pick it up; user-registered clients are shown
//! their credential once and it is never stored.

use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WEB_UI: &str = "web-ui";
pub const BUILTIN_NAMES: [&str; 1] = [WEB_UI];

const MAX_NAME_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    pub id: Uuid,
    pub name: String,
    /// Registered automatically (`web-ui`); credential kept in a file.
    pub builtin: bool,
    /// Unix seconds.
    pub created_at: i64,
    /// Credentials with `iat` below this are rejected. Bumped by reissue.
    pub issued_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
}

impl Client {
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRegistry {
    #[serde(default)]
    pub clients: Vec<Client>,
}

/// Current unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Trim and validate a client name: 1–64 chars, no control characters.
pub fn validate_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("client name must not be empty".into());
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!(
            "client name must be at most {MAX_NAME_LEN} characters"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("client name must not contain control characters".into());
    }
    Ok(name.to_string())
}

impl ClientRegistry {
    /// Load from `path`; a missing file is an empty registry.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    /// Persist atomically with owner-only permissions.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::auth::write_private_atomic(path, &json)
    }

    pub fn find(&self, id: Uuid) -> Option<&Client> {
        self.clients.iter().find(|c| c.id == id)
    }

    pub fn find_by_name(&self, name: &str) -> Option<&Client> {
        self.clients
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name.trim()))
    }

    fn find_mut(&mut self, id: Uuid) -> Result<&mut Client, String> {
        self.clients
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| format!("unknown client {id}"))
    }

    /// Add the built-in clients that are missing. Returns whether anything changed.
    pub fn ensure_builtins(&mut self, now: i64) -> bool {
        let mut changed = false;
        for name in BUILTIN_NAMES {
            if self.find_by_name(name).is_none() {
                self.clients.push(Client {
                    id: Uuid::new_v4(),
                    name: name.to_string(),
                    builtin: true,
                    created_at: now,
                    issued_at: now,
                    revoked_at: None,
                });
                changed = true;
            }
        }
        changed
    }

    /// Register a new (non-built-in) client. Names are unique, case-insensitively.
    pub fn register(&mut self, raw_name: &str, now: i64) -> Result<Client, String> {
        let name = validate_name(raw_name)?;
        if self.find_by_name(&name).is_some() {
            return Err(format!("a client named \"{name}\" already exists"));
        }
        let client = Client {
            id: Uuid::new_v4(),
            name,
            builtin: false,
            created_at: now,
            issued_at: now,
            revoked_at: None,
        };
        self.clients.push(client.clone());
        Ok(client)
    }

    /// Revoke: every credential of this client stops working. Idempotent.
    pub fn revoke(&mut self, id: Uuid, now: i64) -> Result<Client, String> {
        let client = self.find_mut(id)?;
        if client.revoked_at.is_none() {
            client.revoked_at = Some(now);
        }
        Ok(client.clone())
    }

    /// Reissue: un-revoke and bump `issued_at` so that every credential minted
    /// before this call is rejected. The bump is strictly increasing even when
    /// two reissues land in the same second — otherwise a credential issued a
    /// moment earlier would satisfy `iat >= issued_at` and survive.
    pub fn reissue(&mut self, id: Uuid, now: i64) -> Result<Client, String> {
        let client = self.find_mut(id)?;
        client.issued_at = now.max(client.issued_at + 1);
        client.revoked_at = None;
        Ok(client.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_builtins_adds_web_ui_once() {
        let mut reg = ClientRegistry::default();
        assert!(reg.ensure_builtins(100));
        assert_eq!(reg.clients.len(), 1);
        assert!(reg.clients.iter().all(|c| c.builtin));
        assert!(reg.find_by_name("web-ui").is_some());
        assert!(
            reg.find_by_name("WEB-UI").is_some(),
            "lookup is case-insensitive"
        );
        assert!(!reg.ensure_builtins(200), "second call is a no-op");
        assert_eq!(reg.clients.len(), 1);
    }

    #[test]
    fn register_validates_and_dedupes_names() {
        let mut reg = ClientRegistry::default();
        reg.ensure_builtins(1);
        let c = reg.register("  ci-script ", 5).unwrap();
        assert_eq!(c.name, "ci-script");
        assert!(!c.builtin);
        assert_eq!((c.created_at, c.issued_at, c.revoked_at), (5, 5, None));

        assert!(reg.register("CI-SCRIPT", 6).is_err(), "duplicate, any case");
        assert!(reg.register("web-ui", 6).is_err(), "built-in name is taken");
    }

    #[test]
    fn validate_name_rejects_empty_overlong_and_control_characters() {
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN + 1)).is_err());
        assert!(validate_name("bad\nname").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN)).is_ok());
    }

    #[test]
    fn validate_name_counts_characters_not_bytes() {
        // 64 multi-byte characters is 64 characters, not 192 bytes.
        assert!(validate_name(&"é".repeat(MAX_NAME_LEN)).is_ok());
        assert!(validate_name(&"é".repeat(MAX_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn revoke_is_idempotent_and_keeps_the_first_timestamp() {
        let mut reg = ClientRegistry::default();
        let c = reg.register("x", 1).unwrap();
        assert_eq!(reg.revoke(c.id, 10).unwrap().revoked_at, Some(10));
        assert_eq!(
            reg.revoke(c.id, 20).unwrap().revoked_at,
            Some(10),
            "a second revoke does not move the timestamp"
        );
    }

    #[test]
    fn reissue_clears_revocation_and_advances_issued_at() {
        let mut reg = ClientRegistry::default();
        let c = reg.register("x", 1).unwrap();
        reg.revoke(c.id, 10).unwrap();
        let back = reg.reissue(c.id, 20).unwrap();
        assert_eq!(back.revoked_at, None);
        assert_eq!(back.issued_at, 20);
    }

    #[test]
    fn reissue_always_advances_even_within_the_same_second() {
        let mut reg = ClientRegistry::default();
        let c = reg.register("x", 100).unwrap();
        let first = reg.reissue(c.id, 100).unwrap().issued_at;
        let second = reg.reissue(c.id, 100).unwrap().issued_at;
        assert!(first > 100, "same-second reissue still invalidates");
        assert!(second > first, "and the next one advances again");
    }

    #[test]
    fn reissue_never_moves_issued_at_backwards() {
        let mut reg = ClientRegistry::default();
        let c = reg.register("x", 1000).unwrap();
        // A clock that jumped backwards must not resurrect older credentials.
        assert!(reg.reissue(c.id, 5).unwrap().issued_at > 1000);
    }

    #[test]
    fn mutating_an_unknown_id_is_an_error() {
        let mut reg = ClientRegistry::default();
        let missing = Uuid::new_v4();
        assert!(reg.revoke(missing, 1).is_err());
        assert!(reg.reissue(missing, 1).is_err());
    }

    #[test]
    fn load_of_a_missing_file_is_an_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ClientRegistry::load(&dir.path().join("nope.json")).unwrap();
        assert!(reg.clients.is_empty());
    }

    #[test]
    fn load_of_an_unparsable_file_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clients.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(ClientRegistry::load(&path).is_err());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clients.json");
        let mut reg = ClientRegistry::default();
        reg.ensure_builtins(1);
        reg.register("ci", 2).unwrap();
        reg.save(&path).unwrap();
        assert_eq!(ClientRegistry::load(&path).unwrap(), reg);
    }
}
