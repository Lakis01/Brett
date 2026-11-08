//! Persistent, user-editable engine and preparation configuration.
//!
//! Settings live under the XDG config directory so database folders stay
//! untouched. The file is intentionally small and human-readable JSON.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::engine::EngineOptions;

/// Configuration for an individual engine slot (Hotkeys 1, 2, 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineSlotConfig {
    pub path: Option<PathBuf>,
    pub threads: u32,
    pub hash_mb: u32,
    pub enabled: bool,
}

impl Default for EngineSlotConfig {
    fn default() -> Self {
        Self {
            path: None,
            threads: 1,
            hash_mb: 16,
            enabled: false,
        }
    }
}

impl EngineSlotConfig {
    pub fn resolved_path(&self, fallback: Option<PathBuf>) -> PathBuf {
        self.path
            .clone()
            .or(fallback)
            .unwrap_or_else(crate::engine::default_engine_path)
    }

    pub fn options(&self) -> EngineOptions {
        EngineOptions {
            threads: self.threads.clamp(1, 256),
            hash_mb: self.hash_mb.clamp(1, 32_768),
        }
    }
}

/// Engine settings saved between Brett sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnginePrefs {
    /// Explicit executable path. `None` means use `BRETT_ENGINE` or built-in
    /// Stockfish default.
    pub path: Option<PathBuf>,
    pub threads: u32,
    pub hash_mb: u32,
    /// Whether locally analyzed positions are stored in the local SQLite cache.
    pub cache_engine_analysis: bool,
    /// Whether fine-grained Stockfish quality comparison is used to supersede entries.
    pub heuristic_supersede: bool,
    /// Up to 3 parallel engine slots, bound to hotkeys 1, 2, 3.
    pub engines: [EngineSlotConfig; 3],
}

impl Default for EnginePrefs {
    fn default() -> Self {
        Self {
            path: None,
            threads: 1,
            hash_mb: 16,
            cache_engine_analysis: true,
            heuristic_supersede: true,
            engines: [
                EngineSlotConfig {
                    path: None,
                    threads: 1,
                    hash_mb: 16,
                    enabled: true,
                },
                EngineSlotConfig {
                    path: None,
                    threads: 1,
                    hash_mb: 16,
                    enabled: false,
                },
                EngineSlotConfig {
                    path: None,
                    threads: 1,
                    hash_mb: 16,
                    enabled: false,
                },
            ],
        }
    }
}

impl EnginePrefs {
    pub fn new(
        path: Option<PathBuf>,
        threads: u32,
        hash_mb: u32,
        cache_engine_analysis: bool,
        heuristic_supersede: bool,
    ) -> Self {
        let engines = [
            EngineSlotConfig {
                path: path.clone(),
                threads,
                hash_mb,
                enabled: true,
            },
            EngineSlotConfig::default(),
            EngineSlotConfig::default(),
        ];
        Self {
            path,
            threads,
            hash_mb,
            cache_engine_analysis,
            heuristic_supersede,
            engines,
        }
    }

    pub fn resolved_path(&self) -> PathBuf {
        self.engines[0]
            .path
            .clone()
            .or_else(|| self.path.clone())
            .unwrap_or_else(crate::engine::default_engine_path)
    }

    pub fn options(&self) -> EngineOptions {
        self.engines[0].options()
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
        let cache_engine_analysis = value
            .get("cache_engine_analysis")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let heuristic_supersede = value
            .get("heuristic_supersede")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let mut engines = [
            EngineSlotConfig {
                path: path.clone(),
                threads,
                hash_mb,
                enabled: true,
            },
            EngineSlotConfig::default(),
            EngineSlotConfig::default(),
        ];

        if let Some(arr) = value.get("engines").and_then(Value::as_array) {
            for (i, item) in arr.iter().take(3).enumerate() {
                let p = item
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from);
                let t = item
                    .get("threads")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .clamp(1, 256) as u32;
                let h = item
                    .get("hash_mb")
                    .and_then(Value::as_u64)
                    .unwrap_or(16)
                    .clamp(1, 32_768) as u32;
                let en = item
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(i == 0);
                engines[i] = EngineSlotConfig {
                    path: p,
                    threads: t,
                    hash_mb: h,
                    enabled: en,
                };
            }
        }

