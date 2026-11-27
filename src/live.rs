//! Live Lichess broadcasts & server games + Chess.com live streamers,
//! leaderboards, player metadata, avatars, titles, and clock extraction.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::chess::{CastlingMode, Chess, Color, Fen, Position, SanPlus, UciMove};
use crate::country;
use crate::domain::{Clock, Game, Node, Ply, parse_pgn};
use crate::league_import::LichessBroadcastRound;
use crate::titles::{self, PlayerTitle};

const USER_AGENT: &str = "Brett/0.3 (https://github.com/brett-chess/brett)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveSource {
    LichessBroadcast,
    LichessServer,
    ChessComLive,
    ChessComEvent,
}

impl LiveSource {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LichessBroadcast => "Lichess Broadcast",
            Self::LichessServer => "Lichess Live",
            Self::ChessComLive => "Chess.com Live",
            Self::ChessComEvent => "Chess.com Event",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LivePlayerInfo {
    pub name: String,
    pub real_name: Option<String>,
    pub title: Option<PlayerTitle>,
    pub rating: Option<u32>,
    pub country_code: Option<String>,
    pub flair: Option<String>,
    pub avatar_url: Option<String>,
    pub clock: Option<Clock>,
}

#[derive(Clone, Debug)]
pub struct LiveBroadcastSummary {
    pub tour_id: String,
    pub tour_name: String,
    pub round_id: String,
    pub round_name: String,
    pub ongoing: bool,
    pub finished: bool,
    pub is_past: bool,
    pub url: String,
    pub rounds: Vec<LichessBroadcastRound>,
}

#[derive(Clone, Debug)]
pub enum LiveStreamUpdate {
    Snapshot {
        entry: LiveGameEntry,
        initial_centis: Option<u64>,
        increment_centis: u64,
        white_centis: Option<u64>,
        black_centis: Option<u64>,
        ongoing: bool,
    },
    Rematch(String),
    Error(String),
}

/// Return `true` when `raw` represents standard chess or Chess960 supported by Brett.
#[must_use]
pub fn is_supported_standard_variant(raw: &str) -> bool {
    let v = raw.trim().to_ascii_lowercase();
    v.is_empty()
        || matches!(
            v.as_str(),
            "standard"
                | "chess"
                | "normal"
                | "classical"
                | "classic"
                | "fromposition"
                | "from position"
                | "chess960"
                | "fischerandom"
        )
}

#[derive(Clone, Debug)]
pub struct LiveGameEntry {
    pub id: String,
    pub source: LiveSource,
    pub event: String,
    pub url: String,
    pub white: LivePlayerInfo,
    pub black: LivePlayerInfo,
    pub time_control: String,
    pub ongoing: bool,
    pub status_text: String,
    pub pgn_round_id: Option<String>,
    pub refresh_target: Option<LiveRefreshTarget>,
    pub game: Option<Game>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveRefreshTarget {
    LichessGameId(String),
    LichessBroadcastRound { round_id: String, game_key: String },
    ChessComPlayer(String),
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(6))
        .timeout_read(Duration::from_secs(12))
        .build()
}

fn get_json(url: &str, token: Option<&str>) -> Result<Value> {
    let agent = http_agent();
    let call_with = |tok: Option<&str>| {
        let mut req = agent
            .get(url)
            .set("User-Agent", USER_AGENT)
            .set("Accept", "application/json");
        if let Some(t) = tok.map(str::trim).filter(|s| !s.is_empty()) {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        req.call()
    };
    let resp = match call_with(token) {
        Ok(r) => r,
        Err(ureq::Error::Status(401 | 403, _))
            if token.is_some_and(|s| !s.trim().is_empty()) =>
        {
            call_with(None).with_context(|| format!("GET {url}"))?
        }
        Err(err) => return Err(err).with_context(|| format!("GET {url}")),
    };
    let text = resp
        .into_string()
        .with_context(|| format!("reading {url}"))?;
    serde_json::from_str(&text).with_context(|| format!("parsing JSON from {url}"))
}

fn get_text(url: &str, accept: &str, token: Option<&str>) -> Result<String> {
    let agent = http_agent();
    let call_with = |tok: Option<&str>| {
        let mut req = agent
            .get(url)
            .set("User-Agent", USER_AGENT)
            .set("Accept", accept);
        if let Some(t) = tok.map(str::trim).filter(|s| !s.is_empty()) {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        req.call()
    };
    let resp = match call_with(token) {
        Ok(r) => r,
        Err(ureq::Error::Status(401 | 403, _))
            if token.is_some_and(|s| !s.trim().is_empty()) =>
        {
            call_with(None).with_context(|| format!("GET {url}"))?
        }
        Err(err) => return Err(err).with_context(|| format!("GET {url}")),
    };
    resp.into_string().with_context(|| format!("reading {url}"))
}

/// Parse starting clock in centiseconds from a game's `[TimeControl ...]` header
/// or fallback to the earliest recorded mainline clock.
#[must_use]
pub fn parse_initial_time_centis(game: &Game) -> Option<u64> {
    if let Some(tc) = game.header("TimeControl").map(str::trim)
        && !tc.is_empty()
        && tc != "-"
        && tc != "?"
    {
        let first_stage = tc.split([':', ',']).next().unwrap_or(tc).trim();
        let before_plus = first_stage.split('+').next().unwrap_or(first_stage).trim();
        let base_part = before_plus
            .rsplit('/')
            .next()
            .unwrap_or(before_plus)
            .trim();
        // Standard PGN seconds (e.g. "60+0", "180+2", "40/5400")
        if let Ok(secs) = base_part.parse::<u64>()
            && secs > 0
        {
            return Some(secs.saturating_mul(100));
        }
        // Broadcast prose (e.g. "120'/40m + 30'/end" or "90 min + 30s")
        let digits: String = first_stage
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(mins) = digits.parse::<u64>()
            && mins > 0
            && (first_stage.contains('\'') || first_stage.to_ascii_lowercase().contains("min"))
        {
            return Some(mins.saturating_mul(6000));
        }
    }
    let mut node_idx = 0usize;
    let mut ply = 0usize;
    while let Some(&next_idx) = game.nodes.get(node_idx).and_then(|n| n.children.first()) {
        ply += 1;
        if ply <= 2
            && let Some(clk) = game.nodes.get(next_idx).and_then(|n| n.clock)
        {
            return Some(clk.centis);
        }
        if ply > 2 {
            break;
        }
        node_idx = next_idx;
    }
    None
}

/// Extract the latest White and Black clocks along a game's mainline.
#[must_use]
pub fn extract_latest_clocks(game: &Game) -> (Option<Clock>, Option<Clock>) {
    let mut white_clk = None;
    let mut black_clk = None;
    let mut node_idx = 0usize;
    let mut ply = 0usize;
    while let Some(&next_idx) = game.nodes.get(node_idx).and_then(|n| n.children.first()) {
        ply += 1;
        if let Some(clk) = game.nodes.get(next_idx).and_then(|n| n.clock) {
            if ply % 2 == 1 {
                white_clk = Some(clk);
            } else {
                black_clk = Some(clk);
            }
        }
        node_idx = next_idx;
    }
    (white_clk, black_clk)
}

/// Apply player titles, ratings, country flags, and live clock tags to `game`
/// so Brett's player strips and clock badges display complete server metadata.
pub fn apply_live_metadata_to_game(game: &mut Game, entry: &LiveGameEntry) {
    if let Some(t) = entry.white.title
        && game.header("WhiteTitle").unwrap_or("").trim().is_empty()
    {
        game.set_header("WhiteTitle", t.short().to_string());
    }
    if let Some(t) = entry.black.title
        && game.header("BlackTitle").unwrap_or("").trim().is_empty()
    {
        game.set_header("BlackTitle", t.short().to_string());
    }
    if let Some(r) = entry.white.rating
        && game.header("WhiteElo").unwrap_or("").trim().is_empty()
    {
        game.set_header("WhiteElo", r.to_string());
    }
    if let Some(r) = entry.black.rating
        && game.header("BlackElo").unwrap_or("").trim().is_empty()
    {
        game.set_header("BlackElo", r.to_string());
    }
    if let Some(cc) = &entry.white.country_code
        && game.header("WhiteTeam").unwrap_or("").trim().is_empty()
    {
        game.set_header("WhiteTeam", cc.clone());
    }
    if let Some(cc) = &entry.black.country_code
        && game.header("BlackTeam").unwrap_or("").trim().is_empty()
    {
        game.set_header("BlackTeam", cc.clone());
    }
    if !entry.url.is_empty() && game.header("Site").unwrap_or("").trim().is_empty() {
        game.set_header("Site", entry.url.clone());
    }
    // Ensure final mainline node carries the latest known clock if PGN omitted `[%clk]`
    let (w_clk, b_clk) = extract_latest_clocks(game);
    if w_clk.is_none() || b_clk.is_none() {
        let mut last_white = None;
        let mut last_black = None;
        let mut cur = 0usize;
        let mut ply = 0usize;
        while let Some(&next) = game.nodes.get(cur).and_then(|n| n.children.first()) {
            ply += 1;
            if ply % 2 == 1 {
                last_white = Some(next);
            } else {
                last_black = Some(next);
            }
            cur = next;
        }
        if w_clk.is_none()
            && let (Some(idx), Some(clk)) = (last_white, entry.white.clock)
            && let Some(node) = game.nodes.get_mut(idx)
        {
            node.clock = Some(clk);
        }
        if b_clk.is_none()
            && let (Some(idx), Some(clk)) = (last_black, entry.black.clock)
            && let Some(node) = game.nodes.get_mut(idx)
        {
            node.clock = Some(clk);
        }
    }
}

/// Format clock cleanly for compact live lists (e.g. `2:58` or `0:14.5`).
#[must_use]
pub fn format_live_clock(clk: Clock) -> String {
    if clk.centis < 2000 {
        let secs = clk.centis / 100;
        let tenths = (clk.centis % 100) / 10;
        format!("0:{secs:02}.{tenths}")
    } else {
        clk.display()
    }
}

/// Parse Lichess `/api/broadcast` NDJSON lines into [`LiveBroadcastSummary`].
#[must_use]
pub fn parse_lichess_broadcasts(body: &str) -> Vec<LiveBroadcastSummary> {
    let mut out = Vec::new();
    for line in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let tour = v.get("tour").unwrap_or(&v);
        let tour_id = tour
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let tour_name = tour
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Broadcast")
            .to_string();
        let rounds_arr = v
            .get("rounds")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let parsed_rounds: Vec<LichessBroadcastRound> = rounds_arr
            .iter()
            .filter_map(|r| {
                let id = r.get("id").and_then(Value::as_str)?.trim().to_string();
                if id.is_empty() {
                    return None;
                }
                let name = r
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                Some(LichessBroadcastRound {
                    pgn_url: format!("https://lichess.org/api/broadcast/round/{id}.pgn"),
                    id,
                    name,
                    ongoing: r.get("ongoing").and_then(Value::as_bool).unwrap_or(false),
                    finished: r.get("finished").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect();
        if let Some(round) = rounds_arr
            .iter()
            .find(|r| r.get("ongoing").and_then(Value::as_bool) == Some(true))
            .or_else(|| {
                rounds_arr
                    .iter()
                    .rev()
                    .find(|r| r.get("finished").and_then(Value::as_bool) == Some(true))
            })
            .or_else(|| rounds_arr.last())
        {
            let round_id = round
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let round_name = round
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Round")
                .to_string();
            let ongoing = round
                .get("ongoing")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let finished = round
                .get("finished")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let url = round
                .get("url")
                .and_then(Value::as_str)
                .or_else(|| tour.get("url").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            if !round_id.is_empty() {
                out.push(LiveBroadcastSummary {
                    tour_id,
                    tour_name,
                    round_id,
                    round_name,
                    ongoing,
                    finished,
                    is_past: !ongoing && finished,
                    url,
                    rounds: parsed_rounds,
                });
            }
        }
    }
    // Live/ongoing broadcasts first
    out.sort_by_key(|b| !b.ongoing);
    out
}

fn parse_top_broadcast_item(item: &Value, is_past: bool) -> Option<LiveBroadcastSummary> {
    let tour = item.get("tour")?;
    let tour_id = tour.get("id").and_then(Value::as_str)?.trim().to_string();
    if tour_id.is_empty() {
        return None;
    }
    let tour_name = tour
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Broadcast")
        .to_string();
    let round = item
        .get("roundToLink")
        .filter(|r| r.get("id").and_then(Value::as_str).is_some())
        .or_else(|| item.get("round"))?;
    let round_id = round.get("id").and_then(Value::as_str)?.trim().to_string();
    if round_id.is_empty() {
        return None;
    }
    let round_name = round
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Round")
        .to_string();
    let ongoing = round
        .get("ongoing")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let finished = round
        .get("finished")
        .and_then(Value::as_bool)
        .unwrap_or(is_past);
    let url = round
        .get("url")
        .and_then(Value::as_str)
        .or_else(|| tour.get("url").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    Some(LiveBroadcastSummary {
        tour_id,
        tour_name,
        round_id,
        round_name,
        ongoing,
        finished,
        is_past,
        url,
        rounds: Vec::new(),
    })
}

/// Parse Lichess `/api/broadcast/top` JSON (active + past + upcoming broadcasts).
#[must_use]
pub fn parse_lichess_broadcast_top(body: &str) -> Vec<LiveBroadcastSummary> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return parse_lichess_broadcasts(body);
    };
    let mut out = Vec::new();
    let mut seen_tours = std::collections::HashSet::new();
    if let Some(arr) = v.get("active").and_then(Value::as_array) {
        for item in arr {
            if let Some(sum) = parse_top_broadcast_item(item, false)
                && seen_tours.insert(sum.tour_id.clone())
            {
                out.push(sum);
            }
        }
    }
    if let Some(arr) = v
        .get("past")
        .and_then(|p| p.get("currentPageResults"))
        .and_then(Value::as_array)
    {
        for item in arr {
            if let Some(sum) = parse_top_broadcast_item(item, true)
                && seen_tours.insert(sum.tour_id.clone())
            {
                out.push(sum);
            }
        }
    }
    if out.is_empty()
        && let Some(arr) = v.get("upcoming").and_then(Value::as_array)
    {
        for item in arr {
            if let Some(sum) = parse_top_broadcast_item(item, false)
                && seen_tours.insert(sum.tour_id.clone())
            {
                out.push(sum);
            }
        }
    }
    out
}

/// Fetch active and recent Lichess official broadcasts (mirroring `lichess.org/broadcast`).
/// Public broadcast endpoints are called anonymously so scoped user tokens never trigger 403.
pub fn fetch_lichess_broadcasts(_token: Option<&str>) -> Result<Vec<LiveBroadcastSummary>> {
    if let Ok(top_text) = get_text("https://lichess.org/api/broadcast/top", "application/json", None)
    {
        let parsed = parse_lichess_broadcast_top(&top_text);
        if !parsed.is_empty() {
            return Ok(parsed);
        }
    }
    let text = get_text(
        "https://lichess.org/api/broadcast?nb=25",
        "application/x-ndjson, application/json",
        None,
    )?;
    Ok(parse_lichess_broadcasts(&text))
}

/// Fetch all rounds belonging to a specific Lichess broadcast tournament (`/api/broadcast/{tour_id}`).
pub fn fetch_lichess_broadcast_tour_rounds(tour_id: &str) -> Result<Vec<LichessBroadcastRound>> {
    let id = tour_id.trim();
    if id.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("https://lichess.org/api/broadcast/{id}");
    let v = get_json(&url, None)?;
    let Some(arr) = v.get("rounds").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(arr
        .iter()
        .filter_map(|r| {
            let rid = r.get("id").and_then(Value::as_str)?.trim().to_string();
            if rid.is_empty() {
                return None;
            }
            let rname = r
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&rid)
                .to_string();
            Some(LichessBroadcastRound {
                pgn_url: format!("https://lichess.org/api/broadcast/round/{rid}.pgn"),
                id: rid,
                name: rname,
                ongoing: r.get("ongoing").and_then(Value::as_bool).unwrap_or(false),
                finished: r.get("finished").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect())
}

fn game_to_live_entry(
    mut game: Game,
    source: LiveSource,
    url: String,
    ongoing: bool,
    pgn_round_id: Option<String>,
    refresh_target: Option<LiveRefreshTarget>,
) -> LiveGameEntry {
    let (w_clk, b_clk) = extract_latest_clocks(&game);
    let white = LivePlayerInfo {
        name: game.white().to_string(),
        real_name: None,
        title: game.header("WhiteTitle").and_then(titles::normalize),
        rating: game.header("WhiteElo").and_then(|s| s.trim().parse().ok()),
        country_code: game
            .header("WhiteTeam")
            .or_else(|| game.header("WhiteCountry"))
            .and_then(|c| country::normalize(c).map(|co| co.code.to_string())),
        flair: None,
        avatar_url: None,
        clock: w_clk,
    };
    let black = LivePlayerInfo {
        name: game.black().to_string(),
        real_name: None,
        title: game.header("BlackTitle").and_then(titles::normalize),
        rating: game.header("BlackElo").and_then(|s| s.trim().parse().ok()),
        country_code: game
            .header("BlackTeam")
            .or_else(|| game.header("BlackCountry"))
            .and_then(|c| country::normalize(c).map(|co| co.code.to_string())),
        flair: None,
        avatar_url: None,
        clock: b_clk,
    };
    let event = game.event().to_string();
    let tc = game.header("TimeControl").unwrap_or("").to_string();
    let res = game.result().trim().to_string();
    let status_text = if ongoing || res == "*" {
        "Live".to_string()
    } else {
        res.clone()
    };
    let game_url = game
        .header("GameURL")
        .or_else(|| game.header("Link"))
        .or_else(|| game.header("Site"))
        .map(str::to_string)
        .unwrap_or(url);
    let key = format!("{}|{}", white.name, black.name);
    if game.header("Result").is_none() && ongoing {
        game.set_header("Result", "*".to_string());
    }
    LiveGameEntry {
        id: key,
        source,
        event,
        url: game_url,
        white,
        black,
        time_control: tc,
        ongoing: ongoing || res == "*",
        status_text,
        pgn_round_id,
        refresh_target,
        game: Some(game),
    }
}

/// Fetch all games in a Lichess broadcast round PGN (`/api/broadcast/round/{id}.pgn`).
/// Never sends an auth token because public broadcast PGNs reject tokens lacking `study:read`.
pub fn fetch_lichess_broadcast_round_games(
    round_id: &str,
    tour_name: &str,
    _token: Option<&str>,
) -> Result<Vec<LiveGameEntry>> {
    let url = format!("https://lichess.org/api/broadcast/round/{round_id}.pgn");
    let pgn_text = get_text(&url, "application/x-chess-pgn", None)?;
    let games = parse_pgn(pgn_text.as_bytes()).with_context(|| "parsing broadcast round PGN")?;
    Ok(games
        .into_iter()
        .map(|mut g| {
            let key = format!("{}|{}", g.white(), g.black());
            if g.event().trim().is_empty() || g.event() == "?" {
                g.set_header("Event", tour_name.to_string());
            }
            let ongoing = g.result().trim() == "*";
            game_to_live_entry(
                g,
                LiveSource::LichessBroadcast,
                format!("https://lichess.org/broadcast/-/-/{round_id}"),
                ongoing,
                Some(round_id.to_string()),
                Some(LiveRefreshTarget::LichessBroadcastRound {
                    round_id: round_id.to_string(),
                    game_key: key,
                }),
            )
        })
        .collect())
}

/// Parse Lichess `/api/tv/channels` JSON into [`LiveGameEntry`] stubs, ready
/// for full PGN + clock fetch when opened.
#[must_use]
pub fn parse_lichess_tv_channels(v: &Value) -> Vec<LiveGameEntry> {
    let Some(obj) = v.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (channel, ch_val) in obj {
        let Some(game_id) = ch_val.get("gameId").and_then(Value::as_str) else {
            continue;
        };
        let user_obj = ch_val.get("user").unwrap_or(&Value::Null);
        let name = user_obj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
            .to_string();
        let title = user_obj
            .get("title")
            .and_then(Value::as_str)
            .and_then(titles::normalize);
        let flair = user_obj
            .get("flair")
            .and_then(Value::as_str)
            .map(str::to_string);
        let rating = ch_val
            .get("rating")
            .and_then(Value::as_u64)
            .map(|r| r as u32);
        let color = ch_val
            .get("color")
            .and_then(Value::as_str)
            .unwrap_or("white");
        let featured = LivePlayerInfo {
            name: name.clone(),
            title,
            rating,
            country_code: None,
            flair,
            avatar_url: None,
            real_name: None,
            clock: None,
        };
        let opp = LivePlayerInfo {
            name: "Featured opponent".to_string(),
            ..Default::default()
        };
        let (white, black) = if color == "black" {
            (opp, featured)
        } else {
            (featured, opp)
        };
        let channel_lower = channel.to_ascii_lowercase();
        let status_text = if matches!(
            channel_lower.as_str(),
            "antichess"
                | "atomic"
                | "crazyhouse"
                | "horde"
                | "kingofthehill"
                | "racingkings"
                | "threecheck"
        ) {
            format!("Unsupported ({channel})")
        } else {
            "Live TV".to_string()
        };
        out.push(LiveGameEntry {
            id: game_id.to_string(),
            source: LiveSource::LichessServer,
            event: format!("Lichess TV · {channel}"),
            url: format!("https://lichess.org/{game_id}"),
            white,
            black,
            time_control: channel.to_string(),
            ongoing: true,
            status_text,
            pgn_round_id: None,
            refresh_target: Some(LiveRefreshTarget::LichessGameId(game_id.to_string())),
            game: None,
        });
    }
    out
}

/// Parse Lichess JSON game payload (`/game/export/{id}?pgnInJson=true&clocks=true`
/// or `/api/user/{u}/current-game?pgnInJson=true&clocks=true`).
pub fn parse_lichess_game_json(v: &Value) -> Result<LiveGameEntry> {
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let pgn_str = v
        .get("pgn")
        .and_then(Value::as_str)
        .context("Lichess game JSON missing `pgn`")?;
    let mut game = parse_pgn(pgn_str.as_bytes())?
        .into_iter()
        .next()
        .context("empty Lichess game PGN")?;
    let ongoing = v
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| s == "started" || s == "created");

    let parse_side = |side: &str, fallback_name: &str| -> LivePlayerInfo {
        let p = v
            .get("players")
            .and_then(|ps| ps.get(side))
            .unwrap_or(&Value::Null);
        let u = p.get("user").unwrap_or(&Value::Null);
        let name = u
            .get("name")
            .or_else(|| u.get("id"))
            .and_then(Value::as_str)
            .unwrap_or(fallback_name)
            .to_string();
        let title = u
            .get("title")
            .and_then(Value::as_str)
            .and_then(titles::normalize);
        let flair = u.get("flair").and_then(Value::as_str).map(str::to_string);
        let rating = p.get("rating").and_then(Value::as_u64).map(|r| r as u32);
        LivePlayerInfo {
            name,
            title,
            rating,
            country_code: None,
            flair,
            avatar_url: None,
            real_name: None,
            clock: None,
        }
    };

    let mut white = parse_side("white", game.white());
    let mut black = parse_side("black", game.black());
    let (w_clk, b_clk) = extract_latest_clocks(&game);
    white.clock = w_clk.or_else(|| {
        v.get("clock")
            .and_then(|c| c.get("initial"))
            .and_then(Value::as_u64)
            .map(|s| Clock { centis: s * 100 })
    });
    black.clock = b_clk;

    let speed = v
        .get("speed")
        .and_then(Value::as_str)
        .unwrap_or("live")
        .to_string();
    let url = if id.is_empty() {
        "https://lichess.org".to_string()
    } else {
        format!("https://lichess.org/{id}")
    };
    let mut entry = LiveGameEntry {
        id: id.clone(),
        source: LiveSource::LichessServer,
        event: format!("Lichess {speed}"),
        url,
        white,
        black,
        time_control: game.header("TimeControl").unwrap_or(&speed).to_string(),
        ongoing,
        status_text: v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("live")
            .to_string(),
        pgn_round_id: None,
        refresh_target: (!id.is_empty()).then_some(LiveRefreshTarget::LichessGameId(id)),
        game: None,
    };
    apply_live_metadata_to_game(&mut game, &entry);
    entry.game = Some(game);
    Ok(entry)
}

/// Fetch Lichess TV channels (optionally enriching titles/flairs).
pub fn fetch_lichess_tv_channels(_token: Option<&str>) -> Result<Vec<LiveGameEntry>> {
    let val = get_json("https://lichess.org/api/tv/channels", None)?;
    Ok(parse_lichess_tv_channels(&val))
}

/// Fetch full live PGN + metadata + clocks for a specific Lichess game ID.
pub fn fetch_lichess_game_full(game_id: &str, _token: Option<&str>) -> Result<LiveGameEntry> {
    let url = format!(
        "https://lichess.org/game/export/{}?pgnInJson=true&clocks=true&evals=false",
        game_id.trim()
    );
    let val = get_json(&url, None)?;
    parse_lichess_game_json(&val)
}

/// Stream a live Lichess game in real time via `/api/stream/game/{id}` NDJSON.
/// Emits an initial catch-up snapshot and then one snapshot per incoming move/state line.
pub fn stream_lichess_game(
    game_id: &str,
    stop: Arc<AtomicBool>,
    mut on_update: impl FnMut(LiveStreamUpdate),
) {
    let gid = game_id.trim();
    if gid.is_empty() {
        on_update(LiveStreamUpdate::Error("Empty Lichess game ID".to_string()));
        return;
    }
    let url = format!("https://lichess.org/api/stream/game/{gid}");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(90))
        .build();
    let resp = match agent
        .get(&url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/x-ndjson")
        .call()
    {
        Ok(r) => r,
        Err(err) => {
            on_update(LiveStreamUpdate::Error(format!("GET {url}: {err}")));
            return;
        }
    };

    let mut reader = BufReader::new(resp.into_reader());
    let mut line = String::new();

    // Line 1: stream header JSON
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {
                if !line.trim().is_empty() {
                    break;
                }
            }
            Err(err) => {
                on_update(LiveStreamUpdate::Error(format!("Stream read error: {err}")));
                return;
            }
        }
    }
    let Ok(header_val) = serde_json::from_str::<Value>(line.trim()) else {
        on_update(LiveStreamUpdate::Error(
            "Invalid Lichess stream header".to_string(),
        ));
        return;
    };
    let stream_variant = header_val
        .get("variant")
        .and_then(|v| v.get("key").or(Some(v)))
        .and_then(Value::as_str)
        .unwrap_or("standard");
    if !is_supported_standard_variant(stream_variant) {
        on_update(LiveStreamUpdate::Error(format!(
            "Variant '{stream_variant}' is not supported for live board replay."
        )));
        return;
    }

    let parse_side = |side: &str| -> LivePlayerInfo {
        let p = header_val
            .get("players")
            .and_then(|ps| ps.get(side))
            .unwrap_or(&Value::Null);
        let u = p.get("user").unwrap_or(&Value::Null);
        let name = u
            .get("name")
            .or_else(|| u.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let title = u
            .get("title")
            .and_then(Value::as_str)
            .and_then(titles::normalize);
        let flair = u.get("flair").and_then(Value::as_str).map(str::to_string);
        let rating = p.get("rating").and_then(Value::as_u64).map(|r| r as u32);
        LivePlayerInfo {
            name,
            title,
            rating,
            country_code: None,
            flair,
            avatar_url: None,
            real_name: None,
            clock: None,
        }
    };

    let mut white = parse_side("white");
    let mut black = parse_side("black");
    let speed = header_val
        .get("speed")
        .and_then(Value::as_str)
        .unwrap_or("live")
        .to_string();
    let mut initial_centis = header_val
        .get("clock")
        .and_then(|c| c.get("initial"))
        .and_then(Value::as_u64)
        .map(|s| s * 100);
    let increment_centis = header_val
        .get("clock")
        .and_then(|c| c.get("increment"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        * 100;
    let tc_header = match (initial_centis, increment_centis) {
        (Some(init), inc) => format!("{}+{}", init / 100, inc / 100),
        _ => speed.clone(),
    };
    let mut headers = vec![
        ("Event".to_string(), format!("Lichess {speed}")),
        ("Site".to_string(), format!("https://lichess.org/{gid}")),
        ("White".to_string(), white.name.clone()),
        ("Black".to_string(), black.name.clone()),
        ("Result".to_string(), "*".to_string()),
        ("TimeControl".to_string(), tc_header.clone()),
    ];
    if let Some(t) = white.title {
        headers.push(("WhiteTitle".to_string(), t.short().to_string()));
    }
    if let Some(t) = black.title {
        headers.push(("BlackTitle".to_string(), t.short().to_string()));
    }
    if let Some(r) = white.rating {
        headers.push(("WhiteElo".to_string(), r.to_string()));
    }
    if let Some(r) = black.rating {
        headers.push(("BlackElo".to_string(), r.to_string()));
    }

    let mut game = Game {
        headers,
        nodes: vec![Node {
            mv: None,
            comment: String::new(),
            clock: None,
            eval: None,
            shapes: Vec::new(),
            nags: Vec::new(),
            parent: None,
            children: Vec::new(),
            pos: Chess::default(),
        }],
        errors: Vec::new(),
    };
    let mut tip_idx = 0usize;
    let mut white_centis = initial_centis;
    let mut black_centis = initial_centis;
    let mut ongoing = true;
    let mut status_text = "started".to_string();
    let stream_started = Instant::now();
    let mut initial_emitted = false;
    let mut last_line_time = Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                ongoing = false;
                break;
            }
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    if !initial_emitted && !game.nodes[0].children.is_empty() {
                        initial_emitted = true;
                        white.clock = white_centis.map(|c| Clock { centis: c });
                        black.clock = black_centis.map(|c| Clock { centis: c });
                        let entry = LiveGameEntry {
                            id: gid.to_string(),
                            source: LiveSource::LichessServer,
                            event: format!("Lichess {speed}"),
                            url: format!("https://lichess.org/{gid}"),
                            white: white.clone(),
                            black: black.clone(),
                            time_control: tc_header.clone(),
                            ongoing,
                            status_text: status_text.clone(),
                            pgn_round_id: None,
                            refresh_target: Some(LiveRefreshTarget::LichessGameId(gid.to_string())),
                            game: Some(game.clone()),
                        };
                        on_update(LiveStreamUpdate::Snapshot {
                            entry,
                            initial_centis,
                            increment_centis,
                            white_centis,
                            black_centis,
                            ongoing,
                        });
                    }
                    continue;
                }
                let Ok(ev) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                if let Some(wc) = ev.get("wc").and_then(Value::as_u64) {
                    white_centis = Some(wc * 100);
                    if initial_centis.is_none() && tip_idx == 0 {
                        initial_centis = Some(wc * 100);
                    }
                }
                if let Some(bc) = ev.get("bc").and_then(Value::as_u64) {
                    black_centis = Some(bc * 100);
                    if initial_centis.is_none() && tip_idx == 0 {
                        initial_centis = Some(bc * 100);
                    }
                }
                let mut move_applied = false;
                if let Some(lm) = ev.get("lm").and_then(Value::as_str)
                    && let Ok(uci) = UciMove::from_ascii(lm.as_bytes())
                {
                    let cur_pos = game.nodes[tip_idx].pos.clone();
                    if let Ok(mv) = uci.to_move(&cur_pos) {
                        let mover_color = cur_pos.turn();
                        let move_num = cur_pos.fullmoves().get();
                        let san = SanPlus::from_move(cur_pos.clone(), mv).to_string();
                        let next_pos = cur_pos.play_unchecked(mv);
                        let clk = match mover_color {
                            Color::White => white_centis.map(|c| Clock { centis: c }),
                            Color::Black => black_centis.map(|c| Clock { centis: c }),
                        };
                        let new_idx = game.nodes.len();
                        game.nodes.push(Node {
                            mv: Some(Ply {
                                san,
                                uci: lm.to_string(),
                                number: move_num,
                                color: mover_color,
                            }),
                            comment: String::new(),
                            clock: clk,
                            eval: None,
                            shapes: Vec::new(),
                            nags: Vec::new(),
                            parent: Some(tip_idx),
                            children: Vec::new(),
                            pos: next_pos,
                        });
                        game.nodes[tip_idx].children.push(new_idx);
                        tip_idx = new_idx;
                        move_applied = true;
                    }
                } else if tip_idx == 0
                    && let Some(fen_str) = ev.get("fen").and_then(Value::as_str)
                    && let Ok(fen) = Fen::from_ascii(fen_str.as_bytes())
                    && let Ok(parsed_pos) = fen.into_position(CastlingMode::Standard)
                    && !fen_str
                        .trim()
                        .starts_with("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq -")
                {
                    game.nodes[0].pos = parsed_pos;
                    game.set_header("SetUp", "1".to_string());
                    game.set_header("FEN", fen_str.to_string());
                }

                if let Some(st_obj) = ev.get("status") {
                    if let Some(name) = st_obj.get("name").and_then(Value::as_str) {
                        status_text = name.to_string();
                    } else if let Some(s) = st_obj.as_str() {
                        status_text = s.to_string();
                    }
                    let finished_status = matches!(
                        status_text.as_str(),
                        "mate" | "resign" | "stalemate" | "timeout" | "draw" | "outoftime" | "cheat" | "noStart" | "aborted" | "variantEnd"
                    );
                    if finished_status {
                        ongoing = false;
                        let res = match ev.get("winner").and_then(Value::as_str) {
                            Some("white") => "1-0",
                            Some("black") => "0-1",
                            _ if status_text == "aborted" || status_text == "noStart" => "*",
                            _ => "1/2-1/2",
                        };
                        game.set_header("Result", res.to_string());
                    }
                }

                let elapsed_since_last = last_line_time.elapsed();
                last_line_time = Instant::now();
                let should_emit = if !initial_emitted {
                    // During initial history burst (<200ms since stream opened), emit once burst settles or after 300ms
                    elapsed_since_last > Duration::from_millis(40)
                        || stream_started.elapsed() > Duration::from_millis(300)
                        || !ongoing
                } else {
                    move_applied || !ongoing || ev.get("wc").is_some() || ev.get("bc").is_some()
                };
                if should_emit {
                    initial_emitted = true;
                    white.clock = white_centis.map(|c| Clock { centis: c });
                    black.clock = black_centis.map(|c| Clock { centis: c });
                    let entry = LiveGameEntry {
                        id: gid.to_string(),
                        source: LiveSource::LichessServer,
                        event: format!("Lichess {speed}"),
                        url: format!("https://lichess.org/{gid}"),
                        white: white.clone(),
                        black: black.clone(),
                        time_control: tc_header.clone(),
                        ongoing,
                        status_text: status_text.clone(),
                        pgn_round_id: None,
                        refresh_target: Some(LiveRefreshTarget::LichessGameId(gid.to_string())),
                        game: Some(game.clone()),
                    };
                    on_update(LiveStreamUpdate::Snapshot {
                        entry,
                        initial_centis,
                        increment_centis,
                        white_centis,
                        black_centis,
                        ongoing,
                    });
                }
                if let Some(rematch_id) = ev
                    .get("rematch")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && *s != gid)
                {
                    on_update(LiveStreamUpdate::Rematch(rematch_id.to_string()));
                }
                if !ongoing {
                    break;
                }
            }
            Err(err) => {
                on_update(LiveStreamUpdate::Error(format!("Stream closed: {err}")));
                break;
            }
        }
    }
    if !initial_emitted {
        white.clock = white_centis.map(|c| Clock { centis: c });
        black.clock = black_centis.map(|c| Clock { centis: c });
        let entry = LiveGameEntry {
            id: gid.to_string(),
            source: LiveSource::LichessServer,
            event: format!("Lichess {speed}"),
            url: format!("https://lichess.org/{gid}"),
            white,
            black,
            time_control: tc_header,
            ongoing,
            status_text,
            pgn_round_id: None,
            refresh_target: Some(LiveRefreshTarget::LichessGameId(gid.to_string())),
            game: Some(game),
        };
        on_update(LiveStreamUpdate::Snapshot {
            entry,
            initial_centis,
            increment_centis,
            white_centis,
            black_centis,
            ongoing,
        });
    }
}

/// Fetch the currently running Lichess server game for `username`.
pub fn fetch_lichess_user_current_game(
    username: &str,
    token: Option<&str>,
) -> Result<LiveGameEntry> {
    let user = username.trim();
    if user.is_empty() {
        bail!("Enter a Lichess username.");
    }
    let url = format!(
        "https://lichess.org/api/user/{}/current-game?pgnInJson=true&clocks=true",
        user
    );
    let val = get_json(&url, token)?;
    parse_lichess_game_json(&val)
}

fn players_match_pair(a1: &str, a2: &str, b1: &str, b2: &str) -> bool {
    let a1 = a1.trim();
    let a2 = a2.trim();
    let b1 = b1.trim();
    let b2 = b2.trim();
    !a1.is_empty()
        && !a2.is_empty()
        && ((a1.eq_ignore_ascii_case(b1) && a2.eq_ignore_ascii_case(b2))
            || (a1.eq_ignore_ascii_case(b2) && a2.eq_ignore_ascii_case(b1)))
}

/// Poll Lichess for a rematch spawned from `finished_game_id` (via the game's
/// `rematch` field or either player's active current game against the same opponent).
pub fn poll_lichess_rematch(
    finished_game_id: &str,
    white_user: &str,
    black_user: &str,
    token: Option<&str>,
) -> Option<LiveGameEntry> {
    let gid = finished_game_id.trim();
    if !gid.is_empty() {
        let export_url = format!(
            "https://lichess.org/game/export/{gid}?pgnInJson=true&clocks=true&evals=false"
        );
        if let Ok(val) = get_json(&export_url, None)
            && let Some(rematch_id) = val
                .get("rematch")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty() && *s != gid)
            && let Ok(entry) = fetch_lichess_game_full(rematch_id, token)
        {
            return Some(entry);
        }
    }
    for player in [white_user, black_user] {
        let trimmed = player.trim();
        if trimmed.is_empty() || trimmed == "?" || trimmed.eq_ignore_ascii_case("Featured opponent") {
            continue;
        }
        if let Ok(entry) = fetch_lichess_user_current_game(trimmed, token)
            && entry.ongoing
            && entry.id != gid
            && players_match_pair(
                &entry.white.name,
                &entry.black.name,
                white_user,
                black_user,
            )
        {
            return Some(entry);
        }
    }
    None
}

/// Poll Chess.com for a newer game between `white_user` and `black_user`
/// strictly newer than `prev_game_url` in the player's current monthly archive.
pub fn poll_chesscom_rematch(
    prev_game_url: &str,
    white_user: &str,
    black_user: &str,
) -> Option<LiveGameEntry> {
    let prev = prev_game_url.trim();
    let user = [white_user, black_user]
        .into_iter()
        .map(str::trim)
        .find(|u| !u.is_empty() && *u != "?")?;
    let entries = fetch_chesscom_live_games(Some(user)).ok()?;
    for entry in entries {
        if !prev.is_empty() && entry.url == prev {
            break;
        }
        if players_match_pair(
            &entry.white.name,
            &entry.black.name,
            white_user,
            black_user,
        ) {
            return Some(entry);
        }
    }
    None
}

/// Parse Chess.com `/pub/player/{user}` profile metadata.
#[must_use]
pub fn parse_chesscom_profile(v: &Value) -> LivePlayerInfo {
    let name = v
        .get("username")
        .or_else(|| v.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let real_name = v
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| {
            !s.is_empty()
                && !s.eq_ignore_ascii_case(&name)
                && crate::online::is_plausible_human_name(s)
        })
        .map(str::to_string);
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .and_then(titles::normalize);
    let avatar_url = v
        .get("avatar")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let country_code = v
        .get("country")
        .and_then(Value::as_str)
        .and_then(|url| url.rsplit('/').next())
        .and_then(|iso2| country::normalize(iso2).map(|c| c.code.to_string()));
    LivePlayerInfo {
        name,
        real_name,
        title,
        rating: None,
        country_code,
        flair: None,
        avatar_url,
        clock: None,
    }
}

/// Parse live Chess.com streamers (`/pub/streamers`) and leaderboard entries.
#[must_use]
pub fn parse_chesscom_streamers(v: &Value) -> Vec<LivePlayerInfo> {
    let Some(arr) = v.get("streamers").and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter(|s| s.get("is_live").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|s| {
            let username = s.get("username").and_then(Value::as_str)?.to_string();
            Some(LivePlayerInfo {
                name: username,
                title: s
                    .get("title")
                    .and_then(Value::as_str)
                    .and_then(titles::normalize),
                rating: None,
                country_code: None,
                flair: Some("LIVE".to_string()),
                avatar_url: s.get("avatar").and_then(Value::as_str).map(str::to_string),
                real_name: None,
                clock: None,
            })
        })
        .collect()
}

/// Fetch Chess.com live/recent games for a player (with avatars, titles, flags & clocks),
/// or featured live Chess.com streamers/masters when `username_opt` is empty.
pub fn fetch_chesscom_live_games(username_opt: Option<&str>) -> Result<Vec<LiveGameEntry>> {
    let target_users: Vec<LivePlayerInfo> =
        if let Some(u) = username_opt.map(str::trim).filter(|s| !s.is_empty()) {
            let prof_url = format!(
                "https://api.chess.com/pub/player/{}",
                u.to_ascii_lowercase()
            );
            let mut p = get_json(&prof_url, None)
                .map(|v| parse_chesscom_profile(&v))
                .unwrap_or_else(|_| LivePlayerInfo {
                    name: u.to_string(),
                    ..Default::default()
                });
            if p.name.is_empty() {
                p.name = u.to_string();
            }
            vec![p]
        } else {
            let streamers_val = get_json("https://api.chess.com/pub/streamers", None)?;
            let mut live = parse_chesscom_streamers(&streamers_val);
            live.truncate(6);
            live
        };

    let mut profiles: HashMap<String, LivePlayerInfo> = HashMap::new();
    for u in &target_users {
        profiles.insert(u.name.to_ascii_lowercase(), u.clone());
    }

    let mut entries = Vec::new();
    for user in &target_users {
        let arch_url = format!(
            "https://api.chess.com/pub/player/{}/games/archives",
            user.name.to_ascii_lowercase()
        );
        let Ok(arch_val) = get_json(&arch_url, None) else {
            continue;
        };
        let Some(latest_url) = arch_val
            .get("archives")
            .and_then(Value::as_array)
            .and_then(|arr| arr.last())
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Ok(games_val) = get_json(latest_url, None) else {
            continue;
        };
        let Some(games_arr) = games_val.get("games").and_then(Value::as_array) else {
            continue;
        };
        for item in games_arr
            .iter()
            .rev()
            .take(if username_opt.is_some() { 15 } else { 2 })
        {
            let Some(pgn_str) = item.get("pgn").and_then(Value::as_str) else {
                continue;
            };
            let Ok(parsed) = parse_pgn(pgn_str.as_bytes()) else {
                continue;
            };
            let Some(mut game) = parsed.into_iter().next() else {
                continue;
            };
            let game_url = item
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("https://www.chess.com")
                .to_string();
            let tc = item
                .get("time_control")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let time_class = item
                .get("time_class")
                .and_then(Value::as_str)
                .unwrap_or("live");
            let mut entry = game_to_live_entry(
                game.clone(),
                LiveSource::ChessComLive,
                game_url,
                false,
                None,
                Some(LiveRefreshTarget::ChessComPlayer(user.name.clone())),
            );
            entry.time_control = format!("{time_class} {tc}").trim().to_string();
            entry.event = format!("Chess.com {}", time_class);
            if let Some(p) = profiles.get(&entry.white.name.to_ascii_lowercase()) {
                entry.white.real_name = entry.white.real_name.clone().or_else(|| p.real_name.clone());
                entry.white.avatar_url = p.avatar_url.clone();
                entry.white.title = entry.white.title.or(p.title);
                entry.white.country_code = entry
                    .white
                    .country_code
                    .clone()
                    .or_else(|| p.country_code.clone());
                entry.white.flair = entry.white.flair.clone().or_else(|| p.flair.clone());
            }
            if let Some(p) = profiles.get(&entry.black.name.to_ascii_lowercase()) {
                entry.black.real_name = entry.black.real_name.clone().or_else(|| p.real_name.clone());
                entry.black.avatar_url = p.avatar_url.clone();
                entry.black.title = entry.black.title.or(p.title);
                entry.black.country_code = entry
                    .black
                    .country_code
                    .clone()
                    .or_else(|| p.country_code.clone());
                entry.black.flair = entry.black.flair.clone().or_else(|| p.flair.clone());
            }
            apply_live_metadata_to_game(&mut game, &entry);
            entry.game = Some(game);
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Download and disk-cache an avatar URL (`~/.cache/brett/live_avatars/`) and
/// decode into RGBA8 pixels `(bytes, width, height)`.
pub fn fetch_avatar_rgba(url: &str) -> Option<(Vec<u8>, u32, u32)> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cache_dir = dirs_avatar_cache();
    let _ = fs::create_dir_all(&cache_dir);
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for b in trimmed.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    let cache_path = cache_dir.join(format!("{hash:016x}.img"));
    let raw = if let Ok(cached) = fs::read(&cache_path)
        && !cached.is_empty()
    {
        cached
    } else {
        let resp = http_agent()
            .get(trimmed)
            .set("User-Agent", USER_AGENT)
            .call()
            .ok()?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(2 * 1024 * 1024)
            .read_to_end(&mut buf)
            .ok()?;
        if !buf.is_empty() {
            let _ = fs::write(&cache_path, &buf);
        }
        buf
    };
    let dyn_img = image::load_from_memory(&raw).ok()?;
    let rgba = dyn_img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h))
}

fn dirs_avatar_cache() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.trim().is_empty()
    {
        return PathBuf::from(xdg).join("brett").join("live_avatars");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".cache")
        .join("brett")
        .join("live_avatars")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_lichess_broadcast_ndjson_and_prefers_ongoing_round() {
        let ndjson = concat!(
            r#"{"tour":{"id":"t1","name":"Tata Steel"},"rounds":[{"id":"r1","name":"Round 1","ongoing":false},{"id":"r2","name":"Round 2","ongoing":true,"url":"https://lichess.org/broadcast/tata/r2"}]}"#,
            "\n"
        );
        let parsed = parse_lichess_broadcasts(ndjson);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].round_id, "r2");
        assert!(parsed[0].ongoing);
        assert_eq!(parsed[0].tour_name, "Tata Steel");
    }

    #[test]
    fn parses_lichess_broadcast_top_active_and_past() {
        let top = serde_json::json!({
            "active": [{
                "tour": {"id": "a1", "name": "World Championship"},
                "round": {"id": "r2", "name": "Game 2"},
                "roundToLink": {"id": "r1", "name": "Game 1", "finished": true}
            }],
            "past": {
                "currentPageResults": [{
                    "tour": {"id": "p1", "name": "Past Invitational"},
                    "round": {"id": "pr9", "name": "Round 9", "finished": true}
                }]
            }
        })
        .to_string();
        let parsed = parse_lichess_broadcast_top(&top);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].tour_id, "a1");
        assert_eq!(parsed[0].round_id, "r1");
        assert!(!parsed[0].is_past);
        assert_eq!(parsed[1].tour_id, "p1");
        assert!(parsed[1].is_past);
        assert!(parsed[1].finished);
    }

    #[test]
    fn parses_initial_time_centis_from_headers_and_clocks() {
        let g = parse_pgn(b"[TimeControl \"60+0\"]\n\n1. e4 {[%clk 0:01:00]} *")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(parse_initial_time_centis(&g), Some(6000));

        let g2 = parse_pgn(b"[TimeControl \"120'/40m + 30'/end\"]\n\n1. d4 *")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(parse_initial_time_centis(&g2), Some(720_000));
    }

    #[test]
    fn parses_lichess_tv_channels_and_game_clocks_titles() {
        let tv = json!({
            "blitz": {
                "gameId": "AbCd1234",
                "rating": 2820,
                "color": "white",
                "user": {"name": "DrNykterstein", "title": "GM", "flair": "smileys.fire"}
            }
        });
        let list = parse_lichess_tv_channels(&tv);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].white.title, Some(PlayerTitle::Gm));
        assert_eq!(list[0].white.rating, Some(2820));
        assert_eq!(list[0].white.flair.as_deref(), Some("smileys.fire"));

        let game_json = json!({
            "id": "AbCd1234",
            "speed": "blitz",
            "status": "started",
            "players": {
                "white": {"rating": 2820, "user": {"name": "DrNykterstein", "title": "GM"}},
                "black": {"rating": 2790, "user": {"name": "Firouzja2003", "title": "GM"}}
            },
            "pgn": "[White \"DrNykterstein\"]\n[Black \"Firouzja2003\"]\n\n1. e4 {[%clk 0:02:58.4]} e5 {[%clk 0:02:59.1]} *"
        });
        let entry = parse_lichess_game_json(&game_json).unwrap();
        assert!(entry.ongoing);
        assert_eq!(entry.white.clock, Some(Clock { centis: 17840 }));
        assert_eq!(entry.black.clock, Some(Clock { centis: 17910 }));
        let g = entry.game.unwrap();
        assert_eq!(g.header("WhiteTitle"), Some("GM"));
        assert_eq!(g.header("WhiteElo"), Some("2820"));
    }

    #[test]
    fn parses_chesscom_profile_and_live_streamers() {
        let prof = parse_chesscom_profile(&json!({
            "username": "Hikaru",
            "title": "GM",
            "avatar": "https://images.chesscomfiles.com/uploads/v1/user/123.jpg",
            "country": "https://api.chess.com/pub/country/US"
        }));
        assert_eq!(prof.title, Some(PlayerTitle::Gm));
        assert_eq!(prof.country_code.as_deref(), Some("USA"));
        assert!(prof.avatar_url.is_some());

        let streamers = parse_chesscom_streamers(&json!({
            "streamers": [
                {"username": "Hikaru", "is_live": true, "title": "GM", "avatar": "https://x/a.jpg"},
                {"username": "OfflineUser", "is_live": false}
            ]
        }));
        assert_eq!(streamers.len(), 1);
        assert_eq!(streamers[0].name, "Hikaru");
    }

    #[test]
    fn validates_supported_variants_and_rematch_player_pairs() {
        assert!(is_supported_standard_variant("standard"));
        assert!(is_supported_standard_variant("Chess960"));
        assert!(is_supported_standard_variant(""));
        assert!(!is_supported_standard_variant("antichess"));
        assert!(!is_supported_standard_variant("crazyhouse"));
        assert!(!is_supported_standard_variant("atomic"));

        assert!(players_match_pair("Alice", "Bob", "bob", "alice"));
        assert!(!players_match_pair("Alice", "Bob", "Alice", "Charlie"));
    }
}
