//! Unified "My Games" database management, subsection classification,
//! multi-section filtering, format-agnostic database writing, and automatic
//! Lichess / Chess.com account synchronization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::domain::Game;
use crate::library::{SharedStr, open_library};
use crate::online::{OnlineSource, fetch_player_games_filtered_with_token};
use crate::pgn::game_to_pgn;
use crate::players::{
    GameCategory, SourceTag, classify_headers_category, deduplicate_and_merge_games,
};
use crate::settings::{config_dir, read_with_backup_json, write_atomic_with_backup};

const BRETT_SOURCE_PREFIX: &str = "Brett:";

/// Logical subsection within the unified My Games database.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MyGamesSection {
    Classical,
    Rapid,
    Blitz,
    OnlineLichess(String),
    OnlineChessCom(String),
}

impl MyGamesSection {
    /// Value written into the PGN `Source` header tag (`[Source "..."]`) so
    /// sections persist natively inside `.2cbh`, `.si4`, `.cbh`, and `.pgn`.
    #[must_use]
    pub fn source_tag_value(&self) -> String {
        match self {
            Self::Classical => "Brett:Classical".to_string(),
            Self::Rapid => "Brett:Rapid".to_string(),
            Self::Blitz => "Brett:Blitz".to_string(),
            Self::OnlineLichess(user) => format!("Brett:Online:Lichess:{}", user.trim()),
            Self::OnlineChessCom(user) => format!("Brett:Online:ChessCom:{}", user.trim()),
        }
    }

    /// Parse a PGN `Source` header value written by [`Self::source_tag_value`].
    #[must_use]
    pub fn from_source_tag(tag: &str) -> Option<Self> {
        let trimmed = tag.trim();
        let rest = trimmed.strip_prefix(BRETT_SOURCE_PREFIX)?;
        match rest {
            "Classical" => Some(Self::Classical),
            "Rapid" => Some(Self::Rapid),
            "Blitz" => Some(Self::Blitz),
            _ if rest.starts_with("Online:Lichess:") => {
                let user = rest["Online:Lichess:".len()..].trim();
                (!user.is_empty()).then(|| Self::OnlineLichess(user.to_string()))
            }
            _ if rest.starts_with("Online:ChessCom:") => {
                let user = rest["Online:ChessCom:".len()..].trim();
                (!user.is_empty()).then(|| Self::OnlineChessCom(user.to_string()))
            }
            _ => None,
        }
    }

    /// Human-readable label shown in the sidebar tree and filter bar.
    #[must_use]
    pub fn display_label(&self) -> String {
        match self {
            Self::Classical => "Classical".to_string(),
            Self::Rapid => "Rapid".to_string(),
            Self::Blitz => "Blitz".to_string(),
            Self::OnlineLichess(user) => format!("Lichess: {user}"),
            Self::OnlineChessCom(user) => format!("Chess.com: {user}"),
        }
    }

    #[must_use]
    pub fn is_online(&self) -> bool {
        matches!(self, Self::OnlineLichess(_) | Self::OnlineChessCom(_))
    }

    /// Case-insensitive comparison for online account sections.
    #[must_use]
    pub fn matches_section(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Classical, Self::Classical)
            | (Self::Rapid, Self::Rapid)
            | (Self::Blitz, Self::Blitz) => true,
            (Self::OnlineLichess(a), Self::OnlineLichess(b))
            | (Self::OnlineChessCom(a), Self::OnlineChessCom(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}

/// Tag a game with an explicit `MyGamesSection` via its `Source` header.
pub fn tag_game_section(game: &mut Game, section: &MyGamesSection) {
    game.set_header("Source", section.source_tag_value());
}

fn header_value<'a>(headers: &'a [(SharedStr, SharedStr)], key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
}

fn site_is_lichess(site: &str) -> bool {
    site.to_ascii_lowercase().contains("lichess.org")
}

fn site_is_chesscom(site: &str) -> bool {
    let lower = site.to_ascii_lowercase();
    lower.contains("chess.com")
}

/// Determine the My Games section for a header list, consulting explicit
/// `Source` tags first and configured online accounts / game headers next.
/// Defaults to [`MyGamesSection::Classical`] when no other classification fits.
#[must_use]
pub fn classify_headers_section_with_accounts(
    headers: &[(SharedStr, SharedStr)],
    accounts: &[MyGamesOnlineAccount],
) -> MyGamesSection {
    let source_tag = header_value(headers, "Source");
    if let Some(src) = source_tag {
        if let Some(parsed) = MyGamesSection::from_source_tag(src) {
            return parsed;
        }
        // Classic CBH caps `[Source]` at 25 bytes, which can trim long online
        // usernames (`Brett:Online:Lichess:<long>` -> `Brett:Online:Lichess:abcd`).
        if let Some(prefix) = src.strip_prefix("Brett:Online:Lichess:") {
            let white = header_value(headers, "White").unwrap_or("");
            let black = header_value(headers, "Black").unwrap_or("");
            for acct in accounts {
                if acct.source == OnlineSource::Lichess
                    && (white.eq_ignore_ascii_case(&acct.username)
                        || black.eq_ignore_ascii_case(&acct.username))
                {
                    return MyGamesSection::OnlineLichess(acct.username.clone());
                }
            }
            if !prefix.trim().is_empty() {
                return MyGamesSection::OnlineLichess(prefix.trim().to_string());
            }
        }
        if let Some(prefix) = src.strip_prefix("Brett:Online:ChessCom:") {
            let white = header_value(headers, "White").unwrap_or("");
            let black = header_value(headers, "Black").unwrap_or("");
            for acct in accounts {
                if acct.source == OnlineSource::ChessCom
                    && (white.eq_ignore_ascii_case(&acct.username)
                        || black.eq_ignore_ascii_case(&acct.username))
                {
                    return MyGamesSection::OnlineChessCom(acct.username.clone());
                }
            }
            if !prefix.trim().is_empty() {
                return MyGamesSection::OnlineChessCom(prefix.trim().to_string());
            }
        }
    }

    let event = header_value(headers, "Event");
    let site = header_value(headers, "Site");
    let tc = header_value(headers, "TimeControl");
    let white = header_value(headers, "White").unwrap_or("");
    let black = header_value(headers, "Black").unwrap_or("");

    if let Some(site_str) = site {
        if site_is_lichess(site_str) {
            for acct in accounts {
                if acct.source == OnlineSource::Lichess
                    && (white.eq_ignore_ascii_case(&acct.username)
                        || black.eq_ignore_ascii_case(&acct.username))
                {
                    return MyGamesSection::OnlineLichess(acct.username.clone());
                }
            }
            return MyGamesSection::OnlineLichess("Lichess".to_string());
        }
        if site_is_chesscom(site_str) {
            for acct in accounts {
                if acct.source == OnlineSource::ChessCom
                    && (white.eq_ignore_ascii_case(&acct.username)
                        || black.eq_ignore_ascii_case(&acct.username))
                {
                    return MyGamesSection::OnlineChessCom(acct.username.clone());
                }
            }
            return MyGamesSection::OnlineChessCom("Chess.com".to_string());
        }
    }

    match classify_headers_category(event, site, tc) {
        GameCategory::OtbBlitz | GameCategory::OnlineBlitz | GameCategory::OnlineBullet => {
            MyGamesSection::Blitz
        }
        GameCategory::OtbRapid | GameCategory::OnlineRapid => MyGamesSection::Rapid,
        GameCategory::OtbClassical
        | GameCategory::OnlineClassical
        | GameCategory::Correspondence
        | GameCategory::Unknown => MyGamesSection::Classical,
    }
}

/// Determine the My Games section for a header list without account hints.
/// Defaults to [`MyGamesSection::Classical`].
#[must_use]
pub fn classify_headers_section(headers: &[(SharedStr, SharedStr)]) -> MyGamesSection {
    classify_headers_section_with_accounts(headers, &[])
}

/// Determine the My Games section for an in-memory `Game`.
#[must_use]
pub fn classify_game_section(game: &Game) -> MyGamesSection {
    classify_game_section_with_accounts(game, &[])
}

/// Determine the My Games section for a `Game`, matching configured online accounts.
#[must_use]
pub fn classify_game_section_with_accounts(
    game: &Game,
    accounts: &[MyGamesOnlineAccount],
) -> MyGamesSection {
    let shared: Vec<(SharedStr, SharedStr)> = game
        .headers
        .iter()
        .map(|(k, v)| (SharedStr::from(k.as_str()), SharedStr::from(v.as_str())))
        .collect();
    classify_headers_section_with_accounts(&shared, accounts)
}

/// An online Lichess, Chess.com, or ChessABC account/profile linked to My Games.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MyGamesOnlineAccount {
    pub source: OnlineSource,
    pub username: String,
    pub enabled: bool,
    /// Optional cap on games fetched (`None` = sync all games).
    pub max_games: Option<usize>,
    /// For ChessABC: skip online games (chess.com, Lichess, etc.) and keep OTB games only.
    pub exclude_online: bool,
    pub last_sync_unix: Option<u64>,
}