        Self {
            path,
            threads,
            hash_mb,
            cache_engine_analysis,
            heuristic_supersede,
            engines,
        }
    }

    pub fn to_json(&self) -> String {
        let engines_json: Vec<Value> = self
            .engines
            .iter()
            .map(|e| {
                json!({
                    "path": e.path.as_ref().map(|path| path.display().to_string()),
                    "threads": e.threads,
                    "hash_mb": e.hash_mb,
                    "enabled": e.enabled,
                })
            })
            .collect();

        serde_json::to_string_pretty(&json!({
            "engine_path": self.path.as_ref().map(|path| path.display().to_string()),
            "threads": self.threads,
            "hash_mb": self.hash_mb,
            "cache_engine_analysis": self.cache_engine_analysis,
            "heuristic_supersede": self.heuristic_supersede,
            "engines": engines_json,
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

/// Which preparation panel is shown in the Player tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrepMode {
    Classic,
    #[default]
    Smart,
}

impl PrepMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic Stats",
            Self::Smart => "Smart Predictor",
        }
    }

    pub fn config_value(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Smart => "smart",
        }
    }

    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "classic" => Self::Classic,
            _ => Self::Smart,
        }
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
    /// Whether online Chess.com tournaments should be excluded during preparation.
    pub exclude_chesscom: bool,
    /// Whether the prompt to download the correspondence reference has been dismissed.
    pub correspondence_download_dismissed: bool,
    /// Whether the startup prompt to pick a default opening book and reference
    /// database has been dismissed ("No & don't ask again").
    pub reference_setup_dismissed: bool,
    /// Whether online games are included in Smart preparation by default.
    /// The toggle is instant (no refetch); this only stores the default.
    pub prep_include_online: bool,
    /// Default preparation panel mode.
    pub prep_mode: PrepMode,
    /// Online accounts per prepared player, keyed by canonical player key.
    pub prep_online_accounts:
        std::collections::HashMap<String, Vec<crate::players::PrepOnlineAccount>>,
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
            exclude_chesscom: false,
            correspondence_download_dismissed: false,
            reference_setup_dismissed: false,
            prep_include_online: true,
            prep_mode: PrepMode::default(),
            prep_online_accounts: std::collections::HashMap::new(),
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
        let exclude_chesscom = value
            .get("exclude_chesscom")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let correspondence_download_dismissed = value
            .get("correspondence_download_dismissed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let reference_setup_dismissed = value
            .get("reference_setup_dismissed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let prep_include_online = value
            .get("prep_include_online")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let prep_mode = value
            .get("prep_mode")
            .and_then(Value::as_str)
            .map(PrepMode::from_config)
            .unwrap_or_default();
        let prep_online_accounts = value
            .get("prep_online_accounts")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(key, accounts)| {
                        let accounts = accounts
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(crate::players::PrepOnlineAccount::from_value)
                                    .take(20)
                                    .collect()
                            })
                            .unwrap_or_default();
                        (key.clone(), accounts)
                    })
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
            exclude_chesscom,
            correspondence_download_dismissed,
            reference_setup_dismissed,
            prep_include_online,
            prep_mode,
            prep_online_accounts,
        }
    }

    pub fn to_json(&self) -> String {
        let accounts: Value = self
            .prep_online_accounts
            .iter()
            .map(|(key, accounts)| {
                (
                    key.clone(),
                    Value::Array(accounts.iter().map(|account| account.to_value()).collect()),
                )
            })
            .collect::<serde_json::Map<String, Value>>()
            .into();
        serde_json::to_string_pretty(&json!({
            "reference_path": self.reference_path.as_ref().map(|path| path.display().to_string()),
            "correspondence_path": self.correspondence_path.as_ref().map(|path| path.display().to_string()),
            "correspondence_enabled": self.correspondence_enabled,
            "reference_enabled": self.reference_enabled,
            "rating_band": self.rating_band,
            "rating_offset": self.rating_offset,
            "prep_player_names": self.prep_player_names,
            "exclude_chesscom": self.exclude_chesscom,
            "correspondence_download_dismissed": self.correspondence_download_dismissed,
            "reference_setup_dismissed": self.reference_setup_dismissed,
            "prep_include_online": self.prep_include_online,
            "prep_mode": self.prep_mode.config_value(),
            "prep_online_accounts": accounts,
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
    pub fn tags(self, color: crate::chess::Color) -> [(&'static str, &'static str); 3] {
        match (self, color) {
            (Self::Elo, crate::chess::Color::White) => [
                ("WhiteElo", "Elo"),
                ("WhiteDWZ", "DWZ"),
                ("WhiteUSCF", "USCF"),
            ],
            (Self::Elo, crate::chess::Color::Black) => [
                ("BlackElo", "Elo"),
                ("BlackDWZ", "DWZ"),
                ("BlackUSCF", "USCF"),
            ],
            (Self::Dwz, crate::chess::Color::White) => [
                ("WhiteDWZ", "DWZ"),
                ("WhiteElo", "Elo"),
                ("WhiteUSCF", "USCF"),
            ],
            (Self::Dwz, crate::chess::Color::Black) => [
                ("BlackDWZ", "DWZ"),
                ("BlackElo", "Elo"),
                ("BlackUSCF", "USCF"),
            ],
            (Self::Uscf, crate::chess::Color::White) => [
                ("WhiteUSCF", "USCF"),
                ("WhiteElo", "Elo"),
                ("WhiteDWZ", "DWZ"),
            ],
            (Self::Uscf, crate::chess::Color::Black) => [
                ("BlackUSCF", "USCF"),
                ("BlackElo", "Elo"),
                ("BlackDWZ", "DWZ"),
            ],
        }
    }
}

/// Portrait size used by the board strips and the selected-player header.
///
/// The Players list keeps a fixed small thumbnail so rows stay uniform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PhotoSize {
    #[default]
    Small,
    Medium,
    Large,
}

impl PhotoSize {
    pub const ALL: [Self; 3] = [Self::Small, Self::Medium, Self::Large];

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
        }
    }

    /// Avatar edge length in logical pixels.
    pub fn pixels(self) -> f32 {
        match self {
            Self::Small => 38.0,
            Self::Medium => 54.0,
            Self::Large => 72.0,
        }
    }

    pub fn config_value(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "medium" => Self::Medium,
            "large" => Self::Large,
            _ => Self::Small,
        }
    }
}

