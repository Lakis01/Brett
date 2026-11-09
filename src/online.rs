//! Public Lichess, Chess.com, and ChessABC game imports.
//!
//! All three sites expose unauthenticated public APIs. Games are downloaded in
//! the background by the UI and written to Brett's cache, never next to a source
//! database. Chess.com has no bulk PGN endpoint, so recent monthly archives are
//! concatenated; Lichess exposes the whole requested export in one request.
//! ChessABC is searched by player name or FIDE id and paged through its public
//! JSON game API.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::domain;

const USER_AGENT: &str = "Brett/0.1 (native Linux chess preparation app)";
const MAX_ARCHIVES: usize = 36;

/// A public account service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnlineSource {
    #[default]
    Lichess,
    ChessCom,
    ChessAbc,
}

impl OnlineSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lichess => "Lichess",
            Self::ChessCom => "Chess.com",
            Self::ChessAbc => "ChessABC",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Lichess => "lichess",
            Self::ChessCom => "chesscom",
            Self::ChessAbc => "chessabc",
        }
    }

    /// ChessABC accepts a player name or FIDE id rather than an account name.
    pub fn input_hint(self) -> &'static str {
        match self {
            Self::ChessAbc => "player name or FIDE id",
            _ => "username",
        }
    }
}

/// Parse a pasted account/profile URL (or plain username) into a source and
/// clean username/FIDE id.
///
/// Returns `(Some(source), username)` for recognized profile URLs
/// (`lichess.org/@/user[/all]`, `chess.com/member|players/name`,
/// `chessabc.com/player/<id>`, `ratings.fide.com/profile/<id>`), and
/// `(None, trimmed)` for plain input.
pub fn parse_account_or_profile_input(raw: &str) -> (Option<OnlineSource>, String) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (None, String::new());
    }
    // Strip scheme if present.
    let without_scheme = match trimmed.split_once("://") {
        Some((_, rest)) => rest,
        None => trimmed,
    };
    let lower_no_scheme = without_scheme.to_ascii_lowercase();
    let (host, path) = match without_scheme.find('/') {
        Some(pos) => (&without_scheme[..pos], &without_scheme[pos + 1..]),
        None => (without_scheme, ""),
    };
    let host_lower = host.to_ascii_lowercase();
    let host_no_www = host_lower.strip_prefix("www.").unwrap_or(&host_lower);
    // Drop query/fragment from path.
    let path_clean = path.split(['?', '#']).next().unwrap_or(path);
    let segments: Vec<&str> = path_clean.split('/').filter(|s| !s.is_empty()).collect();
    let known_host = matches!(
        host_no_www,
        "lichess.org" | "chess.com" | "chessabc.com" | "ratings.fide.com"
    );
    if !known_host {
        // Not a URL we recognize; keep plain input as-is.
        // Guard against treating a plain username containing dots as host.
        let looks_like_url = lower_no_scheme.contains("://")
            || trimmed.starts_with("http")
            || host_no_www.contains('.');
        if !looks_like_url {
            return (None, trimmed.to_string());
        }
        // Unknown URL host: return trimmed without source.
        return (None, trimmed.to_string());
    }
    match host_no_www {
        "lichess.org" => {
            // /@/user or /@/user/all or /user
            let user = if segments.first() == Some(&"@") {
                segments.get(1).copied()
            } else if segments.len() == 1 && !segments[0].is_empty() {
                // lichess.org/<user> (no @) – accept as username
                Some(segments[0])
            } else {
                None
            };
            match user {
                Some(u) if !u.is_empty() => (Some(OnlineSource::Lichess), u.trim().to_string()),
                _ => (Some(OnlineSource::Lichess), String::new()),
            }
        }
        "chess.com" => {
            let user = match segments.first().map(|s| s.to_ascii_lowercase()).as_deref() {
                Some("member") | Some("players") => segments.get(1).copied(),
                _ => None,
            };
            match user {
                Some(u) if !u.is_empty() => (Some(OnlineSource::ChessCom), u.trim().to_string()),
                _ => (Some(OnlineSource::ChessCom), String::new()),
            }
        }
        "chessabc.com" => {
            let id =
                if segments.first().map(|s| s.to_ascii_lowercase()).as_deref() == Some("player") {
                    segments.get(1).copied()
                } else {
                    None
                };
            match id {
                Some(u) if !u.is_empty() => (Some(OnlineSource::ChessAbc), u.trim().to_string()),
                _ => (Some(OnlineSource::ChessAbc), String::new()),
            }
        }
        "ratings.fide.com" => {
            let id =
                if segments.first().map(|s| s.to_ascii_lowercase()).as_deref() == Some("profile") {
                    segments.get(1).copied()
                } else {
                    None
                };
            match id {
                Some(u) if !u.is_empty() => (Some(OnlineSource::ChessAbc), u.trim().to_string()),
                _ => (Some(OnlineSource::ChessAbc), String::new()),
            }
        }
        _ => (None, trimmed.to_string()),
    }
}

/// Result of a completed online import.
#[derive(Clone, Debug)]
pub struct ImportResult {
    pub source: OnlineSource,
    pub username: String,
    /// Number of games if the source reported it exactly.
    pub games: Option<usize>,
    pub path: PathBuf,
}

/// Download recent games for `username` and save them as PGN in the cache.
pub fn import(
    source: OnlineSource,
    username: &str,
    max_games: usize,
    mut progress: impl FnMut(String),
) -> Result<ImportResult> {
    let username = validate_query(source, username)?;
    let max_games = max_games.clamp(1, 5000);
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build();

    let (body, games) = download_player_pgn(&agent, source, username, max_games, &mut progress)?;

    // A cheap sanity check catches HTML error pages and truncated exports
    // before they become a "database" in the UI.
    let parsed = domain::parse_pgn(body.as_bytes()).context("validating downloaded PGN")?;
    if parsed.is_empty() {
        bail!("{} returned no parseable PGN games", source.label());
    }

    let path = output_path(source, username);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating online import cache {}", parent.display()))?;
    }
    fs::write(&path, body.as_bytes())
        .with_context(|| format!("writing imported games to {}", path.display()))?;

    Ok(ImportResult {
        source,
        username: username.to_string(),
        games: games.or(Some(parsed.len())),
        path,
    })
}

/// Download one public Lichess game by its 8-character game id. This is used
/// by the opening-explorer top-games list so a click can load the game onto
/// the board instead of opening a browser tab.
pub fn fetch_lichess_game(id: &str) -> Result<domain::Game> {
    let id = id.trim();
    if id.is_empty() || id.len() > 16 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("invalid Lichess game id");
    }
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .build();
    let url =
        format!("https://lichess.org/game/export/{id}?clocks=false&evals=false&opening=false");
    let body = get_text(&agent, &url, Some("application/x-chess-pgn"))?;
    let mut games = domain::parse_pgn(body.as_bytes()).context("parsing the Lichess game")?;
    if games.is_empty() {
        bail!("Lichess returned an empty game");
    }
    Ok(games.remove(0))
}

/// Download a player's recent public games without writing them to disk.
pub fn fetch_player_games(
    source: OnlineSource,
    username: &str,
    max_games: usize,
) -> Result<Vec<domain::Game>> {
    fetch_player_games_filtered(source, username, max_games, None, 0)
}

/// Download a player's recent public games with optional account filtering.
pub fn fetch_player_games_filtered(
    source: OnlineSource,
    username: &str,
    max_games: usize,
    filter: Option<&crate::players::AccountFilter>,
    as_of: u32,
) -> Result<Vec<domain::Game>> {
    let username = validate_query(source, username)?;
    let max_games = normalized_max_games(max_games);
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build();

    match source {
        OnlineSource::Lichess => {
            let cache_path = lichess_cache_path(username);
            let cached = read_games_cache(&cache_path);
            let mut games: Vec<domain::Game> = if let Some(cached_games) = cached {
                // Incremental delta: newest cached UTCDate/UTCTime + 1000ms,
                // unfiltered (no perfType/rated), prepend + dedupe.
                let newest_ms = cached_games.first().and_then(game_timestamp_ms);
                let mut merged = cached_games.clone();
                if let Some(newest) = newest_ms {
                    match fetch_lichess_games_paged(
                        &agent,
                        username,
                        usize::MAX,
                        Some(newest + 1000),
                    ) {
                        Ok(delta) if !delta.is_empty() => {
                            let mut seen: std::collections::HashSet<String> =
                                cached_games.iter().filter_map(lichess_game_id).collect();
                            let mut fresh = Vec::new();
                            for game in delta {
                                if let Some(id) = lichess_game_id(&game)
                                    && !seen.insert(id)
                                {
                                    continue;
                                }
                                fresh.push(game);
                            }
                            if !fresh.is_empty() {
                                fresh.extend(merged);
                                merged = fresh;
                            }
                        }
                        Ok(_) => {}
                        Err(err) => {
                            tracing::warn!("Lichess delta fetch failed, using cache: {err:#}");
                        }
                    }
                }
                write_games_cache(&cache_path, &merged)
                    .unwrap_or_else(|err| tracing::warn!("writing Lichess cache: {err:#}"));
                merged
            } else if max_games == usize::MAX || max_games > 500 {
                // Full unfiltered history (paged at 10000) populates cache.
                let full = fetch_lichess_games_paged(&agent, username, usize::MAX, None)?;
                if full.is_empty() {
                    bail!("Lichess returned no parseable games for {username}");
                }
                write_games_cache(&cache_path, &full)
                    .unwrap_or_else(|err| tracing::warn!("writing Lichess cache: {err:#}"));
                full
            } else {
                // Probe (<=500, no cache yet): capped unfiltered fetch, no
                // partial cache is written (Start will populate the full one).
                fetch_lichess_games_paged(&agent, username, max_games, None)?
            };
            if games.is_empty() {
                bail!("Lichess returned no parseable games for {username}");
            }
            if let Some(filter) = filter {
                games.retain(|g| filter.matches_game(g, as_of));
            }
            if max_games != usize::MAX && games.len() > max_games {
                games.truncate(max_games);
            }
            Ok(games)
        }
        OnlineSource::ChessCom => {
            let (body, _) = download_player_pgn(&agent, source, username, max_games, &mut |_| {})?;
            let mut games =
                domain::parse_pgn(body.as_bytes()).context("parsing downloaded games")?;
            if let Some(filter) = filter {
                games.retain(|g| filter.matches_game(g, as_of));
            }
            if games.is_empty() {
                bail!("Chess.com returned no parseable games for {username}");
            }
            Ok(games)
        }
        OnlineSource::ChessAbc => {
            let player = resolve_chessabc_player(&agent, username)?;
            let (body, _) =
                download_chessabc_games(&agent, &player.fide_id, max_games, &mut |_| {})?;
            let mut games =
                domain::parse_pgn(body.as_bytes()).context("parsing downloaded games")?;
            if let Some(filter) = filter {
                games.retain(|g| filter.matches_game(g, as_of));
            }
            if games.is_empty() {
                bail!("ChessABC returned no parseable games for {}", player.name);
            }
            Ok(games)
        }
    }
}

