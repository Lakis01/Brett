//! FIDE player-ID presentation and rating lookup helpers.
//!
//! PGN `WhiteFideId`/`BlackFideId` tags and imported metadata are free-form
//! strings. Only a trimmed, purely numeric value is treated as a real FIDE ID:
//! the stored tag is never rewritten, and no ID is inferred from a name or a
//! rating.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

const FIDE_USER_AGENT: &str = "Brett/0.2 (https://github.com/Lakis01/Brett; contact@brett.chess)";

/// Published FIDE title and ratings for a player (Classical / Rapid / Blitz).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FideRatings {
    pub title: Option<crate::titles::PlayerTitle>,
    pub standard: Option<u16>,
    pub rapid: Option<u16>,
    pub blitz: Option<u16>,
}

impl FideRatings {
    pub fn is_empty(self) -> bool {
        self.title.is_none()
            && self.standard.is_none()
            && self.rapid.is_none()
            && self.blitz.is_none()
    }
}

/// Return the trimmed FIDE ID from a tag, or `None` when the value is absent,
/// empty, a placeholder, or not numeric.
pub fn from_tag(raw: Option<&str>) -> Option<&str> {
    let id = raw?.trim();
    (is_numeric(id) && id != "0").then_some(id)
}

/// True when every character is an ASCII digit and the value is not zero.
pub fn is_numeric(id: &str) -> bool {
    !id.is_empty() && id != "0" && id.bytes().all(|byte| byte.is_ascii_digit())
}

/// Official FIDE rating-profile URL, only for a real numeric ID.
pub fn profile_url(id: &str) -> Option<String> {
    let id = from_tag(Some(id))?;
    Some(format!("https://ratings.fide.com/profile/{id}"))
}

/// Parse classical, rapid, and blitz ratings (and optional title) from a FIDE profile HTML page.
pub fn parse_profile_html(html: &str) -> Option<FideRatings> {
    let ratings = FideRatings {
        title: extract_block_text(html, "profile-info-title")
            .or_else(|| extract_block_text(html, "profile-title"))
            .as_deref()
            .and_then(crate::titles::normalize),
        standard: extract_block_rating(html, "profile-standart")
            .or_else(|| extract_block_rating(html, "profile-standard")),
        rapid: extract_block_rating(html, "profile-rapid"),
        blitz: extract_block_rating(html, "profile-blitz"),
    };
    (!ratings.is_empty()).then_some(ratings)
}

fn extract_block_text(html: &str, class_marker: &str) -> Option<String> {
    let marker_pos = html.find(class_marker)?;
    let rest = &html[marker_pos..];
    let tag_gt = rest.find('>')? + 1;
    let body = &rest[tag_gt..];
    let block_end = body.find("</div>").unwrap_or_else(|| body.len().min(512));
    let raw = &body[..block_end];
    let mut cleaned = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                cleaned.push(' ');
            }
            _ if !in_tag => cleaned.push(ch),
            _ => {}
        }
    }
    let normalized = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty() && !normalized.eq_ignore_ascii_case("none")).then_some(normalized)
}

fn extract_block_rating(html: &str, class_marker: &str) -> Option<u16> {
    let marker_pos = html.find(class_marker)?;
    let rest = &html[marker_pos..];
    let block_end = rest.find("</div>").unwrap_or_else(|| rest.len().min(512));
    let block = &rest[..block_end];
    let p_open = block.find("<p>")? + 3;
    let after_p = &block[p_open..];
    let p_close = after_p.find("</p>")?;
    let text = after_p[..p_close].trim();
    let digits: String = text.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.len() != text.len() || digits.is_empty() {
        return None;
    }
    let rating: u16 = digits.parse().ok()?;
    (100..=4000).contains(&rating).then_some(rating)
}

fn parse_chessabc_player_json(body: &str) -> Option<FideRatings> {
    let value: Value = serde_json::from_str(body).ok()?;
    let parse_field = |key: &str| -> Option<u16> {
        let raw = value.get(key)?.as_u64()?;
        let rating = u16::try_from(raw).ok()?;
        (100..=4000).contains(&rating).then_some(rating)
    };
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .and_then(crate::titles::normalize);
    let ratings = FideRatings {
        title,
        standard: parse_field("rating"),
        rapid: parse_field("rapidRating"),
        blitz: parse_field("blitzRating"),
    };
    (!ratings.is_empty()).then_some(ratings)
}

fn fide_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .user_agent(FIDE_USER_AGENT)
            .timeout_connect(Duration::from_secs(3))
            .timeout(Duration::from_secs(4))
            .build()
    })
}