impl MyGamesOnlineAccount {
    #[must_use]
    pub fn section(&self) -> MyGamesSection {
        match self.source {
            OnlineSource::ChessCom => MyGamesSection::OnlineChessCom(self.username.clone()),
            OnlineSource::Lichess => MyGamesSection::OnlineLichess(self.username.clone()),
            OnlineSource::ChessAbc => MyGamesSection::Classical,
        }
    }
}

/// Returns `true` when `game` came from an online platform/event rather than OTB play.
#[must_use]
pub fn is_online_chessabc_game(game: &Game) -> bool {
    matches!(
        classify_headers_category(
            game.header("Event"),
            game.header("Site"),
            game.header("TimeControl"),
        ),
        GameCategory::OnlineBullet
            | GameCategory::OnlineBlitz
            | GameCategory::OnlineRapid
            | GameCategory::OnlineClassical
    )
}

/// Filter (e.g. OTB-only for ChessABC) and tag `Brett:Source` headers for synced games.
#[must_use]
pub fn filter_and_tag_synced_games(account: &MyGamesOnlineAccount, games: Vec<Game>) -> Vec<Game> {
    if account.source == OnlineSource::ChessAbc {
        let mut kept = Vec::new();
        for mut game in games {
            if account.exclude_online && is_online_chessabc_game(&game) {
                continue;
            }
            // ChessABC bridges OTB/classical records into Classical / Rapid / Blitz,
            // never into `Online*` subsections.
            let section = match classify_headers_category(
                game.header("Event"),
                game.header("Site"),
                game.header("TimeControl"),
            ) {
                GameCategory::OtbBlitz | GameCategory::OnlineBlitz | GameCategory::OnlineBullet => {
                    MyGamesSection::Blitz
                }
                GameCategory::OtbRapid | GameCategory::OnlineRapid => MyGamesSection::Rapid,
                GameCategory::OtbClassical
                | GameCategory::OnlineClassical
                | GameCategory::Correspondence
                | GameCategory::Unknown => MyGamesSection::Classical,
            };
            tag_game_section(&mut game, &section);
            kept.push(game);
        }
        return kept;
    }
    let section = account.section();
    let mut out = games;
    for game in &mut out {
        tag_game_section(game, &section);
    }
    out
}

/// Default behavior when an imported/synced game matches an existing game in My Games.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DuplicatePolicy {
    /// Prompt interactively on every conflict (with "apply to all" option).
    #[default]
    Ask,
    /// Keep Source A (the copy already in My Games).
    PreferExisting,
    /// Replace with Source B (the incoming imported copy).
    PreferIncoming,
    /// Keep whichever copy has more variations, comments, NAGs, or annotations.
    PreferRicher,
}

impl DuplicatePolicy {
    pub const ALL: [Self; 4] = [
        Self::Ask,
        Self::PreferExisting,
        Self::PreferIncoming,
        Self::PreferRicher,
    ];

    #[must_use]
    pub fn config_value(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::PreferExisting => "existing",
            Self::PreferIncoming => "incoming",
            Self::PreferRicher => "richer",
        }
    }

    #[must_use]
    pub fn from_config(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "existing" | "prefer_existing" | "source_a" | "a" => Self::PreferExisting,
            "incoming" | "prefer_incoming" | "source_b" | "b" => Self::PreferIncoming,
            "richer" | "prefer_richer" => Self::PreferRicher,
            _ => Self::Ask,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask every time",
            Self::PreferExisting => "Always keep Existing (Source A)",
            Self::PreferIncoming => "Always use Incoming (Source B)",
            Self::PreferRicher => "Always keep Richer game & variations",
        }
    }
}

/// Sidebar privacy/visibility policy for the `MY GAMES` shelf section.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MyGamesSidebarMode {
    /// Collapsed on launch so online account handles are not immediately exposed.
    #[default]
    Collapsed,
    /// Expanded on launch.
    Expanded,
    /// Locked behind a click-to-unlock toggle that hides account handles.
    Locked,
    /// Completely hidden from the left sidebar.
    Hidden,
}

impl MyGamesSidebarMode {
    pub const ALL: [Self; 4] = [Self::Collapsed, Self::Expanded, Self::Locked, Self::Hidden];

    #[must_use]
    pub fn config_value(self) -> &'static str {
        match self {
            Self::Collapsed => "collapsed",
            Self::Expanded => "expanded",
            Self::Locked => "locked",
            Self::Hidden => "hidden",
        }
    }

    #[must_use]
    pub fn from_config(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "expanded" | "show" | "shown" => Self::Expanded,
            "locked" | "lock" => Self::Locked,
            "hidden" | "hide" => Self::Hidden,
            _ => Self::Collapsed,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Collapsed => "Collapsed by default (recommended)",
            Self::Expanded => "Expanded on launch",
            Self::Locked => "Locked (require click to reveal)",
            Self::Hidden => "Hidden from sidebar",
        }
    }
}

/// Persistent configuration for the unified My Games database and online sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MyGamesConfig {
    pub path: Option<PathBuf>,
    pub auto_sync_on_startup: bool,
    pub duplicate_policy: DuplicatePolicy,
    pub sidebar_mode: MyGamesSidebarMode,
    pub accounts: Vec<MyGamesOnlineAccount>,
}

impl Default for MyGamesConfig {
    fn default() -> Self {
        Self {
            path: None,
            auto_sync_on_startup: true,
            duplicate_policy: DuplicatePolicy::default(),
            sidebar_mode: MyGamesSidebarMode::default(),
            accounts: Vec::new(),
        }
    }
}