/// Resolve a ChessABC player and download their latest games. The resolved
/// FIDE id is returned with the games so preparation can filter by identity
/// instead of relying on abbreviated names such as `Hinterreiter,M`.
pub fn fetch_chessabc_player_games(
    query: &str,
    max_games: usize,
) -> Result<(ChessAbcPlayer, Vec<domain::Game>)> {
    let query = validate_query(OnlineSource::ChessAbc, query)?;
    let max_games = normalized_max_games(max_games);
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build();
    let player = resolve_chessabc_player(&agent, query)?;
    let (body, _) =
        download_chessabc_games(&agent, &player.fide_id, max_games, &mut |_message| {})?;
    let games = domain::parse_pgn(body.as_bytes()).context("parsing downloaded games")?;
    if games.is_empty() {
        bail!("ChessABC returned no parseable games for {}", player.name);
    }
    Ok((player, games))
}

/// Download the newest games across the whole ChessABC database. This is the
/// sample used for recent-popularity trends when no prepared player is loaded.
pub fn fetch_recent_chessabc_games(max_games: usize) -> Result<Vec<domain::Game>> {
    const PAGE_SIZE: usize = 100;
    let max_games = max_games.clamp(1, 5_000);
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build();

    let target_pages = max_games.div_ceil(PAGE_SIZE).min(5);
    let page_results: Vec<Result<Vec<String>>> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(target_pages);
        for page in 0..target_pages {
            let agent = &agent;
            handles.push(s.spawn(move || {
                let url = format!(
                    "https://api.chessabc.com/games/paginated?page={page}&size={PAGE_SIZE}"
                );
                let body = get_text(agent, &url, Some("application/json"))?;
                let value: Value =
                    serde_json::from_str(&body).context("ChessABC returned invalid game data")?;
                let content = value
                    .get("content")
                    .and_then(Value::as_array)
                    .context("ChessABC response has no content array")?;
                let mut page_pgns = Vec::new();
                for game in content {
                    if let Some(pgn) = chessabc_pgn(game) {
                        page_pgns.push(pgn);
                    }
                }
                Ok(page_pgns)
            }));
        }
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("thread panic")))
            })
            .collect()
    });

    let mut pgns = Vec::new();
    for page_res in page_results {
        match page_res {
            Ok(page_pgns) => {
                for pgn in page_pgns {
                    pgns.push(pgn);
                    if pgns.len() >= max_games {
                        break;
                    }
                }
            }
            Err(e) if pgns.is_empty() => return Err(e),
            Err(_) => break,
        }
        if pgns.len() >= max_games {
            break;
        }
    }

    if pgns.is_empty() {
        bail!("ChessABC returned no recent standard games");
    }
    let mut body = pgns.join("\n\n");
    body.push('\n');
    let games = domain::parse_pgn(body.as_bytes()).context("parsing recent ChessABC games")?;
    if games.is_empty() {
        bail!("ChessABC returned no parseable recent games");
    }
    Ok(games)
}

/// Treat `0` and very large limits as "uncapped" and leave small limits
/// untouched. The prep flow passes `usize::MAX` to mean every game.
fn normalized_max_games(max_games: usize) -> usize {
    if max_games == 0 || max_games > 50_000 {
        usize::MAX
    } else {
        max_games
    }
}

pub fn lichess_export_url_filtered(
    username: &str,
    max_games: usize,
    filter: Option<&crate::players::AccountFilter>,
    as_of: u32,
) -> String {
    let mut params = Vec::new();
    if max_games > 0 && max_games <= 50_000 {
        params.push(format!("max={max_games}"));
    }
    if let Some(filter) = filter {
        let mut perfs = Vec::new();
        if filter.time_controls.classical {
            perfs.push("classical");
        }
        if filter.time_controls.rapid {
            perfs.push("rapid");
        }
        if filter.time_controls.blitz {
            perfs.push("blitz");
        }
        if filter.time_controls.bullet {
            perfs.push("bullet");
        }
        if !perfs.is_empty() && perfs.len() < 4 {
            params.push(format!("perfType={}", perfs.join(",")));
        }

        if filter.include_rated && !filter.include_casual {
            params.push("rated=true".to_string());
        } else if !filter.include_rated && filter.include_casual {
            params.push("rated=false".to_string());
        }

        match filter.date_cutoff {
            crate::players::DateCutoff::AllTime => {}
            crate::players::DateCutoff::SinceDate(since) => {
                if let Some(days) = crate::players::date_to_days(since) {
                    let unix_ms = (days.max(0) as u64) * 86_400 * 1000;
                    params.push(format!("since={unix_ms}"));
                }
            }
            crate::players::DateCutoff::Months(months) => {
                if let Some(as_of_days) = crate::players::date_to_days(as_of) {
                    let limit_days = (f64::from(months) * 365.25 / 12.0).ceil() as i64;
                    let since_days = as_of_days - limit_days;
                    let unix_ms = (since_days.max(0) as u64) * 86_400 * 1000;
                    params.push(format!("since={unix_ms}"));
                }
            }
        }
    }
    params.push("clocks=false".to_string());
    params.push("evals=false".to_string());
    params.push("opening=false".to_string());
    params.push("tags=true".to_string());
    params.push("sort=dateDesc".to_string());

    format!(
        "https://lichess.org/api/games/user/{username}?{}",
        params.join("&")
    )
}

fn lichess_export_url(username: &str, max_games: usize) -> String {
    lichess_export_url_filtered(username, max_games, None, 0)
}

/// Read a PGN stream incrementally line-by-line, calling `progress` after
/// each complete game. If a read error or timeout occurs after at least one game
/// was read, returns the completed games instead of failing.
pub fn read_pgn_stream_incremental(
    mut reader: impl std::io::BufRead,
    max_games: usize,
    mut progress: impl FnMut(usize),
) -> Result<(String, usize)> {
    let mut completed_games = String::new();
    let mut current_game = String::new();
    let mut games_count = 0usize;
    let mut seen_moves = false;
    let mut line_buf = String::new();

    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => {
                if seen_moves && !current_game.trim().is_empty() {
                    completed_games.push_str(&current_game);
                    if !completed_games.ends_with('\n') {
                        completed_games.push('\n');
                    }
                    games_count += 1;
                    progress(games_count);
                }
                break;
            }
            Ok(_) => {}
            Err(e) => {
                if games_count >= 1 {
                    return Ok((completed_games, games_count));
                }
                return Err(e).context("reading PGN stream");
            }
        };

        let trimmed = line_buf.trim();

        if trimmed.starts_with('[') && seen_moves {
            completed_games.push_str(&current_game);
            if !completed_games.ends_with('\n') {
                completed_games.push('\n');
            }
            current_game.clear();
            seen_moves = false;
            games_count += 1;
            progress(games_count);
            if games_count >= max_games {
                return Ok((completed_games, games_count));
            }
        }

        current_game.push_str(&line_buf);

        if !trimmed.is_empty() && !trimmed.starts_with('[') {
            seen_moves = true;
            if trimmed.ends_with("1-0")
                || trimmed.ends_with("0-1")
                || trimmed.ends_with("1/2-1/2")
                || trimmed.ends_with('*')
            {
                completed_games.push_str(&current_game);
                if !completed_games.ends_with('\n') {
                    completed_games.push('\n');
                }
                current_game.clear();
                seen_moves = false;
                games_count += 1;
                progress(games_count);
                if games_count >= max_games {
                    return Ok((completed_games, games_count));
                }
            }
        }
    }

    Ok((completed_games, games_count))
}