/// Which championship lineage counts for the crown badge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChampionLineage {
    /// The uninterrupted classical line from Steinitz to the present.
    Classical,
    /// The classical line plus the FIDE knockout champions from 1999–2006.
    #[default]
    Fide,
}

impl ChampionLineage {
    pub const ALL: [Self; 2] = [Self::Fide, Self::Classical];

    pub fn label(self) -> &'static str {
        match self {
            Self::Classical => "Classical/PCA",
            Self::Fide => "FIDE/inclusive",
        }
    }

    pub fn config_value(self) -> &'static str {
        match self {
            Self::Classical => "classical",
            Self::Fide => "fide",
        }
    }

    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "classical" | "pca" => Self::Classical,
            _ => Self::Fide,
        }
    }
}

/// Display preferences that do not affect database contents.
/// Customizable font colors for the 9 game categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameCategoryColors {
    pub otb_classical: [u8; 3],
    pub otb_rapid: [u8; 3],
    pub otb_blitz: [u8; 3],
    pub online_classical: [u8; 3],
    pub online_rapid: [u8; 3],
    pub online_blitz: [u8; 3],
    pub online_bullet: [u8; 3],
    pub correspondence: [u8; 3],
    pub unknown: [u8; 3],
}

impl Default for GameCategoryColors {
    fn default() -> Self {
        Self {
            otb_classical: [0x7E, 0xD3, 0x8A],
            otb_rapid: [0x56, 0xB6, 0xC2],
            otb_blitz: [0x61, 0xAF, 0xEF],
            online_classical: [0xB4, 0x8E, 0xAD],
            online_rapid: [0xC6, 0x78, 0xDD],
            online_blitz: [0xE5, 0xC0, 0x7B],
            online_bullet: [0xE0, 0x6C, 0x75],
            correspondence: [0xD1, 0x9A, 0x66],
            unknown: [0xEC, 0xEF, 0xF4],
        }
    }
}

impl GameCategoryColors {
    pub fn color_for(&self, cat: crate::players::GameCategory) -> [u8; 3] {
        match cat {
            crate::players::GameCategory::OtbClassical => self.otb_classical,
            crate::players::GameCategory::OtbRapid => self.otb_rapid,
            crate::players::GameCategory::OtbBlitz => self.otb_blitz,
            crate::players::GameCategory::OnlineClassical => self.online_classical,
            crate::players::GameCategory::OnlineRapid => self.online_rapid,
            crate::players::GameCategory::OnlineBlitz => self.online_blitz,
            crate::players::GameCategory::OnlineBullet => self.online_bullet,
            crate::players::GameCategory::Correspondence => self.correspondence,
            crate::players::GameCategory::Unknown => self.unknown,
        }
    }

    pub fn set_color_for(&mut self, cat: crate::players::GameCategory, rgb: [u8; 3]) {
        match cat {
            crate::players::GameCategory::OtbClassical => self.otb_classical = rgb,
            crate::players::GameCategory::OtbRapid => self.otb_rapid = rgb,
            crate::players::GameCategory::OtbBlitz => self.otb_blitz = rgb,
            crate::players::GameCategory::OnlineClassical => self.online_classical = rgb,
            crate::players::GameCategory::OnlineRapid => self.online_rapid = rgb,
            crate::players::GameCategory::OnlineBlitz => self.online_blitz = rgb,
            crate::players::GameCategory::OnlineBullet => self.online_bullet = rgb,
            crate::players::GameCategory::Correspondence => self.correspondence = rgb,
            crate::players::GameCategory::Unknown => self.unknown = rgb,
        }
    }
}

fn rgb_to_hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

