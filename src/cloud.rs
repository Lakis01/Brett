//! External position references: chessdb.cn, the Lichess opening explorer,
//! and the public Lichess cloud evaluation.
//!
//! All requests are made from a background thread. The UI receives one
//! [`Snapshot`] per position and can keep it cached; this module therefore
//! contains no application state and never blocks on network I/O.
//!
//! chessdb.cn and the Lichess cloud evaluation are public. The Lichess opening
//! explorer now requires an OAuth bearer token (any token with no additional
//! scopes is sufficient); the token is supplied by the caller.

use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::chess::{Chess, Color, SanPlus, UciMove};

const USER_AGENT: &str = "Brett/0.1 (native Linux chess preparation app)";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Which Lichess opening-explorer corpus is queried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExplorerSource {
    /// Lichess Masters: strong over-the-board games, the useful default.
    #[default]
    Masters,
    /// The complete Lichess database, including online and correspondence games.
    Lichess,
    /// Masters and the full Lichess database combined. A missing Lichess token
    /// leaves the Masters half available and records a note.
    Both,
}

impl ExplorerSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Masters => "Masters",
            Self::Lichess => "Lichess (all games)",
            Self::Both => "Masters + Lichess",
        }
    }

    /// Short label for clickable source selectors.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Masters => "Masters",
            Self::Lichess => "Lichess",
            Self::Both => "Both",
        }
    }

    pub fn config_value(self) -> &'static str {
        match self {
            Self::Masters => "masters",
            Self::Lichess => "lichess",
            Self::Both => "both",
        }
    }

    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "lichess" | "all" | "full" => Self::Lichess,
            "both" | "masters+lichess" | "combined" => Self::Both,
            _ => Self::Masters,
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Masters | Self::Both => "https://explorer.lichess.org/masters",
            Self::Lichess => "https://explorer.lichess.org/lichess",
        }
    }
}

/// All references for one FEN. Each source is independent so one timeout or
/// authentication failure does not hide the other two.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub fen: String,
    pub explorer_source: ExplorerSource,
    pub chessdb: Result<ChessDb, String>,
    pub explorer: Result<Explorer, String>,
    pub cloud_eval: Result<CloudEval, String>,
}

impl Snapshot {
    pub fn has_any_data(&self) -> bool {
        self.chessdb.is_ok() || self.explorer.is_ok() || self.cloud_eval.is_ok()
    }

    /// True only when every source positively reports "no data here" (as
    /// opposed to a network error or timeout). Used as the local transposition
    /// oracle for online tree pruning.
    pub fn is_verified_empty(&self) -> bool {
        is_chessdb_empty(&self.chessdb)
            && is_explorer_empty(&self.explorer)
            && is_cloud_eval_empty(&self.cloud_eval)
    }

    /// Placeholder cached for pruned plies: carries the canonical "absent"
    /// errors so the UI renders "no data" without firing network requests.
    pub fn pruned(fen: String, explorer_source: ExplorerSource) -> Self {
        Self {
            fen,
            explorer_source,
            chessdb: Err("chessdb.cn: unknown".to_string()),
            explorer: Ok(empty_explorer()),
            cloud_eval: Err("Lichess has no cloud evaluation for this position".to_string()),
        }
    }
}

/// chessdb.cn reports absent positions as `unknown` / `nobestmove` or an
/// empty move list; anything else (timeouts, HTTP errors) is not verified.
pub fn is_chessdb_empty(res: &Result<ChessDb, String>) -> bool {
    match res {
        Ok(db) => db.moves.is_empty(),
        Err(err) => {
            err.starts_with("chessdb.cn: unknown")
                || err.starts_with("chessdb.cn: nobestmove")
                || err == "chessdb.cn returned no known moves"
        }
    }
}

/// The explorer is verified empty only when it answered successfully with
/// zero games and zero moves. Auth errors and timeouts do not count.
pub fn is_explorer_empty(res: &Result<Explorer, String>) -> bool {
    match res {
        Ok(explorer) => explorer.total() == 0 && explorer.moves.is_empty(),
        Err(_) => false,
    }
}