fn download_player_pgn(
    agent: &ureq::Agent,
    source: OnlineSource,
    username: &str,
    max_games: usize,
    progress: &mut dyn FnMut(String),
) -> Result<(String, Option<usize>)> {
    match source {
        OnlineSource::Lichess => {
            progress(format!("Downloading {username}'s Lichess games…"));
            let url = lichess_export_url(username, max_games);
            let response = agent
                .get(&url)
                .set("Accept", "application/x-chess-pgn")
                .call()
                .with_context(|| format!("requesting {url}"))?;
            let reader = std::io::BufReader::new(response.into_reader());
            let (body, count) = read_pgn_stream_incremental(reader, max_games, |c| {
                progress(format!(
                    "Downloading {username}'s Lichess games · {c} games…"
                ));
            })?;
            if body.trim().is_empty() {
                bail!("Lichess returned no games for {username}");
            }
            Ok((body, Some(count)))
        }
        OnlineSource::ChessCom => {
            progress(format!("Finding {username}'s Chess.com archives…"));
            let archives_url =
                format!("https://api.chess.com/pub/player/{username}/games/archives");
            let archives_body = get_text(agent, &archives_url, Some("application/json"))?;
            let archives: Value = serde_json::from_str(&archives_body)
                .context("Chess.com returned invalid archive metadata")?;
            let urls = archive_urls(&archives)?;
            if urls.is_empty() {
                bail!("Chess.com returned no game archives for {username}");
            }

            let mut pgns = Vec::new();
            let archives_to_fetch = if max_games > 50_000 {
                urls.len()
            } else {
                urls.len().min(MAX_ARCHIVES)
            };
            let (current_month, previous_month) = chesscom_current_and_previous();
            for (number, archive_url) in urls.iter().rev().take(archives_to_fetch).enumerate() {
                let year_month = chesscom_year_month_from_url(archive_url);
                let cache_path = year_month
                    .as_deref()
                    .map(|ym| chesscom_cache_path(username, ym));
                let is_immutable = year_month
                    .as_deref()
                    .is_some_and(|ym| ym != current_month && ym != previous_month);
                if is_immutable
                    && let Some(path) = &cache_path
                    && let Ok(cached_text) = fs::read_to_string(path)
                    && !cached_text.trim().is_empty()
                {
                    // Immutable past-month archive: reuse disk cache.
                    let cached_games =
                        domain::parse_pgn(cached_text.as_bytes()).unwrap_or_default();
                    for game in cached_games {
                        pgns.push(crate::pgn::game_to_pgn(&game));
                        if pgns.len() >= max_games {
                            break;
                        }
                    }
                    if pgns.len() >= max_games {
                        break;
                    }
                    continue;
                }
                progress(format!(
                    "Downloading Chess.com archive {} · {} games so far…",
                    number + 1,
                    pgns.len()
                ));
                let body = get_text(agent, archive_url, Some("application/json"))?;
                let archive: Value = serde_json::from_str(&body).with_context(|| {
                    format!("Chess.com archive {archive_url} was not valid JSON")
                })?;
                let month_pgns = chesscom_pgns(&archive, usize::MAX);
                if let (Some(path), Some(_ym)) = (&cache_path, &year_month) {
                    // Cache the full month (not truncated by remaining) so
                    // future runs can reuse it whole.
                    if let Some(parent) = path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let mut text = month_pgns.join("\n\n");
                    if !text.is_empty() {
                        text.push('\n');
                        let _ = fs::write(path, text.as_bytes());
                    }
                }
                pgns.extend(
                    month_pgns
                        .into_iter()
                        .take(max_games - pgns.len().min(max_games)),
                );
                if pgns.len() >= max_games {
                    break;
                }
                // Be polite to Chess.com's public API.
                std::thread::sleep(Duration::from_millis(120));
            }
            if pgns.is_empty() {
                bail!("Chess.com returned no standard games for {username}");
            }
            let count = pgns.len();
            let mut body = pgns.join("\n\n");
            body.push('\n');
            Ok((body, Some(count)))
        }
        OnlineSource::ChessAbc => {
            let player = resolve_chessabc_player(agent, username)?;
            progress(format!("Downloading {}'s ChessABC games…", player.name));
            let (body, count) =
                download_chessabc_games(agent, &player.fide_id, max_games, progress)?;
            Ok((body, Some(count)))
        }
    }
}

fn get_text(agent: &ureq::Agent, url: &str, accept: Option<&str>) -> Result<String> {
    let mut request = agent.get(url);
    if let Some(accept) = accept {
        request = request.set("Accept", accept);
    }
    let response = request
        .call()
        .with_context(|| format!("requesting {url}"))?;
    let mut text = String::new();
    response
        .into_reader()
        .read_to_string(&mut text)
        .with_context(|| format!("reading response from {url}"))?;
    Ok(text)
}

fn archive_urls(archives: &Value) -> Result<Vec<String>> {
    let values = archives
        .get("archives")
        .and_then(Value::as_array)
        .context("Chess.com response has no archives array")?;
    let mut urls = Vec::with_capacity(values.len());
    for value in values {
        let url = value
            .as_str()
            .context("Chess.com archive URL was not a string")?;
        urls.push(url.to_string());
    }
    Ok(urls)
}

fn chesscom_pgns(archive: &Value, remaining: usize) -> Vec<String> {
    if remaining == 0 {
        return Vec::new();
    }
    let Some(games) = archive.get("games").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut pgns = Vec::new();
    for game in games {
        let rules = game.get("rules").and_then(Value::as_str).unwrap_or("chess");
        if rules != "chess" {
            continue;
        }
        let Some(pgn) = game.get("pgn").and_then(Value::as_str) else {
            continue;
        };
        if pgn.trim().is_empty() {
            continue;
        }
        pgns.push(pgn.trim().to_string());
        if pgns.len() >= remaining {
            break;
        }
    }
    pgns
}

fn validate_query(source: OnlineSource, raw: &str) -> Result<&str> {
    let query = raw.trim();
    if query.is_empty() {
        bail!(
            "enter {}",
            if source == OnlineSource::ChessAbc {
                "a player name or FIDE id"
            } else {
                "a username"
            }
        );
    }
    if query.len() > 128 {
        bail!("query is too long");
    }
    if source == OnlineSource::ChessAbc {
        if !query.chars().all(|c| {
            c.is_alphanumeric() || c.is_whitespace() || matches!(c, ',' | '.' | '-' | '\'' | '_')
        }) {
            bail!(
                "player names may only contain letters, digits, spaces, commas, dots, apostrophes and hyphens"
            );
        }
    } else if !query
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("username may only contain letters, digits, '_' and '-'");
    }
    Ok(query)
}

fn encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChessAbcPlayer {
    pub fide_id: String,
    pub name: String,
    pub rating: u64,
    pub games: Option<usize>,
}