impl MyGamesConfig {
    #[must_use]
    pub fn config_path() -> Option<PathBuf> {
        config_dir().map(|dir| dir.join("my_games.json"))
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        let accounts: Vec<serde_json::Value> = self
            .accounts
            .iter()
            .filter(|a| !a.username.trim().is_empty())
            .map(|a| {
                let source = match a.source {
                    OnlineSource::ChessCom => "chesscom",
                    OnlineSource::Lichess => "lichess",
                    OnlineSource::ChessAbc => "chessabc",
                };
                serde_json::json!({
                    "source": source,
                    "username": a.username.trim(),
                    "enabled": a.enabled,
                    "max_games": a.max_games.filter(|&n| n > 0),
                    "exclude_online": a.exclude_online,
                    "last_sync_unix": a.last_sync_unix,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({
            "path": self.path.as_ref().map(|p| p.display().to_string()),
            "auto_sync_on_startup": self.auto_sync_on_startup,
            "duplicate_policy": self.duplicate_policy.config_value(),
            "sidebar_mode": self.sidebar_mode.config_value(),
            "accounts": accounts,
        }))
        .unwrap_or_default()
    }

    #[must_use]
    pub fn from_json(text: &str) -> Self {
        let value: serde_json::Value =
            serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
        let path = value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let auto_sync_on_startup = value
            .get("auto_sync_on_startup")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let duplicate_policy = value
            .get("duplicate_policy")
            .and_then(serde_json::Value::as_str)
            .map(DuplicatePolicy::from_config)
            .unwrap_or_default();
        let sidebar_mode = value
            .get("sidebar_mode")
            .and_then(serde_json::Value::as_str)
            .map(MyGamesSidebarMode::from_config)
            .unwrap_or_default();
        let mut accounts = Vec::new();
        if let Some(arr) = value.get("accounts").and_then(serde_json::Value::as_array) {
            for item in arr {
                let username = item
                    .get("username")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                if username.is_empty() {
                    continue;
                }
                let source = match item
                    .get("source")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("lichess")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "chesscom" | "chess.com" => OnlineSource::ChessCom,
                    "chessabc" | "chessabc.com" => OnlineSource::ChessAbc,
                    _ => OnlineSource::Lichess,
                };
                let enabled = item
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                let max_games = item
                    .get("max_games")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|&n| n > 0)
                    .map(|n| n as usize);
                let exclude_online = item
                    .get("exclude_online")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(source == OnlineSource::ChessAbc);
                let last_sync_unix = item
                    .get("last_sync_unix")
                    .and_then(serde_json::Value::as_u64);
                let dup = accounts.iter().any(|existing: &MyGamesOnlineAccount| {
                    existing.source == source && existing.username.eq_ignore_ascii_case(username)
                });
                if !dup {
                    accounts.push(MyGamesOnlineAccount {
                        source,
                        username: username.to_string(),
                        enabled,
                        max_games,
                        exclude_online,
                        last_sync_unix,
                    });
                }
            }
        }
        Self {
            path,
            auto_sync_on_startup,
            duplicate_policy,
            sidebar_mode,
            accounts,
        }
    }

    #[must_use]
    pub fn load() -> Self {
        Self::config_path().map_or_else(Self::default, |p| Self::load_from(&p))
    }

    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        read_with_backup_json(path).map_or_else(Self::default, |text| Self::from_json(&text))
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = Self::config_path() else {
            return Ok(());
        };
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        write_atomic_with_backup(path, &self.to_json())
    }
}

/// Multi-select filter over My Games subsections. Empty means all sections.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MyGamesFilter {
    pub selected: BTreeSet<MyGamesSection>,
}

impl MyGamesFilter {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn clear(&mut self) {
        self.selected.clear();
    }

    #[must_use]
    pub fn contains(&self, section: &MyGamesSection) -> bool {
        self.selected.iter().any(|s| s.matches_section(section))
    }

    pub fn toggle(&mut self, section: MyGamesSection) {
        if let Some(existing) = self
            .selected
            .iter()
            .find(|s| s.matches_section(&section))
            .cloned()
        {
            self.selected.remove(&existing);
        } else {
            self.selected.insert(section);
        }
    }

    #[must_use]
    pub fn matches(&self, section: &MyGamesSection) -> bool {
        self.selected.is_empty() || self.contains(section)
    }
}

/// Write `games` to `path`, choosing `.2cbh`, `.si4`, `.cbh`, or `.pgn` by file extension.
pub fn write_games_to_path(path: &Path, games: Vec<Game>) -> Result<usize> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent folder {}", parent.display()))?;
    }
    let total = games.len();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("pgn")
        .to_ascii_lowercase();
    match ext.as_str() {
        "2cbh" | "2cbg" | "2cba" | "2lid" => {
            crate::twocbh::write_database_with_options(
                path,
                crate::twocbh::TwoCbhWriteOptions {
                    allow_annotation_loss: false,
                    write_brett_annotations_sidecar: true,
                },
                games.into_iter().map(Ok),
            )
            .with_context(|| format!("writing 2CBH database at {}", path.display()))?;
        }
        "si4" => {
            crate::scid::write_database(path, "My Games", games.into_iter().map(Ok))
                .with_context(|| format!("writing SCID database at {}", path.display()))?;
        }
        "cbh" => {
            crate::cbh::write_database(path, games.into_iter().map(Ok))
                .with_context(|| format!("writing CBH database at {}", path.display()))?;
        }
        _ => {
            let content = if games.is_empty() {
                String::new()
            } else {
                let mut out = games
                    .iter()
                    .map(game_to_pgn)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                out.push('\n');
                out
            };
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let tmp = path.with_extension(format!("pgn.tmp.{}.{}", std::process::id(), stamp));
            fs::write(&tmp, content.as_bytes())
                .with_context(|| format!("writing temporary PGN {}", tmp.display()))?;
            if let Err(err) = fs::rename(&tmp, path) {
                let _ = fs::remove_file(&tmp);
                return Err(err).with_context(|| format!("installing {}", path.display()));
            }
        }
    }
    Ok(total)
}

/// Create a new empty database file in the format implied by `path`'s extension.
pub fn create_empty_database(path: &Path) -> Result<()> {
    write_games_to_path(path, Vec::new())?;
    Ok(())
}

/// Result of merging games into a specific section of My Games.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MyGamesMergeReport {
    pub total_games: usize,
    pub added: usize,
    pub skipped: usize,
}

pub fn load_all_games_if_exists(path: &Path) -> Result<Vec<Game>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    // Check companion existence for multi-file databases or non-empty PGN.
    let lib = open_library(path).with_context(|| format!("opening {}", path.display()))?;
    let mut games = Vec::with_capacity(lib.len());
    for idx in 0..lib.len() {
        games.push(
            lib.load(idx)
                .with_context(|| format!("loading game {idx} from {}", path.display()))?,
        );
    }
    Ok(games)
}

/// Tag `incoming` games with `section`, deduplicate them against existing
/// games at `db_path`, and write the combined database back atomically.
pub fn merge_games_into_my_games(
    db_path: &Path,
    incoming: Vec<Game>,
    section: &MyGamesSection,
) -> Result<MyGamesMergeReport> {
    merge_games_into_my_games_canonical(db_path, incoming, section, None)
}

/// Same as [`merge_games_into_my_games`], optionally normalizing abbreviated/enumerated
/// player names matching `canonical_my_name` (e.g. `Hinterreiter, M.` -> `Hinterreiter, Markus`).
pub fn merge_games_into_my_games_canonical(
    db_path: &Path,
    mut incoming: Vec<Game>,
    section: &MyGamesSection,
    canonical_my_name: Option<&str>,
) -> Result<MyGamesMergeReport> {
    let canonical_pgn = canonical_my_name
        .map(crate::players::canonical_pgn_player_name)
        .filter(|s| !s.trim().is_empty());
    if incoming.is_empty() {
        let existing_count = load_all_games_if_exists(db_path)?.len();
        return Ok(MyGamesMergeReport {
            total_games: existing_count,
            added: 0,
            skipped: 0,
        });
    }
    for game in &mut incoming {
        tag_game_section(game, section);
        if let Some(canon) = canonical_pgn.as_deref() {
            crate::players::canonicalize_player_tags(game, canon);
        }
    }
    let mut existing = load_all_games_if_exists(db_path)?;
    if let Some(canon) = canonical_pgn.as_deref() {
        for game in &mut existing {
            crate::players::canonicalize_player_tags(game, canon);
        }
    }
    let existing_count = existing.len();
    let incoming_count = incoming.len();
    let merged = deduplicate_and_merge_games(vec![
        (SourceTag::Owoju, existing),
        (SourceTag::Other(section.source_tag_value()), incoming),
    ]);
    let all_games: Vec<Game> = merged.into_iter().map(|(_, g)| g).collect();
    let total_games = all_games.len();
    let added = total_games.saturating_sub(existing_count);
    let skipped = incoming_count.saturating_sub(added);
    write_games_to_path(db_path, all_games)?;
    Ok(MyGamesMergeReport {
        total_games,
        added,
        skipped,
    })
}

/// Result of syncing enabled online accounts into My Games.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MyGamesSyncReport {
    pub accounts_synced: usize,
    pub fetched: usize,
    pub added: usize,
    pub skipped: usize,
    pub skipped_deleted: usize,
    pub skipped_variants: usize,
    pub total_games: usize,
    pub errors: Vec<String>,
    pub updated_timestamps: Vec<(OnlineSource, String, u64)>,
}

/// Persistent fingerprint of a manually deleted game so automatic Lichess,
/// Chess.com, and ChessABC syncs do not re-import it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletedGameEntry {
    pub online_id: Option<String>,
    pub white_key: String,
    pub black_key: String,
    pub white: String,
    pub black: String,
    pub date: String,
    pub date_num: u32,
    pub result: String,
    pub event: String,
    pub move_prefix: Vec<String>,
    pub spine_hash: u64,
    pub spine_plies: usize,
    pub deleted_unix: u64,
}

