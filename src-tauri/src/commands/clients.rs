//! Settings → Accepted clients: list, register, reissue, revoke.
//!
//! Mirrors `commands/cors.rs`: plain impl functions shared by the Tauri IPC
//! commands and the HTTP routes, with thin `#[tauri::command]` wrappers.

use std::sync::Arc;

use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::clients::Client;
use crate::state::AppState;

/// A freshly minted credential. The credential is returned exactly once — the
/// backend never stores it (built-in clients excepted, whose file the backend
/// rewrites so the dev server can read it).
#[derive(Serialize, Debug)]
pub struct IssuedCredential {
    pub client: Client,
    pub credential: String,
}

pub fn list_clients_impl(state: &AppState) -> Vec<Client> {
    state.clients_snapshot()
}

pub fn register_client_impl(state: &AppState, name: &str) -> Result<IssuedCredential, String> {
    let (client, credential) = state.register_client(name)?;
    Ok(IssuedCredential { client, credential })
}

pub fn reissue_client_impl(state: &AppState, id: &str) -> Result<IssuedCredential, String> {
    let id = parse_id(id)?;
    let (client, credential) = state.reissue_client(id)?;
    Ok(IssuedCredential { client, credential })
}

pub fn revoke_client_impl(state: &AppState, id: &str) -> Result<Client, String> {
    state.revoke_client(parse_id(id)?)
}

fn parse_id(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id.trim()).map_err(|_| format!("invalid client id: {id}"))
}

#[tauri::command]
pub async fn list_clients(state: State<'_, Arc<AppState>>) -> Result<Vec<Client>, String> {
    Ok(list_clients_impl(&state))
}

#[tauri::command]
pub async fn register_client(
    name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IssuedCredential, String> {
    register_client_impl(&state, &name)
}

#[tauri::command]
pub async fn reissue_client(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IssuedCredential, String> {
    reissue_client_impl(&state, &id)
}

#[tauri::command]
pub async fn revoke_client(id: String, state: State<'_, Arc<AppState>>) -> Result<Client, String> {
    revoke_client_impl(&state, &id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthMode, KeySource};

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn enabled() -> AppState {
        AppState::for_tests(AuthMode::Enabled {
            key: KEY.to_vec(),
            source: KeySource::Ephemeral,
        })
    }

    #[test]
    fn register_returns_a_credential_that_verifies_and_lists_without_it() {
        let state = enabled();
        let issued = register_client_impl(&state, "ci-script").unwrap();
        let claims = crate::jwt::verify(&issued.credential, KEY).unwrap();
        assert_eq!(claims.sub, issued.client.id.to_string());
        assert_eq!(claims.name, "ci-script");

        let listed = list_clients_impl(&state);
        assert!(listed.iter().any(|c| c.id == issued.client.id));
        let json = serde_json::to_string(&listed).unwrap();
        assert!(
            !json.contains(&issued.credential),
            "credentials are never listed"
        );
    }

    #[test]
    fn reissue_and_revoke_take_string_ids_and_reject_bad_ones() {
        let state = enabled();
        let issued = register_client_impl(&state, "a").unwrap();
        let id = issued.client.id.to_string();

        let re = reissue_client_impl(&state, &format!(" {id} ")).unwrap();
        assert_ne!(re.credential, issued.credential);
        assert!(re.client.issued_at > issued.client.issued_at);

        let revoked = revoke_client_impl(&state, &id).unwrap();
        assert!(revoked.is_revoked());

        assert!(reissue_client_impl(&state, "not-a-uuid")
            .unwrap_err()
            .contains("invalid client id"));
        assert!(revoke_client_impl(&state, &Uuid::new_v4().to_string()).is_err());
    }

    #[test]
    fn a_reissued_credential_authenticates_and_the_old_one_stops() {
        let state = enabled();
        let first = register_client_impl(&state, "a").unwrap();
        let second = reissue_client_impl(&state, &first.client.id.to_string()).unwrap();

        let old = crate::jwt::verify(&first.credential, KEY).unwrap();
        let new = crate::jwt::verify(&second.credential, KEY).unwrap();
        assert!(state.authenticate(&new).is_some(), "the new one works");
        assert!(state.authenticate(&old).is_none(), "the old one is dead");
    }

    #[test]
    fn revoking_one_client_leaves_the_others_working() {
        let state = enabled();
        let a = register_client_impl(&state, "a").unwrap();
        let b = register_client_impl(&state, "b").unwrap();

        revoke_client_impl(&state, &a.client.id.to_string()).unwrap();

        let a_claims = crate::jwt::verify(&a.credential, KEY).unwrap();
        let b_claims = crate::jwt::verify(&b.credential, KEY).unwrap();
        assert!(state.authenticate(&a_claims).is_none());
        assert!(state.authenticate(&b_claims).is_some());
    }

    #[test]
    fn a_duplicate_name_is_rejected() {
        let state = enabled();
        register_client_impl(&state, "dup").unwrap();
        assert!(register_client_impl(&state, "DUP").is_err());
    }

    #[test]
    fn disabled_auth_cannot_issue_credentials() {
        let state = AppState::for_tests(AuthMode::Disabled);
        let err = register_client_impl(&state, "a").unwrap_err();
        assert!(err.contains("disabled"), "{err}");
    }
}
