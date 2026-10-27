//! Persistent, user-editable engine and preparation configuration.
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

/// Reference-database settings for preparation. These live in a separate file
/// so a player database never needs to know about the reference corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparationPrefs {
    /// Reference PGN, SCID, or classic CBH database. `None` means use the
    /// current database prior with the visible fallback warning.
    pub reference_path: Option<PathBuf>,
    /// Optional correspondence database used as an automatic position
    /// reference in the Engine tab. `None` falls back to
    /// `BRETT_CORRESPONDENCE` or a conventional local path.
    pub correspondence_path: Option<PathBuf>,
    /// Whether the correspondence database should be loaded and shown in the
    /// Engine tab.
    pub correspondence_enabled: bool,
    /// Whether the reference database should be used when preparing.
    pub reference_enabled: bool,
    /// Half-width of the rating window in Elo.
    pub rating_band: u32,
    /// Explicit rating-scale mapping applied to the prepared player's average
    /// Elo before selecting the reference window. Negative values lower the
    /// target for an underrated corpus; positive values raise it.
    pub rating_offset: i32,
    /// The user's aliases in descending priority. The first name replaces
    /// `You` in preparation tags; the first matching name orients the board to
    /// that side when a game is opened.
    pub prep_player_names: Vec<String>,
}

impl Default for PreparationPrefs {
    fn default() -> Self {
        Self {
            reference_path: None,
            correspondence_path: None,
            correspondence_enabled: true,
            reference_enabled: true,
            rating_band: 200,
            rating_offset: 0,
            prep_player_names: Vec::new(),
        }
    }
}

impl PreparationPrefs {
    pub fn from_json(text: &str) -> Self {
        let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let reference_path = value
            .get("reference_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let correspondence_path = value
            .get("correspondence_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let correspondence_enabled = value
            .get("correspondence_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let reference_enabled = value
            .get("reference_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let rating_band = value
            .get("rating_band")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .clamp(25, 1_000) as u32;
        let rating_offset = value
            .get("rating_offset")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(-1_000, 1_000) as i32;
        let prep_player_names = value
            .get("prep_player_names")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .take(100)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            reference_path,
            correspondence_path,
            correspondence_enabled,
            reference_enabled,
            rating_band,
            rating_offset,
            prep_player_names,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "reference_path": self.reference_path.as_ref().map(|path| path.display().to_string()),
            "correspondence_path": self.correspondence_path.as_ref().map(|path| path.display().to_string()),
            "correspondence_enabled": self.correspondence_enabled,
            "reference_enabled": self.reference_enabled,
            "rating_band": self.rating_band,
            "rating_offset": self.rating_offset,
            "prep_player_names": self.prep_player_names,
        }))
        .unwrap_or_default()
    }

    pub fn load() -> Self {
        preparation_path().map_or_else(Self::default, |path| Self::load_from(&path))
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
        let path = preparation_path().context("no XDG config or HOME directory is available")?;
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

/// Which rating tag is preferred when a player has more than one rating.
///
/// The default order is Elo, then DWZ, then USCF. Choosing DWZ or USCF only
/// changes which source wins; the remaining sources still act as fallbacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RatingPriority {
    #[default]
    Elo,
    Dwz,
    Uscf,
}

impl RatingPriority {
    pub fn label(self) -> &'static str {
        match self {
            Self::Elo => "Elo",
            Self::Dwz => "DWZ",
            Self::Uscf => "USCF",
        }
    }

    pub fn config_value(self) -> &'static str {
        match self {
            Self::Elo => "elo",
            Self::Dwz => "dwz",
            Self::Uscf => "uscf",
        }
    }

    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "dwz" => Self::Dwz,
            "uscf" => Self::Uscf,
            _ => Self::Elo,
        }
    }

    /// Rating tags in fallback order, together with the label shown for each.
    pub fn tags(self, color: shakmaty::Color) -> [(&'static str, &'static str); 3] {
        match (self, color) {
            (Self::Elo, shakmaty::Color::White) => [
                ("WhiteElo", "Elo"),
                ("WhiteDWZ", "DWZ"),
                ("WhiteUSCF", "USCF"),
            ],
            (Self::Elo, shakmaty::Color::Black) => [
                ("BlackElo", "Elo"),
                ("BlackDWZ", "DWZ"),
                ("BlackUSCF", "USCF"),
            ],
            (Self::Dwz, shakmaty::Color::White) => [
                ("WhiteDWZ", "DWZ"),
                ("WhiteElo", "Elo"),
                ("WhiteUSCF", "USCF"),
            ],
            (Self::Dwz, shakmaty::Color::Black) => [
                ("BlackDWZ", "DWZ"),
                ("BlackElo", "Elo"),
                ("BlackUSCF", "USCF"),
            ],
            (Self::Uscf, shakmaty::Color::White) => [
                ("WhiteUSCF", "USCF"),
                ("WhiteElo", "Elo"),
                ("WhiteDWZ", "DWZ"),
            ],
            (Self::Uscf, shakmaty::Color::Black) => [
                ("BlackUSCF", "USCF"),
                ("BlackElo", "Elo"),
                ("BlackDWZ", "DWZ"),
            ],
        }
    }
}

