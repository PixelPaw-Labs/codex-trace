use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions_dir: Option<String>,
    /// Extra origins allowed to call the local HTTP API cross-origin, on top of
    /// the built-in dev/web UI origins. Managed from Settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_origins: Vec<String>,
}

/// Env var that relocates the whole config root, so a test run never touches a
/// developer's real settings, signing key or client registry.
pub const ENV_CONFIG_DIR: &str = "CODEXTRACE_CONFIG_DIR";

/// The app's config root — `$CODEXTRACE_CONFIG_DIR` when set, else
/// `<OS config dir>/codex-trace`. `None` when the OS has no config dir and no
/// override is given.
pub fn config_root() -> Option<PathBuf> {
    config_root_from(std::env::var(ENV_CONFIG_DIR).ok(), dirs::config_dir())
}

/// Pure core of [`config_root`] for tests.
pub fn config_root_from(
    override_dir: Option<String>,
    os_config: Option<PathBuf>,
) -> Option<PathBuf> {
    match override_dir
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(dir) => Some(PathBuf::from(dir)),
        None => os_config.map(|c| c.join("codex-trace")),
    }
}

fn settings_path() -> Result<PathBuf, String> {
    let root = config_root().ok_or("no config directory")?;
    Ok(root.join("settings.json"))
}

pub fn load_settings() -> Settings {
    settings_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_has_no_sessions_dir() {
        let s = Settings::default();
        assert!(s.sessions_dir.is_none());
    }

    #[test]
    fn deserialize_empty_json_gives_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.sessions_dir.is_none());
        assert!(s.allowed_origins.is_empty());
    }

    #[test]
    fn default_settings_has_no_allowed_origins() {
        assert!(Settings::default().allowed_origins.is_empty());
    }

    #[test]
    fn empty_allowed_origins_omitted_from_json() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(!json.contains("allowed_origins"), "{json}");
    }

    #[test]
    fn allowed_origins_roundtrip_through_json() {
        let s = Settings {
            sessions_dir: None,
            allowed_origins: vec![
                "http://a.example".to_string(),
                "https://b.example".to_string(),
            ],
        };
        let json = serde_json::to_string(&s).unwrap();
        let loaded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.allowed_origins, s.allowed_origins);
    }

    #[test]
    fn settings_written_before_allowed_origins_existed_still_parse() {
        let s: Settings = serde_json::from_str(r#"{"sessions_dir":"/tmp/x"}"#).unwrap();
        assert_eq!(s.sessions_dir.as_deref(), Some("/tmp/x"));
        assert!(s.allowed_origins.is_empty());
    }
}