fn hash_uci_list(ucis: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for u in ucis {
        for &b in u.as_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn date_num_to_days(date_num: u32) -> Option<i64> {
    if date_num < 10_000 {
        return None;
    }
    let y = (date_num / 10_000) as i64;
    let m = ((date_num / 100) % 100).clamp(1, 12) as i64;
    let d = (date_num % 100).clamp(1, 31) as i64;
    Some(y * 366 + m * 31 + d)
}

fn deleted_dates_compatible(a: u32, b: u32, window_days: i64) -> bool {
    let (Some(da), Some(db)) = (date_num_to_days(a), date_num_to_days(b)) else {
        return true;
    };
    if a.is_multiple_of(10_000) || b.is_multiple_of(10_000) {
        return a / 10_000 == b / 10_000;
    }
    (da - db).abs() <= window_days
}

fn deleted_names_compatible(a_raw: &str, a_key: &str, b_raw: &str, b_key: &str) -> bool {
    if !a_key.is_empty() && !b_key.is_empty() && a_key == b_key {
        return true;
    }
    let a_low = a_raw.trim().to_ascii_lowercase();
    let b_low = b_raw.trim().to_ascii_lowercase();
    !a_low.is_empty() && a_low == b_low
}

impl DeletedGameEntry {
    #[must_use]
    pub fn from_game(game: &Game) -> Self {
        let ucis = game_spine_ucis(game);
        let spine_hash = hash_uci_list(&ucis);
        let spine_plies = ucis.len();
        let move_prefix: Vec<String> = ucis.into_iter().take(24).collect();
        let white = game.white().trim().to_string();
        let black = game.black().trim().to_string();
        let white_key = crate::players::player_key(&white).unwrap_or_default();
        let black_key = crate::players::player_key(&black).unwrap_or_default();
        let date = game.date().trim().to_string();
        let date_num = crate::library::parse_date_num(&date);
        let deleted_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            online_id: crate::players::online_game_id(game),
            white_key,
            black_key,
            white,
            black,
            date,
            date_num,
            result: game.result().trim().to_string(),
            event: game.event().trim().to_string(),
            move_prefix,
            spine_hash,
            spine_plies,
            deleted_unix,
        }
    }

    #[must_use]
    pub fn matches(&self, game: &Game) -> bool {
        if let (Some(a), Some(b)) = (&self.online_id, crate::players::online_game_id(game))
            && *a == b
        {
            return true;
        }
        let ucis = game_spine_ucis(game);
        let shared = self.move_prefix.len().min(ucis.len());
        let both_empty = self.spine_plies == 0
            && ucis.is_empty()
            && !self.event.is_empty()
            && self.event.eq_ignore_ascii_case(game.event().trim());
        let spine_match = both_empty
            || (self.spine_plies == ucis.len() && self.spine_hash == hash_uci_list(&ucis))
            || (shared >= 12 && self.move_prefix[..shared] == ucis[..shared]);
        if !spine_match {
            return false;
        }

        let res_a = self.result.trim();
        let res_b = game.result().trim();
        let res_ok =
            res_a == res_b || res_a.is_empty() || res_a == "*" || res_b.is_empty() || res_b == "*";
        if !res_ok {
            return false;
        }

        let inc_date_num = crate::library::parse_date_num(game.date());
        let is_online = self.online_id.is_some() || crate::players::online_game_id(game).is_some();
        let window = if shared >= 14 && !is_online { 14 } else { 3 };
        if !deleted_dates_compatible(self.date_num, inc_date_num, window) {
            return false;
        }

        let w_key = crate::players::player_key(game.white()).unwrap_or_default();
        let b_key = crate::players::player_key(game.black()).unwrap_or_default();
        if deleted_names_compatible(&self.white, &self.white_key, game.white(), &w_key)
            && deleted_names_compatible(&self.black, &self.black_key, game.black(), &b_key)
        {
            return true;
        }

        self.date_num > 0
            && self.date_num == inc_date_num
            && shared >= 16
            && !res_a.is_empty()
            && res_a != "*"
            && res_a == res_b
    }

    #[must_use]
    pub fn summary_label(&self) -> String {
        let mut s = format!("{} – {}", self.white, self.black);
        if !self.result.is_empty() && self.result != "*" {
            s.push_str(&format!(" ({})", self.result));
        }
        if !self.date.is_empty() && self.date != "????.??.??" {
            s.push_str(&format!(" · {}", self.date));
        }
        if !self.event.is_empty() && self.event != "?" {
            s.push_str(&format!(" · {}", self.event));
        }
        s
    }

    fn to_json_value(&self) -> serde_json::Value {
        serde_json::json!({
            "online_id": self.online_id,
            "white_key": self.white_key,
            "black_key": self.black_key,
            "white": self.white,
            "black": self.black,
            "date": self.date,
            "date_num": self.date_num,
            "result": self.result,
            "event": self.event,
            "move_prefix": self.move_prefix,
            "spine_hash": format!("{:016x}", self.spine_hash),
            "spine_plies": self.spine_plies,
            "deleted_unix": self.deleted_unix,
        })
    }

    fn from_json_value(v: &serde_json::Value) -> Option<Self> {
        let spine_hash_raw = v.get("spine_hash").and_then(serde_json::Value::as_str)?;
        let spine_hash = u64::from_str_radix(spine_hash_raw.trim(), 16).ok()?;
        let move_prefix = v
            .get("move_prefix")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            online_id: v
                .get("online_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            white_key: v
                .get("white_key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            black_key: v
                .get("black_key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            white: v
                .get("white")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string(),
            black: v
                .get("black")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string(),
            date: v
                .get("date")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("????.??.??")
                .to_string(),
            date_num: v
                .get("date_num")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32,
            result: v
                .get("result")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("*")
                .to_string(),
            event: v
                .get("event")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string(),
            move_prefix,
            spine_hash,
            spine_plies: v
                .get("spine_plies")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize,
            deleted_unix: v
                .get("deleted_unix")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        })
    }
}

/// Persistent list of games manually deleted by the user so online/ChessABC
/// syncs remember not to re-import them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeletedGamesRegistry {
    entries: Vec<DeletedGameEntry>,
}

impl DeletedGamesRegistry {
    #[must_use]
    pub fn config_path() -> Option<PathBuf> {
        config_dir().map(|dir| dir.join("deleted_games.json"))
    }

    #[must_use]
    pub fn load() -> Self {
        Self::config_path().map_or_else(Self::default, |p| Self::load_from(&p))
    }

    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        read_with_backup_json(path).map_or_else(Self::default, |text| Self::from_json(&text))
    }

    #[must_use]
    pub fn from_json(text: &str) -> Self {
        let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(text)
        else {
            return Self::default();
        };
        let entries = arr
            .iter()
            .filter_map(DeletedGameEntry::from_json_value)
            .collect();
        Self { entries }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        let arr: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(DeletedGameEntry::to_json_value)
            .collect();
        serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".to_string())
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = Self::config_path() else {
            return Ok(());
        };
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        write_atomic_with_backup(path, &self.to_json())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn entries(&self) -> &[DeletedGameEntry] {
        &self.entries
    }

    #[must_use]
    pub fn contains(&self, game: &Game) -> bool {
        self.entries.iter().any(|entry| entry.matches(game))
    }

    pub fn add_game(&mut self, game: &Game) -> bool {
        if self.contains(game) {
            return false;
        }
        self.entries.push(DeletedGameEntry::from_game(game));
        true
    }

    pub fn remove(&mut self, index: usize) -> Option<DeletedGameEntry> {
        (index < self.entries.len()).then(|| self.entries.remove(index))
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Filter out any games recorded as deleted, returning `(kept, skipped_count)`.
    #[must_use]
    pub fn filter_games(&self, games: Vec<Game>) -> (Vec<Game>, usize) {
        if self.entries.is_empty() {
            return (games, 0);
        }
        let total = games.len();
        let kept: Vec<Game> = games.into_iter().filter(|g| !self.contains(g)).collect();
        let skipped = total.saturating_sub(kept.len());
        (kept, skipped)
    }
}

fn db_supports_game_variant(path: &Path, game: &Game) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !matches!(ext.as_str(), "2cbh" | "cbh" | "si4") {
        return true;
    }
    let variant = game
        .header("Variant")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let root = &game.nodes[0].pos;
    if root.castling_mode().is_chess960() {
        return variant.is_empty() || variant.contains("960") || variant.contains("fischer");
    }
    variant.is_empty()
        || variant == "standard"
        || variant == "chess"
        || variant == "from position"
        || variant == "fromposition"
        || variant.contains("960")
        || variant.contains("fischer")
}

/// Fetch games for all enabled Lichess / Chess.com / ChessABC accounts in `config`,
/// tag each account's games, deduplicate into `config.path`, and write back cleanly.
pub fn sync_online_accounts_into_db(
    config: &MyGamesConfig,
    lichess_token: Option<&str>,
) -> Result<MyGamesSyncReport> {
    let deleted = DeletedGamesRegistry::load();
    sync_online_accounts_into_db_with_options(config, lichess_token, &deleted, None)
}

/// Same as [`sync_online_accounts_into_db`], using an explicit [`DeletedGamesRegistry`].
pub fn sync_online_accounts_into_db_with_deleted(
    config: &MyGamesConfig,
    lichess_token: Option<&str>,
    deleted: &DeletedGamesRegistry,
) -> Result<MyGamesSyncReport> {
    sync_online_accounts_into_db_with_options(config, lichess_token, deleted, None)
}

/// Same as [`sync_online_accounts_into_db_with_deleted`], optionally canonicalizing
/// abbreviated/enumerated OTB player tags (e.g. `Hinterreiter, M.` -> `Hinterreiter, Markus`).
pub fn sync_online_accounts_into_db_with_options(
    config: &MyGamesConfig,
    lichess_token: Option<&str>,
    deleted: &DeletedGamesRegistry,
    canonical_my_name: Option<&str>,
) -> Result<MyGamesSyncReport> {
    let Some(db_path) = config.path.as_deref() else {
        bail!("No My Games database path is configured.");
    };
    let enabled: Vec<&MyGamesOnlineAccount> =
        config.accounts.iter().filter(|a| a.enabled).collect();
    if enabled.is_empty() {
        return Ok(MyGamesSyncReport::default());
    }

    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let canonical_pgn = canonical_my_name
        .map(crate::players::canonical_pgn_player_name)
        .filter(|s| !s.trim().is_empty());

    let mut existing = load_all_games_if_exists(db_path)?;
    let mut existing_healed = false;
    if let Some(canon) = canonical_pgn.as_deref() {
        for game in &mut existing {
            let before_w = game.white().to_string();
            let before_b = game.black().to_string();
            crate::players::canonicalize_player_tags(game, canon);
            if game.white() != before_w || game.black() != before_b {
                existing_healed = true;
            }
        }
    }
    let existing_count = existing.len();
    let mut sources: Vec<(SourceTag, Vec<Game>)> =
        vec![(SourceTag::Owoju, std::mem::take(&mut existing))];
    let mut report = MyGamesSyncReport::default();

    for account in enabled {
        match fetch_player_games_filtered_with_token(
            account.source,
            &account.username,
            account.max_games.unwrap_or(0),
            None,
            0,
            lichess_token,
        ) {
            Ok(games) => {
                let mut games = filter_and_tag_synced_games(account, games);
                if account.source == OnlineSource::ChessAbc
                    && let Some(canon) = canonical_pgn.as_deref()
                {
                    for game in &mut games {
                        crate::players::canonicalize_player_tags(game, canon);
                    }
                }
                let (games, skipped_deleted) = deleted.filter_games(games);
                report.skipped_deleted += skipped_deleted;
                let mut supported = Vec::with_capacity(games.len());
                for game in games {
                    if db_supports_game_variant(db_path, &game) {
                        supported.push(game);
                    } else {
                        report.skipped_variants += 1;
                    }
                }
                report.accounts_synced += 1;
                report.fetched += supported.len();
                report.updated_timestamps.push((
                    account.source,
                    account.username.clone(),
                    now_unix,
                ));
                if !supported.is_empty() {
                    sources.push((SourceTag::Online(account.source), supported));
                }
            }
            Err(err) => {
                report.errors.push(format!(
                    "{} ({}): {err:#}",
                    account.username,
                    account.source.label()
                ));
            }
        }
    }

    if (report.accounts_synced > 0 && report.fetched > 0) || existing_healed {
        let merged = deduplicate_and_merge_games(sources);
        let all_games: Vec<Game> = merged.into_iter().map(|(_, g)| g).collect();
        report.total_games = all_games.len();
        report.added = report.total_games.saturating_sub(existing_count);
        report.skipped = report.fetched.saturating_sub(report.added);
        write_games_to_path(db_path, all_games)?;
    } else {
        report.total_games = existing_count;
    }

    Ok(report)
}

/// Summary of per-section game counts in a loaded My Games database.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MyGamesCounts {
    pub total: usize,
    pub classical: usize,
    pub rapid: usize,
    pub blitz: usize,
    pub online: BTreeMap<MyGamesSection, usize>,
}