/// Display preferences that do not affect database contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayPrefs {
    pub rating_priority: RatingPriority,
    /// Show every available rating (Elo, DWZ, USCF) instead of only the
    /// preferred one.
    pub show_all_ratings: bool,
    /// Engine-tab view: `local`, `cloud`, or `both`.
    pub engine_view: String,
    /// Re-analyze automatically when the position changes. Persisted so the
    /// checkbox keeps its last state across restarts.
    pub engine_auto: bool,
    /// Optional default opening-tree database shown in the Tree tab.
    pub opening_tree_path: String,
    /// Optional native opening book (`.bin`, `.ctg`, or `.abk`) shown in the
    /// Tree tab.
    pub opening_book_path: String,
    /// Maximum number of recent preparation games shown for a position.
    pub prep_recent_limit: usize,
    /// Show PGN fields that are absent or empty. Hidden by default so a game
    /// only displays the information it actually contains.
    pub show_empty_pgn_tags: bool,
    /// Preview a move on the board when hovering it in the open game's move
    /// list. Other previews (tree, engine, references) are always enabled.
    pub hover_moves: bool,
    /// Show player portraits in the board strips when a photo pack is present.
    pub show_player_photos: bool,
    /// Show the start/previous/next/end buttons. Keyboard navigation always
    /// remains available.
    pub show_nav_buttons: bool,
    /// Show the editable PGN-tags card above the move list.
    pub show_pgn_tags: bool,
}

impl Default for DisplayPrefs {
    fn default() -> Self {
        Self {
            rating_priority: RatingPriority::default(),
            show_all_ratings: false,
            engine_view: "both".to_string(),
            engine_auto: true,
            opening_tree_path: String::new(),
            opening_book_path: String::new(),
            prep_recent_limit: 10,
            show_empty_pgn_tags: false,
            hover_moves: false,
            show_player_photos: true,
            show_nav_buttons: false,
            show_pgn_tags: true,
        }
    }
}

impl DisplayPrefs {
    pub fn from_json(text: &str) -> Self {
        let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let rating_priority = value
            .get("rating_priority")
            .and_then(Value::as_str)
            .map(RatingPriority::from_config)
            .unwrap_or_default();
        let show_all_ratings = value
            .get("show_all_ratings")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let engine_view = match value
            .get("engine_view")
            .and_then(Value::as_str)
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("local") => "local",
            Some("cloud") => "cloud",
            _ => "both",
        }
        .to_string();
        let engine_auto = value
            .get("engine_auto")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let opening_tree_path = value
            .get("opening_tree_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let opening_book_path = value
            .get("opening_book_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let prep_recent_limit = value
            .get("prep_recent_limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .clamp(1, 50) as usize;
        let show_empty_pgn_tags = value
            .get("show_empty_pgn_tags")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let hover_moves = value
            .get("hover_moves")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let show_player_photos = value
            .get("show_player_photos")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let show_nav_buttons = value
            .get("show_nav_buttons")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let show_pgn_tags = value
            .get("show_pgn_tags")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Self {
            rating_priority,
            show_all_ratings,
            engine_view,
            engine_auto,
            opening_tree_path,
            opening_book_path,
            prep_recent_limit,
            show_empty_pgn_tags,
            hover_moves,
            show_player_photos,
            show_nav_buttons,
            show_pgn_tags,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "rating_priority": self.rating_priority.config_value(),
            "show_all_ratings": self.show_all_ratings,
            "engine_view": self.engine_view,
            "engine_auto": self.engine_auto,
            "opening_tree_path": self.opening_tree_path,
            "opening_book_path": self.opening_book_path,
            "prep_recent_limit": self.prep_recent_limit,
            "show_empty_pgn_tags": self.show_empty_pgn_tags,
            "hover_moves": self.hover_moves,
            "show_player_photos": self.show_player_photos,
            "show_nav_buttons": self.show_nav_buttons,
            "show_pgn_tags": self.show_pgn_tags,
        }))
        .unwrap_or_default()
    }