pub fn candidate_matches_query(query: &str, candidate_name: &str) -> bool {
    let query_tokens = normalized_name_tokens(query);
    if query_tokens.is_empty() {
        return false;
    }
    if query_tokens.len() >= 2 {
        let query_surname = if let Some((head, _)) = query.split_once(',') {
            normalized_name_tokens(head).join("")
        } else {
            query_tokens.last().cloned().unwrap_or_default()
        };
        let query_given_tokens: Vec<String> = query_tokens
            .iter()
            .filter(|t| **t != query_surname)
            .cloned()
            .collect();

        let (cand_surname, cand_given_tokens) = if let Some((head, tail)) =
            candidate_name.split_once(',')
        {
            let s = normalized_name_tokens(head).join("");
            let g: Vec<String> = normalized_name_tokens(tail)
                .into_iter()
                .filter(|t| !["dr", "prof", "ing", "mag", "dipl", "jr", "sr"].contains(&t.as_str()))
                .map(|t| {
                    let clean = t.trim_end_matches(|c: char| c.is_ascii_digit());
                    if !clean.is_empty() {
                        clean.to_string()
                    } else {
                        t
                    }
                })
                .collect();
            (s, g)
        } else {
            let c_tokens = normalized_name_tokens(candidate_name);
            let s = c_tokens.first().cloned().unwrap_or_default();
            let g = if c_tokens.len() > 1 {
                c_tokens[1..]
                    .iter()
                    .filter(|t| {
                        !["dr", "prof", "ing", "mag", "dipl", "jr", "sr"].contains(&t.as_str())
                    })
                    .map(|t| {
                        let clean = t.trim_end_matches(|c: char| c.is_ascii_digit());
                        if !clean.is_empty() {
                            clean.to_string()
                        } else {
                            t.clone()
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (s, g)
        };

        let surname_matches = query_surname == cand_surname
            || cand_surname.split_whitespace().any(|t| t == query_surname)
            || token_match(&query_surname, &cand_surname).is_some_and(|s| s >= 0.8);
        if !surname_matches {
            return false;
        }

        if !query_given_tokens.is_empty() && !cand_given_tokens.is_empty() {
            let given_matches = query_given_tokens.iter().any(|q_g| {
                cand_given_tokens.iter().any(|c_g| {
                    if q_g == c_g {
                        return true;
                    }
                    if (q_g.chars().count() == 1 && c_g.starts_with(q_g))
                        || (c_g.chars().count() == 1 && q_g.starts_with(c_g))
                    {
                        return true;
                    }
                    if let (Some(q_first), Some(c_first)) = (q_g.chars().next(), c_g.chars().next())
                        && q_first == c_first
                        && (q_g.chars().count() == 1 || c_g.chars().count() == 1)
                    {
                        return true;
                    }
                    token_match(q_g, c_g).is_some_and(|s| s >= 0.75)
                })
            });
            if !given_matches {
                return false;
            }
        }
    }

    if crate::players::player_key(query).is_some()
        && crate::players::player_key(query) == crate::players::player_key(candidate_name)
    {
        return true;
    }
    if crate::players::matches_truncated_or_enumerated_name(query, candidate_name) {
        return true;
    }
    name_match_score(query, candidate_name).is_some_and(|score| score >= 0.55)
}

fn chessabc_player_game_count(agent: &ureq::Agent, fide_id: &str) -> usize {
    let url = format!(
        "https://api.chessabc.com/games/player/paginated?fideId={}&page=0&size=1",
        fide_id.trim()
    );
    let Ok(body) = get_text(agent, &url, Some("application/json")) else {
        return 0;
    };
    let Ok(val) = serde_json::from_str::<Value>(&body) else {
        return 0;
    };
    val.get("totalElements")
        .and_then(Value::as_u64)
        .map(|count| count as usize)
        .unwrap_or(0)
}

fn collect_chessabc_candidates(agent: &ureq::Agent, query: &str) -> Result<Vec<ChessAbcPlayer>> {
    let mut players = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for variant in chessabc_search_queries(query) {
        fetch_chessabc_search(agent, &variant, &mut players, &mut seen)?;
    }
    // Names that have been transliterated differently by the source often
    // still share a long surname prefix. Retry with that prefix and the given
    // name, for example "Nepomniaschi Ian" -> "Nepomni Ian".
    if !players
        .iter()
        .any(|player| name_match_score(query, &player.name).is_some_and(|score| score >= 0.55))
    {
        for variant in chessabc_prefix_queries(query) {
            fetch_chessabc_search(agent, &variant, &mut players, &mut seen)?;
        }
    }
    players.retain(|player| candidate_matches_query(query, &player.name));
    Ok(players)
}

fn resolve_chessabc_player(agent: &ureq::Agent, query: &str) -> Result<ChessAbcPlayer> {
    if query.chars().all(|c| c.is_ascii_digit()) {
        let count = chessabc_player_game_count(agent, query);
        return Ok(ChessAbcPlayer {
            fide_id: query.to_string(),
            name: query.to_string(),
            rating: 0,
            games: Some(count),
        });
    }

    let mut players = collect_chessabc_candidates(agent, query)?;
    for p in &mut players {
        let count = chessabc_player_game_count(agent, &p.fide_id);
        p.games = Some(count);
    }
    players.retain(|p| p.games.unwrap_or(0) >= 1);
    if players.is_empty() {
        bail!("ChessABC found no player matching {query}");
    }
    Ok(choose_chessabc_player(&players, query).expect("non-empty player list"))
}

/// Search ChessABC for every player profile matching `query`, highest rated
/// first. Used by the preparation setup dialog so an ambiguous name shows a
/// checklist instead of silently picking one profile.
pub fn search_chessabc_candidates(query: &str) -> Result<Vec<ChessAbcPlayer>> {
    let query = validate_query(OnlineSource::ChessAbc, query)?.to_string();
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build();
    if query.chars().all(|c| c.is_ascii_digit()) {
        let count = chessabc_player_game_count(&agent, &query);
        return Ok(vec![ChessAbcPlayer {
            fide_id: query.clone(),
            name: query,
            rating: 0,
            games: Some(count),
        }]);
    }
    let mut players = collect_chessabc_candidates(&agent, &query)?;
    for p in &mut players {
        let count = chessabc_player_game_count(&agent, &p.fide_id);
        p.games = Some(count);
    }
    players.retain(|p| p.games.unwrap_or(0) >= 1);
    players.sort_by(|a, b| {
        b.rating
            .cmp(&a.rating)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.fide_id.cmp(&b.fide_id))
    });
    Ok(players)
}

/// Pick the single most probable candidate from a candidate list for `query`.
pub fn best_chessabc_candidate(
    candidates: &[ChessAbcPlayer],
    query: &str,
    player_elo: Option<u32>,
) -> Option<ChessAbcPlayer> {
    let filtered: Vec<ChessAbcPlayer> = candidates
        .iter()
        .filter(|c| c.games != Some(0))
        .cloned()
        .collect();
    if filtered.is_empty() {
        return None;
    }

    // 1. First try auto_select_chessabc_candidate
    if let Some(selected) = auto_select_chessabc_candidate(&filtered, query, player_elo) {
        return Some(selected);
    }

    // 2. Next, check exact player_key matches
    if let Some(query_key) = crate::players::player_key(query) {
        let exact_matches: Vec<&ChessAbcPlayer> = filtered
            .iter()
            .filter(|c| crate::players::player_key(&c.name).as_deref() == Some(&query_key))
            .collect();
        if !exact_matches.is_empty() {
            return exact_matches
                .into_iter()
                .min_by(|a, b| match player_elo {
                    Some(elo) => {
                        let diff_a = a.rating.abs_diff(u64::from(elo));
                        let diff_b = b.rating.abs_diff(u64::from(elo));
                        diff_a
                            .cmp(&diff_b)
                            .then_with(|| b.games.unwrap_or(0).cmp(&a.games.unwrap_or(0)))
                            .then_with(|| b.rating.cmp(&a.rating))
                    }
                    None => b
                        .rating
                        .cmp(&a.rating)
                        .then_with(|| b.games.unwrap_or(0).cmp(&a.games.unwrap_or(0))),
                })
                .cloned();
        }
    }

    // 3. Next, check players::matches_truncated_or_enumerated_name(query, &c.name)
    let truncated: Vec<&ChessAbcPlayer> = filtered
        .iter()
        .filter(|c| crate::players::matches_truncated_or_enumerated_name(query, &c.name))
        .collect();
    if !truncated.is_empty() {
        return truncated
            .into_iter()
            .min_by(|a, b| match player_elo {
                Some(elo) => {
                    let a_has_rating = a.rating > 0;
                    let b_has_rating = b.rating > 0;
                    b_has_rating
                        .cmp(&a_has_rating)
                        .then_with(|| {
                            let diff_a = a.rating.abs_diff(u64::from(elo));
                            let diff_b = b.rating.abs_diff(u64::from(elo));
                            diff_a.cmp(&diff_b)
                        })
                        .then_with(|| b.games.unwrap_or(0).cmp(&a.games.unwrap_or(0)))
                        .then_with(|| b.rating.cmp(&a.rating))
                }
                None => b
                    .games
                    .unwrap_or(0)
                    .cmp(&a.games.unwrap_or(0))
                    .then_with(|| b.rating.cmp(&a.rating)),
            })
            .cloned();
    }

    // 4. Finally fall back to choose_chessabc_player
    choose_chessabc_player(&filtered, query)
}

/// Pick the unambiguous ChessABC profile for `query`, if there is one:
/// an exact FIDE id, a single full-name exact match, or a single
/// surname-initial match whose rating is within 100 Elo of `player_elo`
/// (or any single surname-initial match when no Elo is known).
/// Anything else returns `None` so the UI can ask the user to choose.
pub fn auto_select_chessabc_candidate(
    candidates: &[ChessAbcPlayer],
    query: &str,
    player_elo: Option<u32>,
) -> Option<ChessAbcPlayer> {
    let trimmed = query.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return candidates
            .iter()
            .find(|player| player.fide_id == trimmed)
            .cloned();
    }
    let query_key = crate::players::player_key(trimmed)?;
    let exact: Vec<&ChessAbcPlayer> = candidates
        .iter()
        .filter(|player| {
            crate::players::player_key(&player.name).as_deref() == Some(query_key.as_str())
        })
        .collect();
    if exact.len() == 1 {
        return Some((*exact[0]).clone());
    }
    if !exact.is_empty() {
        return None;
    }
    let truncated: Vec<&ChessAbcPlayer> = candidates
        .iter()
        .filter(|player| {
            crate::players::matches_truncated_or_enumerated_name(trimmed, &player.name)
        })
        .collect();
    if truncated.len() != 1 {
        return None;
    }
    let only = truncated[0];
    match player_elo {
        Some(elo) => {
            let rating = u64::from(elo);
            if only.rating.abs_diff(rating) <= 100 {
                Some(only.clone())
            } else {
                None
            }
        }
        None => Some(only.clone()),
    }
}

/// Download a player's ChessABC games directly by FIDE id. Used when the
/// preparation setup dialog resolved the opponent to explicit profiles.
pub fn fetch_chessabc_games_by_fide_id(
    fide_id: &str,
    max_games: usize,
) -> Result<Vec<domain::Game>> {
    let fide_id = fide_id.trim();
    if fide_id.is_empty() || fide_id.len() > 16 || !fide_id.chars().all(|c| c.is_ascii_digit()) {
        bail!("invalid FIDE id");
    }
    let max_games = normalized_max_games(max_games);
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build();
    let (body, _) = download_chessabc_games(&agent, fide_id, max_games, &mut |_message| {})?;
    let games = domain::parse_pgn(body.as_bytes()).context("parsing downloaded games")?;
    if games.is_empty() {
        bail!("ChessABC returned no parseable games for FIDE id {fide_id}");
    }
    Ok(games)
}

fn fetch_chessabc_search(
    agent: &ureq::Agent,
    query: &str,
    players: &mut Vec<ChessAbcPlayer>,
    seen: &mut std::collections::HashSet<String>,
) -> Result<()> {
    let url = format!(
        "https://api.chessabc.com/players/search?name={}",
        encode_query(query)
    );
    let body = get_text(agent, &url, Some("application/json"))?;
    let values: Vec<Value> =
        serde_json::from_str(&body).context("ChessABC returned invalid player data")?;
    for value in values {
        let fide_id = match value.get("id") {
            Some(Value::Number(number)) => number.to_string(),
            Some(Value::String(id)) => id.clone(),
            _ => continue,
        };
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        let Some(name) = name else {
            continue;
        };
        if seen.insert(fide_id.clone()) {
            players.push(ChessAbcPlayer {
                fide_id,
                name,
                rating: value.get("rating").and_then(Value::as_u64).unwrap_or(0),
                games: None,
            });
        }
    }
    Ok(())
}

fn choose_chessabc_player(players: &[ChessAbcPlayer], query: &str) -> Option<ChessAbcPlayer> {
    players
        .iter()
        .filter_map(|player| name_match_score(query, &player.name).map(|score| (score, player)))
        .max_by(|(a_score, a), (b_score, b)| {
            a_score
                .total_cmp(b_score)
                .then(a.rating.cmp(&b.rating))
                .then(b.fide_id.cmp(&a.fide_id))
        })
        .map(|(_, player)| player.clone())
        .or_else(|| players.iter().max_by_key(|player| player.rating).cloned())
}

/// Query spellings that cope with ChessABC's exact substring search. The API
/// treats a comma as an empty query and does not find hyphens, so punctuation
/// is removed first. Accent folding, German umlaut expansion, and a small
/// phonetic transliteration pass cover common name variants.
fn chessabc_search_queries(query: &str) -> Vec<String> {
    let base = clean_search_name(query);
    if base.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    push_unique(&mut out, base.clone());
    push_unique(&mut out, deunicode::deunicode(&base));
    push_unique(&mut out, deunicode::deunicode(&expand_german(&base)));
    push_unique(&mut out, phonetic_query(&base));
    if let Some(initial_query) = surname_initial_query(query) {
        push_unique(&mut out, initial_query.clone());
        push_unique(&mut out, deunicode::deunicode(&initial_query));
    }
    out
}

/// `"Surname, First"` / `"First Surname"` → `"Surname F"`, the spelling
/// ChessABC's substring search needs to return truncated or enumerated
/// profiles such as `"Kern, J"` and `"Kern, J1"`.
fn surname_initial_query(query: &str) -> Option<String> {
    let trimmed = query.trim();
    let (surname_raw, given_raw) = if let Some((surname, given)) = trimmed.split_once(',') {
        (surname, given)
    } else {
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() < 2 {
            return None;
        }
        (tokens[tokens.len() - 1], tokens[0])
    };
    let surname = clean_search_name(surname_raw);
    if surname.is_empty() {
        return None;
    }
    let initial = given_raw.chars().find(|ch| ch.is_alphabetic())?;
    Some(format!("{surname} {}", initial.to_uppercase()))
}

fn chessabc_prefix_queries(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    for variant in chessabc_search_queries(query).into_iter().take(3) {
        let tokens: Vec<&str> = variant.split_whitespace().collect();
        if tokens.len() < 2 {
            continue;
        }
        let Some((longest, token)) = tokens
            .iter()
            .enumerate()
            .max_by_key(|(_, token)| token.chars().count())
        else {
            continue;
        };
        if token.chars().count() <= 5 {
            continue;
        }
        let prefix: String = token.chars().take(6).collect();
        let mut parts: Vec<String> = tokens.iter().map(|token| (*token).to_string()).collect();
        parts[longest] = prefix;
        push_unique(&mut out, parts.join(" "));
    }
    out
}

fn clean_search_name(query: &str) -> String {
    let mut cleaned = String::with_capacity(query.len());
    for ch in query.chars() {
        if ch.is_alphanumeric() {
            cleaned.push(ch);
        } else if ch.is_whitespace() || matches!(ch, ',' | '.' | '-' | '\'' | '_' | '/') {
            cleaned.push(' ');
        }
    }
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn expand_german(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            'ä' | 'Ä' => out.push_str("ae"),
            'ö' | 'Ö' => out.push_str("oe"),
            'ü' | 'Ü' => out.push_str("ue"),
            'ß' | 'ẞ' => out.push_str("ss"),
            _ => out.push(ch),
        }
    }
    out
}

fn phonetic_query(value: &str) -> String {
    value
        .split_whitespace()
        .map(phonetic_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn phonetic_token(token: &str) -> String {
    let mut text = token.to_ascii_lowercase();
    for (from, to) in [
        ("sch", "s"),
        ("sh", "s"),
        ("ph", "f"),
        ("ck", "k"),
        ("ch", "k"),
        ("kh", "k"),
        ("gh", "g"),
        ("th", "t"),
        ("zh", "j"),
        ("tz", "c"),
        ("ts", "c"),
        ("x", "ks"),
    ] {
        text = text.replace(from, to);
    }
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let mapped = match ch {
            'w' => 'v',
            'j' => 'i',
            'y' => 'i',
            'z' => 's',
            'q' => 'k',
            'c' => 'k',
            'v' => 'f',
            _ => ch,
        };
        if !out.ends_with(mapped) {
            out.push(mapped);
        }
    }
    out
}

fn push_unique(values: &mut Vec<String>, value: String) {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if !value.is_empty() && !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn normalized_name_tokens(name: &str) -> Vec<String> {
    deunicode::deunicode(name)
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn name_match_score(query: &str, candidate: &str) -> Option<f64> {
    let query_tokens = normalized_name_tokens(query);
    let candidate_tokens = normalized_name_tokens(candidate);
    if query_tokens.is_empty() || candidate_tokens.is_empty() {
        return None;
    }
    // A single surname is ambiguous by design; score every candidate that
    // contains it equally and let rating break the tie. This preserves the
    // useful "search by surname" behavior without letting a long extra given
    // name bury the well-known player.
    if query_tokens.len() == 1 {
        return candidate_tokens
            .iter()
            .any(|token| token_match(&query_tokens[0], token).is_some())
            .then_some(1.0);
    }
    if candidate_tokens.len() == 1 {
        return query_tokens
            .iter()
            .any(|token| token_match(token, &candidate_tokens[0]).is_some())
            .then_some(1.0);
    }
    let (score, matched) = best_token_match(&query_tokens, &candidate_tokens)?;
    if matched < query_tokens.len().min(candidate_tokens.len()) {
        return None;
    }
    Some(score)
}

fn best_token_match(query: &[String], candidate: &[String]) -> Option<(f64, usize)> {
    let mut pairs = Vec::new();
    for (query_index, query_token) in query.iter().enumerate() {
        for (candidate_index, candidate_token) in candidate.iter().enumerate() {
            if let Some(score) = token_match(query_token, candidate_token) {
                pairs.push((score, query_index, candidate_index));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut used_query = vec![false; query.len()];
    let mut used_candidate = vec![false; candidate.len()];
    let mut sum = 0.0;
    let mut matched = 0usize;
    for (score, query_index, candidate_index) in pairs {
        if used_query[query_index] || used_candidate[candidate_index] {
            continue;
        }
        used_query[query_index] = true;
        used_candidate[candidate_index] = true;
        sum += score;
        matched += 1;
    }
    if matched == 0 {
        None
    } else {
        Some((sum / query.len().max(candidate.len()) as f64, matched))
    }
}

fn token_match(left: &str, right: &str) -> Option<f64> {
    if left == right {
        return Some(1.0);
    }
    if (left.chars().count() == 1 && right.starts_with(left))
        || (right.chars().count() == 1 && left.starts_with(right))
    {
        return Some(0.9);
    }
    if phonetic_token(left) == phonetic_token(right) {
        return Some(0.8);
    }
    if name_alias(left, right) {
        return Some(0.78);
    }
    let distance = levenshtein(left, right);
    let longest = left.chars().count().max(right.chars().count());
    if longest >= 8 && distance <= 3 {
        Some(0.55)
    } else if longest >= 6 && distance <= 2 {
        Some(0.6)
    } else if longest >= 4 && distance <= 1 {
        Some(0.75)
    } else {
        None
    }
}

/// Common given-name transliterations/anglizations. These are deliberately
/// conservative and only supplement the surname match.
fn name_alias(left: &str, right: &str) -> bool {
    const ALIASES: &[(&str, &str)] = &[
        ("alexei", "alexey"),
        ("alexei", "alexi"),
        ("alexey", "alexi"),
        ("andrei", "andrey"),
        ("dmitri", "dmitry"),
        ("evgeni", "evgeny"),
        ("evgeni", "eugene"),
        ("fyodor", "theodore"),
        ("mikhail", "michael"),
        ("nikolai", "nicholas"),
        ("pyotr", "peter"),
        ("petr", "peter"),
        ("sergei", "sergey"),
        ("yakov", "jacob"),
        ("yuri", "juri"),
        ("yuri", "jurij"),
    ];
    ALIASES
        .iter()
        .any(|&(a, b)| (left == a && right == b) || (left == b && right == a))
}

fn levenshtein(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != right_char);
            current[j + 1] = (previous[j + 1] + 1).min(current[j] + 1).min(substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn download_chessabc_games(
    agent: &ureq::Agent,
    fide_id: &str,
    max_games: usize,
    progress: &mut dyn FnMut(String),
) -> Result<(String, usize)> {
    const PAGE_SIZE: usize = 100;
    let mut page = 0usize;
    let mut pgns = Vec::new();
    loop {
        progress(format!(
            "Downloading ChessABC page {} · {} games so far…",
            page + 1,
            pgns.len()
        ));
        let url = format!(
            "https://api.chessabc.com/games/player/paginated?fideId={fide_id}&page={page}&size={PAGE_SIZE}"
        );
        let body = get_text(agent, &url, Some("application/json"))?;
        let value: Value =
            serde_json::from_str(&body).context("ChessABC returned invalid game data")?;
        let content = value
            .get("content")
            .and_then(Value::as_array)
            .context("ChessABC response has no content array")?;
        if content.is_empty() {
            break;
        }
        for game in content {
            if let Some(pgn) = chessabc_pgn(game) {
                pgns.push(pgn);
                if pgns.len() >= max_games {
                    break;
                }
            }
        }
        if pgns.len() >= max_games {
            break;
        }
        let total_pages = value.get("totalPages").and_then(Value::as_u64).unwrap_or(1);
        if (page as u64) + 1 >= total_pages {
            break;
        }
        page += 1;
        std::thread::sleep(Duration::from_millis(80));
    }
    if pgns.is_empty() {
        bail!("ChessABC returned no standard games for FIDE id {fide_id}");
    }
    let count = pgns.len();
    let mut body = pgns.join("\n\n");
    body.push('\n');
    Ok((body, count))
}

fn chessabc_pgn(game: &Value) -> Option<String> {
    let moves = game.get("moves")?.as_str()?.trim();
    if moves.is_empty() {
        return None;
    }
    let result = match game.get("result").and_then(Value::as_str).unwrap_or("") {
        "WHITE_WINS" => "1-0",
        "BLACK_WINS" => "0-1",
        "DRAW" => "1/2-1/2",
        _ => "*",
    };
    let date = game
        .get("gameDate")
        .and_then(Value::as_str)
        .map(short_pgn_date)
        .unwrap_or_else(|| "????.??.??".to_string());
    let mut headers = vec![
        ("Event", text_field(game, "event", "?")),
        ("Site", text_field(game, "site", "?")),
        ("Date", date),
        ("Round", text_field(game, "round", "?")),
        ("White", text_field(game, "whitePlayer", "?")),
        ("Black", text_field(game, "blackPlayer", "?")),
        ("Result", result.to_string()),
    ];
    for (tag, field) in [
        ("WhiteElo", "whiteElo"),
        ("BlackElo", "blackElo"),
        ("WhiteTitle", "whiteTitle"),
        ("BlackTitle", "blackTitle"),
        ("WhiteFideId", "whiteFideId"),
        ("BlackFideId", "blackFideId"),
        ("ECO", "eco"),
    ] {
        if let Some(value) = game.get(field) {
            let text = match value {
                Value::String(text) => text.trim().to_string(),
                Value::Number(number) => number.to_string(),
                _ => continue,
            };
            if !text.is_empty() {
                headers.push((tag, text));
            }
        }
    }
    if let Some(fen) = game.get("fen").and_then(Value::as_str).map(str::trim)
        && !fen.is_empty()
    {
        headers.push(("SetUp", "1".to_string()));
        headers.push(("FEN", fen.to_string()));
    }

    let mut pgn = String::new();
    for (tag, value) in headers {
        pgn.push('[');
        pgn.push_str(tag);
        pgn.push_str(" \"");
        pgn.push_str(&escape_tag(&value));
        pgn.push_str("\"]\n");
    }
    pgn.push('\n');
    pgn.push_str(moves);
    if !moves
        .split_whitespace()
        .last()
        .is_some_and(|token| token == result)
    {
        pgn.push(' ');
        pgn.push_str(result);
    }
    pgn.push('\n');
    Some(pgn)
}

fn text_field(game: &Value, key: &str, fallback: &str) -> String {
    game.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn short_pgn_date(raw: &str) -> String {
    let date = raw.get(..10).unwrap_or(raw);
    if date.len() == 10
        && date.as_bytes()[4] == b'-'
        && date.as_bytes()[7] == b'-'
        && date
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        date.replace('-', ".")
    } else {
        "????.??.??".to_string()
    }
}

fn escape_tag(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Directory holding per-account online caches.
pub fn online_cache_dir() -> PathBuf {
    cache_root().join("online")
}

/// Lichess incremental cache: unfiltered full history, newest first.
pub fn lichess_cache_path(username: &str) -> PathBuf {
    online_cache_dir()
        .join("lichess")
        .join(format!("{}.pgn", username.trim().to_lowercase()))
}

/// Chess.com per-month cache directory for one user.
pub fn chesscom_cache_dir(username: &str) -> PathBuf {
    online_cache_dir()
        .join("chesscom")
        .join(username.trim().to_lowercase())
}

/// One immutable (or current/previous) month archive file.
pub fn chesscom_cache_path(username: &str, year_month: &str) -> PathBuf {
    chesscom_cache_dir(username).join(format!("{year_month}.pgn"))
}

/// Total bytes used by the online game cache.
pub fn online_cache_size_bytes() -> u64 {
    fn walk(dir: &std::path::Path, total: &mut u64) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, total);
            } else if let Ok(meta) = path.metadata() {
                *total += meta.len();
            }
        }
    }
    let mut total = 0u64;
    walk(&online_cache_dir(), &mut total);
    total
}

/// Remove every cached online game file.
pub fn clear_online_cache() -> Result<()> {
    let dir = online_cache_dir();
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(())
}

/// Quick cached game count for probe display (counts `[Event ` tags).
pub fn cached_game_count(source: OnlineSource, username: &str) -> usize {
    fn count_pgn_file(path: &std::path::Path) -> usize {
        let Ok(text) = fs::read_to_string(path) else {
            return 0;
        };
        // `[Event ` starts every game in our caches.
        text.matches("[Event ").count()
    }
    match source {
        OnlineSource::Lichess => count_pgn_file(&lichess_cache_path(username)),
        OnlineSource::ChessCom => {
            let dir = chesscom_cache_dir(username);
            let mut total = 0usize;
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "pgn") {
                        total += count_pgn_file(&path);
                    }
                }
            }
            total
        }
        OnlineSource::ChessAbc => 0,
    }
}

/// Extract a Lichess game id from `Site` (`https://lichess.org/<id>`) or `GameId`.
pub fn lichess_game_id(game: &domain::Game) -> Option<String> {
    if let Some(site) = game.header("Site") {
        let site = site.trim();
        if let Some(pos) = site.rfind("lichess.org/") {
            let id = &site[pos + "lichess.org/".len()..];
            let id = id.split(['?', '#', '/', ' ']).next().unwrap_or(id).trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    if let Some(id) = game.header("GameId")
        && !id.trim().is_empty()
    {
        return Some(id.trim().to_string());
    }
    if let Some(id) = game.header("ID")
        && !id.trim().is_empty()
    {
        return Some(id.trim().to_string());
    }
    None
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn parse_ymd(text: &str) -> Option<(i64, u32, u32)> {
    let text = text.trim();
    let parts: Vec<&str> = text
        .split(['.', '-', '/'])
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

fn parse_hms(text: &str) -> Option<(u32, u32, u32)> {
    let text = text.trim();
    let parts: Vec<&str> = text.split(':').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Some((0, 0, 0));
    }
    let h = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
    let m = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let s = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((h, m, s))
}

/// Unix milliseconds for a game from `UTCDate`/`UTCTime` (falling back to `Date`).
pub fn game_timestamp_ms(game: &domain::Game) -> Option<u64> {
    let (y, m, d) = if let Some(utc) = game.header("UTCDate").and_then(parse_ymd) {
        utc
    } else {
        parse_ymd(game.header("Date")?)?
    };
    let (hh, mm, ss) = game
        .header("UTCTime")
        .and_then(parse_hms)
        .unwrap_or((0, 0, 0));
    let days = days_from_civil(y, m, d);
    let secs = days * 86_400 + hh as i64 * 3600 + mm as i64 * 60 + ss as i64;
    if secs < 0 {
        None
    } else {
        Some((secs as u64) * 1000)
    }
}

fn lichess_url_unfiltered(
    username: &str,
    max: Option<usize>,
    since_ms: Option<u64>,
    until_ms: Option<u64>,
) -> String {
    let mut params = Vec::new();
    if let Some(max) = max {
        params.push(format!("max={max}"));
    }
    if let Some(since) = since_ms {
        params.push(format!("since={since}"));
    }
    if let Some(until) = until_ms {
        params.push(format!("until={until}"));
    }
    params.push("clocks=false".to_string());
    params.push("evals=false".to_string());
    params.push("opening=false".to_string());
    params.push("tags=true".to_string());
    params.push("sort=dateDesc".to_string());
    format!(
        "https://lichess.org/api/games/user/{username}?{}",
        params.join("&")
    )
}

/// Paged Lichess export (unfiltered). Pages backwards with
/// `until=<oldest ms>` until a page returns no new games, deduping by id.
/// `max_games == usize::MAX` means uncapped (page size 10000).
fn fetch_lichess_games_paged(
    agent: &ureq::Agent,
    username: &str,
    max_games: usize,
    since_ms: Option<u64>,
) -> Result<Vec<domain::Game>> {
    const PAGE: usize = 10_000;
    let mut collected: Vec<domain::Game> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut until_ms: Option<u64> = None;
    loop {
        let remaining = if max_games == usize::MAX {
            PAGE
        } else {
            max_games.saturating_sub(collected.len()).min(PAGE)
        };
        if remaining == 0 {
            break;
        }
        let url = lichess_url_unfiltered(username, Some(remaining), since_ms, until_ms);
        let response = agent
            .get(&url)
            .set("Accept", "application/x-chess-pgn")
            .call()
            .with_context(|| format!("requesting {url}"))?;
        let reader = std::io::BufReader::new(response.into_reader());
        let (body, _) = read_pgn_stream_incremental(reader, remaining, |_| {})?;
        if body.trim().is_empty() {
            break;
        }
        let page_games = domain::parse_pgn(body.as_bytes()).context("parsing downloaded games")?;
        if page_games.is_empty() {
            break;
        }
        let page_len = page_games.len();
        let mut new_in_page = 0usize;
        for game in page_games {
            let id = lichess_game_id(&game);
            if let Some(id) = &id
                && !seen.insert(id.clone())
            {
                continue;
            }
            new_in_page += 1;
            collected.push(game);
            if max_games != usize::MAX && collected.len() >= max_games {
                break;
            }
        }
        if new_in_page == 0 {
            break;
        }
        if max_games != usize::MAX && collected.len() >= max_games {
            break;
        }
        // If this page was not full, there is nothing older to page to
        // (except the exact-10000 case, which is full and falls through).
        if page_len < remaining {
            break;
        }
        let oldest_ms = collected.last().and_then(game_timestamp_ms);
        let Some(oldest) = oldest_ms else {
            break;
        };
        if until_ms == Some(oldest) {
            break;
        }
        until_ms = Some(oldest);
    }
    Ok(collected)
}

fn write_games_cache(path: &std::path::Path, games: &[domain::Game]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating online cache {}", parent.display()))?;
    }
    let mut text = String::new();
    for game in games {
        text.push_str(&crate::pgn::game_to_pgn(game));
    }
    fs::write(path, text.as_bytes())
        .with_context(|| format!("writing online cache to {}", path.display()))?;
    Ok(())
}

fn read_games_cache(path: &std::path::Path) -> Option<Vec<domain::Game>> {
    let text = fs::read_to_string(path).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    domain::parse_pgn(text.as_bytes())
        .ok()
        .filter(|g| !g.is_empty())
}

fn chesscom_current_and_previous() -> (String, String) {
    let today = crate::players::today();
    let year = today / 10_000;
    let month = (today / 100) % 100;
    let current = format!("{year:04}-{month:02}");
    let (py, pm) = if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    };
    let previous = format!("{py:04}-{pm:02}");
    (current, previous)
}

fn chesscom_year_month_from_url(url: &str) -> Option<String> {
    // https://api.chess.com/pub/player/<user>/games/2024/01
    let segments: Vec<&str> = url.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 2 {
        let year = segments[segments.len() - 2];
        let month = segments[segments.len() - 1];
        if year.len() == 4
            && year.chars().all(|c| c.is_ascii_digit())
            && let Ok(m) = month.parse::<u32>()
            && (1..=12).contains(&m)
        {
            return Some(format!("{year}-{m:02}"));
        }
    }
    None
}

fn output_path(source: OnlineSource, username: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    cache_root().join("online").join(format!(
        "{}-{}-{stamp}.pgn",
        source.slug(),
        username.to_lowercase()
    ))
}

fn cache_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(dir).join("brett");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache").join("brett");
    }
    std::env::temp_dir().join("brett")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_restricted_to_safe_url_characters() {
        assert_eq!(
            validate_query(OnlineSource::Lichess, " DrNykterstein ").unwrap(),
            "DrNykterstein"
        );
        assert!(validate_query(OnlineSource::Lichess, "Magnus Carlsen").is_err());
        assert!(validate_query(OnlineSource::ChessCom, "name/../other").is_err());
        assert!(validate_query(OnlineSource::Lichess, "").is_err());
        assert_eq!(
            validate_query(OnlineSource::ChessAbc, " Carlsen, Magnus ").unwrap(),
            "Carlsen, Magnus"
        );
        assert!(validate_query(OnlineSource::ChessAbc, "name/../other").is_err());
    }

    #[test]
    fn chessabc_auto_select_picks_only_unambiguous_profiles() {
        let carlsen = |fide_id: &str, rating: u64| ChessAbcPlayer {
            fide_id: fide_id.to_string(),
            name: "Carlsen, Magnus".to_string(),
            rating,
            games: None,
        };
        // Exact FIDE id wins even among several profiles.
        let candidates = vec![
            carlsen("1503014", 2823),
            ChessAbcPlayer {
                fide_id: "999".to_string(),
                name: "Carlsen, Christian".to_string(),
                rating: 1626,
                games: None,
            },
        ];
        assert_eq!(
            auto_select_chessabc_candidate(&candidates, "1503014", None)
                .unwrap()
                .fide_id,
            "1503014"
        );
        assert!(auto_select_chessabc_candidate(&candidates, "1234567", None).is_none());
        // A single full-name exact match is unambiguous.
        assert_eq!(
            auto_select_chessabc_candidate(&candidates, "Magnus Carlsen", None)
                .unwrap()
                .fide_id,
            "1503014"
        );
        // Two full-name exact matches are ambiguous.
        let doubled = vec![carlsen("1503014", 2823), carlsen("1704444", 2500)];
        assert!(auto_select_chessabc_candidate(&doubled, "Magnus Carlsen", None).is_none());
        // A single surname-initial match with compatible Elo is selected.
        let initial = vec![
            ChessAbcPlayer {
                fide_id: "1".to_string(),
                name: "Hinterreiter,M".to_string(),
                rating: 2450,
                games: None,
            },
            ChessAbcPlayer {
                fide_id: "2".to_string(),
                name: "Huber, M".to_string(),
                rating: 1800,
                games: None,
            },
        ];
        assert_eq!(
            auto_select_chessabc_candidate(&initial, "Hinterreiter, Martin", Some(2500))
                .unwrap()
                .fide_id,
            "1"
        );
        // An incompatible Elo (>100 away) stays ambiguous.
        assert!(
            auto_select_chessabc_candidate(&initial, "Hinterreiter, Martin", Some(1800)).is_none()
        );
        // Two surname-initial matches for the same query stay ambiguous.
        let paired = vec![
            ChessAbcPlayer {
                fide_id: "1".to_string(),
                name: "Meyer,M 1".to_string(),
                rating: 2400,
                games: None,
            },
            ChessAbcPlayer {
                fide_id: "2".to_string(),
                name: "Meyer,M 2".to_string(),
                rating: 2350,
                games: None,
            },
        ];
        assert!(auto_select_chessabc_candidate(&paired, "Meyer, Max", Some(2400)).is_none());
    }

    #[test]
    fn chessabc_player_search_prefers_an_exact_name_then_rating() {
        let players = vec![
            ChessAbcPlayer {
                fide_id: "1".to_string(),
                name: "Carlsen, Magnus".to_string(),
                rating: 2823,
                games: None,
            },
            ChessAbcPlayer {
                fide_id: "2".to_string(),
                name: "Carlsen, Magnus Junior".to_string(),
                rating: 2900,
                games: None,
            },
            ChessAbcPlayer {
                fide_id: "3".to_string(),
                name: "Carlsen, Christian".to_string(),
                rating: 1626,
                games: None,
            },
        ];
        assert_eq!(
            choose_chessabc_player(&players, "Magnus Carlsen")
                .unwrap()
                .fide_id,
            "1"
        );
        assert_eq!(
            choose_chessabc_player(&players, "Carlsen").unwrap().fide_id,
            "2"
        );
    }

    #[test]
    fn filter_matching_chessabc_candidates_drops_substring_noise() {
        let query = "Johannes Kern";
        assert!(!candidate_matches_query(query, "Brustkern, Juergen"));
        assert!(!candidate_matches_query(query, "Bikernieks, Anrijs"));
        assert!(!candidate_matches_query(query, "Kern, Benjamin"));
        assert!(candidate_matches_query(query, "Kern, Johannes, Dr."));
        assert!(candidate_matches_query(query, "Kern, J"));
        assert!(candidate_matches_query(query, "Kern, J1"));
    }

    #[test]
    fn best_chessabc_candidate_prefers_exact_and_closest_elo() {
        let candidates = vec![
            ChessAbcPlayer {
                fide_id: "1646885".to_string(),
                name: "Kern, Johann".to_string(),
                rating: 0,
                games: Some(0),
            },
            ChessAbcPlayer {
                fide_id: "4604776".to_string(),
                name: "Kern, Johannes, Dr.".to_string(),
                rating: 2156,
                games: Some(11),
            },
        ];
        let best = best_chessabc_candidate(&candidates, "Johannes Kern", Some(2150));
        assert_eq!(best.as_ref().map(|c| c.fide_id.as_str()), Some("4604776"));

        // Even when Johann has games, Dr. stripped makes Johannes Dr. exact player_key match
        let candidates2 = vec![
            ChessAbcPlayer {
                fide_id: "1646885".to_string(),
                name: "Kern, Johann".to_string(),
                rating: 0,
                games: Some(5),
            },
            ChessAbcPlayer {
                fide_id: "4604776".to_string(),
                name: "Kern, Johannes, Dr.".to_string(),
                rating: 2156,
                games: Some(11),
            },
        ];
        let best2 = best_chessabc_candidate(&candidates2, "Johannes Kern", Some(2150));
        assert_eq!(best2.as_ref().map(|c| c.fide_id.as_str()), Some("4604776"));
    }

    #[test]
    fn chessabc_games_are_converted_to_valid_pgn() {
        let game: Value = serde_json::from_str(
            r#"{
                "event":"Test Open",
                "site":"chess.com INT",
                "round":"3",
                "whitePlayer":"Carlsen,M",
                "blackPlayer":"Nakamura,H",
                "whiteTitle":"GM",
                "blackTitle":"GM",
                "whiteFideId":1503014,
                "blackFideId":2016192,
                "whiteElo":2823,
                "blackElo":2802,
                "result":"WHITE_WINS",
                "eco":"A01",
                "moves":"1. b3 d5 2. Bb2 c6 1-0",
                "fen":null,
                "gameDate":"2026-09-24T00:00:00"
            }"#,
        )
        .unwrap();
        let pgn = chessabc_pgn(&game).unwrap();
        assert!(pgn.contains("[White \"Carlsen,M\"]"));
        assert!(pgn.contains("[WhiteFideId \"1503014\"]"));
        assert!(pgn.contains("[Date \"2026.09.24\"]"));
        assert!(pgn.contains("[Result \"1-0\"]"));
        let parsed = domain::parse_pgn(pgn.as_bytes()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].result(), "1-0");
        assert_eq!(parsed[0].mainline_len(), 4);
    }

    #[test]
    fn chesscom_archives_keep_standard_games_and_respect_the_limit() {
        let json: Value = serde_json::from_str(
            r#"{"games":[
                {"rules":"chess","pgn":"[Event \"A\"]\n1. e4 *"},
                {"rules":"chess960","pgn":"[Event \"B\"]\n1. e4 *"},
                {"rules":"chess","pgn":"[Event \"C\"]\n1. d4 *"}
            ]}"#,
        )
        .unwrap();
        let pgns = chesscom_pgns(&json, 1);
        assert_eq!(pgns.len(), 1);
        assert!(pgns[0].contains("[Event \"A\"]"));
    }

    #[test]
    fn archive_urls_are_parsed() {
        let json: Value = serde_json::from_str(
            r#"{"archives":["https://api.chess.com/pub/player/test/games/2024/01"]}"#,
        )
        .unwrap();
        assert_eq!(
            archive_urls(&json).unwrap(),
            ["https://api.chess.com/pub/player/test/games/2024/01"]
        );
    }

    #[test]
    fn imported_files_live_in_the_cache() {
        let path = output_path(OnlineSource::Lichess, "TestUser");
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("lichess-testuser-"));
        assert!(name.ends_with(".pgn"));
        assert!(path.starts_with(cache_root()));
        assert!(path.parent().unwrap().ends_with("online"));
    }

    #[test]
    fn chessabc_search_queries_include_surname_and_first_initial() {
        let queries = chessabc_search_queries("Kern, Johannes");
        assert!(
            queries.iter().any(|q| q.eq_ignore_ascii_case("Kern J")),
            "expected 'Kern J' in {queries:?}"
        );
    }

    #[test]
    fn lichess_export_url_omits_max_when_uncapped() {
        assert_eq!(
            lichess_export_url("DrNykterstein", usize::MAX),
            "https://lichess.org/api/games/user/DrNykterstein?clocks=false&evals=false&opening=false&tags=true&sort=dateDesc"
        );
        assert_eq!(
            lichess_export_url("DrNykterstein", 300),
            "https://lichess.org/api/games/user/DrNykterstein?max=300&clocks=false&evals=false&opening=false&tags=true&sort=dateDesc"
        );
    }

    #[test]
    fn lichess_export_url_includes_perf_type_rated_and_since() {
        let mut filter = crate::players::AccountFilter::default();
        // Default now includes bullet, so all four perfs = no perfType param.
        let url = lichess_export_url_filtered("Makefrench", usize::MAX, Some(&filter), 20261108);
        assert!(!url.contains("perfType="), "url was {url}");
        assert!(!url.contains("max="), "url was {url}");
        filter.time_controls.bullet = false;
        let url = lichess_export_url_filtered("Makefrench", usize::MAX, Some(&filter), 20261108);
        assert!(
            url.contains("perfType=classical,rapid,blitz"),
            "url was {url}"
        );

        filter.include_casual = false;
        filter.date_cutoff = crate::players::DateCutoff::SinceDate(20260101);
        let url2 = lichess_export_url_filtered("Makefrench", 200, Some(&filter), 20261108);
        assert!(url2.contains("max=200"), "url was {url2}");
        assert!(url2.contains("rated=true"), "url was {url2}");
        assert!(url2.contains("since="), "url was {url2}");
    }

    struct TimeoutAfterReader {
        data: Vec<u8>,
        pos: usize,
    }

    impl std::io::Read for TimeoutAfterReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos < self.data.len() {
                let bytes_to_copy = (self.data.len() - self.pos).min(buf.len());
                buf[..bytes_to_copy]
                    .copy_from_slice(&self.data[self.pos..self.pos + bytes_to_copy]);
                self.pos += bytes_to_copy;
                Ok(bytes_to_copy)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "simulated read timeout",
                ))
            }
        }
    }

    #[test]
    fn read_pgn_stream_incremental_keeps_complete_games_on_partial_timeout() {
        let pgn_data = b"[Event \"Game 1\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n\n[Event \"Game 2\"]\n[Result \"0-1\"]\n\n1. d4 d5 0-1\n\n[Event \"Game 3 incomplete\"]\n";
        let reader = std::io::BufReader::new(TimeoutAfterReader {
            data: pgn_data.to_vec(),
            pos: 0,
        });
        let mut progress_calls = Vec::new();
        let result = read_pgn_stream_incremental(reader, 10, |count| {
            progress_calls.push(count);
        });
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let (body, count) = result.unwrap();
        assert_eq!(count, 2);
        assert!(body.contains("Game 1"));
        assert!(body.contains("Game 2"));
        assert!(!body.contains("Game 3"));
        assert_eq!(progress_calls, vec![1, 2]);
    }

    #[test]
    fn parse_account_or_profile_input_cases() {
        assert_eq!(
            parse_account_or_profile_input("https://lichess.org/@/Makefrench"),
            (Some(OnlineSource::Lichess), "Makefrench".to_string())
        );
        assert_eq!(
            parse_account_or_profile_input("https://lichess.org/@/Makefrench/all"),
            (Some(OnlineSource::Lichess), "Makefrench".to_string())
        );
        assert_eq!(
            parse_account_or_profile_input("https://www.chess.com/member/hikaru"),
            (Some(OnlineSource::ChessCom), "hikaru".to_string())
        );
        assert_eq!(
            parse_account_or_profile_input("https://chess.com/players/hikaru"),
            (Some(OnlineSource::ChessCom), "hikaru".to_string())
        );
        assert_eq!(
            parse_account_or_profile_input("https://www.chessabc.com/player/4604776"),
            (Some(OnlineSource::ChessAbc), "4604776".to_string())
        );
        assert_eq!(
            parse_account_or_profile_input("https://ratings.fide.com/profile/4604776"),
            (Some(OnlineSource::ChessAbc), "4604776".to_string())
        );
        assert_eq!(
            parse_account_or_profile_input("Makefrench"),
            (None, "Makefrench".to_string())
        );
        assert_eq!(parse_account_or_profile_input("  "), (None, String::new()));
    }

    #[test]
    fn cache_paths_use_spec_layout() {
        let lichess = lichess_cache_path("MakeFRENCH");
        assert!(lichess.ends_with("online/lichess/makefrench.pgn"));
        let chesscom = chesscom_cache_path("Hikaru", "2024-01");
        assert!(chesscom.ends_with("online/chesscom/hikaru/2024-01.pgn"));
        assert_eq!(
            chesscom_year_month_from_url("https://api.chess.com/pub/player/hikaru/games/2024/01"),
            Some("2024-01".to_string())
        );
    }

    #[test]
    fn lichess_game_id_and_timestamp() {
        let pgn = "[Event \"Rated Blitz\"]\n[Site \"https://lichess.org/abcd1234\"]\n[Date \"2024.01.15\"]\n[UTCDate \"2024.01.15\"]\n[UTCTime \"12:00:00\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n";
        let games = domain::parse_pgn(pgn.as_bytes()).unwrap();
        assert_eq!(lichess_game_id(&games[0]).as_deref(), Some("abcd1234"));
        // 2024-01-15 12:00 UTC
        assert!(game_timestamp_ms(&games[0]).is_some());
        // Days-from-civil sanity: 1970-01-01 is 0
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }
}