/// Per-conflict decision chosen manually or derived from [`DuplicatePolicy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictChoice {
    KeepExisting,
    UseIncoming,
    KeepRicher,
}

/// One duplicate conflict between an existing My Games entry (Source A) and
/// an incoming game (Source B).
#[derive(Clone, Debug)]
pub struct DuplicateConflict {
    pub existing_index: usize,
    pub incoming_index: usize,
    pub existing: Game,
    pub incoming: Game,
    pub existing_richness: (usize, usize, usize, usize, usize),
    pub incoming_richness: (usize, usize, usize, usize, usize),
    pub recommended: ConflictChoice,
}

/// Pre-scanned import plan separating unique incoming games from duplicate conflicts.
#[derive(Clone, Debug)]
pub struct ImportScan {
    pub existing: Vec<Game>,
    pub incoming: Vec<Game>,
    pub unique_incoming_indices: Vec<usize>,
    pub conflicts: Vec<DuplicateConflict>,
    pub identical_skipped: usize,
}

/// Summary of an applied deduplicated import into My Games.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeduplicatedImportReport {
    pub total_games: usize,
    pub added: usize,
    pub replaced_with_incoming: usize,
    pub kept_existing: usize,
    pub identical_skipped: usize,
}

fn game_spine_ucis(game: &Game) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = game.nodes.first().and_then(|n| n.children.first().copied());
    while let Some(idx) = cur {
        let Some(node) = game.nodes.get(idx) else {
            break;
        };
        if let Some(mv) = &node.mv {
            out.push(mv.uci.clone());
        }
        cur = node.children.first().copied();
    }
    out
}

fn merge_non_brett_headers(dst: &mut Game, src: &Game) {
    for (k, v) in &src.headers {
        let val = v.trim();
        if val.is_empty() || val == "?" || val == "????.??.??" || val == "*" {
            continue;
        }
        if k.eq_ignore_ascii_case("Source") && val.starts_with("Brett:") {
            continue;
        }
        let existing_val = dst.header(k).unwrap_or("").trim();
        let should_take = existing_val.is_empty()
            || existing_val == "?"
            || existing_val == "????.??.??"
            || existing_val == "*"
            || (!k.eq_ignore_ascii_case("Source") && val.len() > existing_val.len() + 3);
        if should_take {
            dst.set_header(k, val.to_string());
        }
    }
}