/// Lichess cloud eval is verified empty on HTTP 404 (no cached eval) or an
/// `Ok` with zero PVs. Query failures do not count.
pub fn is_cloud_eval_empty(res: &Result<CloudEval, String>) -> bool {
    match res {
        Ok(eval) => eval.pvs.is_empty(),
        Err(err) => err == "Lichess has no cloud evaluation for this position",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChessDb {
    pub moves: Vec<ChessDbMove>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChessDbMove {
    pub uci: String,
    /// Centipawn score from the side-to-move's point of view, as returned by
    /// chessdb.cn. Use [`Self::white_score`] for display.
    pub score: i32,
    pub rank: u8,
    pub note: String,
    pub winrate: f32,
}

impl ChessDbMove {
    /// Normalize chessdb.cn's side-to-move score to White's point of view.
    /// The service flips the sign for Black, which would otherwise make the
    /// same advantage look negative in the Cloud Engine table.
    pub fn white_score(&self, turn: Color) -> i32 {
        if turn == Color::White {
            self.score
        } else {
            -self.score
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Explorer {
    pub white: u64,
    pub draws: u64,
    pub black: u64,
    pub opening: Option<ExplorerOpening>,
    pub moves: Vec<ExplorerMove>,
    pub top_games: Vec<ExplorerGame>,
    /// A non-fatal explanation when a combined source is only partly available.
    pub note: Option<String>,
}

impl Explorer {
    pub fn total(&self) -> u64 {
        self.white
            .saturating_add(self.draws)
            .saturating_add(self.black)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExplorerOpening {
    pub eco: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExplorerMove {
    pub uci: String,
    pub san: String,
    pub white: u64,
    pub draws: u64,
    pub black: u64,
    pub average_rating: Option<u32>,
}

impl ExplorerMove {
    pub fn total(&self) -> u64 {
        self.white
            .saturating_add(self.draws)
            .saturating_add(self.black)
    }

    /// White's score in the usual draw-as-half convention.
    pub fn score(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.5;
        }
        (self.white as f64 + self.draws as f64 / 2.0) / total as f64
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExplorerGame {
    pub id: String,
    pub white: String,
    pub black: String,
    pub white_rating: Option<u32>,
    pub black_rating: Option<u32>,
    pub winner: Option<String>,
    pub year: Option<u32>,
    pub month: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CloudEval {
    pub depth: u32,
    pub knodes: u64,
    pub pvs: Vec<CloudPv>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CloudPv {
    pub moves: String,
    pub cp: Option<i32>,
    pub mate: Option<i32>,
}

/// Fetch all three references synchronously. Call this from a worker thread.
///
/// The Masters explorer is public and is the default. The full Lichess
/// database requires a token; without one it reports a useful error while
/// chessdb and the cloud evaluation still work.
/// Refresh only the Lichess opening explorer. The chessdb.cn and cloud-eval
/// results for the same position stay on screen while the corpus changes.
pub fn fetch_explorer_only(
    fen: &str,
    lichess_token: Option<&str>,
    explorer_source: ExplorerSource,
) -> Result<Explorer, String> {
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(10))
        .timeout(REQUEST_TIMEOUT)
        .build();
    let token = lichess_token
        .map(str::trim)
        .filter(|token| !token.is_empty());
    match explorer_source {
        ExplorerSource::Masters => fetch_explorer(&agent, fen, token, explorer_source),
        ExplorerSource::Lichess if token.is_some() => {
            fetch_explorer(&agent, fen, token, explorer_source)
        }
        ExplorerSource::Lichess => Err(anyhow!(
            "The full Lichess explorer needs an API token. Put \"lichess_token\" in the Brett references config, or set BRETT_LICHESS_TOKEN. Masters works without one."
        )),
        ExplorerSource::Both => fetch_both_explorers(&agent, fen, token),
    }
    .map_err(|err| format!("{err:#}"))
}

pub fn fetch(fen: &str, lichess_token: Option<&str>, explorer_source: ExplorerSource) -> Snapshot {
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(10))
        .timeout(REQUEST_TIMEOUT)
        .build();

    let chessdb = fetch_chessdb(&agent, fen);
    let token = lichess_token
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let explorer = match explorer_source {
        ExplorerSource::Masters => fetch_explorer(&agent, fen, token, explorer_source),
        ExplorerSource::Lichess if token.is_some() => {
            fetch_explorer(&agent, fen, token, explorer_source)
        }
        ExplorerSource::Lichess => Err(anyhow!(
            "The full Lichess explorer needs an API token. Put \"lichess_token\" in the Brett references config, or set BRETT_LICHESS_TOKEN. Masters works without one."
        )),
        ExplorerSource::Both => fetch_both_explorers(&agent, fen, token),
    };
    let cloud_eval = fetch_cloud_eval(&agent, fen);

    Snapshot {
        fen: fen.to_string(),
        explorer_source,
        chessdb: chessdb.map_err(|err| format!("{err:#}")),
        explorer: explorer.map_err(|err| format!("{err:#}")),
        cloud_eval: cloud_eval.map_err(|err| format!("{err:#}")),
    }
}

/// Fetch only the Lichess cloud evaluation (`multiPv=1`) for the evaluation bar.
///
/// Returns `Ok(None)` when Lichess does not have a cached cloud evaluation for
/// `fen` (HTTP 404).
pub fn fetch_eval_only(fen: &str) -> Result<Option<CloudEval>, String> {
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(6))
        .build();
    let url = format!(
        "https://lichess.org/api/cloud-eval?fen={}&multiPv=1",
        encode_component(fen)
    );
    match agent.get(&url).set("Accept", "application/json").call() {
        Ok(response) => read_body(response)
            .and_then(|body| parse_cloud_eval(&body))
            .map(Some)
            .map_err(|err| format!("{err:#}")),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(err) => Err(format!(
            "{:#}",
            anyhow::Error::new(err).context("querying the Lichess cloud evaluation")
        )),
    }
}

/// Query the public Lichess Masters opening explorer with a caller-supplied timeout.
pub fn fetch_masters_explorer_with_timeout(
    fen: &str,
    lichess_token: Option<&str>,
    timeout: Duration,
) -> Result<Explorer, String> {
    let connect_timeout = timeout.min(Duration::from_secs(3));
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(connect_timeout)
        .timeout(timeout)
        .build();
    let token = lichess_token
        .map(str::trim)
        .filter(|token| !token.is_empty());
    fetch_explorer(&agent, fen, token, ExplorerSource::Masters).map_err(|err| format!("{err:#}"))
}

pub fn explorer_since_url(
    fen: &str,
    source: ExplorerSource,
    since_year: u32,
    since_month: u32,
) -> String {
    let mut url = format!(
        "{}?variant=standard&fen={}&moves=12&topGames=4&recentGames=0",
        source.endpoint(),
        encode_component(fen)
    );
    match source {
        ExplorerSource::Masters | ExplorerSource::Both => {
            url.push_str(&format!("&since={since_year}"));
        }
        ExplorerSource::Lichess => {
            url.push_str(&format!("&since={since_year:04}-{since_month:02}"));
        }
    }
    url
}

/// Query the Lichess opening explorer for games since a given year/month.
pub fn fetch_explorer_since(
    fen: &str,
    lichess_token: Option<&str>,
    explorer_source: ExplorerSource,
    since_year: u32,
    since_month: u32,
) -> Result<Explorer, String> {
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .build();
    let token = lichess_token
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let url = explorer_since_url(fen, explorer_source, since_year, since_month);
    let text = get_text(&agent, &url, token)
        .map_err(|err| format!("querying Lichess explorer with since: {err:#}"))?;
    parse_explorer(&text).map_err(|err| format!("parsing explorer response: {err:#}"))
}

fn fetch_both_explorers(agent: &ureq::Agent, fen: &str, token: Option<&str>) -> Result<Explorer> {
    let masters = fetch_explorer(agent, fen, token, ExplorerSource::Masters);
    let lichess = if token.is_some() {
        fetch_explorer(agent, fen, token, ExplorerSource::Lichess)
    } else {
        Err(anyhow!(
            "the full Lichess half needs an API token; showing Masters only"
        ))
    };
    match (masters, lichess) {
        (Ok(masters), Ok(lichess)) => Ok(merge_explorers(masters, lichess, None)),
        (Ok(masters), Err(err)) => Ok(merge_explorers(
            masters,
            empty_explorer(),
            Some(format!("Lichess unavailable: {err}")),
        )),
        (Err(err), Ok(lichess)) => Ok(merge_explorers(
            empty_explorer(),
            lichess,
            Some(format!("Masters unavailable: {err}")),
        )),
        (Err(masters), Err(lichess)) => Err(anyhow!("Masters: {masters:#}; Lichess: {lichess:#}")),
    }
}

pub(crate) fn empty_explorer() -> Explorer {
    Explorer {
        white: 0,
        draws: 0,
        black: 0,
        opening: None,
        moves: Vec::new(),
        top_games: Vec::new(),
        note: None,
    }
}

fn merge_explorers(mut masters: Explorer, lichess: Explorer, note: Option<String>) -> Explorer {
    let mut merged = empty_explorer();
    merged.white = masters.white.saturating_add(lichess.white);
    merged.draws = masters.draws.saturating_add(lichess.draws);
    merged.black = masters.black.saturating_add(lichess.black);
    merged.opening = masters.opening.take().or(lichess.opening);
    merged.note = note;

    let mut moves: std::collections::BTreeMap<String, ExplorerMove> =
        std::collections::BTreeMap::new();
    for source in [masters.moves, lichess.moves] {
        for mv in source {
            if let Some(existing) = moves.get_mut(&mv.uci) {
                let old_total = existing.total();
                let new_total = mv.total();
                let combined = old_total.saturating_add(new_total);
                existing.white = existing.white.saturating_add(mv.white);
                existing.draws = existing.draws.saturating_add(mv.draws);
                existing.black = existing.black.saturating_add(mv.black);
                existing.average_rating = match (existing.average_rating, mv.average_rating) {
                    (Some(a), Some(b)) => {
                        let weighted = u64::from(a) * old_total + u64::from(b) * new_total;
                        Some(
                            weighted
                                .checked_div(combined)
                                .map_or(a, |value| value as u32),
                        )
                    }
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                };
            } else {
                moves.insert(mv.uci.clone(), mv);
            }
        }
    }
    merged.moves = moves.into_values().collect();
    merged.moves.sort_by(|a, b| {
        b.total()
            .cmp(&a.total())
            .then_with(|| b.score().total_cmp(&a.score()))
            .then_with(|| a.uci.cmp(&b.uci))
    });

    let mut seen = std::collections::HashSet::new();
    for game in masters.top_games.into_iter().chain(lichess.top_games) {
        if seen.insert(game.id.clone()) {
            merged.top_games.push(game);
        }
        if merged.top_games.len() >= 8 {
            break;
        }
    }
    merged
}

fn fetch_chessdb(agent: &ureq::Agent, fen: &str) -> Result<ChessDb> {
    let url = format!(
        "https://www.chessdb.cn/cdb.php?action=queryall&board={}",
        encode_component(fen)
    );
    let text = get_text(agent, &url, None)?;
    parse_chessdb(&text)
}

fn fetch_explorer(
    agent: &ureq::Agent,
    fen: &str,
    token: Option<&str>,
    source: ExplorerSource,
) -> Result<Explorer> {
    let url = format!(
        "{}?variant=standard&fen={}&moves=12&topGames=4&recentGames=0",
        source.endpoint(),
        encode_component(fen)
    );
    let text = get_text(agent, &url, token).with_context(|| {
        format!(
            "querying the Lichess {} opening explorer (the token may be missing, expired, or lack access)",
            source.label()
        )
    })?;
    parse_explorer(&text)
}

fn fetch_cloud_eval(agent: &ureq::Agent, fen: &str) -> Result<CloudEval> {
    let url = format!(
        "https://lichess.org/api/cloud-eval?fen={}&multiPv=5",
        encode_component(fen)
    );
    match agent.get(&url).set("Accept", "application/json").call() {
        Ok(response) => parse_cloud_eval(&read_body(response)?),
        Err(ureq::Error::Status(404, _)) => {
            bail!("Lichess has no cloud evaluation for this position")
        }
        Err(err) => Err(err).context("querying the Lichess cloud evaluation"),
    }
}

fn get_text(agent: &ureq::Agent, url: &str, token: Option<&str>) -> Result<String> {
    let mut request = agent.get(url).set("Accept", "application/json, text/plain");
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    let response = request.call().with_context(|| match token {
        Some(_) => format!("requesting {url} with the configured Lichess token"),
        None => format!("requesting {url}"),
    })?;
    read_body(response)
}

fn read_body(response: ureq::Response) -> Result<String> {
    let mut text = String::new();
    response
        .into_reader()
        .take(4 * 1024 * 1024)
        .read_to_string(&mut text)
        .context("reading the reference response")?;
    Ok(text
        .trim_matches(|ch: char| ch == '\0' || ch.is_whitespace())
        .to_string())
}

/// Parse chessdb.cn's compact `key:value|key:value` response.
fn parse_chessdb(text: &str) -> Result<ChessDb> {
    let text = text.trim_matches(|ch: char| ch == '\0' || ch.is_whitespace());
    if text.is_empty() {
        bail!("chessdb.cn returned an empty response");
    }
    if !text.contains("move:") {
        // The service reports terminal positions and malformed FENs as a
        // single plain-text word, for example "stalemate" or "invalid board".
        bail!("chessdb.cn: {text}");
    }

    let mut moves = Vec::new();
    for record in text.split('|') {
        let mut uci = String::new();
        let mut score = 0;
        let mut rank = 0;
        let mut note = String::new();
        let mut winrate = 0.0;
        for field in record.split(',') {
            let Some((key, value)) = field.split_once(':') else {
                continue;
            };
            match key {
                "move" => uci = value.to_string(),
                "score" => score = value.parse().unwrap_or(0),
                "rank" => rank = value.parse().unwrap_or(0),
                "note" => note = value.to_string(),
                "winrate" => winrate = value.parse().unwrap_or(0.0),
                _ => {}
            }
        }
        if uci.is_empty() {
            continue;
        }
        moves.push(ChessDbMove {
            uci,
            score,
            rank,
            note,
            winrate,
        });
    }
    if moves.is_empty() {
        bail!("chessdb.cn returned no known moves");
    }
    // Rank 2 is the best chessdb marker; then prefer the higher score and
    // finally the higher practical win rate.
    moves.sort_by(|a, b| {
        b.rank
            .cmp(&a.rank)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| {
                b.winrate
                    .partial_cmp(&a.winrate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    Ok(ChessDb { moves })
}

pub(crate) fn parse_explorer(text: &str) -> Result<Explorer> {
    let value: Value =
        serde_json::from_str(text).context("Lichess explorer returned invalid JSON")?;
    let white = value.get("white").and_then(Value::as_u64).unwrap_or(0);
    let draws = value.get("draws").and_then(Value::as_u64).unwrap_or(0);
    let black = value.get("black").and_then(Value::as_u64).unwrap_or(0);
    let opening = value.get("opening").map(|opening| ExplorerOpening {
        eco: opening
            .get("eco")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name: opening
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    });
    let moves = value
        .get("moves")
        .and_then(Value::as_array)
        .map(|moves| {
            moves
                .iter()
                .filter_map(|entry| {
                    let uci = entry.get("uci")?.as_str()?.to_string();
                    let san = entry
                        .get("san")
                        .and_then(Value::as_str)
                        .unwrap_or(&uci)
                        .to_string();
                    Some(ExplorerMove {
                        uci,
                        san,
                        white: entry.get("white").and_then(Value::as_u64).unwrap_or(0),
                        draws: entry.get("draws").and_then(Value::as_u64).unwrap_or(0),
                        black: entry.get("black").and_then(Value::as_u64).unwrap_or(0),
                        average_rating: entry
                            .get("averageRating")
                            .and_then(Value::as_u64)
                            .map(|rating| rating as u32),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let top_games = value
        .get("topGames")
        .and_then(Value::as_array)
        .map(|games| {
            games
                .iter()
                .filter_map(|game| {
                    let id = game.get("id")?.as_str()?.to_string();
                    let player = |color: &str| {
                        game.get(color).map(|player| {
                            (
                                player
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("?")
                                    .to_string(),
                                player
                                    .get("rating")
                                    .and_then(Value::as_u64)
                                    .map(|rating| rating as u32),
                            )
                        })
                    };
                    let (white, white_rating) = player("white")?;
                    let (black, black_rating) = player("black")?;
                    Some(ExplorerGame {
                        id,
                        white,
                        black,
                        white_rating,
                        black_rating,
                        winner: game
                            .get("winner")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        year: game.get("year").and_then(Value::as_u64).map(|v| v as u32),
                        month: game.get("month").and_then(Value::as_u64).map(|v| v as u32),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Explorer {
        white,
        draws,
        black,
        opening,
        moves,
        top_games,
        note: None,
    })
}

fn parse_cloud_eval(text: &str) -> Result<CloudEval> {
    let value: Value =
        serde_json::from_str(text).context("Lichess cloud eval returned invalid JSON")?;
    let pvs: Vec<CloudPv> = value
        .get("pvs")
        .and_then(Value::as_array)
        .map(|pvs| {
            pvs.iter()
                .filter_map(|pv| {
                    let moves = pv.get("moves")?.as_str()?.to_string();
                    Some(CloudPv {
                        moves,
                        cp: pv.get("cp").and_then(Value::as_i64).map(|cp| cp as i32),
                        mate: pv
                            .get("mate")
                            .and_then(Value::as_i64)
                            .map(|mate| mate as i32),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if pvs.is_empty() {
        bail!("Lichess returned no cloud evaluation lines");
    }
    Ok(CloudEval {
        depth: value.get("depth").and_then(Value::as_u64).unwrap_or(0) as u32,
        knodes: value.get("knodes").and_then(Value::as_u64).unwrap_or(0),
        pvs,
    })
}

/// Percent-encode a query-string component. FENs contain spaces and slashes,
/// and the Lichess and chessdb endpoints expect them encoded rather than
/// interpreted as path separators.
fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 16);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0f));
        }
    }
    out
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'A' + value - 10) as char,
        _ => '0',
    }
}

/// Convert the first move of a UCI principal variation to SAN for display.
pub fn first_move_san(pos: &Chess, pv: &str) -> Option<String> {
    let uci = pv.split_whitespace().next()?;
    let uci = UciMove::from_ascii(uci.as_bytes()).ok()?;
    let mv = uci.to_move(pos).ok()?;
    Some(SanPlus::from_move(pos.clone(), mv).to_string())
}

/// Convert a UCI move to SAN for display.
pub fn uci_san(pos: &Chess, uci: &str) -> Option<String> {
    let uci = UciMove::from_ascii(uci.as_bytes()).ok()?;
    let mv = uci.to_move(pos).ok()?;
    Some(SanPlus::from_move(pos.clone(), mv).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_since_url_appends_correct_since_parameter() {
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let masters_url = explorer_since_url(fen, ExplorerSource::Masters, 2025, 4);
        assert!(masters_url.starts_with("https://explorer.lichess.org/masters?"));
        assert!(masters_url.ends_with("&since=2025"));

        let lichess_url = explorer_since_url(fen, ExplorerSource::Lichess, 2025, 4);
        assert!(lichess_url.starts_with("https://explorer.lichess.org/lichess?"));
        assert!(lichess_url.ends_with("&since=2025-04"));
    }

    #[test]
    fn explorer_source_defaults_to_masters_and_accepts_the_full_database() {
        assert_eq!(ExplorerSource::default(), ExplorerSource::Masters);
        assert_eq!(
            ExplorerSource::from_config("masters"),
            ExplorerSource::Masters
        );
        assert_eq!(
            ExplorerSource::from_config("lichess"),
            ExplorerSource::Lichess
        );
        assert_eq!(
            ExplorerSource::from_config(" FULL "),
            ExplorerSource::Lichess
        );
        assert_eq!(ExplorerSource::from_config("both"), ExplorerSource::Both);
        assert_eq!(ExplorerSource::Masters.label(), "Masters");
        assert_eq!(ExplorerSource::Lichess.config_value(), "lichess");
        assert_eq!(ExplorerSource::Both.config_value(), "both");
        assert_eq!(ExplorerSource::Both.short_label(), "Both");
    }

    #[test]
    fn chessdb_response_parses_and_orders_moves() {
        let text = "move:e2e4,score:1,rank:2,note:! (20-05),winrate:50.08|move:d2d4,score:0,rank:1,note:* (20-04),winrate:50.00|move:g2g4,score:-210,rank:0,note:? (20-01),winrate:34.61\0";
        let parsed = parse_chessdb(text).unwrap();
        assert_eq!(parsed.moves.len(), 3);
        assert_eq!(parsed.moves[0].uci, "e2e4");
        assert_eq!(parsed.moves[0].rank, 2);
        assert_eq!(parsed.moves[0].score, 1);
        assert!((parsed.moves[0].winrate - 50.08).abs() < 0.001);
        assert_eq!(parsed.moves[1].uci, "d2d4");
        assert_eq!(parsed.moves[0].white_score(Color::White), 1);
        assert_eq!(parsed.moves[0].white_score(Color::Black), -1);
    }

    #[test]
    fn chessdb_errors_are_human_readable() {
        assert_eq!(
            parse_chessdb("stalemate\0").unwrap_err().to_string(),
            "chessdb.cn: stalemate"
        );
        assert!(parse_chessdb("").is_err());
    }

    #[test]
    fn explorer_response_parses_moves_and_games() {
        let text = r#"{
            "white": 100,
            "draws": 60,
            "black": 40,
            "opening": {"eco": "B00", "name": "King's Pawn Game"},
            "moves": [
                {"uci":"e2e4","san":"e4","white":60,"draws":30,"black":20,"averageRating":2450},
                {"uci":"d2d4","san":"d4","white":30,"draws":20,"black":15}
            ],
            "topGames": [
                {"id":"abcd1234","winner":"white","white":{"name":"A","rating":2500},"black":{"name":"B","rating":2400},"year":2025,"month":3}
            ]
        }"#;
        let parsed = parse_explorer(text).unwrap();
        assert_eq!(parsed.total(), 200);
        assert_eq!(parsed.opening.unwrap().name, "King's Pawn Game");
        assert_eq!(parsed.moves.len(), 2);
        assert_eq!(parsed.moves[0].san, "e4");
        assert!((parsed.moves[0].score() - 0.6818181818).abs() < 0.0001);
        assert_eq!(parsed.top_games[0].id, "abcd1234");
        assert_eq!(parsed.top_games[0].white_rating, Some(2500));
    }

    #[test]
    fn both_explorer_merges_counts_ratings_and_games() {
        let masters = Explorer {
            white: 10,
            draws: 5,
            black: 5,
            opening: Some(ExplorerOpening {
                eco: "B00".to_string(),
                name: "King's Pawn".to_string(),
            }),
            moves: vec![ExplorerMove {
                uci: "e2e4".to_string(),
                san: "e4".to_string(),
                white: 8,
                draws: 4,
                black: 2,
                average_rating: Some(2500),
            }],
            top_games: vec![ExplorerGame {
                id: "masters-game".to_string(),
                winner: Some("white".to_string()),
                white: "A".to_string(),
                white_rating: Some(2500),
                black: "B".to_string(),
                black_rating: Some(2400),
                year: Some(2024),
                month: Some(1),
            }],
            note: None,
        };
        let lichess = Explorer {
            white: 20,
            draws: 10,
            black: 10,
            opening: None,
            moves: vec![
                ExplorerMove {
                    uci: "e2e4".to_string(),
                    san: "e4".to_string(),
                    white: 10,
                    draws: 5,
                    black: 5,
                    average_rating: Some(2200),
                },
                ExplorerMove {
                    uci: "d2d4".to_string(),
                    san: "d4".to_string(),
                    white: 12,
                    draws: 4,
                    black: 4,
                    average_rating: None,
                },
            ],
            top_games: vec![
                ExplorerGame {
                    id: "masters-game".to_string(),
                    winner: None,
                    white: "A".to_string(),
                    white_rating: None,
                    black: "B".to_string(),
                    black_rating: None,
                    year: Some(2024),
                    month: Some(1),
                },
                ExplorerGame {
                    id: "lichess-game".to_string(),
                    winner: Some("black".to_string()),
                    white: "C".to_string(),
                    white_rating: None,
                    black: "D".to_string(),
                    black_rating: None,
                    year: Some(2025),
                    month: Some(2),
                },
            ],
            note: None,
        };
        let merged = merge_explorers(masters, lichess, None);
        assert_eq!(merged.total(), 60);
        assert_eq!(merged.opening.unwrap().name, "King's Pawn");
        assert_eq!(merged.moves.len(), 2);
        let e4 = merged.moves.iter().find(|mv| mv.uci == "e2e4").unwrap();
        assert_eq!(e4.total(), 34);
        assert_eq!(e4.average_rating, Some(2323));
        assert_eq!(merged.top_games.len(), 2);
        assert_eq!(merged.top_games[0].id, "masters-game");
        assert_eq!(merged.top_games[1].id, "lichess-game");
    }

    #[test]
    fn cloud_eval_response_parses_pvs() {
        let text = r#"{"fen":"x","knodes":695524,"depth":75,"pvs":[{"moves":"e2e4 e7e5","cp":19},{"moves":"d2d4 d7d5","mate":3}]}"#;
        let parsed = parse_cloud_eval(text).unwrap();
        assert_eq!(parsed.depth, 75);
        assert_eq!(parsed.pvs.len(), 2);
        assert_eq!(parsed.pvs[0].cp, Some(19));
        assert_eq!(parsed.pvs[1].mate, Some(3));
    }

    #[test]
    fn component_encoding_keeps_fen_structure_safe() {
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let encoded = encode_component(fen);
        assert!(!encoded.contains(' '));
        assert!(encoded.contains("%2F"));
        assert!(encoded.contains("%20"));
    }

    #[test]
    fn verified_empty_distinguishes_absent_positions_from_network_errors() {
        let empty = empty_explorer();
        assert!(is_explorer_empty(&Ok(empty.clone())));
        assert!(!is_explorer_empty(&Err("boom".to_string())));
        let with_games = Explorer {
            moves: vec![ExplorerMove {
                uci: "e2e4".to_string(),
                san: "e4".to_string(),
                white: 1,
                draws: 0,
                black: 0,
                average_rating: None,
            }],
            ..empty.clone()
        };
        assert!(!is_explorer_empty(&Ok(with_games)));

        assert!(is_chessdb_empty(&Ok(ChessDb { moves: vec![] })));
        assert!(is_chessdb_empty(&Err("chessdb.cn: unknown".to_string())));
        assert!(is_chessdb_empty(&Err("chessdb.cn: nobestmove".to_string())));
        assert!(is_chessdb_empty(&Err(
            "chessdb.cn returned no known moves".to_string()
        )));
        assert!(!is_chessdb_empty(&Err("requesting https://x".to_string())));
        assert!(!is_chessdb_empty(&Ok(ChessDb {
            moves: vec![ChessDbMove {
                uci: "e2e4".to_string(),
                score: 10,
                rank: 2,
                note: String::new(),
                winrate: 50.0,
            }],
        })));

        assert!(is_cloud_eval_empty(&Ok(CloudEval {
            depth: 0,
            knodes: 0,
            pvs: vec![],
        })));
        assert!(is_cloud_eval_empty(&Err(
            "Lichess has no cloud evaluation for this position".to_string()
        )));
        assert!(!is_cloud_eval_empty(&Err(
            "querying the Lichess cloud evaluation".to_string()
        )));
        assert!(!is_cloud_eval_empty(&Ok(CloudEval {
            depth: 70,
            knodes: 1,
            pvs: vec![CloudPv {
                moves: "e2e4".to_string(),
                cp: Some(20),
                mate: None,
            }],
        })));

        let pruned = Snapshot::pruned("fen".to_string(), ExplorerSource::Masters);
        assert!(pruned.is_verified_empty());
        let with_data = Snapshot {
            cloud_eval: Ok(CloudEval {
                depth: 70,
                knodes: 1,
                pvs: vec![CloudPv {
                    moves: "e2e4".to_string(),
                    cp: Some(20),
                    mate: None,
                }],
            }),
            ..pruned
        };
        assert!(!with_data.is_verified_empty());
    }
}
