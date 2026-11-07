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
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
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
    let username = validate_query(source, username)?;
    let max_games = normalized_max_games(max_games);
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
        .build();
    let (body, _) = download_player_pgn(&agent, source, username, max_games, &mut |_message| {})?;
    let games = domain::parse_pgn(body.as_bytes()).context("parsing downloaded games")?;
    if games.is_empty() {
        bail!(
            "{} returned no parseable games for {username}",
            source.label()
        );
    }
    Ok(games)
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
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
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
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
        .build();
    let mut pgns = Vec::new();
    let mut page = 0usize;
    loop {
        let url = format!("https://api.chessabc.com/games/paginated?page={page}&size={PAGE_SIZE}");
        let body = get_text(&agent, &url, Some("application/json"))?;
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
        std::thread::sleep(Duration::from_millis(60));
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

fn lichess_export_url(username: &str, max_games: usize) -> String {
    if max_games == 0 || max_games > 50_000 {
        format!(
            "https://lichess.org/api/games/user/{username}?clocks=false&evals=false&opening=false&tags=true&sort=dateDesc"
        )
    } else {
        format!(
            "https://lichess.org/api/games/user/{username}?max={max_games}&clocks=false&evals=false&opening=false&tags=true&sort=dateDesc"
        )
    }
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
            let body = get_text(agent, &url, Some("application/x-chess-pgn"))?;
            if body.trim().is_empty() {
                bail!("Lichess returned no games for {username}");
            }
            Ok((body, None))
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
            for (number, archive_url) in urls.iter().rev().take(archives_to_fetch).enumerate() {
                progress(format!(
                    "Downloading Chess.com archive {} · {} games so far…",
                    number + 1,
                    pgns.len()
                ));
                let body = get_text(agent, archive_url, Some("application/json"))?;
                let archive: Value = serde_json::from_str(&body).with_context(|| {
                    format!("Chess.com archive {archive_url} was not valid JSON")
                })?;
                pgns.extend(chesscom_pgns(&archive, max_games - pgns.len()));
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
    Ok(players)
}

fn resolve_chessabc_player(agent: &ureq::Agent, query: &str) -> Result<ChessAbcPlayer> {
    if query.chars().all(|c| c.is_ascii_digit()) {
        return Ok(ChessAbcPlayer {
            fide_id: query.to_string(),
            name: query.to_string(),
            rating: 0,
        });
    }

    let players = collect_chessabc_candidates(agent, query)?;
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
    if query.chars().all(|c| c.is_ascii_digit()) {
        return Ok(vec![ChessAbcPlayer {
            fide_id: query.clone(),
            name: query,
            rating: 0,
        }]);
    }
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
        .build();
    let mut players = collect_chessabc_candidates(&agent, &query)?;
    players.sort_by(|a, b| {
        b.rating
            .cmp(&a.rating)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.fide_id.cmp(&b.fide_id))
    });
    Ok(players)
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
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
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
        };
        // Exact FIDE id wins even among several profiles.
        let candidates = vec![
            carlsen("1503014", 2823),
            ChessAbcPlayer {
                fide_id: "999".to_string(),
                name: "Carlsen, Christian".to_string(),
                rating: 1626,
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
            },
            ChessAbcPlayer {
                fide_id: "2".to_string(),
                name: "Huber, M".to_string(),
                rating: 1800,
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
            },
            ChessAbcPlayer {
                fide_id: "2".to_string(),
                name: "Meyer,M 2".to_string(),
                rating: 2350,
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
            },
            ChessAbcPlayer {
                fide_id: "2".to_string(),
                name: "Carlsen, Magnus Junior".to_string(),
                rating: 2900,
            },
            ChessAbcPlayer {
                fide_id: "3".to_string(),
                name: "Carlsen, Christian".to_string(),
                rating: 1626,
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
}
