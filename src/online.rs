//! Public Lichess and Chess.com game imports.
//!
//! Both sites expose unauthenticated public APIs. Games are downloaded in the
//! background by the UI and written to Brett's cache, never next to a source
//! database. Chess.com has no bulk PGN endpoint, so recent monthly archives are
//! concatenated; Lichess exposes the whole requested export in one request.

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
}

impl OnlineSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lichess => "Lichess",
            Self::ChessCom => "Chess.com",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Lichess => "lichess",
            Self::ChessCom => "chesscom",
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
    let username = validate_username(username)?;
    let max_games = max_games.clamp(1, 5000);
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
        .build();

    let (body, games) = match source {
        OnlineSource::Lichess => {
            progress(format!("Downloading {username}'s Lichess games…"));
            let url = format!(
                "https://lichess.org/api/games/user/{username}?max={max_games}&clocks=false&evals=false&opening=false&tags=true&sort=dateDesc"
            );
            let body = get_text(&agent, &url, Some("application/x-chess-pgn"))?;
            if body.trim().is_empty() {
                bail!("Lichess returned no games for {username}");
            }
            (body, None)
        }
        OnlineSource::ChessCom => {
            progress(format!("Finding {username}'s Chess.com archives…"));
            let archives_url =
                format!("https://api.chess.com/pub/player/{username}/games/archives");
            let archives_body = get_text(&agent, &archives_url, Some("application/json"))?;
            let archives: Value = serde_json::from_str(&archives_body)
                .context("Chess.com returned invalid archive metadata")?;
            let urls = archive_urls(&archives)?;
            if urls.is_empty() {
                bail!("Chess.com returned no game archives for {username}");
            }

            let mut pgns = Vec::new();
            for (number, archive_url) in urls.iter().rev().take(MAX_ARCHIVES).enumerate() {
                progress(format!(
                    "Downloading Chess.com archive {} · {} games so far…",
                    number + 1,
                    pgns.len()
                ));
                let body = get_text(&agent, archive_url, Some("application/json"))?;
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
            (body, Some(count))
        }
    };

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

fn validate_username(raw: &str) -> Result<&str> {
    let username = raw.trim();
    if username.is_empty() {
        bail!("enter a username");
    }
    if username.len() > 64 {
        bail!("username is too long");
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("username may only contain letters, digits, '_' and '-'");
    }
    Ok(username)
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
            validate_username(" DrNykterstein ").unwrap(),
            "DrNykterstein"
        );
        assert!(validate_username("Magnus Carlsen").is_err());
        assert!(validate_username("name/../other").is_err());
        assert!(validate_username("").is_err());
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
}