fn hex_to_rgb(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b])
}

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
    /// Folder of the last database opened or saved, used as the initial
    /// location for database pickers.
    pub last_database_dir: Option<PathBuf>,
    /// Folder of the last game exported for printing, used as the initial
    /// location for print exports.
    pub last_print_dir: Option<PathBuf>,
    /// Last 5 loaded databases or opening books.
    pub recent_files: Vec<PathBuf>,
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
    /// Portrait size for board strips and the selected-player header.
    pub photo_size: PhotoSize,
    /// Championship lineage used for the world-champion crown.
    pub champion_lineage: ChampionLineage,
    /// Show the start/previous/next/end buttons. Keyboard navigation always
    /// remains available.
    pub show_nav_buttons: bool,
    /// Show the editable PGN-tags card above the move list.
    pub show_pgn_tags: bool,
    /// Show the move annotations and comment editor card below the move list.
    pub show_annotations: bool,
    /// Show PGN clock times in the board strips when the game has clocks.
    pub show_clocks: bool,
    /// Show the vertical evaluation bar beside the board.
    pub engine_bar: bool,
    /// Skip leading opening-book moves when running game review.
    pub review_skip_book: bool,
    /// Minimum games in the local opening tree or Lichess Masters explorer to
    /// treat a move as book during review (`1..=100`).
    pub review_book_min_games: u32,
    /// Maximum plies from the start of the game that can be skipped as book
    /// during review (`4..=80`).
    pub review_book_max_plies: u32,
    /// Rolling recent window used for move-popularity trends in the Tree tab.
    pub trend_window: crate::recent::TrendWindow,
    /// Historical baseline period used for move-popularity trends.
    pub trend_baseline: crate::recent::TrendBaseline,
    /// Minimum dated games required in both windows at a position before
    /// showing a trend badge (`3..=200`).
    pub trend_min_games: u32,
    /// Corpus used to compute move-popularity trends.
    pub trend_source: crate::recent::TrendSource,
    /// Engine search limit mode: `infinite` (default), `depth`, or `time`.
    pub engine_limit_mode: String,
    /// Target depth when limit mode is `depth`.
    pub engine_depth: u32,
    /// Target milliseconds when limit mode is `time`.
    pub engine_time_ms: u64,
    /// When preparation is active, restrict the left game list to games where
    /// the opponent plays the facing colour. The chip offers Show all.
    pub prep_filter_facing_color: bool,
    /// Customizable game category font colors.
    pub category_colors: GameCategoryColors,
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
            last_database_dir: None,
            last_print_dir: None,
            recent_files: Vec::new(),
            prep_recent_limit: 10,
            show_empty_pgn_tags: false,
            hover_moves: false,
            show_player_photos: true,
            photo_size: PhotoSize::default(),
            champion_lineage: ChampionLineage::default(),
            show_nav_buttons: false,
            show_pgn_tags: true,
            show_annotations: true,
            show_clocks: true,
            engine_bar: true,
            prep_filter_facing_color: true,
            category_colors: GameCategoryColors::default(),
            review_skip_book: true,
            review_book_min_games: 3,
            review_book_max_plies: 30,
            trend_window: crate::recent::TrendWindow::default(),
            trend_baseline: crate::recent::TrendBaseline::default(),
            trend_min_games: 10,
            trend_source: crate::recent::TrendSource::default(),
            engine_limit_mode: "infinite".to_string(),
            engine_depth: 25,
            engine_time_ms: 1_500,
        }
    }
}