/// Scan `incoming` against `existing` (after collapsing duplicates inside
/// `incoming` itself to their richest copy). Identical copies with equal
/// richness are silently skipped; copies differing in variations, comments,
/// NAGs, clocks, evals, or boards raise a [`DuplicateConflict`].
#[must_use]
pub fn scan_import_conflicts(existing: Vec<Game>, raw_incoming: Vec<Game>) -> ImportScan {
    // 1. Deduplicate incoming batch internally, always keeping the richer copy.
    let deduped_incoming: Vec<Game> =
        crate::players::deduplicate_and_merge_games(vec![(SourceTag::Owoju, raw_incoming)])
            .into_iter()
            .map(|(_, g)| g)
            .collect();

    // 2. Index existing games for fast O(1) online-ID or spine lookup, plus a
    // fallback pairwise check on date/online buckets.
    let mut existing_online: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut existing_prefix: std::collections::HashMap<Vec<String>, Vec<usize>> =
        std::collections::HashMap::new();
    let mut existing_richness: Vec<(usize, usize, usize, usize, usize)> =
        Vec::with_capacity(existing.len());

    for (idx, g) in existing.iter().enumerate() {
        if let Some(oid) = crate::players::online_game_id(g) {
            existing_online.entry(oid).or_insert(idx);
        }
        let ucis = game_spine_ucis(g);
        let key: Vec<String> = ucis.iter().take(12).cloned().collect();
        existing_prefix.entry(key).or_default().push(idx);
        existing_richness.push(crate::players::game_richness_score(g));
    }

    let mut unique_incoming_indices = Vec::new();
    let mut conflicts = Vec::new();
    let mut identical_skipped = 0usize;
    let mut matched_existing: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for (inc_idx, inc) in deduped_incoming.iter().enumerate() {
        let mut match_idx =
            crate::players::online_game_id(inc).and_then(|oid| existing_online.get(&oid).copied());

        if match_idx.is_none() {
            let ucis = game_spine_ucis(inc);
            let pkey: Vec<String> = ucis.iter().take(12).cloned().collect();
            if let Some(cands) = existing_prefix.get(&pkey) {
                for &ex_idx in cands {
                    if crate::players::games_are_duplicates(&existing[ex_idx], inc) {
                        match_idx = Some(ex_idx);
                        break;
                    }
                }
            }
            if match_idx.is_none() && ucis.len() < 12 {
                // Short games (< 12 plies): check exact spine bucket too.
                if let Some(cands) = existing_prefix.get(&ucis) {
                    for &ex_idx in cands {
                        if crate::players::games_are_duplicates(&existing[ex_idx], inc) {
                            match_idx = Some(ex_idx);
                            break;
                        }
                    }
                }
            }
        }

        if let Some(ex_idx) = match_idx {
            if !matched_existing.insert(ex_idx) {
                // Already matched by an earlier incoming copy; keep richer skip.
                identical_skipped += 1;
                continue;
            }
            let r_ex = existing_richness[ex_idx];
            let r_in = crate::players::game_richness_score(inc);
            if r_ex == r_in && existing[ex_idx].nodes.len() == inc.nodes.len() {
                identical_skipped += 1;
                continue;
            }
            let recommended = if r_in > r_ex {
                ConflictChoice::UseIncoming
            } else {
                ConflictChoice::KeepExisting
            };
            conflicts.push(DuplicateConflict {
                existing_index: ex_idx,
                incoming_index: inc_idx,
                existing: existing[ex_idx].clone(),
                incoming: inc.clone(),
                existing_richness: r_ex,
                incoming_richness: r_in,
                recommended,
            });
        } else {
            unique_incoming_indices.push(inc_idx);
        }
    }

    ImportScan {
        existing,
        incoming: deduped_incoming,
        unique_incoming_indices,
        conflicts,
        identical_skipped,
    }
}

/// Apply conflict resolutions to `scan` and write the deduplicated result atomically
/// to `db_path`. Preserves existing `Brett:*` section tags unless overridden.
pub fn apply_deduplicated_import(
    db_path: &Path,
    mut scan: ImportScan,
    choices: &[ConflictChoice],
    default_section: Option<&MyGamesSection>,
) -> Result<DeduplicatedImportReport> {
    let mut replaced_with_incoming = 0usize;
    let mut kept_existing = 0usize;

    for (idx, conflict) in scan.conflicts.iter().enumerate() {
        let choice = choices
            .get(idx)
            .copied()
            .unwrap_or(ConflictChoice::KeepRicher);
        let use_incoming = match choice {
            ConflictChoice::KeepExisting => false,
            ConflictChoice::UseIncoming => true,
            ConflictChoice::KeepRicher => conflict.incoming_richness > conflict.existing_richness,
        };
        if use_incoming {
            let ex_source = scan.existing[conflict.existing_index]
                .header("Source")
                .map(str::to_string);
            let mut replacement = conflict.incoming.clone();
            merge_non_brett_headers(&mut replacement, &scan.existing[conflict.existing_index]);
            if let Some(src) = ex_source.filter(|s| s.starts_with("Brett:")) {
                replacement.set_header("Source", src);
            } else if replacement
                .header("Source")
                .is_none_or(|s| !s.starts_with("Brett:"))
            {
                let sec = default_section
                    .cloned()
                    .unwrap_or_else(|| classify_game_section(&replacement));
                tag_game_section(&mut replacement, &sec);
            }
            scan.existing[conflict.existing_index] = replacement;
            replaced_with_incoming += 1;
        } else {
            let incoming_ref = &scan.incoming[conflict.incoming_index];
            merge_non_brett_headers(&mut scan.existing[conflict.existing_index], incoming_ref);
            kept_existing += 1;
        }
    }

    let mut added = 0usize;
    for inc_idx in scan.unique_incoming_indices {
        let mut game = scan.incoming[inc_idx].clone();
        if game
            .header("Source")
            .is_none_or(|s| !s.starts_with("Brett:"))
        {
            let sec = default_section
                .cloned()
                .unwrap_or_else(|| classify_game_section(&game));
            tag_game_section(&mut game, &sec);
        }
        if db_supports_game_variant(db_path, &game) {
            scan.existing.push(game);
            added += 1;
        }
    }

    let total_games = scan.existing.len();
    write_games_to_path(db_path, scan.existing)?;
    Ok(DeduplicatedImportReport {
        total_games,
        added,
        replaced_with_incoming,
        kept_existing,
        identical_skipped: scan.identical_skipped,
    })
}

/// Merge `incoming` into `db_path` using a hard [`DuplicatePolicy`] (must not be `Ask`).
pub fn import_games_with_policy(
    db_path: &Path,
    incoming: Vec<Game>,
    policy: DuplicatePolicy,
    default_section: Option<&MyGamesSection>,
) -> Result<DeduplicatedImportReport> {
    let existing = load_all_games_if_exists(db_path)?;
    let scan = scan_import_conflicts(existing, incoming);
    let choice = match policy {
        DuplicatePolicy::PreferExisting => ConflictChoice::KeepExisting,
        DuplicatePolicy::PreferIncoming => ConflictChoice::UseIncoming,
        DuplicatePolicy::PreferRicher | DuplicatePolicy::Ask => ConflictChoice::KeepRicher,
    };
    let choices = vec![choice; scan.conflicts.len()];
    apply_deduplicated_import(db_path, scan, &choices, default_section)
}