fn get_limited_text(agent: &ureq::Agent, url: &str, max_bytes: u64) -> Option<String> {
    let response = agent.get(url).call().ok()?;
    let mut text = String::new();
    response
        .into_reader()
        .take(max_bytes)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// Fetch FIDE classical, rapid, and blitz ratings for a FIDE ID.
///
/// Queries `ratings.fide.com/profile/{id}` directly first, falling back to
/// ChessABC's mirrored FIDE player record (`api.chessabc.com/players/id/{id}`)
/// if FIDE is unreachable or throttles requests.
pub fn fetch_ratings(fide_id: &str) -> Option<FideRatings> {
    let url = profile_url(fide_id)?;
    let id = from_tag(Some(fide_id))?;
    let agent = fide_agent();
    if let Some(html) = get_limited_text(agent, &url, 1024 * 1024)
        && let Some(ratings) = parse_profile_html(&html)
    {
        return Some(ratings);
    }
    let fallback_url = format!("https://api.chessabc.com/players/id/{id}");
    let json_body = get_limited_text(agent, &fallback_url, 128 * 1024)?;
    parse_chessabc_player_json(&json_body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_trimmed_numeric_ids() {
        assert_eq!(from_tag(Some(" 1503014 ")), Some("1503014"));
        assert_eq!(from_tag(Some("1503014")), Some("1503014"));
    }

    #[test]
    fn rejects_missing_placeholder_and_non_numeric_values() {
        for raw in [
            None,
            Some(""),
            Some("   "),
            Some("?"),
            Some("0"),
            Some("N/A"),
            Some("15030a4"),
            Some("-1503014"),
        ] {
            assert_eq!(from_tag(raw), None, "raw FIDE id {raw:?}");
        }
    }

    #[test]
    fn builds_profile_urls_only_for_numeric_ids() {
        assert_eq!(
            profile_url("1503014").as_deref(),
            Some("https://ratings.fide.com/profile/1503014")
        );
        assert_eq!(
            profile_url(" 1503014 ").as_deref(),
            Some("https://ratings.fide.com/profile/1503014")
        );
        assert_eq!(profile_url("Magnus"), None);
        assert_eq!(profile_url(""), None);
    }

    #[test]
    fn parses_fide_profile_html_ratings_and_not_rated_fields() {
        let html = r#"
            <div class="profile-games ">
                <div class="profile-standart profile-game ">
                    <img src="/img/logo_std.svg" alt="standart" height=25>
                    <p>2792</p><p style="font-size: 8px;">STANDARD</p>
                </div>
                <div class="profile-rapid profile-game ">
                    <img src="/img/logo_rpd.svg" alt="rapid" height=25>
                    <p>2718</p><p style="font-size: 8px;">RAPID</p>
                </div>
                <div class="profile-blitz profile-game ">
                    <img src="/img/logo_blitz.svg" alt="blitz" height=25>
                    <p>2800</p><p style="font-size: 8px;">BLITZ</p>
                </div>
            </div>
        "#;
        assert_eq!(
            parse_profile_html(html),
            Some(FideRatings {
                title: None,
                standard: Some(2792),
                rapid: Some(2718),
                blitz: Some(2800),
            })
        );

        let with_title = r#"
            <div class="profile-info-title col-lg-8"><p>Grandmaster</p></div>
            <div class="profile-standart profile-game ">
                <p>2602</p>
            </div>
        "#;
        assert_eq!(
            parse_profile_html(with_title),
            Some(FideRatings {
                title: Some(crate::titles::PlayerTitle::Gm),
                standard: Some(2602),
                rapid: None,
                blitz: None,
            })
        );

        let partial = r#"
            <div class="profile-standart profile-game ">
                <p>Not rated</p>
            </div>
            <div class="profile-rapid profile-game ">
                <p>2410</p>
            </div>
            <div class="profile-blitz profile-game ">
                <p>0</p>
            </div>
        "#;
        assert_eq!(
            parse_profile_html(partial),
            Some(FideRatings {
                title: None,
                standard: None,
                rapid: Some(2410),
                blitz: None,
            })
        );
    }

    #[test]
    fn parses_chessabc_player_json_ratings() {
        let json =
            r#"{"id":2016192,"title":"GM","rating":2792,"rapidRating":2718,"blitzRating":2800}"#;
        assert_eq!(
            parse_chessabc_player_json(json),
            Some(FideRatings {
                title: Some(crate::titles::PlayerTitle::Gm),
                standard: Some(2792),
                rapid: Some(2718),
                blitz: Some(2800),
            })
        );
        let zeroed = r#"{"id":1,"rating":0,"rapidRating":null,"blitzRating":2300}"#;
        assert_eq!(
            parse_chessabc_player_json(zeroed),
            Some(FideRatings {
                title: None,
                standard: None,
                rapid: None,
                blitz: Some(2300),
            })
        );
    }
}
