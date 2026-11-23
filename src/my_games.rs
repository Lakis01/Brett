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
use crate::players::{GameCategory, SourceTag, classify_headers_category, deduplicate_and_merge_games};
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

/// Persistent configuration for the unified My Games database and online sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MyGamesConfig {
    pub path: Option<PathBuf>,
    pub auto_sync_on_startup: bool,
    pub accounts: Vec<MyGamesOnlineAccount>,
}

impl Default for MyGamesConfig {
    fn default() -> Self {
        Self {
            path: None,
            auto_sync_on_startup: true,
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
            "accounts": accounts,
        }))
        .unwrap_or_default()
    }

    #[must_use]
    pub fn from_json(text: &str) -> Self {
        let value: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
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
                    allow_annotation_loss: true,
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

fn load_all_games_if_exists(path: &Path) -> Result<Vec<Game>> {
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
    mut incoming: Vec<Game>,
    section: &MyGamesSection,
) -> Result<MyGamesMergeReport> {
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
    }
    let existing = load_all_games_if_exists(db_path)?;
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
    pub skipped_variants: usize,
    pub total_games: usize,
    pub errors: Vec<String>,
    pub updated_timestamps: Vec<(OnlineSource, String, u64)>,
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

/// Fetch games for all enabled Lichess / Chess.com accounts in `config`, tag
/// each account's games under `Brett:Online:<Platform>:<User>`, deduplicate
/// into `config.path`, and write back cleanly.
pub fn sync_online_accounts_into_db(
    config: &MyGamesConfig,
    lichess_token: Option<&str>,
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

    let mut existing = load_all_games_if_exists(db_path)?;
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
                let games = filter_and_tag_synced_games(account, games);
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

    if report.accounts_synced > 0 && report.fetched > 0 {
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
        p.push(format!("brett-my-games-{name}-{}-{nanos}", std::process::id()));
        p
    }

    fn cleanup_db(path: &Path) {
        let stem = path.with_extension("");
        for ext in [
            "pgn", "si4", "sn4", "sg4", "cbh", "cbg", "cba", "cbp", "cbt", "cbc", "cbs", "cbj",
            "2cbh", "2cbg", "2cba", "2lid", "2lgd", "json", "json.bak",
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
        assert_eq!(classify_game_section(&minimal[0]), MyGamesSection::Classical);

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

        let r1 = merge_games_into_my_games(&path, vec![games[0].clone()], &MyGamesSection::Classical)
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
        let tagged = filter_and_tag_synced_games(&acct, vec![classical, rapid, blitz, online_chesscom]);
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

        let std_game = crate::domain::parse_pgn(b"[Variant \"From Position\"]\n\n*").unwrap().remove(0);
        let crazy_game = crate::domain::parse_pgn(b"[Variant \"Crazyhouse\"]\n\n*").unwrap().remove(0);
        assert!(db_supports_game_variant(Path::new("/tmp/a.2cbh"), &std_game));
        assert!(!db_supports_game_variant(Path::new("/tmp/a.2cbh"), &crazy_game));
        assert!(db_supports_game_variant(Path::new("/tmp/a.pgn"), &crazy_game));
    }
}