impl DisplayPrefs {
    pub fn trend_config(&self) -> crate::recent::TrendConfig {
        crate::recent::TrendConfig {
            window: self.trend_window,
            baseline: self.trend_baseline,
            min_recent_games: self.trend_min_games.clamp(3, 200),
        }
    }

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
        let last_database_dir = value
            .get("last_database_dir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let last_print_dir = value
            .get("last_print_dir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let recent_files = value
            .get("recent_files")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from)
                    .take(5)
                    .collect()
            })
            .unwrap_or_default();
        let prep_recent_limit = value
            .get("prep_recent_limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .clamp(1, 100) as usize;
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
        let photo_size = value
            .get("photo_size")
            .and_then(Value::as_str)
            .map(PhotoSize::from_config)
            .unwrap_or_default();
        let champion_lineage = value
            .get("champion_lineage")
            .and_then(Value::as_str)
            .map(ChampionLineage::from_config)
            .unwrap_or_default();
        let show_nav_buttons = value
            .get("show_nav_buttons")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let show_pgn_tags = value
            .get("show_pgn_tags")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let show_annotations = value
            .get("show_annotations")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let show_clocks = value
            .get("show_clocks")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let engine_bar = value
            .get("engine_bar")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let review_skip_book = value
            .get("review_skip_book")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let review_book_min_games = value
            .get("review_book_min_games")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .clamp(1, 100) as u32;
        let review_book_max_plies = value
            .get("review_book_max_plies")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(4, 80) as u32;
        let trend_window = value
            .get("trend_window")
            .and_then(Value::as_str)
            .map(crate::recent::TrendWindow::from_config)
            .unwrap_or_default();
        let trend_baseline = value
            .get("trend_baseline")
            .and_then(Value::as_str)
            .map(crate::recent::TrendBaseline::from_config)
            .unwrap_or_default();
        let trend_min_games = value
            .get("trend_min_games")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .clamp(3, 200) as u32;
        let trend_source = value
            .get("trend_source")
            .and_then(Value::as_str)
            .map(crate::recent::TrendSource::from_config)
            .unwrap_or_default();
        let engine_limit_mode = value
            .get("engine_limit_mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("infinite")
            .to_string();
        let engine_depth = value
            .get("engine_depth")
            .and_then(Value::as_u64)
            .unwrap_or(25)
            .clamp(1, 100) as u32;
        let engine_time_ms = value
            .get("engine_time_ms")
            .and_then(Value::as_u64)
            .unwrap_or(1_500)
            .clamp(100, 60_000);
        let prep_filter_facing_color = value
            .get("prep_filter_facing_color")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let mut category_colors = GameCategoryColors::default();
        if let Some(colors_obj) = value.get("category_colors") {
            let parse = |key: &str| {
                if let Some(s) = colors_obj.get(key).and_then(Value::as_str) {
                    hex_to_rgb(s)
                } else if let Some(arr) = colors_obj.get(key).and_then(Value::as_array) {
                    if arr.len() == 3 {
                        let r = arr[0].as_u64()? as u8;
                        let g = arr[1].as_u64()? as u8;
                        let b = arr[2].as_u64()? as u8;
                        Some([r, g, b])
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(c) = parse("otb_classical") {
                category_colors.otb_classical = c;
            }
            if let Some(c) = parse("otb_rapid") {
                category_colors.otb_rapid = c;
            }
            if let Some(c) = parse("otb_blitz") {
                category_colors.otb_blitz = c;
            }
            if let Some(c) = parse("online_classical") {
                category_colors.online_classical = c;
            }
            if let Some(c) = parse("online_rapid") {
                category_colors.online_rapid = c;
            }
            if let Some(c) = parse("online_blitz") {
                category_colors.online_blitz = c;
            }
            if let Some(c) = parse("online_bullet") {
                category_colors.online_bullet = c;
            }
            if let Some(c) = parse("correspondence") {
                category_colors.correspondence = c;
            }
            if let Some(c) = parse("unknown") {
                category_colors.unknown = c;
            }
        }
        Self {
            rating_priority,
            show_all_ratings,
            engine_view,
            engine_auto,
            opening_tree_path,
            opening_book_path,
            last_database_dir,
            last_print_dir,
            recent_files,
            prep_recent_limit,
            show_empty_pgn_tags,
            hover_moves,
            show_player_photos,
            photo_size,
            champion_lineage,
            show_nav_buttons,
            show_pgn_tags,
            show_annotations,
            show_clocks,
            engine_bar,
            review_skip_book,
            review_book_min_games,
            review_book_max_plies,
            trend_window,
            trend_baseline,
            trend_min_games,
            trend_source,
            engine_limit_mode,
            engine_depth,
            engine_time_ms,
            prep_filter_facing_color,
            category_colors,
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
            "last_database_dir": self.last_database_dir.as_ref().map(|path| path.display().to_string()),
            "last_print_dir": self.last_print_dir.as_ref().map(|path| path.display().to_string()),
            "recent_files": self.recent_files.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "prep_recent_limit": self.prep_recent_limit,
            "show_empty_pgn_tags": self.show_empty_pgn_tags,
            "hover_moves": self.hover_moves,
            "show_player_photos": self.show_player_photos,
            "photo_size": self.photo_size.config_value(),
            "champion_lineage": self.champion_lineage.config_value(),
            "show_nav_buttons": self.show_nav_buttons,
            "show_pgn_tags": self.show_pgn_tags,
            "show_annotations": self.show_annotations,
            "show_clocks": self.show_clocks,
            "engine_bar": self.engine_bar,
            "review_skip_book": self.review_skip_book,
            "review_book_min_games": self.review_book_min_games,
            "review_book_max_plies": self.review_book_max_plies,
            "trend_window": self.trend_window.config_value(),
            "trend_baseline": self.trend_baseline.config_value(),
            "trend_min_games": self.trend_min_games,
            "trend_source": self.trend_source.config_value(),
            "engine_limit_mode": self.engine_limit_mode,
            "engine_depth": self.engine_depth,
            "engine_time_ms": self.engine_time_ms,
            "prep_filter_facing_color": self.prep_filter_facing_color,
            "category_colors": {
                "otb_classical": rgb_to_hex(self.category_colors.otb_classical),
                "otb_rapid": rgb_to_hex(self.category_colors.otb_rapid),
                "otb_blitz": rgb_to_hex(self.category_colors.otb_blitz),
                "online_classical": rgb_to_hex(self.category_colors.online_classical),
                "online_rapid": rgb_to_hex(self.category_colors.online_rapid),
                "online_blitz": rgb_to_hex(self.category_colors.online_blitz),
                "online_bullet": rgb_to_hex(self.category_colors.online_bullet),
                "correspondence": rgb_to_hex(self.category_colors.correspondence),
                "unknown": rgb_to_hex(self.category_colors.unknown),
            },
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudPrefs {
    pub lichess_token: Option<String>,
    pub explorer_source: crate::cloud::ExplorerSource,
    /// Optional default account for the online-reference card.
    pub lichess_username: Option<String>,
    /// Optional default Chess.com account for the online-reference card.
    /// Public Chess.com game data does not require a password.
    pub chesscom_username: Option<String>,
    /// Fetch a Wikipedia summary when a player profile is opened. This is the
    /// only place Brett requests Wikipedia content; database indexing and
    /// player lists never do.
    pub wikipedia_summaries: bool,
}

impl Default for CloudPrefs {
    fn default() -> Self {
        Self {
            lichess_token: None,
            explorer_source: crate::cloud::ExplorerSource::default(),
            lichess_username: None,
            chesscom_username: None,
            wikipedia_summaries: true,
        }
    }
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
            wikipedia_summaries: value
                .get("wikipedia_summaries")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "lichess_token": self.lichess_token,
            "explorer_source": self.explorer_source.config_value(),
            "lichess_username": self.lichess_username,
            "chesscom_username": self.chesscom_username,
            "wikipedia_summaries": self.wikipedia_summaries,
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

    pub fn save(&self) -> Result<()> {
        let path = cloud_path().context("no XDG config or HOME directory is available")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, self.to_json()).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
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

    /// Persist only the Wikipedia-summary choice, leaving credentials and the
    /// explorer choice untouched.
    pub fn save_wikipedia_summaries(enabled: bool) -> Result<()> {
        let path = cloud_path().context("no XDG config or HOME directory is available")?;
        let mut prefs = match fs::read_to_string(&path) {
            Ok(text) => Self::from_json(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        prefs.wikipedia_summaries = enabled;
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

/// Unified bundle containing all configuration files for seamless migration
/// across machines and operating systems.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsBundle {
    pub engine: EnginePrefs,
    pub references: CloudPrefs,
    pub preparation: PreparationPrefs,
    pub display: DisplayPrefs,
}

impl SettingsBundle {
    pub fn current() -> Self {
        Self {
            engine: EnginePrefs::load(),
            references: CloudPrefs::load(),
            preparation: PreparationPrefs::load(),
            display: DisplayPrefs::load(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "engine": serde_json::from_str::<Value>(&self.engine.to_json()).unwrap_or(Value::Null),
            "references": serde_json::from_str::<Value>(&self.references.to_json()).unwrap_or(Value::Null),
            "preparation": serde_json::from_str::<Value>(&self.preparation.to_json()).unwrap_or(Value::Null),
            "display": serde_json::from_str::<Value>(&self.display.to_json()).unwrap_or(Value::Null),
        }))
        .unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text).context("parsing settings bundle JSON")?;
        let engine_str = value
            .get("engine")
            .map(|v| v.to_string())
            .unwrap_or_default();
        let ref_str = value
            .get("references")
            .map(|v| v.to_string())
            .unwrap_or_default();
        let prep_str = value
            .get("preparation")
            .map(|v| v.to_string())
            .unwrap_or_default();
        let disp_str = value
            .get("display")
            .map(|v| v.to_string())
            .unwrap_or_default();

        Ok(Self {
            engine: EnginePrefs::from_json(&engine_str),
            references: CloudPrefs::from_json(&ref_str),
            preparation: PreparationPrefs::from_json(&prep_str),
            display: DisplayPrefs::from_json(&disp_str),
        })
    }

    pub fn save_all(&self) -> Result<()> {
        self.engine.save()?;
        self.references.save()?;
        self.preparation.save()?;
        self.display.save()?;
        Ok(())
    }

    pub fn export_to_file(&self, target_path: &std::path::Path) -> Result<()> {
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target_path, self.to_json())?;
        Ok(())
    }

    pub fn import_from_file(source_path: &std::path::Path) -> Result<Self> {
        let text = fs::read_to_string(source_path)?;
        let bundle = Self::from_json(&text)?;
        bundle.save_all()?;
        Ok(bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn engine_prefs_round_trip() {
        let prefs = EnginePrefs::new(
            Some(PathBuf::from("/opt/engines/stockfish")),
            6,
            1024,
            true,
            true,
        );
        assert_eq!(EnginePrefs::from_json(&prefs.to_json()), prefs);
    }

    #[test]
    fn engine_cache_prefs() {
        let default_prefs = EnginePrefs::default();
        assert!(default_prefs.cache_engine_analysis);
        assert!(default_prefs.heuristic_supersede);

        let custom = EnginePrefs::new(None, 4, 256, false, false);
        let json = custom.to_json();
        let loaded = EnginePrefs::from_json(&json);
        assert_eq!(loaded, custom);
        assert!(!loaded.cache_engine_analysis);
        assert!(!loaded.heuristic_supersede);
    }

    #[test]
    fn engine_slots_round_trip() {
        let mut prefs = EnginePrefs::default();
        prefs.engines[0] = EngineSlotConfig {
            path: Some(PathBuf::from("/usr/bin/stockfish")),
            threads: 4,
            hash_mb: 256,
            enabled: true,
        };
        prefs.engines[1] = EngineSlotConfig {
            path: Some(PathBuf::from("/usr/bin/lc0")),
            threads: 2,
            hash_mb: 128,
            enabled: true,
        };
        prefs.engines[2] = EngineSlotConfig {
            path: Some(PathBuf::from("/usr/bin/komodo")),
            threads: 2,
            hash_mb: 64,
            enabled: false,
        };
        let json = prefs.to_json();
        let loaded = EnginePrefs::from_json(&json);
        assert_eq!(loaded, prefs);
        assert_eq!(
            loaded.engines[0].path,
            Some(PathBuf::from("/usr/bin/stockfish"))
        );
        assert!(loaded.engines[0].enabled);
        assert_eq!(loaded.engines[1].path, Some(PathBuf::from("/usr/bin/lc0")));
        assert!(loaded.engines[1].enabled);
        assert!(!loaded.engines[2].enabled);
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
            exclude_chesscom: false,
            correspondence_download_dismissed: false,
            reference_setup_dismissed: true,
            prep_include_online: true,
            prep_mode: PrepMode::Smart,
            prep_online_accounts: std::collections::HashMap::new(),
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
            wikipedia_summaries: false,
        };
        assert_eq!(CloudPrefs::from_json(&prefs.to_json()), prefs);
        assert!(CloudPrefs::default().wikipedia_summaries);
        assert!(!CloudPrefs::from_json(r#"{"wikipedia_summaries":false}"#).wikipedia_summaries);
        assert!(CloudPrefs::from_json("{}").wikipedia_summaries);

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
            last_database_dir: Some(PathBuf::from("/tmp/databases")),
            last_print_dir: Some(PathBuf::from("/tmp/prints")),
            recent_files: vec![PathBuf::from("/tmp/game.pgn")],
            prep_recent_limit: 25,
            show_empty_pgn_tags: true,
            hover_moves: true,
            show_player_photos: false,
            photo_size: PhotoSize::Medium,
            champion_lineage: ChampionLineage::Classical,
            show_nav_buttons: true,
            show_pgn_tags: false,
            show_annotations: false,
            show_clocks: false,
            engine_bar: false,
            review_skip_book: false,
            review_book_min_games: 5,
            review_book_max_plies: 24,
            trend_window: crate::recent::TrendWindow::SixMonths,
            trend_baseline: crate::recent::TrendBaseline::Prior24Months,
            trend_min_games: 15,
            trend_source: crate::recent::TrendSource::LichessMasters,
            engine_limit_mode: "depth".to_string(),
            engine_depth: 30,
            engine_time_ms: 2_500,
            prep_filter_facing_color: true,
            category_colors: GameCategoryColors::default(),
        };
        assert_eq!(DisplayPrefs::from_json(&prefs.to_json()), prefs);
        assert_eq!(DisplayPrefs::from_json("not json"), DisplayPrefs::default());
        assert_eq!(
            DisplayPrefs::default().trend_source,
            crate::recent::TrendSource::ActiveTree
        );
        assert_eq!(
            DisplayPrefs::from_json(r#"{"trend_source":"chessabc"}"#).trend_source,
            crate::recent::TrendSource::ChessAbc
        );
        assert_eq!(
            DisplayPrefs::from_json(r#"{"trend_source":"lichess_masters"}"#).trend_source,
            crate::recent::TrendSource::LichessMasters
        );
        assert_eq!(
            DisplayPrefs::from_json(r#"{"rating_priority":"uscf"}"#).rating_priority,
            RatingPriority::Uscf
        );
        assert!(!DisplayPrefs::default().show_empty_pgn_tags);
        assert!(!DisplayPrefs::default().hover_moves);
        assert!(DisplayPrefs::default().show_player_photos);
        assert!(!DisplayPrefs::default().show_nav_buttons);
        assert!(DisplayPrefs::default().show_pgn_tags);
        assert!(DisplayPrefs::default().show_annotations);
        assert!(DisplayPrefs::default().engine_bar);
        assert!(DisplayPrefs::default().review_skip_book);
        assert_eq!(DisplayPrefs::default().review_book_min_games, 3);
        assert_eq!(DisplayPrefs::default().review_book_max_plies, 30);
        assert_eq!(
            DisplayPrefs::from_json(r#"{"review_book_min_games":0,"review_book_max_plies":999}"#)
                .review_book_min_games,
            1
        );
        assert_eq!(
            DisplayPrefs::from_json(r#"{"review_book_min_games":0,"review_book_max_plies":999}"#)
                .review_book_max_plies,
            80
        );
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
    fn display_prefs_remember_the_last_database_directory() {
        let parsed = DisplayPrefs::from_json(r#"{"last_database_dir":"/home/tester/Chess"}"#);
        assert_eq!(
            parsed.last_database_dir.as_deref(),
            Some(std::path::Path::new("/home/tester/Chess"))
        );
        assert!(parsed.to_json().contains("last_database_dir"));
        assert_eq!(
            DisplayPrefs::from_json(r#"{"last_database_dir":"  "}"#).last_database_dir,
            None
        );
        assert_eq!(DisplayPrefs::default().last_database_dir, None);
    }

    #[test]
    fn display_prefs_remember_the_last_print_directory() {
        let parsed = DisplayPrefs::from_json(r#"{"last_print_dir":"/home/tester/Documents"}"#);
        assert_eq!(
            parsed.last_print_dir.as_deref(),
            Some(std::path::Path::new("/home/tester/Documents"))
        );
        assert!(parsed.to_json().contains("last_print_dir"));
        assert_eq!(
            DisplayPrefs::from_json(r#"{"last_print_dir":"  "}"#).last_print_dir,
            None
        );
        assert_eq!(DisplayPrefs::default().last_print_dir, None);
    }

    #[test]
    fn display_prefs_show_clocks_defaults_on_and_round_trips() {
        assert!(DisplayPrefs::default().show_clocks);
        let prefs = DisplayPrefs {
            show_clocks: false,
            ..DisplayPrefs::default()
        };
        assert_eq!(DisplayPrefs::from_json(&prefs.to_json()), prefs);
        assert!(!DisplayPrefs::from_json(r#"{"show_clocks":false}"#).show_clocks);
        assert!(DisplayPrefs::from_json(r#"{"show_clocks":"false"}"#).show_clocks);
    }

    #[test]
    fn display_prefs_photo_size_and_champion_lineage() {
        let prefs = DisplayPrefs {
            photo_size: PhotoSize::Large,
            champion_lineage: ChampionLineage::Classical,
            ..DisplayPrefs::default()
        };
        assert_eq!(DisplayPrefs::from_json(&prefs.to_json()), prefs);

        assert_eq!(DisplayPrefs::default().photo_size, PhotoSize::Small);
        assert_eq!(
            DisplayPrefs::default().champion_lineage,
            ChampionLineage::Fide
        );
        assert_eq!(PhotoSize::Small.pixels(), 38.0);
        assert_eq!(PhotoSize::Medium.pixels(), 54.0);
        assert_eq!(PhotoSize::Large.pixels(), 72.0);
        assert_eq!(PhotoSize::from_config("MEDIUM"), PhotoSize::Medium);
        assert_eq!(PhotoSize::from_config("nonsense"), PhotoSize::Small);
        assert_eq!(
            ChampionLineage::from_config("classical"),
            ChampionLineage::Classical
        );
        assert_eq!(
            ChampionLineage::from_config("nonsense"),
            ChampionLineage::Fide
        );
        assert_eq!(
            DisplayPrefs::from_json(r#"{"photo_size":"large"}"#).photo_size,
            PhotoSize::Large
        );
        assert_eq!(
            DisplayPrefs::from_json(r#"{"champion_lineage":"classical"}"#).champion_lineage,
            ChampionLineage::Classical
        );
    }

    #[test]
    fn preparation_and_display_prefs_clamp_new_fields() {
        let parsed = PreparationPrefs::from_json(
            r#"{"correspondence_enabled":false,"prep_player_names":[" Carlsen, Magnus ","","Nakamura, Hikaru"]}"#,
        );
        assert!(!parsed.correspondence_enabled);
        assert!(!parsed.reference_setup_dismissed);
        assert!(
            PreparationPrefs::from_json(r#"{"reference_setup_dismissed":true}"#)
                .reference_setup_dismissed
        );
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
        assert_eq!(display.prep_recent_limit, 100);
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
        let prefs = EnginePrefs::new(
            Some(PathBuf::from("/opt/engines/stockfish")),
            4,
            256,
            true,
            true,
        );
        assert_eq!(EnginePrefs::load_from(&path), EnginePrefs::default());
        prefs.save_to(&path).unwrap();
        assert_eq!(EnginePrefs::load_from(&path), prefs);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_bundle_round_trip() {
        let bundle = SettingsBundle {
            engine: EnginePrefs::new(
                Some(PathBuf::from("/usr/bin/stockfish")),
                8,
                512,
                true,
                false,
            ),
            references: CloudPrefs {
                lichess_token: Some("token123".to_string()),
                lichess_username: Some("grandmaster".to_string()),
                chesscom_username: None,
                explorer_source: crate::cloud::ExplorerSource::Lichess,
                wikipedia_summaries: true,
            },
            preparation: PreparationPrefs::default(),
            display: DisplayPrefs::default(),
        };

        let json = bundle.to_json();
        let loaded = SettingsBundle::from_json(&json).unwrap();
        assert_eq!(loaded.engine, bundle.engine);
        assert_eq!(
            loaded.references.lichess_token,
            Some("token123".to_string())
        );
        assert_eq!(
            loaded.references.lichess_username,
            Some("grandmaster".to_string())
        );
    }

    #[test]
    fn display_prefs_round_trips_game_category_colors() {
        let mut prefs = DisplayPrefs::default();
        prefs.category_colors.otb_classical = [0x11, 0x22, 0x33];
        prefs.category_colors.online_bullet = [0xAA, 0xBB, 0xCC];
        let json = prefs.to_json();
        assert!(json.contains("#112233"));
        assert!(json.contains("#AABBCC"));
        let loaded = DisplayPrefs::from_json(&json);
        assert_eq!(loaded.category_colors.otb_classical, [0x11, 0x22, 0x33]);
        assert_eq!(loaded.category_colors.online_bullet, [0xAA, 0xBB, 0xCC]);
        assert_eq!(
            loaded.category_colors.otb_rapid,
            GameCategoryColors::default().otb_rapid
        );
    }

    #[test]
    fn game_category_colors_color_for_and_set_color_for_all_variants() {
        let mut colors = GameCategoryColors::default();
        for (i, &cat) in crate::players::GameCategory::ALL.iter().enumerate() {
            let rgb = [(i * 10) as u8, (i * 20) as u8, (i * 25) as u8];
            colors.set_color_for(cat, rgb);
            assert_eq!(colors.color_for(cat), rgb);
        }
    }
}
