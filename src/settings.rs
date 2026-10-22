//! Persistent, user-editable engine configuration.
//!
//! Settings live under the XDG config directory so database folders stay
//! untouched. The file is intentionally small and human-readable JSON.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::engine::EngineOptions;

/// Engine settings saved between Brett sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnginePrefs {
    /// Explicit executable path. `None` means use `BRETT_ENGINE` or built-in
    /// Stockfish default.
    pub path: Option<PathBuf>,
    pub threads: u32,
    pub hash_mb: u32,
}

impl Default for EnginePrefs {
    fn default() -> Self {
        Self {
            path: None,
            threads: 1,
            hash_mb: 16,
        }
    }
}

impl EnginePrefs {
    pub fn resolved_path(&self) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(crate::engine::default_engine_path)
    }

    pub fn options(&self) -> EngineOptions {
        EngineOptions {
            threads: self.threads.clamp(1, 256),
            hash_mb: self.hash_mb.clamp(1, 32_768),
        }
    }

    /// Parse a config file. Unknown or malformed fields fall back to defaults;
    /// the next save writes a clean file.
    pub fn from_json(text: &str) -> Self {
        let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let path = value
            .get("engine_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let threads = value
            .get("threads")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .clamp(1, 256) as u32;
        let hash_mb = value
            .get("hash_mb")
            .and_then(Value::as_u64)
            .unwrap_or(16)
            .clamp(1, 32_768) as u32;
        Self {
            path,
            threads,
            hash_mb,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "engine_path": self.path.as_ref().map(|path| path.display().to_string()),
            "threads": self.threads,
            "hash_mb": self.hash_mb,
        }))
        .unwrap_or_default()
    }

    pub fn load() -> Self {
        config_path().map_or_else(Self::default, |path| Self::load_from(&path))
    }

    fn load_from(path: &std::path::Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!("could not read {}: {err:#}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path().context("no XDG config or HOME directory is available")?;
        self.save_to(&path)
    }

    fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(path, self.to_json()).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// `$XDG_CONFIG_HOME/brett/engine.json`, or `$HOME/.config/...`.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })?;
    Some(base.join("brett").join("engine.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn engine_prefs_round_trip() {
        let prefs = EnginePrefs {
            path: Some(PathBuf::from("/opt/engines/stockfish")),
            threads: 6,
            hash_mb: 1024,
        };
        assert_eq!(EnginePrefs::from_json(&prefs.to_json()), prefs);
    }

    #[test]
    fn engine_prefs_clamp_and_default_on_bad_input() {
        let parsed = EnginePrefs::from_json(
            r#"{"engine_path":" /tmp/engine ","threads":0,"hash_mb":999999}"#,
        );
        assert_eq!(parsed.path, Some(PathBuf::from("/tmp/engine")));
        assert_eq!(parsed.threads, 1);
        assert_eq!(parsed.hash_mb, 32_768);
        assert_eq!(EnginePrefs::from_json("not json"), EnginePrefs::default());
    }

    #[test]
    fn engine_prefs_save_and_load_file() {
        let dir = std::env::temp_dir().join(format!(
            "brett-settings-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("brett/engine.json");
        let prefs = EnginePrefs {
            path: Some(PathBuf::from("/opt/engines/stockfish")),
            threads: 4,
            hash_mb: 256,
        };
        assert_eq!(EnginePrefs::load_from(&path), EnginePrefs::default());
        prefs.save_to(&path).unwrap();
        assert_eq!(EnginePrefs::load_from(&path), prefs);
        fs::remove_dir_all(dir).unwrap();
    }
}
