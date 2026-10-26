//! Persistent node configuration, stored as a single TOML file next to the
//! node identity (`bitchatd.toml` by default, `BITCHATD_CONFIG` overrides).
//!
//! Centralizing every durable preference in one file keeps runtime state out
//! of the repository and leaves room for future settings (favorite
//! conversations, window layout, notification policy, …) without multiplying
//! files. The legacy `bitchatd.nickname` file is still read once as a
//! migration source when no TOML exists yet.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    pub node: NodeSection,
    pub ui: UiSection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeSection {
    /// Display nickname announced to the mesh. `None` = built-in default.
    pub nickname: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSection {
    /// Chat transcript layout (`bubbles` or `matrix`).
    pub theme: Option<ChatTheme>,
}

/// Chat transcript presentation, mirroring the mobile `ChatUiMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatTheme {
    /// Classic messenger bubbles: own messages right (no name), others left
    /// with their `@nickname`, both with a greyed timestamp bottom-right.
    #[default]
    Bubbles,
    /// Terminal-style flat transcript: `@nickname`, message below, timestamp
    /// greyed and smaller on the right, everything left-aligned.
    Matrix,
}

impl NodeConfig {
    /// Path of the config file: `BITCHATD_CONFIG`, otherwise
    /// `bitchatd.toml` beside the identity file.
    pub fn path(identity_path: &Path) -> PathBuf {
        if let Ok(path) = std::env::var("BITCHATD_CONFIG") {
            return PathBuf::from(path);
        }
        identity_path.with_file_name("bitchatd.toml")
    }

    /// Load the configuration; missing or malformed files fall back to
    /// defaults. A legacy `bitchatd.nickname` next to the identity migrates
    /// into `node.nickname` when the TOML does not exist yet.
    pub fn load(identity_path: &Path) -> Self {
        let path = Self::path(identity_path);
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(config) = toml::from_str::<NodeConfig>(&contents) {
                return config;
            }
        }
        let mut config = NodeConfig::default();
        let legacy = identity_path.with_file_name("bitchatd.nickname");
        if let Ok(nickname) = std::fs::read_to_string(&legacy) {
            let nickname = nickname.trim().to_string();
            if !nickname.is_empty() {
                config.node.nickname = Some(nickname);
            }
        }
        config
    }

    pub fn save(&self, identity_path: &Path) -> std::io::Result<()> {
        let path = Self::path(identity_path);
        let contents = toml::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        std::fs::write(path, contents)
    }

    /// Effective nickname: config value or the built-in default.
    pub fn nickname_or_default(&self) -> String {
        self.node
            .nickname
            .clone()
            .map(|nick| nick.trim().to_string())
            .filter(|nick| !nick.is_empty())
            .unwrap_or_else(|| "bitchatd".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_default() {
        let dir = std::env::temp_dir().join(format!("bitchatd-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let identity = dir.join("bitchatd.identity");

        let mut config = NodeConfig::default();
        assert_eq!(config.nickname_or_default(), "bitchatd");
        config.node.nickname = Some("momo".to_string());
        config.save(&identity).unwrap();

        let loaded = NodeConfig::load(&identity);
        assert_eq!(loaded.nickname_or_default(), "momo");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_nickname_migrates() {
        let dir = std::env::temp_dir().join(format!("bitchatd-config-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let identity = dir.join("bitchatd.identity");
        std::fs::write(dir.join("bitchatd.nickname"), "pixmelt-pc\n").unwrap();

        let loaded = NodeConfig::load(&identity);
        assert_eq!(loaded.nickname_or_default(), "pixmelt-pc");

        std::fs::remove_dir_all(&dir).ok();
    }
}