/// Tally games across `Classical`, `Rapid`, `Blitz`, and `Online` subsections.
#[must_use]
pub fn tally_sections<'a, I>(games: I, accounts: &[MyGamesOnlineAccount]) -> MyGamesCounts
where
    I: IntoIterator<Item = &'a [(SharedStr, SharedStr)]>,
{
    let mut counts = MyGamesCounts::default();
    for headers in games {
        counts.total += 1;
        match classify_headers_section_with_accounts(headers, accounts) {
            MyGamesSection::Classical => counts.classical += 1,
            MyGamesSection::Rapid => counts.rapid += 1,
            MyGamesSection::Blitz => counts.blitz += 1,
            online @ (MyGamesSection::OnlineLichess(_) | MyGamesSection::OnlineChessCom(_)) => {
                *counts.online.entry(online).or_insert(0) += 1;
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::parse_pgn;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!(
            "brett-my-games-{name}-{}-{nanos}",
            std::process::id()
        ));
        p
    }

    fn cleanup_db(path: &Path) {
        let stem = path.with_extension("");
        for ext in [
            "pgn", "si4", "sn4", "sg4", "cbh", "cbg", "cba", "cbp", "cbt", "cbc", "cbs", "cbj",
            "2cbh", "2cbg", "2cba", "2lid", "2lgd", "brettann", "json", "json.bak",
        ] {
            let p = stem.with_extension(ext);
            let _ = fs::remove_file(p);
        }
        let _ = fs::remove_file(path);
    }

    #[test]
    fn source_tag_round_trip_and_default_classical() {
        let sections = [
            MyGamesSection::Classical,
            MyGamesSection::Rapid,
            MyGamesSection::Blitz,
            MyGamesSection::OnlineLichess("MagnusCarlsen".to_string()),
            MyGamesSection::OnlineChessCom("Hikaru".to_string()),
        ];
        for sec in &sections {
            let tag = sec.source_tag_value();
            assert_eq!(MyGamesSection::from_source_tag(&tag).as_ref(), Some(sec));
        }

        let minimal = parse_pgn(b"[White \"A\"]\n[Black \"B\"]\n\n1. e4 e5 *").unwrap();
        assert_eq!(
            classify_game_section(&minimal[0]),
            MyGamesSection::Classical
        );

        let mut blitz_game = minimal[0].clone();
        blitz_game.set_header("TimeControl", "180+2".to_string());
        assert_eq!(classify_game_section(&blitz_game), MyGamesSection::Blitz);

        let mut tagged_classical = blitz_game.clone();
        tag_game_section(&mut tagged_classical, &MyGamesSection::Classical);
        assert_eq!(
            classify_game_section(&tagged_classical),
            MyGamesSection::Classical
        );
    }

    #[test]
    fn filter_supports_multi_section_and_case_insensitive_online() {
        let mut filter = MyGamesFilter::default();
        assert!(filter.matches(&MyGamesSection::Classical));
        assert!(filter.matches(&MyGamesSection::Blitz));

        filter.toggle(MyGamesSection::Classical);
        filter.toggle(MyGamesSection::OnlineLichess("Lakis01".to_string()));
        filter.toggle(MyGamesSection::OnlineChessCom("UserA".to_string()));
        filter.toggle(MyGamesSection::OnlineChessCom("UserB".to_string()));

        assert!(filter.matches(&MyGamesSection::Classical));
        assert!(!filter.matches(&MyGamesSection::Rapid));
        assert!(!filter.matches(&MyGamesSection::Blitz));
        assert!(filter.matches(&MyGamesSection::OnlineLichess("lakis01".to_string())));
        assert!(filter.matches(&MyGamesSection::OnlineChessCom("usera".to_string())));
        assert!(filter.matches(&MyGamesSection::OnlineChessCom("UserB".to_string())));
        assert!(!filter.matches(&MyGamesSection::OnlineChessCom("UserC".to_string())));

        filter.toggle(MyGamesSection::Classical);
        assert!(!filter.matches(&MyGamesSection::Classical));
    }

    #[test]
    fn create_empty_database_and_reopen_all_formats() {
        for ext in ["2cbh", "si4", "cbh", "pgn"] {
            let path = temp_path("empty").with_extension(ext);
            create_empty_database(&path)
                .unwrap_or_else(|e| panic!("failed creating empty .{ext}: {e:#}"));
            let lib = open_library(&path)
                .unwrap_or_else(|e| panic!("failed reopening empty .{ext}: {e:#}"));
            assert_eq!(lib.len(), 0, "expected 0 games in .{ext}");
            cleanup_db(&path);
        }
    }

    #[test]
    fn merge_games_tags_section_and_deduplicates_across_2cbh() {
        let path = temp_path("merge").with_extension("2cbh");
        create_empty_database(&path).unwrap();

        let pgn = b"[Event \"Club Championship\"]\n[White \"Alice\"]\n[Black \"Bob\"]\n[Date \"2025.03.01\"]\n[Result \"1-0\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1-0\n\n[Event \"Weekend Rapid\"]\n[White \"Carol\"]\n[Black \"Dave\"]\n[Date \"2025.03.02\"]\n[Result \"0-1\"]\n\n1. d4 d5 2. c4 e6 0-1\n";
        let games = parse_pgn(pgn).unwrap();

        let r1 =
            merge_games_into_my_games(&path, vec![games[0].clone()], &MyGamesSection::Classical)
                .unwrap();
        assert_eq!(r1.added, 1);
        assert_eq!(r1.total_games, 1);

        // Merging duplicate + new Rapid game should skip duplicate and keep 2 total
        let r2 = merge_games_into_my_games(&path, games, &MyGamesSection::Rapid).unwrap();
        assert_eq!(r2.added, 1);
        assert_eq!(r2.skipped, 1);
        assert_eq!(r2.total_games, 2);

        let lib = open_library(&path).unwrap();
        assert_eq!(lib.len(), 2);
        let s0 = classify_headers_section(&lib.game_headers(0));
        let s1 = classify_headers_section(&lib.game_headers(1));
        assert_eq!(s0, MyGamesSection::Classical);
        assert_eq!(s1, MyGamesSection::Rapid);

        cleanup_db(&path);
    }

    #[test]
    fn config_json_round_trip_and_atomic_persistence() {
        let dir = temp_path("cfg");
        fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("my_games.json");

        let cfg = MyGamesConfig {
            path: Some(PathBuf::from("/tmp/MyGames.2cbh")),
            auto_sync_on_startup: true,
            duplicate_policy: DuplicatePolicy::PreferRicher,
            sidebar_mode: MyGamesSidebarMode::Locked,
            accounts: vec![
                MyGamesOnlineAccount {
                    source: OnlineSource::Lichess,
                    username: "AliceLichess".to_string(),
                    enabled: true,
                    max_games: None,
                    exclude_online: false,
                    last_sync_unix: Some(1_700_000_000),
                },
                MyGamesOnlineAccount {
                    source: OnlineSource::ChessCom,
                    username: "AliceChess".to_string(),
                    enabled: false,
                    max_games: Some(100),
                    exclude_online: false,
                    last_sync_unix: None,
                },
                MyGamesOnlineAccount {
                    source: OnlineSource::ChessAbc,
                    username: "4604776".to_string(),
                    enabled: true,
                    max_games: None,
                    exclude_online: true,
                    last_sync_unix: None,
                },
            ],
        };
        cfg.save_to(&cfg_path).unwrap();
        let loaded = MyGamesConfig::load_from(&cfg_path);
        assert_eq!(loaded, cfg);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn chessabc_sync_excludes_online_and_routes_otb_into_classical_rapid_blitz() {
        let classical = crate::domain::parse_pgn(
            b"[Event \"Bavaria Liga\"]\n[Site \"Munich GER\"]\n[White \"A\"]\n[Black \"B\"]\n\n1. e4 e5 *\n",
        )
        .unwrap()
        .remove(0);

        let rapid = crate::domain::parse_pgn(
            b"[Event \"Bayerische Schnellschach EM\"]\n[Site \"Augsburg GER\"]\n[White \"A\"]\n[Black \"B\"]\n\n1. d4 d5 *\n",
        )
        .unwrap()
        .remove(0);

        let blitz = crate::domain::parse_pgn(
            b"[Event \"German Blitz Cup\"]\n[Site \"Berlin GER\"]\n[White \"A\"]\n[Black \"B\"]\n\n1. c4 c5 *\n",
        )
        .unwrap()
        .remove(0);

        let online_chesscom = crate::domain::parse_pgn(
            b"[Event \"Titled Tuesday\"]\n[Site \"chess.com INT\"]\n[White \"A\"]\n[Black \"B\"]\n\n1. e4 c5 *\n",
        )
        .unwrap()
        .remove(0);

        let acct = MyGamesOnlineAccount {
            source: OnlineSource::ChessAbc,
            username: "4604776".to_string(),
            enabled: true,
            max_games: None,
            exclude_online: true,
            last_sync_unix: None,
        };
        let tagged =
            filter_and_tag_synced_games(&acct, vec![classical, rapid, blitz, online_chesscom]);
        assert_eq!(tagged.len(), 3);
        assert_eq!(classify_game_section(&tagged[0]), MyGamesSection::Classical);
        assert_eq!(classify_game_section(&tagged[1]), MyGamesSection::Rapid);
        assert_eq!(classify_game_section(&tagged[2]), MyGamesSection::Blitz);
        assert!(!tagged.iter().any(|g| classify_game_section(g).is_online()));

        // Even with exclude_online = false, ChessABC games never file under Online*.
        let mut all_acct = acct.clone();
        all_acct.exclude_online = false;
        let online_chesscom2 = crate::domain::parse_pgn(
            b"[Event \"Titled Tuesday\"]\n[Site \"chess.com INT\"]\n[White \"A\"]\n[Black \"B\"]\n\n1. e4 c5 *\n",
        )
        .unwrap()
        .remove(0);
        let tagged_all = filter_and_tag_synced_games(&all_acct, vec![online_chesscom2]);
        assert_eq!(tagged_all.len(), 1);
        assert!(!classify_game_section(&tagged_all[0]).is_online());
        assert!(!acct.section().is_online());

        let std_game = crate::domain::parse_pgn(b"[Variant \"From Position\"]\n\n*")
            .unwrap()
            .remove(0);
        let crazy_game = crate::domain::parse_pgn(b"[Variant \"Crazyhouse\"]\n\n*")
            .unwrap()
            .remove(0);
        assert!(db_supports_game_variant(
            Path::new("/tmp/a.2cbh"),
            &std_game
        ));
        assert!(!db_supports_game_variant(
            Path::new("/tmp/a.2cbh"),
            &crazy_game
        ));
        assert!(db_supports_game_variant(
            Path::new("/tmp/a.pgn"),
            &crazy_game
        ));
    }

    #[test]
    fn deduplicated_import_prefers_richer_variations_and_preserves_brett_tags() {
        let path = temp_path("dedupe-import").with_extension("2cbh");
        create_empty_database(&path).unwrap();

        let lichess_bare = crate::domain::parse_pgn(
            b"[Event \"Rated Blitz game\"]\n[Site \"https://lichess.org/AbCdEfGh\"]\n[White \"user123\"]\n[Black \"Opponent\"]\n[Date \"2025.06.01\"]\n[Result \"1-0\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 5. O-O Be7 6. Re1 b5 1-0\n",
        )
        .unwrap();
        let analyzed_copy = crate::domain::parse_pgn(
            b"[Event \"Annotated Study\"]\n[Site \"https://lichess.org/AbCdEfGh\"]\n[White \"Real Name\"]\n[Black \"Opponent\"]\n[Date \"2025.06.01\"]\n[Result \"1-0\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 (3... Nf6 {Berlin option} 4. O-O) 4. Ba4 Nf6 5. O-O Be7 6. Re1 b5 {Mainline Ruy Lopez} 1-0\n",
        )
        .unwrap();

        merge_games_into_my_games(
            &path,
            lichess_bare,
            &MyGamesSection::OnlineLichess("user123".to_string()),
        )
        .unwrap();

        let existing = load_all_games_if_exists(&path).unwrap();
        let scan = scan_import_conflicts(existing, analyzed_copy);
        assert_eq!(scan.conflicts.len(), 1);
        assert_eq!(scan.conflicts[0].recommended, ConflictChoice::UseIncoming);

        let report = apply_deduplicated_import(
            &path,
            scan,
            &[ConflictChoice::KeepRicher],
            Some(&MyGamesSection::Classical),
        )
        .unwrap();
        assert_eq!(report.total_games, 1);
        assert_eq!(report.replaced_with_incoming, 1);

        let lib = open_library(&path).unwrap();
        let got = lib.load(0).unwrap();
        assert!(got.nodes.iter().any(|n| n.comment == "Berlin option"));
        assert!(got.nodes.iter().any(|n| n.comment == "Mainline Ruy Lopez"));
        assert_eq!(got.header("Source"), Some("Brett:Online:Lichess:user123"));

        // Subsequent bare sync must NOT overwrite the analyzed copy or duplicate it.
        let bare_again = crate::domain::parse_pgn(
            b"[Event \"Rated Blitz game\"]\n[Site \"https://lichess.org/AbCdEfGh\"]\n[White \"user123\"]\n[Black \"Opponent\"]\n[Date \"2025.06.01\"]\n[Result \"1-0\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 5. O-O Be7 6. Re1 b5 1-0\n",
        )
        .unwrap();
        let merged = merge_games_into_my_games(
            &path,
            bare_again,
            &MyGamesSection::OnlineLichess("user123".to_string()),
        )
        .unwrap();
        assert_eq!(merged.total_games, 1);
        assert_eq!(merged.added, 0);
        let reloaded = open_library(&path).unwrap().load(0).unwrap();
        assert!(
            reloaded
                .nodes
                .iter()
                .any(|n| n.comment == "Mainline Ruy Lopez")
        );

        cleanup_db(&path);
    }

    #[test]
    fn deleted_games_registry_round_trips_and_suppresses_online_and_otb_resync() {
        let dir = temp_path("deleted-reg");
        fs::create_dir_all(&dir).unwrap();
        let reg_path = dir.join("deleted_games.json");

        let lichess_game = crate::domain::parse_pgn(
            b"[Event \"Rated Blitz game\"]\n[Site \"https://lichess.org/XyZ12345\"]\n[White \"GermanoMosconi32\"]\n[Black \"Opponent\"]\n[Date \"2025.08.01\"]\n[Result \"1-0\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 5. O-O Be7 6. Re1 b5 1-0\n",
        )
        .unwrap()
        .remove(0);

        let otb_game = crate::domain::parse_pgn(
            b"[Event \"Bavaria Liga\"]\n[Site \"Munich\"]\n[White \"Hinterreiter, Markus\"]\n[Black \"Mueller, Hans\"]\n[Date \"2025.10.12\"]\n[Result \"1/2-1/2\"]\n\n1. d4 d5 2. c4 c6 3. Nf3 Nf6 4. Nc3 e6 5. e3 Nbd7 6. Bd3 dxc4 7. Bxc4 b5 1/2-1/2\n",
        )
        .unwrap()
        .remove(0);

        let distinct_game = crate::domain::parse_pgn(
            b"[Event \"Rated Blitz game\"]\n[Site \"https://lichess.org/AbCd9999\"]\n[White \"GermanoMosconi32\"]\n[Black \"Other\"]\n[Date \"2025.08.02\"]\n[Result \"0-1\"]\n\n1. c4 e5 2. Nc3 Nf6 3. g3 d5 0-1\n",
        )
        .unwrap()
        .remove(0);

        let mut reg = DeletedGamesRegistry::default();
        assert!(reg.add_game(&lichess_game));
        assert!(!reg.add_game(&lichess_game));
        assert!(reg.add_game(&otb_game));
        reg.save_to(&reg_path).unwrap();

        let reloaded = DeletedGamesRegistry::load_from(&reg_path);
        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.contains(&lichess_game));
        // Even if White/Black tag is formatted differently on re-fetch, online ID still matches
        let mut renamed_lichess = lichess_game.clone();
        renamed_lichess.set_header("White", "Markus Hinterreiter".to_string());
        assert!(reloaded.contains(&renamed_lichess));
        // OTB / ChessABC match by move spine + players + date
        assert!(reloaded.contains(&otb_game));
        assert!(!reloaded.contains(&distinct_game));

        let (kept, skipped) =
            reloaded.filter_games(vec![lichess_game, otb_game, distinct_game.clone()]);
        assert_eq!(skipped, 2);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].header("Site"), distinct_game.header("Site"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_games_canonicalizes_abbreviated_player_name() {
        let path = temp_path("canon-merge").with_extension("2cbh");
        create_empty_database(&path).unwrap();

        let games = crate::domain::parse_pgn(
            b"[Event \"Oberliga Bayern\"]\n[White \"Hinterreiter, M.\"]\n[Black \"Hinterreiter, A.\"]\n[Date \"2026.02.01\"]\n[Result \"1-0\"]\n\n1. d4 d5 1-0\n",
        )
        .unwrap();

        merge_games_into_my_games_canonical(
            &path,
            games,
            &MyGamesSection::Classical,
            Some("Markus Hinterreiter"),
        )
        .unwrap();

        let reloaded = open_library(&path).unwrap().load(0).unwrap();
        assert_eq!(reloaded.white(), "Hinterreiter, Markus");
        assert_eq!(reloaded.black(), "Hinterreiter, A.");

        cleanup_db(&path);
    }
}
