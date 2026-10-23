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

const USER_AGENT: &str = "Brett/0.1 (native Linux chess preparation app)";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// All references for one FEN. Each source is independent so one timeout or
/// authentication failure does not hide the other two.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub fen: String,
    pub chessdb: Result<ChessDb, String>,
    pub explorer: Result<Explorer, String>,
    pub cloud_eval: Result<CloudEval, String>,
}

impl Snapshot {
    pub fn has_any_data(&self) -> bool {
        self.chessdb.is_ok() || self.explorer.is_ok() || self.cloud_eval.is_ok()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChessDb {
    pub moves: Vec<ChessDbMove>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChessDbMove {
    pub uci: String,
    /// Centipawn score from the side-to-move's point of view.
    pub score: i32,
    pub rank: u8,
    pub note: String,
    pub winrate: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Explorer {
    pub white: u64,
    pub draws: u64,
    pub black: u64,
    pub opening: Option<ExplorerOpening>,
    pub moves: Vec<ExplorerMove>,
    pub top_games: Vec<ExplorerGame>,
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
/// `lichess_token` is optional; without it the opening explorer reports a
/// useful error while chessdb and the cloud evaluation still work.
pub fn fetch(fen: &str, lichess_token: Option<&str>) -> Snapshot {
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(10))
        .timeout(REQUEST_TIMEOUT)
        .build();

    let chessdb = fetch_chessdb(&agent, fen);
    let explorer = match lichess_token {
        Some(token) if !token.trim().is_empty() => fetch_explorer(&agent, fen, token.trim()),
        _ => Err(anyhow!(
            "Lichess explorer needs an API token. Put \"lichess_token\" in the Brett references config, or set BRETT_LICHESS_TOKEN."
        )),
    };
    let cloud_eval = fetch_cloud_eval(&agent, fen);

    Snapshot {
        fen: fen.to_string(),
        chessdb: chessdb.map_err(|err| format!("{err:#}")),
        explorer: explorer.map_err(|err| format!("{err:#}")),
        cloud_eval: cloud_eval.map_err(|err| format!("{err:#}")),
    }
}

fn fetch_chessdb(agent: &ureq::Agent, fen: &str) -> Result<ChessDb> {
    let url = format!(
        "https://www.chessdb.cn/cdb.php?action=queryall&board={}",
        encode_component(fen)
    );
    let text = get_text(agent, &url, None)?;
    parse_chessdb(&text)
}

fn fetch_explorer(agent: &ureq::Agent, fen: &str, token: &str) -> Result<Explorer> {
    let url = format!(
        "https://explorer.lichess.org/lichess?variant=standard&fen={}&moves=12&topGames=4&recentGames=0",
        encode_component(fen)
    );
    let text = get_text(agent, &url, Some(token)).context(
        "querying the Lichess opening explorer (the token may be missing, expired, or lack access)",
    )?;
    parse_explorer(&text)
}

fn fetch_cloud_eval(agent: &ureq::Agent, fen: &str) -> Result<CloudEval> {
    let url = format!(
        "https://lichess.org/api/cloud-eval?fen={}&multiPv=3",
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

fn parse_explorer(text: &str) -> Result<Explorer> {
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
pub fn first_move_san(pos: &shakmaty::Chess, pv: &str) -> Option<String> {
    let uci = pv.split_whitespace().next()?;
    let uci = shakmaty::uci::UciMove::from_ascii(uci.as_bytes()).ok()?;
    let mv = uci.to_move(pos).ok()?;
    Some(shakmaty::san::SanPlus::from_move(pos.clone(), mv).to_string())
}

/// Convert a UCI move to SAN for display.
pub fn uci_san(pos: &shakmaty::Chess, uci: &str) -> Option<String> {
    let uci = shakmaty::uci::UciMove::from_ascii(uci.as_bytes()).ok()?;
    let mv = uci.to_move(pos).ok()?;
    Some(shakmaty::san::SanPlus::from_move(pos.clone(), mv).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