    pub fn load() -> Self {
        display_path().map_or_else(Self::default, |path| Self::load_from(&path))
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
        let path = display_path().context("no XDG config or HOME directory is available")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, self.to_json()).with_context(|| format!("writing {}", path.display()))
    }
}

/// Optional credentials for the external reference tab.
///
/// Lichess opening-explorer requests are authenticated. A token with no
/// additional scopes is enough. The environment variable
/// `BRETT_LICHESS_TOKEN` takes precedence, which keeps the token out of the
/// config file for users who prefer that.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloudPrefs {
    pub lichess_token: Option<String>,
    pub explorer_source: crate::cloud::ExplorerSource,
    /// Optional default account for the online-reference card.
    pub lichess_username: Option<String>,
    /// Optional default Chess.com account for the online-reference card.
    /// Public Chess.com game data does not require a password.
    pub chesscom_username: Option<String>,
}

impl CloudPrefs {
    pub fn from_json(text: &str) -> Self {
        let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let lichess_token = value
            .get("lichess_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        let explorer_source = value
            .get("explorer_source")
            .and_then(Value::as_str)
            .map(crate::cloud::ExplorerSource::from_config)
            .unwrap_or_default();
        let username = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        };
        Self {
            lichess_token,
            explorer_source,
            lichess_username: username("lichess_username"),
            chesscom_username: username("chesscom_username"),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "lichess_token": self.lichess_token,
            "explorer_source": self.explorer_source.config_value(),
            "lichess_username": self.lichess_username,
            "chesscom_username": self.chesscom_username,
        }))
        .unwrap_or_default()
    }

    pub fn load() -> Self {
        let mut prefs = cloud_path().map_or_else(Self::default, |path| Self::load_from(&path));
        if let Ok(token) = std::env::var("BRETT_LICHESS_TOKEN") {
            let token = token.trim();
            if !token.is_empty() {
                prefs.lichess_token = Some(token.to_string());
            }
        }
        prefs
    }

    /// Persist only the explorer choice, leaving the token from the config
    /// file untouched. In particular, an environment token is not copied to
    /// disk just because the corpus was changed.
    pub fn save_explorer_source(source: crate::cloud::ExplorerSource) -> Result<()> {
        let path = cloud_path().context("no XDG config or HOME directory is available")?;
        let mut prefs = match fs::read_to_string(&path) {
            Ok(text) => Self::from_json(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        prefs.explorer_source = source;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, prefs.to_json()).with_context(|| format!("writing {}", path.display()))
    }

    /// Persist only the Lichess token, leaving the explorer and account
    /// choices untouched. Passing `None` removes the token from the file.
    pub fn save_token(token: Option<&str>) -> Result<()> {
        let path = cloud_path().context("no XDG config or HOME directory is available")?;
        let mut prefs = match fs::read_to_string(&path) {
            Ok(text) => Self::from_json(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        prefs.lichess_token = token
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, prefs.to_json()).with_context(|| format!("writing {}", path.display()))
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
}

/// `$XDG_CONFIG_HOME/brett/engine.json`, or `$HOME/.config/...`.
pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|path| path.join("engine.json"))
}

/// `$XDG_CONFIG_HOME/brett/references.json`, or `$HOME/.config/...`.
pub fn cloud_path() -> Option<PathBuf> {
    config_dir().map(|path| path.join("references.json"))
}

/// `$XDG_CONFIG_HOME/brett/preparation.json`, or `$HOME/.config/...`.
pub fn preparation_path() -> Option<PathBuf> {
    config_dir().map(|path| path.join("preparation.json"))
}

/// `$XDG_CONFIG_HOME/brett/display.json`, or `$HOME/.config/...`.
pub fn display_path() -> Option<PathBuf> {
    config_dir().map(|path| path.join("display.json"))
}

fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })?;
    Some(base.join("brett"))
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
    fn preparation_prefs_round_trip_and_clamp() {
        let prefs = PreparationPrefs {
            reference_path: Some(PathBuf::from("/data/masters.pgn")),
            correspondence_path: Some(PathBuf::from("/data/correspondence.si4")),
            correspondence_enabled: false,
            reference_enabled: false,
            rating_band: 250,
            rating_offset: 75,
            prep_player_names: vec!["Carlsen, Magnus".to_string()],
        };
        assert_eq!(PreparationPrefs::from_json(&prefs.to_json()), prefs);

        let parsed = PreparationPrefs::from_json(
            r#"{"reference_path":" /tmp/ref.pgn ","reference_enabled":true,"rating_band":9999,"rating_offset":-9999}"#,
        );
        assert_eq!(parsed.reference_path, Some(PathBuf::from("/tmp/ref.pgn")));
        assert_eq!(parsed.rating_band, 1_000);
        assert_eq!(parsed.rating_offset, -1_000);
    }

    #[test]
    fn cloud_prefs_round_trip_and_trim() {
        let prefs = CloudPrefs {
            lichess_token: Some("lip_abc123".to_string()),
            explorer_source: crate::cloud::ExplorerSource::Lichess,
            lichess_username: Some("DrNykterstein".to_string()),
            chesscom_username: Some("MagnusCarlsen".to_string()),
        };
        assert_eq!(CloudPrefs::from_json(&prefs.to_json()), prefs);

        let parsed = CloudPrefs::from_json(r#"{"lichess_token":"  lip_trim  "}"#);
        assert_eq!(parsed.lichess_token.as_deref(), Some("lip_trim"));
        assert_eq!(
            parsed.explorer_source,
            crate::cloud::ExplorerSource::Masters
        );
        assert_eq!(
            CloudPrefs::from_json(r#"{"explorer_source":"lichess"}"#).explorer_source,
            crate::cloud::ExplorerSource::Lichess
        );
        assert_eq!(
            CloudPrefs::from_json(r#"{"lichess_token":"   "}"#).lichess_token,
            None
        );
        assert_eq!(
            CloudPrefs::from_json(r#"{"chesscom_username":"  Hikaru  "}"#)
                .chesscom_username
                .as_deref(),
            Some("Hikaru")
        );
    }

    #[test]
    fn display_prefs_round_trip_and_default() {
        let prefs = DisplayPrefs {
            rating_priority: RatingPriority::Dwz,
            show_all_ratings: true,
            engine_view: "cloud".to_string(),
            engine_auto: false,
            opening_tree_path: "/tmp/masters.pgn".to_string(),
            opening_book_path: "/tmp/perfect.abk".to_string(),
            prep_recent_limit: 25,
            show_empty_pgn_tags: true,
            hover_moves: true,
            show_player_photos: false,
            show_nav_buttons: true,
            show_pgn_tags: false,
        };
        assert_eq!(DisplayPrefs::from_json(&prefs.to_json()), prefs);
        assert_eq!(DisplayPrefs::from_json("not json"), DisplayPrefs::default());
        assert_eq!(
            DisplayPrefs::from_json(r#"{"rating_priority":"uscf"}"#).rating_priority,
            RatingPriority::Uscf
        );
        assert!(!DisplayPrefs::default().show_empty_pgn_tags);
        assert!(!DisplayPrefs::default().hover_moves);
        assert!(DisplayPrefs::default().show_player_photos);
        assert!(!DisplayPrefs::default().show_nav_buttons);
        assert!(DisplayPrefs::default().show_pgn_tags);
        assert!(!DisplayPrefs::default().show_all_ratings);
        assert!(DisplayPrefs::default().engine_auto);
        assert!(!DisplayPrefs::from_json(r#"{"engine_auto":false}"#).engine_auto);
        assert_eq!(DisplayPrefs::default().engine_view, "both");
        assert_eq!(
            DisplayPrefs::from_json(r#"{"engine_view":"CLOUD"}"#).engine_view,
            "cloud"
        );
        assert_eq!(
            DisplayPrefs::from_json(r#"{"engine_view":"nonsense"}"#).engine_view,
            "both"
        );
    }

    #[test]
    fn preparation_and_display_prefs_clamp_new_fields() {
        let parsed = PreparationPrefs::from_json(
            r#"{"correspondence_enabled":false,"prep_player_names":[" Carlsen, Magnus ","","Nakamura, Hikaru"]}"#,
        );
        assert!(!parsed.correspondence_enabled);
        assert_eq!(
            parsed.prep_player_names,
            vec![
                "Carlsen, Magnus".to_string(),
                "Nakamura, Hikaru".to_string()
            ]
        );
        let display = DisplayPrefs::from_json(
            r#"{"opening_book_path":" /tmp/book.ctg ","prep_recent_limit":999}"#,
        );
        assert_eq!(display.opening_book_path, "/tmp/book.ctg");
        assert_eq!(display.prep_recent_limit, 50);
        assert_eq!(
            DisplayPrefs::from_json(r#"{"prep_recent_limit":0}"#).prep_recent_limit,
            1
        );
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
