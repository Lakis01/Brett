//! Wikipedia player summaries for the selected player profile.
//!
//! Lookups happen on profile open or through the explicit batch-enrichment
//! action in Settings, never while a database is indexed or a player list is
//! rendered. A confident chess-player match is shown as a short, attributed
//! summary; anything else becomes a search link. Results are cached locally by
//! article title, so reopening a profile does not repeat the request. No
//! article text is bundled with Brett.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::country::{self, Country};
use crate::titles::{self, PlayerTitle, TitleAward};

const USER_AGENT: &str = "Brett/0.2 (https://github.com/Lakis01/Brett; contact@brett.chess)";
const SUMMARY_ENDPOINT: &str = "https://en.wikipedia.org/api/rest_v1/page/summary/";
const SEARCH_ENDPOINT: &str = "https://en.wikipedia.org/w/rest.php/v1/search/page";
const SEARCH_PAGE: &str = "https://en.wikipedia.org/w/index.php";
const WIKIDATA_ENDPOINT: &str = "https://www.wikidata.org/wiki/Special:EntityData/";
const CACHE_VERSION: u32 = 4;
const MAX_SUMMARIES: usize = 10_000;
const MAX_AMBIGUOUS: usize = 10_000;
static TITLE_HISTORY_SAVE_LOCK: Mutex<()> = Mutex::new(());

/// A cleaned Wikipedia article summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub title: String,
    pub description: String,
    pub extract: String,
    /// Canonical article URL on Wikipedia.
    pub url: String,
    /// Canonical 3-letter FIDE codes ordered oldest → latest; last is the
    /// latest/current federation. Empty when no nationality was detected.
    pub countries: Vec<String>,
    /// Federation/country intervals for historical flag tooltips. Open ends
    /// mean that the source did not specify a boundary.
    pub country_periods: Vec<CountryPeriod>,
    /// FIDE id from Wikidata `P1440`, when available.
    pub fide_id: Option<String>,
    /// Birth year from Wikidata `P569` (or conservative lead-text fallback), when available.
    pub birth_year: Option<i32>,
    /// Death year from Wikidata `P570` (or conservative lead-text fallback), when available.
    pub death_year: Option<i32>,
    /// Peak Elo from Wikidata `P1087`, when available.
    pub peak_elo: Option<u16>,
    /// Latest dated Elo from Wikidata `P1087` (with `P585` point-in-time), when available.
    pub latest_elo: Option<u16>,
    /// Date of `latest_elo` encoded as `YYYYMMDD` (`0` if undated).
    pub latest_elo_date: Option<u32>,
    /// Live FIDE standard rating fetched via `ratings.fide.com`, when available.
    pub fide_standard: Option<u16>,
    /// Live FIDE rapid rating fetched via `ratings.fide.com`, when available.
    pub fide_rapid: Option<u16>,
    /// Live FIDE blitz rating fetched via `ratings.fide.com`, when available.
    pub fide_blitz: Option<u16>,
    /// FIDE title history from Wikidata `P2962`, with award years when the
    /// source supplies them (`P580`, then `P585`).
    pub title_history: Vec<TitleAward>,
}

/// A federation or country associated with a player during a period.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountryPeriod {
    pub code: String,
    pub start_year: Option<i32>,
    pub end_year: Option<i32>,
    /// Context label for historically specific cases such as wartime Germany.
    pub context: Option<String>,
}

fn parse_json_year(value: &Value, key: &str) -> Option<i32> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .filter(|year| (0..=2100).contains(year))
        .map(|year| year as i32)
        .or_else(|| {
            value
                .get(key)
                .and_then(Value::as_u64)
                .filter(|year| *year <= 2100)
                .map(|year| year as i32)
        })
}

/// Extract a 4-digit calendar year (`1400..=2100`) embedded as a standalone token
/// at the end of a lifespan half-segment (e.g. `"31 October 1892"` -> `Some(1892)`).
fn extract_trailing_year(segment: &str) -> Option<i32> {
    let trimmed = segment.trim().trim_end_matches([',', ';', '.']);
    let token = trimmed.split_whitespace().last()?;
    if token.len() == 4 && token.chars().all(|c| c.is_ascii_digit()) {
        let year = token.parse::<i32>().ok()?;
        return (1400..=2100).contains(&year).then_some(year);
    }
    None
}

/// Extract the first 4-digit calendar year (`1400..=2100`) on the right side
/// of a lifespan dash (e.g. `"24 March 1946"`, `"March 24, 1946"`, or `"1946, aged 53"`).
fn extract_death_year_from_right(segment: &str) -> Option<i32> {
    let candidate = segment.split([';', '(']).next().unwrap_or(segment).trim();
    for token in candidate.split_whitespace() {
        let clean = token.trim_matches(|c: char| !c.is_ascii_digit());
        if clean.len() == 4 && clean.chars().all(|c| c.is_ascii_digit()) {
            let year = clean.parse::<i32>().ok()?;
            return (1400..=2100).contains(&year).then_some(year);
        }
    }
    None
}

/// Parse a conservative `(birth_year, death_year)` from a short parenthetical
/// lifespan like `"(1916–1975)"` or `"(7 January 1916 – 5 June 1975)"`.
fn parse_lifespan_paren(inner: &str) -> Option<(Option<i32>, Option<i32>)> {
    let lower = inner.to_ascii_lowercase();
    if lower.contains("tournament")
        || lower.contains("championship")
        || lower.contains("olympiad")
        || lower.contains("rating")
        || lower.contains("match")
    {
        return None;
    }
    for sep in ['–', '—', '−', '-'] {
        if let Some((left, right)) = inner.split_once(sep) {
            let left_part = left.rsplit(';').next().unwrap_or(left);
            if let (Some(born), Some(died)) = (
                extract_trailing_year(left_part),
                extract_death_year_from_right(right),
            ) && born <= died
                && died - born <= 115
                && died - born >= 10
            {
                return Some((Some(born), Some(died)));
            }
        }
    }
    for prefix in ["born ", "b. "] {
        if let Some(pos) = lower.find(prefix) {
            let tail = &inner[pos + prefix.len()..];
            for token in tail
                .split([',', ';', ')'])
                .next()
                .unwrap_or(tail)
                .split_whitespace()
            {
                let clean = token.trim_matches(|c: char| !c.is_ascii_digit());
                if clean.len() == 4
                    && let Ok(year) = clean.parse::<i32>()
                    && (1400..=2025).contains(&year)
                {
                    return Some((Some(year), None));
                }
            }
        }
    }
    None
}

/// Conservatively extract biographical lifespan years from Wikipedia
/// `description` and lead sentence before the copula (`" is "` / `" was "`).
fn lifespan_years_from_texts(description: &str, extract: &str) -> (Option<i32>, Option<i32>) {
    if let Some(open) = description.rfind('(')
        && let Some(close) = description[open..].find(')')
    {
        let inner = &description[open + 1..open + close];
        if let Some(years) = parse_lifespan_paren(inner) {
            return years;
        }
    }
    let lead_limit = [" is ", " was ", " were "]
        .iter()
        .filter_map(|verb| extract.to_ascii_lowercase().find(verb))
        .min()
        .unwrap_or_else(|| extract.len().min(180));
    let lead = &extract[..lead_limit];
    let mut rest = lead;
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(')') else {
            break;
        };
        let inner = &after[..close];
        if let Some(years) = parse_lifespan_paren(inner) {
            return years;
        }
        rest = &after[close + 1..];
    }
    (None, None)
}

fn title_history_from_value(value: &Value) -> Vec<TitleAward> {
    let mut awards = parse_title_history_entries(value.get("title_history"));
    sort_title_history(&mut awards);
    awards
}

fn parse_title_history_entries(value: Option<&Value>) -> Vec<TitleAward> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let title = titles::normalize(entry.get("title")?.as_str()?)?;
            let year = entry
                .get("year")
                .and_then(Value::as_i64)
                .filter(|year| (0..=2100).contains(year))
                .map(|year| year as i32);
            Some(TitleAward { title, year })
        })
        .collect()
}

/// Conservative fallback for articles without a Wikidata `P2962` claim. Only
/// explicit FIDE-title phrases in the lead are accepted, so "grandmaster" in a
/// Extract conservative title awards from Wikipedia's `description` and lead
/// `extract`, requiring explicit chess/FIDE context so non-chess phrases (e.g.
/// "grandmaster of the local lodge") are never misread.
fn title_awards_from_texts(description: &str, extract: &str) -> Vec<TitleAward> {
    let desc_lead: String = description
        .chars()
        .take(240)
        .collect::<String>()
        .to_lowercase();
    let extract_lead: String = extract.chars().take(600).collect::<String>().to_lowercase();
    let has_chess_context = desc_lead.contains("chess")
        || desc_lead.contains("fide")
        || extract_lead.contains("chess")
        || extract_lead.contains("fide");
    if !has_chess_context {
        return Vec::new();
    }

    let is_word_boundary = |text: &str, idx: usize| -> bool {
        idx == 0
            || !text[..idx]
                .chars()
                .next_back()
                .is_some_and(char::is_alphabetic)
    };
    let is_word_end = |text: &str, end: usize| -> bool {
        end >= text.len() || !text[end..].chars().next().is_some_and(char::is_alphabetic)
    };
    let is_preceded_by_woman = |text: &str, idx: usize| -> bool {
        let prefix = text[..idx].trim_end();
        prefix.ends_with("woman") || prefix.ends_with("women's") || prefix.ends_with("womens")
    };
    let extract_award_year = |text: &str, match_end: usize| -> Option<i32> {
        let rest = &text[match_end..];
        let sentence_end = rest.find(['.', ';', '\n']).unwrap_or(rest.len()).min(64);
        let window = &rest[..sentence_end];
        let mut search_from = 0;
        while let Some(rel) = window[search_from..].find("in ") {
            let pos = search_from + rel;
            let before = window[..pos].trim_end();
            let digits_start = pos + 3;
            let digits = &window[digits_start..];
            if digits.len() >= 4
                && digits.as_bytes()[..4].iter().all(u8::is_ascii_digit)
                && digits[4..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_ascii_digit())
                && !before.ends_with("born")
                && let Ok(year) = digits[..4].parse::<i32>()
                && (1940..=2100).contains(&year)
            {
                return Some(year);
            }
            search_from = pos + 3;
        }
        None
    };

    let find_best_in = |text: &str,
                        phrases: &[&str],
                        reject_woman: bool,
                        parse_year: bool|
     -> Option<Option<i32>> {
        let mut found: Option<Option<i32>> = None;
        for phrase in phrases {
            let mut start = 0;
            while let Some(rel) = text[start..].find(phrase) {
                let idx = start + rel;
                let end = idx + phrase.len();
                if is_word_boundary(text, idx)
                    && is_word_end(text, end)
                    && !(reject_woman && is_preceded_by_woman(text, idx))
                    && !(phrase.contains("grandmaster")
                        && text[..idx].trim_end().ends_with("correspondence"))
                    && !(phrase.contains("international master")
                        && text[..idx].trim_end().ends_with("senior"))
                {
                    let year = if parse_year {
                        extract_award_year(text, end)
                    } else {
                        None
                    };
                    match found {
                        None => found = Some(year),
                        Some(None) if year.is_some() => found = Some(year),
                        _ => {}
                    }
                }
                start = end;
            }
        }
        found
    };

    let candidates: &[(PlayerTitle, &[&str], bool)] = &[
        (
            PlayerTitle::Wgm,
            &[
                "woman grandmaster",
                "women's grandmaster",
                "womens grandmaster",
            ],
            false,
        ),
        (
            PlayerTitle::Wim,
            &[
                "woman international master",
                "women's international master",
                "womens international master",
            ],
            false,
        ),
        (
            PlayerTitle::Wfm,
            &[
                "woman fide master",
                "women's fide master",
                "womens fide master",
            ],
            false,
        ),
        (
            PlayerTitle::Wcm,
            &[
                "woman candidate master",
                "women's candidate master",
                "womens candidate master",
            ],
            false,
        ),
        (
            PlayerTitle::Gm,
            &[
                "chess grandmaster",
                "fide grandmaster",
                "grandmaster title",
                "title of grandmaster",
                "title of fide grandmaster",
                "awarded the grandmaster",
                "earned the grandmaster",
                "became a grandmaster",
                "become a grandmaster",
                "international grandmaster",
            ],
            true,
        ),
        (
            PlayerTitle::Im,
            &[
                "chess international master",
                "fide international master",
                "international master title",
                "title of international master",
                "awarded the international master",
                "became an international master",
                "international master",
            ],
            true,
        ),
        (
            PlayerTitle::Fm,
            &[
                "chess fide master",
                "fide master title",
                "title of fide master",
                "fide master",
            ],
            true,
        ),
        (
            PlayerTitle::Cm,
            &[
                "chess candidate master",
                "candidate master title",
                "title of candidate master",
                "candidate master",
            ],
            true,
        ),
    ];

    let mut awards = Vec::new();
    for &(title, phrases, reject_woman) in candidates {
        let from_extract = find_best_in(&extract_lead, phrases, reject_woman, true);
        let from_desc = find_best_in(&desc_lead, phrases, reject_woman, false);
        if let Some(year) = from_extract.or(from_desc) {
            let final_year = year.or_else(|| from_desc.flatten());
            awards.push(TitleAward {
                title,
                year: final_year,
            });
        }
    }
    sort_title_history(&mut awards);
    awards
}

fn sort_title_history(awards: &mut Vec<TitleAward>) {
    awards.sort_by_key(|award| (award.year.unwrap_or(i32::MAX), award.title.rank()));
    awards.dedup_by(|a, b| a.title == b.title && a.year == b.year);
}

impl Summary {
    /// Short display form for the narrow profile panel. It stops at a word
    /// boundary and appends an ellipsis when the article is longer.
    pub fn excerpt(&self, max_chars: usize) -> String {
        let text = self.extract.trim();
        if text.chars().count() <= max_chars {
            return text.to_string();
        }
        let mut out = String::new();
        let mut count = 0usize;
        for word in text.split_whitespace() {
            let word_len = word.chars().count();
            let extra = usize::from(!out.is_empty());
            if count + extra + word_len > max_chars {
                break;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(word);
            count += extra + word_len;
        }
        if out.is_empty() {
            out = text.chars().take(max_chars).collect();
        }
        out.push('…');
        out
    }

    /// Latest (current) federation, i.e. the last entry of
    /// [`Self::countries`] resolved to a [`Country`].
    pub fn latest_country(&self) -> Option<Country> {
        self.countries
            .last()
            .and_then(|code| country::normalize(code))
    }

    /// Highest title currently held, e.g. `GM` for a player whose history
    /// also contains an earlier `IM` award.
    pub fn current_title(&self) -> Option<PlayerTitle> {
        titles::strongest(&self.title_history)
    }

    /// Highest title awarded by `year`, or `None` when no dated award is known.
    pub fn title_at(&self, year: u32) -> Option<PlayerTitle> {
        titles::at_year(&self.title_history, year)
    }

    /// All resolved federations ordered oldest → latest.
    pub fn all_countries(&self) -> Vec<Country> {
        self.countries
            .iter()
            .filter_map(|code| country::normalize(code))
            .collect()
    }

    /// Hover label for a federation flag, including dates when known.
    pub fn country_period_label(period: &CountryPeriod) -> String {
        let name = period
            .context
            .as_deref()
            .or_else(|| country::normalize(&period.code).map(|item| item.name))
            .unwrap_or(period.code.as_str());
        match (period.start_year, period.end_year) {
            (Some(start), Some(end)) => format!("{name} ({start}–{end})"),
            (Some(start), None) => format!("{name} (since {start})"),
            (None, Some(end)) => format!("{name} (until {end})"),
            (None, None) => name.to_string(),
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "title": self.title,
            "description": self.description,
            "extract": self.extract,
            "url": self.url,
            "countries": self.countries,
            "country_periods": self.country_periods.iter().map(|period| json!({
                "code": period.code,
                "start_year": period.start_year,
                "end_year": period.end_year,
                "context": period.context,
            })).collect::<Vec<_>>(),
            "fide_id": self.fide_id,
            "birth_year": self.birth_year,
            "death_year": self.death_year,
            "peak_elo": self.peak_elo,
            "latest_elo": self.latest_elo,
            "latest_elo_date": self.latest_elo_date,
            "fide_standard": self.fide_standard,
            "fide_rapid": self.fide_rapid,
            "fide_blitz": self.fide_blitz,
            "title_history": self.title_history.iter().map(|award| json!({
                "title": award.title.short(),
                "year": award.year,
            })).collect::<Vec<_>>(),
        })
    }

    fn from_value(value: &Value) -> Option<Self> {
        let title = value.get("title")?.as_str()?.trim();
        if title.is_empty() || title.starts_with("List of") || title.contains("(disambiguation)") {
            return None;
        }
        if value.get("type").and_then(Value::as_str) == Some("disambiguation") {
            return None;
        }
        let extract = clean_text(value.get("extract")?.as_str()?);
        if extract.is_empty() {
            return None;
        }
        let description = value
            .get("description")
            .and_then(Value::as_str)
            .map(clean_text)
            .unwrap_or_default();
        let url = value
            .get("url")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .pointer("/content_urls/desktop/page")
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|url| url.starts_with("https://"))
            .map(str::to_string)
            .unwrap_or_else(|| article_url(title));
        // Cached entries from before enrichment may lack the new fields.
        // Prefer the current short article text over cached country codes so
        // stale enrichment can be corrected without another network lookup.
        let stored_countries: Vec<String> = value
            .get("countries")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|code| country::normalize(code).is_some())
                    .map(|code| country::normalize(code).unwrap().code.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let stored_periods: Vec<CountryPeriod> = value
            .get("country_periods")
            .and_then(Value::as_array)
            .map(|periods| {
                periods
                    .iter()
                    .filter_map(country_period_from_value)
                    .collect()
            })
            .unwrap_or_default();
        let (text_birth_year, text_death_year) = lifespan_years_from_texts(&description, &extract);
        let birth_year = parse_json_year(value, "birth_year").or(text_birth_year);
        let death_year = parse_json_year(value, "death_year").or(text_death_year);
        let text_periods = country_periods_from_texts(&description, &extract);
        let country_periods = if text_periods.is_empty() {
            if stored_periods.is_empty() {
                stored_countries
                    .iter()
                    .map(|code| CountryPeriod {
                        code: code.clone(),
                        start_year: None,
                        end_year: None,
                        context: None,
                    })
                    .collect()
            } else {
                stored_periods
            }
        } else if stored_periods.is_empty() {
            text_periods
        } else {
            let text_codes = country_codes_in_order(&text_periods);
            let mut periods: Vec<CountryPeriod> = stored_periods
                .into_iter()
                .filter(|period| {
                    text_codes
                        .iter()
                        .any(|tc| code_matches_text_family(&period.code, tc))
                })
                .collect();
            for text_period in &text_periods {
                if text_period.context.is_some() {
                    periods.retain(|period| period.code != text_period.code);
                    periods.push(text_period.clone());
                } else if periods.iter().any(|period| {
                    period.code == text_period.code
                        && period.start_year.is_none()
                        && period.end_year.is_none()
                }) {
                    periods.retain(|period| {
                        period.code != text_period.code
                            || period.start_year.is_some()
                            || period.end_year.is_some()
                    });
                    periods.push(text_period.clone());
                } else if !periods
                    .iter()
                    .any(|period| text_period_covered_by(&text_period.code, period, death_year))
                {
                    periods.push(text_period.clone());
                }
            }
            periods = upgrade_periods_for_lifespan(periods, birth_year, death_year);
            sort_country_periods(&mut periods, &text_codes);
            periods
        };
        let country_periods = upgrade_periods_for_lifespan(country_periods, birth_year, death_year);
        let countries = country_codes_in_order(&country_periods);
        let fide_id = value
            .get("fide_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        let peak_elo = value
            .get("peak_elo")
            .and_then(Value::as_u64)
            .filter(|elo| *elo > 0 && *elo <= 4000)
            .map(|elo| elo as u16)
            .or_else(|| {
                value
                    .get("peak_elo")
                    .and_then(Value::as_i64)
                    .filter(|elo| *elo > 0 && *elo <= 4000)
                    .map(|elo| elo as u16)
            })
            .or_else(|| {
                peak_elo_from_text(value.get("extract").and_then(Value::as_str).unwrap_or(""))
            });
        let parse_opt_elo = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_u64)
                .filter(|elo| *elo > 0 && *elo <= 4000)
                .map(|elo| elo as u16)
        };
        let latest_elo = parse_opt_elo("latest_elo");
        let latest_elo_date = value
            .get("latest_elo_date")
            .and_then(Value::as_u64)
            .and_then(|d| u32::try_from(d).ok())
            .filter(|&d| d > 0);
        let fide_standard = parse_opt_elo("fide_standard");
        let fide_rapid = parse_opt_elo("fide_rapid");
        let fide_blitz = parse_opt_elo("fide_blitz");
        let mut title_history = title_history_from_value(value);
        for text_award in title_awards_from_texts(&description, &extract) {
            if let Some(existing) = title_history
                .iter_mut()
                .find(|award| award.title == text_award.title)
            {
                if existing.year.is_none() && text_award.year.is_some() {
                    existing.year = text_award.year;
                }
            } else {
                title_history.push(text_award);
            }
        }
        sort_title_history(&mut title_history);
        Some(Self {
            title: title.to_string(),
            description,
            extract,
            url,
            countries,
            country_periods,
            fide_id,
            birth_year,
            death_year,
            peak_elo,
            latest_elo,
            latest_elo_date,
            fide_standard,
            fide_rapid,
            fide_blitz,
            title_history,
        })
    }
}

/// Demonym tokens → canonical FIDE code. Multi-word entries are matched
/// before single words so "south african" wins over "african".
static DEMONYMS: &[(&str, &str)] = &[
    ("afghan", "AFG"),
    ("albanian", "ALB"),
    ("algerian", "ALG"),
    ("andorran", "AND"),
    ("angolan", "ANG"),
    ("argentine", "ARG"),
    ("argentinian", "ARG"),
    ("armenian", "ARM"),
    ("australian", "AUS"),
    ("austrian", "AUT"),
    ("azerbaijani", "AZE"),
    ("bahamian", "BAH"),
    ("bahraini", "BRN"),
    ("bangladeshi", "BAN"),
    ("barbadian", "BAR"),
    ("belarusian", "BLR"),
    ("belgian", "BEL"),
    ("bermudian", "BER"),
    ("bhutanese", "BHU"),
    ("bolivian", "BOL"),
    ("bosnian", "BIH"),
    ("botswanan", "BOT"),
    ("brazilian", "BRA"),
    ("bulgarian", "BUL"),
    ("burkinabe", "BUR"),
    ("burundian", "BDI"),
    ("cambodian", "CAM"),
    ("cameroonian", "CMR"),
    ("canadian", "CAN"),
    ("cape verdean", "CPV"),
    ("cayman", "CAY"),
    ("chadian", "CHA"),
    ("chilean", "CHI"),
    ("chinese", "CHN"),
    ("colombian", "COL"),
    ("comorian", "COM"),
    ("congolese", "CGO"),
    ("costa rican", "CRC"),
    ("ivorian", "CIV"),
    ("croatian", "CRO"),
    ("cuban", "CUB"),
    ("cypriot", "CYP"),
    ("czech", "CZE"),
    ("danish", "DEN"),
    ("djiboutian", "DJI"),
    ("dominican", "DOM"),
    ("ecuadorian", "ECU"),
    ("egyptian", "EGY"),
    ("salvadoran", "ESA"),
    ("british", "GBR"),
    ("english", "ENG"),
    ("scottish", "SCO"),
    ("welsh", "WLS"),
    ("equatorial guinean", "GEQ"),
    ("eritrean", "ERI"),
    ("estonian", "EST"),
    ("swazi", "SWZ"),
    ("eswatini", "SWZ"),
    ("ethiopian", "ETH"),
    ("faroese", "FAI"),
    ("fijian", "FIJ"),
    ("finnish", "FIN"),
    ("french", "FRA"),
    ("gabonese", "GAB"),
    ("gambian", "GAM"),
    ("georgian", "GEO"),
    ("german", "GER"),
    ("ghanaian", "GHA"),
    ("greek", "GRE"),
    ("guatemalan", "GUA"),
    ("guyanese", "GUY"),
    ("haitian", "HAI"),
    ("honduran", "HON"),
    ("hong kong", "HKG"),
    ("hungarian", "HUN"),
    ("icelandic", "ISL"),
    ("indian", "IND"),
    ("indonesian", "INA"),
    ("iranian", "IRI"),
    ("persian", "IRI"),
    ("iraqi", "IRQ"),
    ("irish", "IRL"),
    ("manx", "IMN"),
    ("israeli", "ISR"),
    ("italian", "ITA"),
    ("jamaican", "JAM"),
    ("japanese", "JPN"),
    ("jordanian", "JOR"),
    ("kazakh", "KAZ"),
    ("kazakhstani", "KAZ"),
    ("kenyan", "KEN"),
    ("kosovan", "KOS"),
    ("kosovar", "KOS"),
    ("kuwaiti", "KUW"),
    ("kyrgyz", "KGZ"),
    ("laotian", "LAO"),
    ("latvian", "LAT"),
    ("lebanese", "LBN"),
    ("liberian", "LBR"),
    ("libyan", "LBA"),
    ("liechtenstein", "LIE"),
    ("lithuanian", "LTU"),
    ("luxembourgish", "LUX"),
    ("macanese", "MAC"),
    ("malagasy", "MAD"),
    ("malawian", "MAW"),
    ("malaysian", "MAS"),
    ("maldivian", "MDV"),
    ("malian", "MLI"),
    ("maltese", "MLT"),
    ("mauritanian", "MTN"),
    ("mauritian", "MRI"),
    ("mexican", "MEX"),
    ("moldovan", "MDA"),
    ("monacan", "MNC"),
    ("mongolian", "MGL"),
    ("montenegrin", "MNE"),
    ("moroccan", "MAR"),
    ("mozambican", "MOZ"),
    ("myanmar", "MYA"),
    ("burmese", "MYA"),
    ("namibian", "NAM"),
    ("nauruan", "NRU"),
    ("nepali", "NEP"),
    ("nepalese", "NEP"),
    ("dutch", "NED"),
    ("new zealand", "NZL"),
    ("nicaraguan", "NCA"),
    ("nigerien", "NIG"),
    ("nigerian", "NGR"),
    ("north macedonian", "MKD"),
    ("macedonian", "MKD"),
    ("norwegian", "NOR"),
    ("omani", "OMA"),
    ("pakistani", "PAK"),
    ("palauan", "PLW"),
    ("palestinian", "PLE"),
    ("panamanian", "PAN"),
    ("paraguayan", "PAR"),
    ("peruvian", "PER"),
    ("filipino", "PHI"),
    ("Philippine", "PHI"),
    ("polish", "POL"),
    ("portuguese", "POR"),
    ("puerto rican", "PUR"),
    ("qatari", "QAT"),
    ("romanian", "ROU"),
    ("russian", "RUS"),
    ("rwandan", "RWA"),
    ("san marinese", "SMR"),
    ("saudi arabian", "KSA"),
    ("senegalese", "SEN"),
    ("serbian", "SRB"),
    ("seychellois", "SEY"),
    ("sierra leone", "SLE"),
    ("singaporean", "SGP"),
    ("slovak", "SVK"),
    ("slovenian", "SLO"),
    ("somali", "SOM"),
    ("south african", "RSA"),
    ("south korean", "KOR"),
    ("korean", "KOR"),
    ("south sudanese", "SSD"),
    ("spanish", "ESP"),
    ("sri lankan", "SRI"),
    ("sudanese", "SUD"),
    ("surinamese", "SUR"),
    ("swedish", "SWE"),
    ("swiss", "SUI"),
    ("syrian", "SYR"),
    ("tajik", "TJK"),
    ("tanzanian", "TAN"),
    ("thai", "THA"),
    ("timorese", "TLS"),
    ("togolese", "TOG"),
    ("tongan", "TGA"),
    ("trinidadian", "TTO"),
    ("tunisian", "TUN"),
    ("turkish", "TUR"),
    ("turkmen", "TKM"),
    ("ugandan", "UGA"),
    ("ukrainian", "UKR"),
    ("emirati", "UAE"),
    ("american", "USA"),
    ("uruguayan", "URU"),
    ("uzbek", "UZB"),
    ("vanuatu", "VAN"),
    ("venezuelan", "VEN"),
    ("vietnamese", "VIE"),
    ("yemenite", "YEM"),
    ("zambian", "ZAM"),
    ("zimbabwean", "ZIM"),
    ("soviet", "URS"),
    ("yugoslav", "YUG"),
    ("east german", "GDR"),
    ("west german", "FRG"),
    ("czechoslovak", "TCH"),
    ("serbia and montenegro", "SCG"),
    ("nazi germany", "GER"),
    ("austro-hungarian empire", "AUH"),
    ("austro hungarian empire", "AUH"),
    ("austro-hungarian", "AUH"),
    ("austro hungarian", "AUH"),
    ("austria-hungary", "AUH"),
    ("austria hungary", "AUH"),
    ("austrian empire", "AUE"),
    ("kingdom of bohemia", "BOH"),
    ("bohemian", "BOH"),
    ("kingdom of prussia", "PRU"),
    ("prussian", "PRU"),
    ("german empire", "DEI"),
    ("russian empire", "RUE"),
];

/// Lowercase alphanumeric tokens in order of appearance.
fn demonym_tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| deunicode::deunicode(token).to_lowercase())
        .collect()
}

/// Nationality demonyms in `text`, as canonical FIDE codes ordered by first
/// appearance (oldest → latest), deduplicated.
///
/// Matching prefers multi-word phrases ("south african", "east german")
/// over single words. Unknown words are ignored.
pub(crate) fn demonym_countries(text: &str) -> Vec<String> {
    let tokens = demonym_tokens(text);
    if tokens.is_empty() {
        return Vec::new();
    }
    // Demonym map keyed by phrase tokens, longest first.
    let mut phrases: Vec<(Vec<String>, &str)> = DEMONYMS
        .iter()
        .map(|(phrase, code)| {
            (
                phrase
                    .split_whitespace()
                    .map(|word| deunicode::deunicode(word).to_lowercase())
                    .collect::<Vec<_>>(),
                *code,
            )
        })
        .collect();
    phrases.sort_by_key(|(words, _)| std::cmp::Reverse(words.len()));
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < tokens.len() {
        let mut matched: Option<(&str, usize)> = None;
        for (words, code) in &phrases {
            if words.is_empty() || i + words.len() > tokens.len() {
                continue;
            }
            if tokens[i..i + words.len()] == words[..] {
                matched = Some((code, words.len()));
                break;
            }
        }
        if let Some((code, len)) = matched {
            if !out.iter().any(|existing| existing == code) {
                out.push(code.to_string());
            }
            i += len;
        } else {
            i += 1;
        }
    }
    out
}

/// Countries from a Wikipedia description plus the lead sentence of the
/// extract, oldest → latest.
///
/// Wikipedia short descriptions almost always name only the player's current
/// nationality (e.g. `"Canadian chess grandmaster"`), whereas the lead sentence
/// often gives birth-to-current order (`"American-born Canadian chess
/// grandmaster"`). Lead-only codes (like birth citizenship in `X-born Y` or
/// `"former Soviet"`) therefore belong *before* the description codes, so
/// `.last()` remains the player's current/dominant federation.
fn countries_from_texts(description: &str, extract: &str) -> Vec<String> {
    let mut desc_codes = demonym_countries(description);
    let lead = extract.split('.').next().unwrap_or(extract);
    let lead_codes = demonym_countries(lead);
    // Wikipedia short descriptions often simplify "Czechoslovak" to "Czech"
    // (e.g. Vlastimil Hort: desc "Czech and German...", lead "Czechoslovak and German...").
    // Do not invent a post-1993 CZE federation when the lead specifically says TCH.
    if lead_codes.iter().any(|c| c == "TCH") && !lead_codes.iter().any(|c| c == "CZE") {
        desc_codes.retain(|c| c != "CZE");
    }
    let mut out: Vec<String> = Vec::new();
    for code in &lead_codes {
        if !desc_codes.iter().any(|existing| existing == code)
            && !out.iter().any(|existing| existing == code)
        {
            out.push(code.clone());
        }
    }
    for code in desc_codes {
        if !out.iter().any(|existing| existing == &code) {
            out.push(code);
        }
    }
    // Wikipedia's short extracts sometimes put a federation change in the
    // second sentence ("represented the United States since 2021") or name
    // historical federations only in the biographical context. These are
    // still player-history facts, unlike incidental country mentions in a
    // full article.
    let lower = extract.to_lowercase();
    if lower.contains("united states since") && !out.iter().any(|code| code == "USA") {
        out.push("USA".to_string());
    }
    if lower.contains("nazi germany") && !out.iter().any(|code| code == "GER") {
        out.push("GER".to_string());
    }
    if lower.contains("soviet union") && !out.iter().any(|code| code == "URS") {
        out.push("URS".to_string());
    }
    // Include the remainder of the extract only when neither the
    // description nor the lead sentence named a nationality — avoids
    // picking up incidental country mentions later in the article.
    if out.is_empty() {
        out = demonym_countries(extract);
    }
    out
}

fn country_codes_in_order(periods: &[CountryPeriod]) -> Vec<String> {
    let mut periods: Vec<(usize, &CountryPeriod)> = periods.iter().enumerate().collect();
    periods.sort_by_key(|(index, period)| {
        (
            period.start_year.or(period.end_year).unwrap_or(i32::MAX),
            *index,
        )
    });
    let mut codes = Vec::new();
    for (_, period) in periods {
        if country::normalize(&period.code).is_some()
            && !codes.iter().any(|code| code == &period.code)
        {
            codes.push(period.code.clone());
        }
    }
    codes
}

fn country_period_from_value(value: &Value) -> Option<CountryPeriod> {
    let code = value.get("code")?.as_str()?.trim();
    let code = country::normalize(code)?.code.to_string();
    let year = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_i64)
            .filter(|year| (0..=2100).contains(year))
            .map(|year| year as i32)
            .or_else(|| {
                value
                    .get(key)
                    .and_then(Value::as_u64)
                    .filter(|year| *year <= 2100)
                    .map(|year| year as i32)
            })
    };
    let context = value
        .get("context")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|context| !context.is_empty())
        .map(str::to_string);
    Some(CountryPeriod {
        code,
        start_year: year("start_year"),
        end_year: year("end_year"),
        context,
    })
}

fn year_range_after(text: &str, marker: &str) -> Option<(i32, i32)> {
    let start = text.to_lowercase().find(&marker.to_lowercase())? + marker.len();
    let tail = &text[start..];
    let open = tail.find('(')?;
    let close = tail[open + 1..].find(')')? + open + 1;
    let range = &tail[open + 1..close];
    let (first, last) = range.split_once(['–', '-', '—'])?;
    let first = first.trim().parse::<i32>().ok()?;
    let last = last.trim().parse::<i32>().ok()?;
    ((0..=2100).contains(&first) && (0..=2100).contains(&last)).then_some((first, last))
}

fn year_after(text: &str, marker: &str) -> Option<i32> {
    let start = text.to_lowercase().find(&marker.to_lowercase())? + marker.len();
    let digits: String = text[start..]
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    let year = digits.parse::<i32>().ok()?;
    (0..=2100).contains(&year).then_some(year)
}

fn code_matches_text_family(wikidata_code: &str, text_code: &str) -> bool {
    if wikidata_code == text_code {
        return true;
    }
    match wikidata_code {
        "AUE" | "AUH" | "BOH" => matches!(text_code, "AUT" | "AUH" | "AUE" | "BOH" | "CZE" | "HUN"),
        "DEI" | "PRU" | "GDR" | "FRG" => matches!(text_code, "GER" | "DEI" | "PRU" | "GDR" | "FRG" | "POL"),
        "RUE" | "URS" => matches!(
            text_code,
            "RUS" | "RUE" | "URS" | "POL" | "UKR" | "LAT" | "LTU" | "EST" | "BLR" | "GEO" | "ARM"
        ),
        "TCH" => matches!(text_code, "TCH" | "CZE" | "SVK"),
        "YUG" | "SCG" => matches!(
            text_code,
            "YUG" | "SCG" | "SRB" | "CRO" | "SLO" | "BIH" | "MNE" | "MKD"
        ),
        _ => false,
    }
}

fn text_period_covered_by(text_code: &str, existing: &CountryPeriod, death_year: Option<i32>) -> bool {
    if existing.code == text_code {
        return true;
    }
    match text_code {
        "AUT" => matches!(existing.code.as_str(), "AUE" | "AUH"),
        "GER" if death_year.is_some_and(|d| d <= 1918) => {
            matches!(existing.code.as_str(), "DEI" | "PRU")
        }
        "RUS" if death_year.is_some_and(|d| d <= 1917) => existing.code == "RUE",
        _ => false,
    }
}

fn default_historical_bounds(code: &str) -> (Option<i32>, Option<i32>) {
    match code {
        "AUE" => (Some(1804), Some(1867)),
        "AUH" => (Some(1867), Some(1918)),
        "DEI" => (Some(1871), Some(1918)),
        "PRU" => (None, Some(1918)),
        "RUE" => (None, Some(1917)),
        "TCH" => (Some(1918), Some(1992)),
        _ => (None, None),
    }
}

fn upgrade_periods_for_lifespan(
    mut periods: Vec<CountryPeriod>,
    birth_year: Option<i32>,
    death_year: Option<i32>,
) -> Vec<CountryPeriod> {
    if let Some(died) = death_year {
        for p in &mut periods {
            if died <= 1918 && p.code == "AUT" {
                p.code = if died < 1867 { "AUE" } else { "AUH" }.to_string();
            } else if died <= 1918 && p.code == "GER" {
                p.code = if died < 1871 { "PRU" } else { "DEI" }.to_string();
            } else if died <= 1917 && p.code == "RUS" {
                p.code = "RUE".to_string();
            }
        }
        if died <= 1918
            && birth_year.is_some_and(|b| b < 1867)
            && periods.iter().any(|p| p.code == "AUH")
            && !periods.iter().any(|p| p.code == "AUE")
            && !periods.iter().any(|p| p.code == "BOH")
        {
            periods.insert(
                0,
                CountryPeriod {
                    code: "AUE".to_string(),
                    start_year: None,
                    end_year: Some(1867),
                    context: None,
                },
            );
        }
    }
    for p in &mut periods {
        if p.start_year.is_none() && p.end_year.is_none() {
            let (ds, de) = default_historical_bounds(&p.code);
            if ds.is_some() || de.is_some() {
                p.start_year = ds;
                p.end_year = de;
            }
        }
    }
    let mut deduped: Vec<CountryPeriod> = Vec::with_capacity(periods.len());
    for p in periods {
        if let Some(existing) = deduped.iter_mut().find(|e| {
            if e.code != p.code {
                return false;
            }
            let compat_start =
                e.start_year.is_none() || p.start_year.is_none() || e.start_year == p.start_year;
            let compat_end =
                e.end_year.is_none() || p.end_year.is_none() || e.end_year == p.end_year;
            let merged_start = e.start_year.or(p.start_year);
            let merged_end = e.end_year.or(p.end_year);
            let valid_range = merged_start
                .zip(merged_end)
                .is_none_or(|(start, end)| start <= end);
            compat_start && compat_end && valid_range
        }) {
            if existing.start_year.is_none() {
                existing.start_year = p.start_year;
            }
            if existing.end_year.is_none() {
                existing.end_year = p.end_year;
            }
            if existing.context.is_none() {
                existing.context = p.context;
            }
        } else {
            deduped.push(p);
        }
    }
    deduped
}

fn sort_country_periods(periods: &mut [CountryPeriod], text_codes: &[String]) {
    let text_pos = |code: &str| -> usize {
        text_codes
            .iter()
            .position(|tc| code_matches_text_family(code, tc))
            .unwrap_or(usize::MAX)
    };
    periods.sort_by_key(|period| {
        let historical_rank = match period.code.as_str() {
            "BOH" => 0,
            "AUE" | "PRU" => 1,
            "AUH" | "DEI" | "RUE" => 2,
            "TCH" | "URS" | "YUG" => 3,
            _ => 4,
        };
        (
            period.end_year.is_none(),
            period.start_year.or(period.end_year).unwrap_or(i32::MAX),
            historical_rank,
            text_pos(&period.code),
        )
    });
}

fn country_periods_from_texts(description: &str, extract: &str) -> Vec<CountryPeriod> {
    let countries = countries_from_texts(description, extract);
    let mut periods: Vec<CountryPeriod> = countries
        .iter()
        .map(|code| CountryPeriod {
            code: code.clone(),
            start_year: None,
            end_year: None,
            context: None,
        })
        .collect();
    let lower = extract.to_lowercase();

    if lower.contains("represented the united states since")
        && let Some(year) = year_after(extract, "United States since")
    {
        if let Some(usa) = periods.iter_mut().find(|period| period.code == "USA") {
            usa.start_year = Some(year);
        }
        if let Some(arm) = periods.iter_mut().find(|period| period.code == "ARM") {
            arm.end_year = Some(year);
        }
    }

    if lower.contains("former soviet")
        && periods.iter().any(|period| period.code == "URS")
        && periods.iter().any(|period| period.code == "RUS")
    {
        periods = vec![
            CountryPeriod {
                code: "URS".to_string(),
                start_year: None,
                end_year: Some(1991),
                context: None,
            },
            CountryPeriod {
                code: "RUS".to_string(),
                start_year: Some(1991),
                end_year: None,
                context: None,
            },
        ];
    }

    if lower.contains("nazi germany")
        && let Some((start, end)) = year_range_after(extract, "Nazi Germany")
    {
        periods = vec![
            CountryPeriod {
                code: "EST".to_string(),
                start_year: None,
                end_year: Some(1940),
                context: None,
            },
            CountryPeriod {
                code: "GER".to_string(),
                start_year: Some(start),
                end_year: Some(end),
                context: Some("Nazi Germany".to_string()),
            },
            CountryPeriod {
                code: "URS".to_string(),
                start_year: Some(end),
                end_year: Some(1975),
                context: None,
            },
        ];
    }

    let (born, died) = lifespan_years_from_texts(description, extract);
    upgrade_periods_for_lifespan(periods, born, died)
}

fn wikibase_item(value: &Value) -> Option<String> {
    value
        .get("wikibase_item")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|qid| {
            qid.len() > 1
                && (qid.starts_with('Q') || qid.starts_with('q'))
                && qid[1..].chars().all(|c| c.is_ascii_digit())
        })
        .map(str::to_string)
}

fn wikidata_url(qid: &str) -> String {
    format!("{WIKIDATA_ENDPOINT}{qid}.json")
}

/// First entity object inside a Wikidata `Special:EntityData` payload.
fn wikidata_entity(value: &Value) -> Option<&Value> {
    if let Some(entity) = value.get("entities").and_then(Value::as_object) {
        return entity.values().next();
    }
    // Bare entity object (fabricated claims payloads used in tests).
    if value.get("claims").is_some() {
        return Some(value);
    }
    None
}

fn claim_string_value(claim: &Value) -> Option<String> {
    // P1440: datavalue.value is a plain string.
    claim
        .pointer("/mainsnak/datavalue/value")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .or_else(|| {
            claim
                .pointer("/mainsnak/datavalue/value")
                .and_then(Value::as_u64)
                .map(|n| n.to_string())
        })
}

fn claim_entity_qid(claim: &Value) -> Option<String> {
    claim
        .pointer("/mainsnak/datavalue/value/id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|qid| !qid.is_empty())
        .map(str::to_string)
}

fn claim_rank(claim: &Value) -> &str {
    claim
        .get("rank")
        .and_then(Value::as_str)
        .unwrap_or("normal")
}

fn parse_time_year(claim: &Value) -> Option<i32> {
    let time = claim.pointer("/mainsnak/datavalue/value/time")?.as_str()?;
    parse_wikidata_time_year(time)
}

fn parse_qualifier_year(claim: &Value, qualifier: &str) -> Option<i32> {
    let time = claim
        .pointer(&format!(
            "/qualifiers/{qualifier}/0/mainsnak/datavalue/value/time"
        ))
        .or_else(|| claim.pointer(&format!("/qualifiers/{qualifier}/0/datavalue/value/time")))?
        .as_str()?;
    parse_wikidata_time_year(time)
}

fn parse_qualifier_date(claim: &Value, qualifier: &str) -> Option<u32> {
    let time = claim
        .pointer(&format!(
            "/qualifiers/{qualifier}/0/mainsnak/datavalue/value/time"
        ))
        .or_else(|| claim.pointer(&format!("/qualifiers/{qualifier}/0/datavalue/value/time")))?
        .as_str()?;
    parse_wikidata_time_ymd(time)
}

fn parse_wikidata_time_year(time: &str) -> Option<i32> {
    let digits: String = time
        .trim()
        .trim_start_matches('+')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.len() < 4 {
        return None;
    }
    let year: i32 = digits[..4].parse().ok()?;
    (0..=2100).contains(&year).then_some(year)
}

fn parse_wikidata_time_ymd(time: &str) -> Option<u32> {
    let date_part = time
        .trim()
        .trim_start_matches('+')
        .split('T')
        .next()
        .unwrap_or("");
    let mut parts = date_part.split('-');
    let year: u32 = parts.next()?.parse().ok()?;
    if year == 0 || year > 2100 {
        return None;
    }
    let month: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|&m| (1..=12).contains(&m))
        .unwrap_or(1);
    let day: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|&d| (1..=31).contains(&d))
        .unwrap_or(1);
    Some(year * 10_000 + month * 100 + day)
}

fn parse_quantity_elo(claim: &Value) -> Option<u16> {
    if let Some(amount) = claim
        .pointer("/mainsnak/datavalue/value/amount")
        .and_then(Value::as_str)
    {
        let cleaned = amount.trim().trim_start_matches('+');
        if let Ok(elo) = cleaned.parse::<u16>()
            && elo > 0
            && elo <= 4000
        {
            return Some(elo);
        }
    }
    if let Some(value) = claim
        .pointer("/mainsnak/datavalue/value")
        .and_then(Value::as_u64)
        && value > 0
        && value <= 4000
    {
        return Some(value as u16);
    }
    None
}

/// Wikidata fields extracted from an `EntityData` payload.
#[derive(Clone, Debug, Default)]
pub(crate) struct WikidataFields {
    pub countries: Vec<String>,
    pub country_periods: Vec<CountryPeriod>,
    pub fide_id: Option<String>,
    pub birth_year: Option<i32>,
    pub death_year: Option<i32>,
    pub peak_elo: Option<u16>,
    pub latest_elo: Option<u16>,
    pub latest_elo_date: Option<u32>,
    pub title_history: Vec<TitleAward>,
}

/// Map the Wikidata items used by `P2962` (title of chess person) onto the
/// canonical short titles. Only the eight FIDE over-the-board player titles
/// are mapped; honorary, correspondence, arbiter, trainer, and composition
/// titles are intentionally left out.
fn title_from_wikidata_qid(qid: &str) -> Option<PlayerTitle> {
    Some(match qid {
        "Q105269" => PlayerTitle::Gm,
        "Q752119" => PlayerTitle::Im,
        "Q1049520" => PlayerTitle::Fm,
        "Q1973600" => PlayerTitle::Cm,
        "Q3417060" => PlayerTitle::Wgm,
        "Q3314851" => PlayerTitle::Wim,
        "Q3352252" => PlayerTitle::Wfm,
        "Q11982685" => PlayerTitle::Wcm,
        _ => return None,
    })
}

/// Extract a year from a Wikidata time property, preferring `"preferred"` rank
/// and falling back to `"normal"` while ignoring `"deprecated"` claims.
fn parse_wikidata_claim_year(claims: &Value, property: &str) -> Option<i32> {
    let snatched = claims.get(property).and_then(Value::as_array)?;
    let mut fallback = None;
    for claim in snatched {
        let rank = claim_rank(claim);
        if rank == "deprecated" {
            continue;
        }
        if let Some(year) = parse_time_year(claim) {
            if rank == "preferred" {
                return Some(year);
            }
            if fallback.is_none() {
                fallback = Some(year);
            }
        }
    }
    fallback
}

/// Parse Wikidata `EntityData` claims: `P1440` → `fide_id`, `P1532`/`P27`
/// → `countries` (oldest → latest), `P569` → `birth_year`, `P570` → `death_year`,
/// `P1087` → `peak_elo`, and `P2962` → `title_history` (with the `P580` award year
/// when stated). Malformed payloads yield defaults; this never panics.
pub(crate) fn parse_wikidata_value(value: &Value) -> WikidataFields {
    let mut fields = WikidataFields::default();
    let Some(entity) = wikidata_entity(value) else {
        return fields;
    };
    let Some(claims) = entity.get("claims") else {
        return fields;
    };
    // P1440 → fide_id (first non-deprecated claim wins).
    if let Some(snatched) = claims.get("P1440").and_then(Value::as_array) {
        for claim in snatched {
            if claim_rank(claim) == "deprecated" {
                continue;
            }
            if let Some(id) = claim_string_value(claim)
                && !id.is_empty()
            {
                fields.fide_id = Some(id);
                break;
            }
        }
    }
    // P569 → birth_year, P570 → death_year.
    fields.birth_year = parse_wikidata_claim_year(claims, "P569");
    fields.death_year = parse_wikidata_claim_year(claims, "P570");
    // P1087 → peak_elo (max across non-deprecated claims) and latest_elo
    // (preferring the latest P585 point-in-time date, then preferred rank).
    if let Some(snatched) = claims.get("P1087").and_then(Value::as_array) {
        let mut best_latest: Option<(u32, bool, usize, u16)> = None;
        for (idx, claim) in snatched.iter().enumerate() {
            if claim_rank(claim) == "deprecated" {
                continue;
            }
            if let Some(elo) = parse_quantity_elo(claim) {
                fields.peak_elo = Some(match fields.peak_elo {
                    Some(current) => current.max(elo),
                    None => elo,
                });
                let date = parse_qualifier_date(claim, "P585")
                    .or_else(|| parse_qualifier_date(claim, "P580"))
                    .unwrap_or(0);
                let is_preferred = claim_rank(claim) == "preferred";
                let candidate = (date, is_preferred, idx, elo);
                if best_latest.is_none_or(|cur| {
                    (candidate.0, candidate.1, candidate.2) > (cur.0, cur.1, cur.2)
                }) {
                    best_latest = Some(candidate);
                }
            }
        }
        if let Some((date, _, _, elo)) = best_latest {
            fields.latest_elo = Some(elo);
            fields.latest_elo_date = (date > 0).then_some(date);
        }
    }
    // P2962 → chess titles. Every non-deprecated claim is a dated step in the
    // player's title history (IM in 2003, GM in 2004), which lets historical
    // game rows show the title actually held at the time.
    if let Some(snatched) = claims.get("P2962").and_then(Value::as_array) {
        for claim in snatched {
            if claim_rank(claim) == "deprecated" {
                continue;
            }
            let Some(qid) = claim_entity_qid(claim) else {
                continue;
            };
            let Some(title) = title_from_wikidata_qid(&qid) else {
                continue;
            };
            let year =
                parse_qualifier_year(claim, "P580").or_else(|| parse_qualifier_year(claim, "P585"));
            fields.title_history.push(TitleAward { title, year });
        }
    }
    // P1532 / P27 → countries and tenure qualifiers. P1532 is the sport
    // federation history when available; P27 is citizenship context / dated
    // fallback (e.g. Karpov). Undated or open-ended active sport federations
    // (P1532) and preferred claims must sort last so `latest_country()` picks
    // the active sport federation over birth/dual citizenship.
    let mut sourced: Vec<(CountryPeriod, bool, bool)> = Vec::new();
    for (prop, is_sport) in [("P1532", true), ("P27", false)] {
        if let Some(snatched) = claims.get(prop).and_then(Value::as_array) {
            for claim in snatched {
                if claim_rank(claim) == "deprecated" {
                    continue;
                }
                let Some(qid) = claim_entity_qid(claim) else {
                    continue;
                };
                let Some(country) = country::from_wikidata_qid(&qid) else {
                    continue;
                };
                sourced.push((
                    CountryPeriod {
                        code: country.code.to_string(),
                        start_year: parse_qualifier_year(claim, "P580"),
                        end_year: parse_qualifier_year(claim, "P582"),
                        context: None,
                    },
                    is_sport,
                    claim_rank(claim) == "preferred",
                ));
            }
        }
    }
    sourced.sort_by_key(|(period, is_sport, is_preferred)| {
        (
            period.end_year.is_none(),
            period.end_year.unwrap_or(i32::MAX),
            if period.end_year.is_some() {
                period.start_year.unwrap_or(i32::MIN)
            } else {
                0
            },
            *is_sport,
            *is_preferred,
            period.start_year.unwrap_or(i32::MIN),
        )
    });
    for (period, _, _) in sourced {
        if let Some(existing) = fields.country_periods.iter_mut().find(|existing| {
            existing.code == period.code
                && (existing.start_year.is_none()
                    || period.start_year.is_none()
                    || existing.start_year == period.start_year)
                && (existing.end_year.is_none()
                    || period.end_year.is_none()
                    || existing.end_year == period.end_year)
        }) {
            if existing.start_year.is_none() {
                existing.start_year = period.start_year;
            }
            if existing.end_year.is_none() {
                existing.end_year = period.end_year;
            }
        } else {
            fields.country_periods.push(period);
        }
    }
    fields.countries = country_codes_in_order(&fields.country_periods);
    fields
}

/// Merge Wikidata fields into a summary. Country/federation claims are
/// constrained by the Wikipedia lead/description when that text identifies
/// the player's federations; this avoids treating a residence/citizenship
/// claim as a sporting federation (e.g. a temporary home country).
pub(crate) fn apply_wikidata_value(summary: &mut Summary, value: &Value) {
    let fields = parse_wikidata_value(value);
    if let Some(fide_id) = fields.fide_id {
        summary.fide_id = Some(fide_id);
    }
    if let Some(birth_year) = fields.birth_year {
        summary.birth_year = Some(birth_year);
    }
    if let Some(death_year) = fields.death_year {
        summary.death_year = Some(death_year);
    }
    if summary.birth_year.is_none() || summary.death_year.is_none() {
        let (text_birth, text_death) =
            lifespan_years_from_texts(&summary.description, &summary.extract);
        if summary.birth_year.is_none() {
            summary.birth_year = text_birth;
        }
        if summary.death_year.is_none() {
            summary.death_year = text_death;
        }
    }
    if !fields.country_periods.is_empty() {
        let text_codes = summary.countries.clone();
        let mut periods: Vec<CountryPeriod> = if text_codes.is_empty() {
            fields.country_periods
        } else {
            fields
                .country_periods
                .into_iter()
                .filter(|period| {
                    text_codes
                        .iter()
                        .any(|tc| code_matches_text_family(&period.code, tc))
                })
                .collect()
        };

        // Keep any federation explicitly established by the Wikipedia text that
        // Wikidata omitted or expressed under a different historical entity
        // (e.g. Bohemia for Steinitz, Czechoslovakia for Hort, Nazi Germany for Keres).
        for text_period in &summary.country_periods {
            if text_period.context.is_some() {
                periods.retain(|period| period.code != text_period.code);
                periods.push(text_period.clone());
            } else if (text_period.start_year.is_some() || text_period.end_year.is_some())
                && periods.iter().any(|period| {
                    period.code == text_period.code
                        && period.start_year.is_none()
                        && period.end_year.is_none()
                })
            {
                periods.retain(|period| {
                    period.code != text_period.code
                        || period.start_year.is_some()
                        || period.end_year.is_some()
                });
                periods.push(text_period.clone());
            } else if !periods
                .iter()
                .any(|period| text_period_covered_by(&text_period.code, period, summary.death_year))
            {
                periods.push(text_period.clone());
            }
        }
        periods = upgrade_periods_for_lifespan(periods, summary.birth_year, summary.death_year);
        sort_country_periods(&mut periods, &text_codes);
        summary.countries = country_codes_in_order(&periods);
        summary.country_periods = periods;
    }
    if !fields.title_history.is_empty() {
        // Keep the undated lead-text title as a current-profile fallback even
        // when Wikidata has an incomplete, older title history. Historical
        // game rows ignore undated entries.
        summary.title_history.extend(fields.title_history);
        sort_title_history(&mut summary.title_history);
    }
    if let Some(peak_elo) = fields.peak_elo {
        summary.peak_elo = Some(peak_elo);
    } else if summary.peak_elo.is_none() {
        // Fallback: "with a rating of 2830" / "No. 2 ... March 2014 ...
        // rating of 2830" style claims in the lead extract.
        if let Some(elo) = peak_elo_from_text(&summary.extract) {
            summary.peak_elo = Some(elo);
        }
    }
    if let Some(latest_elo) = fields.latest_elo {
        summary.latest_elo = Some(latest_elo);
        summary.latest_elo_date = fields.latest_elo_date;
    }
}

/// Extract a peak/highest rating claim from lead text
/// (e.g. "with a rating of 2830").
pub(crate) fn peak_elo_from_text(text: &str) -> Option<u16> {
    let lower = text.to_lowercase();
    let mut best: Option<u16> = None;
    for marker in ["with a rating of ", "rating of ", "peak rating "] {
        let mut search_from = 0usize;
        while let Some(pos) = lower[search_from..].find(marker) {
            let start = search_from + pos + marker.len();
            let rest = &lower[start..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.len() == 4
                && let Ok(elo) = digits.parse::<u16>()
                && (2000..=4000).contains(&elo)
            {
                best = Some(match best {
                    Some(current) => current.max(elo),
                    None => elo,
                });
            }
            search_from = start;
        }
    }
    best
}

/// Fetch the Wikidata entity for `qid` and merge it into `summary`.
/// Fail-soft: any error leaves the summary untouched.
fn enrich_with_wikidata(
    summary: &mut Summary,
    qid: &str,
    fetch: &mut impl FnMut(&str) -> std::result::Result<Value, FetchError>,
) {
    let url = wikidata_url(qid);
    if let Ok(value) = fetch(&url) {
        apply_wikidata_value(summary, &value);
    }
}

/// The result of looking up a player name.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Lookup {
    /// A confidently matched chess player, ready to display.
    Summary(Summary),
    /// No confident chess-player match; open a search instead.
    Ambiguous { search_url: String },
}

/// Persistent cache of summaries and ambiguous names.
///
/// Summaries are keyed by the resolved article title. A second index maps a
/// normalized player name to that title, and ambiguous names are remembered so
/// reopening a profile does not repeat a failed search.
#[derive(Clone, Debug, Default)]
pub struct Cache {
    summaries: BTreeMap<String, Summary>,
    queries: BTreeMap<String, String>,
    /// Normalized name -> search URL, so every spelling of an ambiguous name
    /// opens the same search.
    ambiguous: BTreeMap<String, String>,
}

impl Cache {
    pub fn get(&self, name: &str) -> Option<Lookup> {
        let key = query_key(name)?;
        if let Some(title) = self.queries.get(&key)
            && let Some(summary) = self.summaries.get(title)
            && is_confident(summary)
            && summary_matches_name(name, summary)
        {
            return Some(Lookup::Summary(summary.clone()));
        }
        if let Some(summary) = self.summaries.values().find(|summary| {
            query_key(&summary.title).as_deref() == Some(key.as_str())
                && is_confident(summary)
                && summary_matches_name(name, summary)
        }) {
            return Some(Lookup::Summary(summary.clone()));
        }
        self.ambiguous
            .get(&key)
            .map(|search_url| Lookup::Ambiguous {
                search_url: search_url.clone(),
            })
    }

    pub fn remove(&mut self, name: &str) {
        if let Some(key) = query_key(name) {
            self.queries.remove(&key);
            self.ambiguous.remove(&key);
        }
    }

    pub fn insert(&mut self, name: &str, lookup: &Lookup) {
        let Some(key) = query_key(name) else {
            return;
        };
        match lookup {
            Lookup::Summary(summary) => {
                self.summaries
                    .insert(summary.title.clone(), summary.clone());
                self.queries.insert(key.clone(), summary.title.clone());
                self.ambiguous.remove(&key);
            }
            Lookup::Ambiguous { search_url } => {
                self.queries.remove(&key);
                self.ambiguous.insert(key, search_url.clone());
            }
        }
        self.trim();
    }

    /// Cached summaries paired with the normalized player key they answer.
    /// Used to restore titles in a player list without another network call.
    pub fn title_entries(&self) -> impl Iterator<Item = (String, &Summary)> {
        let queried = self.queries.iter().filter_map(|(key, title)| {
            self.summaries
                .get(title)
                .map(|summary| (key.clone(), summary))
        });
        let titled = self.summaries.iter().filter_map(|(title, summary)| {
            crate::players::player_key(title).map(|key| (key, summary))
        });
        queried.chain(titled)
    }

    pub fn len(&self) -> usize {
        self.summaries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.summaries.is_empty() && self.ambiguous.is_empty()
    }

    pub fn from_json(text: &str) -> Self {
        let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let version = value.get("version").and_then(Value::as_u64).unwrap_or(0);
        let summaries: BTreeMap<String, Summary> = value
            .get("summaries")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(title, value)| Some((title.clone(), Summary::from_value(value)?)))
                    .collect()
            })
            .unwrap_or_default();
        let queries: BTreeMap<String, String> = value
            .get("queries")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| {
                        let title = value.as_str()?;
                        let summary = summaries.get(title)?;
                        if is_confident(summary) && title_matches_query_key(key, &summary.title) {
                            Some((key.clone(), title.to_string()))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Discard stale `ambiguous` entries from older cache versions so
        // previously missed names (e.g. umlaut/digraph spellings) are retried.
        let ambiguous: BTreeMap<String, String> = if version == u64::from(CACHE_VERSION) {
            value
                .get("ambiguous")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        Self {
            summaries,
            queries,
            ambiguous,
        }
    }

    pub fn to_json(&self) -> String {
        let summaries = self
            .summaries
            .iter()
            .map(|(title, summary)| (title.clone(), summary.to_value()))
            .collect::<serde_json::Map<String, Value>>();
        let queries = self
            .queries
            .iter()
            .map(|(key, title)| (key.clone(), Value::String(title.clone())))
            .collect::<serde_json::Map<String, Value>>();
        let ambiguous = self
            .ambiguous
            .iter()
            .map(|(key, url)| (key.clone(), Value::String(url.clone())))
            .collect::<serde_json::Map<String, Value>>();
        json!({
            "version": CACHE_VERSION,
            "summaries": Value::Object(summaries),
            "queries": Value::Object(queries),
            "ambiguous": Value::Object(ambiguous),
        })
        .to_string()
    }

    pub fn load() -> Self {
        cache_path().map_or_else(Self::default, |path| Self::load_from(&path))
    }

    pub fn save(&self) -> Result<()> {
        let path = cache_path().context("no XDG cache or HOME directory is available")?;
        self.save_to(&path)
    }

    fn load_from(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!("could not read {}: {err:#}", path.display());
                Self::default()
            }
        }
    }

    fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(path, self.to_json()).with_context(|| format!("writing {}", path.display()))
    }

    fn trim(&mut self) {
        while self.summaries.len() > MAX_SUMMARIES {
            let Some(title) = self.summaries.keys().next().cloned() else {
                break;
            };
            self.summaries.remove(&title);
            self.queries.retain(|_, mapped| mapped != &title);
        }
        while self.ambiguous.len() > MAX_AMBIGUOUS {
            let Some(key) = self.ambiguous.keys().next().cloned() else {
                break;
            };
            self.ambiguous.remove(&key);
        }
    }
}

fn wiki_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .user_agent(USER_AGENT)
            .timeout_connect(Duration::from_secs(6))
            .timeout(Duration::from_secs(12))
            .build()
    })
}

/// Look up a player name on Wikipedia. Network errors are returned as
/// human-readable strings so the profile can offer a retry and a search link.
pub fn lookup(name: &str) -> std::result::Result<Lookup, String> {
    let agent = wiki_agent();
    lookup_with(name, |url| get_json(agent, url))
}

/// Look up a player on Wikipedia/Wikidata and, when a FIDE ID is known from
/// Wikidata (`P1440`) or the database (`known_fide_id`), fetch live FIDE
/// classical, rapid, and blitz ratings.
pub fn lookup_with_fide(
    name: &str,
    known_fide_id: Option<&str>,
) -> std::result::Result<Lookup, String> {
    let mut result = lookup(name);
    if let Ok(Lookup::Summary(summary)) = &mut result {
        enrich_fide_ratings(summary, known_fide_id);
    }
    result
}

/// Populate `summary.fide_standard`/`rapid`/`blitz` and missing FIDE title via
/// FIDE when a FIDE ID is known. Returns `true` if any FIDE field was populated.
pub fn enrich_fide_ratings(summary: &mut Summary, known_fide_id: Option<&str>) -> bool {
    let Some(id) = summary
        .fide_id
        .as_deref()
        .and_then(|s| crate::fide::from_tag(Some(s)))
        .or_else(|| crate::fide::from_tag(known_fide_id))
    else {
        return false;
    };
    let Some(ratings) = crate::fide::fetch_ratings(id) else {
        return false;
    };
    let mut updated = false;
    if let Some(title) = ratings.title
        && !summary
            .title_history
            .iter()
            .any(|award| award.title == title)
    {
        summary.title_history.push(TitleAward { title, year: None });
        sort_title_history(&mut summary.title_history);
        updated = true;
    }
    if ratings.standard.is_some() {
        summary.fide_standard = ratings.standard;
        updated = true;
    }
    if ratings.rapid.is_some() {
        summary.fide_rapid = ratings.rapid;
        updated = true;
    }
    if ratings.blitz.is_some() {
        summary.fide_blitz = ratings.blitz;
        updated = true;
    }
    updated
}

fn lookup_with(
    name: &str,
    mut fetch: impl FnMut(&str) -> std::result::Result<Value, FetchError>,
) -> std::result::Result<Lookup, String> {
    let trimmed = name.trim();
    let primary = lookup_single(trimmed, &mut fetch);
    if matches!(primary, Ok(Lookup::Summary(_)) | Err(_)) {
        return primary;
    }
    // Fallback: the database may spell a German umlaut as an ASCII digraph
    // (`Bluebaum` vs `Blübaum`). Retry with umlauts restored.
    for variant in umlaut_variants(trimmed) {
        if variant.eq_ignore_ascii_case(trimmed) {
            continue;
        }
        match lookup_single(&variant, &mut fetch) {
            Ok(Lookup::Summary(summary)) => return Ok(Lookup::Summary(summary)),
            Ok(Lookup::Ambiguous { .. }) => {}
            Err(err) => return Err(err),
        }
    }
    // Fallback for patronymics/middle names (`Kortschnoj, Viktor Lvovich`):
    // search Wikipedia with `Surname, FirstGiven` while still requiring
    // `title_matches_name` on the original full query.
    if let Some((surname, given)) = trimmed.split_once(',') {
        let given_words: Vec<&str> = given.split_whitespace().collect();
        if given_words.len() >= 2 && !surname.trim().is_empty() {
            let shortened = format!("{}, {}", surname.trim(), given_words[0]);
            if !shortened.eq_ignore_ascii_case(trimmed) {
                match lookup_single(&shortened, &mut fetch) {
                    Ok(Lookup::Summary(summary)) if summary_matches_name(trimmed, &summary) => {
                        return Ok(Lookup::Summary(summary));
                    }
                    Ok(_) => {}
                    Err(err) => return Err(err),
                }
            }
        }
    }
    // Fallback for generational suffixes (`Moroni, Luca Jr` -> `Luca Moroni`):
    // retry without the suffix, requiring `summary_matches_name` to confirm
    // either the database comma form or an explicit generational token in the extract.
    if let Some(stripped) = strip_generational_suffix(trimmed) {
        match lookup_single(&stripped, &mut fetch) {
            Ok(Lookup::Summary(summary)) if summary_matches_name(trimmed, &summary) => {
                return Ok(Lookup::Summary(summary));
            }
            Ok(_) => {}
            Err(err) => return Err(err),
        }
    }
    // Fallback for known World Champion name aliases (e.g. `Steinitz, William` -> `Wilhelm Steinitz`).
    if let Some(champ) = crate::champions::champion(trimmed, crate::settings::ChampionLineage::Fide)
        && !champ.name.eq_ignore_ascii_case(trimmed)
    {
        match lookup_single(champ.name, &mut fetch) {
            Ok(Lookup::Summary(summary)) => return Ok(Lookup::Summary(summary)),
            Ok(_) => {}
            Err(err) => return Err(err),
        }
    }
    primary
}

fn lookup_single(
    name: &str,
    mut fetch: &mut impl FnMut(&str) -> std::result::Result<Value, FetchError>,
) -> std::result::Result<Lookup, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("The player name is empty.".to_string());
    }
    let fallback_search = search_url(name);
    let mut direct_error = None;
    let mut direct_title = None;

    match fetch(&summary_url(name)) {
        Ok(value) => {
            if let Some(mut summary) = Summary::from_value(&value) {
                direct_title = Some(summary.title.clone());
                if is_confident(&summary) && summary_matches_name(name, &summary) {
                    if let Some(qid) = wikibase_item(&value) {
                        enrich_with_wikidata(&mut summary, &qid, &mut fetch);
                    }
                    return Ok(Lookup::Summary(summary));
                }
            }
        }
        Err(FetchError::NotFound) => {}
        Err(FetchError::Other(err)) => direct_error = Some(err),
    }

    let search = match fetch(&search_api_url(name)) {
        Ok(value) => value,
        Err(FetchError::NotFound) => {
            return match direct_error {
                Some(err) => Err(err),
                None => Ok(Lookup::Ambiguous {
                    search_url: fallback_search,
                }),
            };
        }
        Err(FetchError::Other(err)) => return Err(err),
    };

    let mut candidates = Vec::new();
    if let Some(pages) = search.get("pages").and_then(Value::as_array) {
        for page in pages.iter().take(5) {
            let Some(title) = page
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| page.get("key").and_then(Value::as_str))
            else {
                continue;
            };
            if !title_matches_name(name, title)
                && !strip_generational_suffix(name)
                    .is_some_and(|stripped| title_matches_name(&stripped, title))
            {
                continue;
            }
            let preferred = page
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|description| has_word(description, "chess"));
            candidates.push((preferred, title.to_string()));
        }
    }
    candidates.sort_by_key(|(preferred, _)| !*preferred);

    let mut seen = HashSet::new();
    if let Some(title) = direct_title {
        seen.insert(title);
    }
    let mut last_error = direct_error;
    let mut parsed_any = false;
    for (_, title) in candidates.into_iter().take(3) {
        if !seen.insert(title.clone()) {
            continue;
        }
        match fetch(&summary_url_for_title(&title)) {
            Ok(value) => {
                if let Some(mut summary) = Summary::from_value(&value) {
                    parsed_any = true;
                    if is_confident(&summary) && summary_matches_name(name, &summary) {
                        if let Some(qid) = wikibase_item(&value) {
                            enrich_with_wikidata(&mut summary, &qid, &mut fetch);
                        }
                        return Ok(Lookup::Summary(summary));
                    }
                }
            }
            Err(FetchError::NotFound) => {}
            Err(FetchError::Other(err)) => last_error = Some(err),
        }
    }

    if !parsed_any && let Some(err) = last_error {
        return Err(err);
    }
    Ok(Lookup::Ambiguous {
        search_url: fallback_search,
    })
}

enum FetchError {
    NotFound,
    Other(String),
}

fn retry_delay(attempt: usize, retry_after: Option<&str>) -> Duration {
    if let Some(raw) = retry_after
        && let Ok(secs) = raw.trim().parse::<u64>()
    {
        return Duration::from_secs(secs.clamp(1, 10));
    }
    const BACKOFF_MS: [u64; 3] = [1_200, 2_500, 5_000];
    Duration::from_millis(BACKOFF_MS.get(attempt).copied().unwrap_or(5_000))
}

fn get_json(agent: &ureq::Agent, url: &str) -> std::result::Result<Value, FetchError> {
    let mut attempt = 0usize;
    let response = loop {
        match agent.get(url).call() {
            Ok(response) => break response,
            Err(ureq::Error::Status(404, _)) => return Err(FetchError::NotFound),
            Err(ureq::Error::Status(code @ (429 | 503), response)) => {
                if attempt < 3 {
                    let delay = retry_delay(attempt, response.header("retry-after"));
                    attempt += 1;
                    std::thread::sleep(delay);
                    continue;
                }
                return Err(FetchError::Other(format!(
                    "Wikipedia returned HTTP {code}."
                )));
            }
            Err(ureq::Error::Status(code, _)) => {
                return Err(FetchError::Other(format!(
                    "Wikipedia returned HTTP {code}."
                )));
            }
            Err(err @ ureq::Error::Transport(_)) => {
                if attempt < 3 {
                    let delay = retry_delay(attempt, None);
                    attempt += 1;
                    std::thread::sleep(delay);
                    continue;
                }
                return Err(FetchError::Other(format!(
                    "Wikipedia request failed: {err}"
                )));
            }
        }
    };
    let text = response
        .into_string()
        .map_err(|err| FetchError::Other(format!("reading the Wikipedia response: {err}")))?;
    serde_json::from_str(&text)
        .map_err(|err| FetchError::Other(format!("Wikipedia returned invalid JSON: {err}")))
}

fn is_confident(summary: &Summary) -> bool {
    let description = summary.description.to_lowercase();
    let extract = summary.extract.to_lowercase();
    let chess = has_word(&description, "chess") || has_word(&extract, "chess");
    if !chess {
        return false;
    }

    // Non-person entities (tournaments, leagues, places, media, clubs).
    let non_person = [
        "championship",
        "competition",
        "tournament",
        "organization",
        "club",
        "federation",
        "association",
        "league",
        "stadium",
        "event",
        "memorial",
        "award",
        "museum",
        "village",
        "town",
        "city",
        "river",
        "mountain",
        "company",
        "album",
        "song",
        "film",
        "movie",
        "disambiguation",
        "surname",
        "given name",
    ];
    if non_person.iter().any(|word| has_word(&description, word)) {
        return false;
    }

    // Non-chess professions (footballer, actor, politician, etc.) that might
    // mention chess in passing or share a name with a chess player.
    let non_chess_professions = [
        "football",
        "footballer",
        "soccer",
        "basketball",
        "baseball",
        "cricket",
        "cricketer",
        "hockey",
        "rugby",
        "tennis",
        "golf",
        "golfer",
        "boxer",
        "boxing",
        "swimmer",
        "swimming",
        "athletics",
        "politician",
        "actor",
        "actress",
        "singer",
        "musician",
        "band",
        "physician",
        "lawyer",
        "judge",
        "general",
        "admiral",
    ];
    let has_non_chess_profession = non_chess_professions
        .iter()
        .any(|word| has_word(&description, word));
    if has_non_chess_profession && !has_word(&description, "chess") {
        return false;
    }

    // Direct chess description: description identifies a chess role.
    let chess_description = (has_word(&description, "chess")
        && [
            "player",
            "grandmaster",
            "master",
            "champion",
            "prodigy",
            "writer",
            "author",
            "theorist",
            "composer",
            "official",
            "arbiter",
            "instructor",
            "coach",
            "figure",
        ]
        .iter()
        .any(|word| has_word(&description, word)))
        || has_word(&description, "grandmaster");
    if chess_description {
        return true;
    }

    // If description is generic or brief, the lead extract must clearly
    // establish the subject as a chess player/grandmaster.
    if has_non_chess_profession {
        return false;
    }

    [
        "chess player",
        "chess grandmaster",
        "chess master",
        "chess prodigy",
        "world chess champion",
        "world chess championship",
        "international master of chess",
        "chess international master",
        "fide master",
        "grandmaster of chess",
        "chess woman grandmaster",
        "woman grandmaster",
        "chess author",
        "chess composer",
        "chess problemist",
    ]
    .iter()
    .any(|phrase| extract.contains(phrase))
}

fn has_word(text: &str, word: &str) -> bool {
    text.split(|ch: char| !ch.is_alphanumeric())
        .any(|token| token == word)
}

/// Fold ASCII transliteration digraphs and common Slavic/German romanizations
/// so PGN names match Wikipedia titles (`Bluebaum` -> `Blübaum`,
/// `Ivanchuk, Vassily` -> `Vasyl Ivanchuk`, `Kortschnoj, Viktor Lvovich` ->
/// `Viktor Korchnoi`, `Indjic` -> `Inđić`, `Aleksey` -> `Alexey`).
fn fold_transliteration_digraphs(token: &str) -> String {
    let mut folded = token
        .replace("schtsch", "shch")
        .replace("tsch", "ch")
        .replace("sch", "sh")
        .replace("kh", "ch")
        .replace("ue", "u")
        .replace("ae", "a")
        .replace("oe", "o")
        .replace("ss", "s")
        .replace("dj", "d")
        .replace("ks", "x")
        .replace("ja", "ia")
        .replace("je", "ie")
        .replace("ji", "i")
        .replace("jo", "io")
        .replace("ju", "iu")
        .replace("aj", "ai")
        .replace("ej", "ei")
        .replace("ij", "i")
        .replace("oj", "oi")
        .replace("uj", "ui")
        .replace('y', "i")
        .replace("ii", "i");
    if folded.len() > 3 && (folded.ends_with("ow") || folded.ends_with("ew")) {
        folded.pop();
        folded.push('v');
    }
    match folded.as_str() {
        "vasil" | "vasili" | "vasiliy" | "vasilii" => "vasili".to_string(),
        "victor" => "viktor".to_string(),
        "michail" | "mihail" => "michail".to_string(),
        "wladimir" => "vladimir".to_string(),
        "oleksandr" | "olexandr" => "aleksandr".to_string(),
        "ievgeni" | "jevgeni" => "evgeni".to_string(),
        "william" | "wilhelm" => "wilhelm".to_string(),
        _ => folded,
    }
}

fn name_tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphabetic())
        .filter(|token| !token.is_empty())
        .map(|token| fold_transliteration_digraphs(&deunicode::deunicode(token).to_lowercase()))
        .collect()
}

/// Alternative spellings with German umlauts restored, used as fallback
/// Wikipedia searches when the ASCII-spelled database name finds no match
/// (e.g. `Bluebaum, Matthias` → `Blübaum, Matthias`).
fn umlaut_variants(name: &str) -> Vec<String> {
    const DIGRAPHS: [(&str, char, char); 4] = [
        ("ue", 'ü', 'Ü'),
        ("ae", 'ä', 'Ä'),
        ("oe", 'ö', 'Ö'),
        ("ss", 'ß', 'ß'),
    ];
    let mut matches: Vec<(usize, char)> = Vec::new();
    let chars: Vec<(usize, char)> = name.char_indices().collect();
    let mut i = 0usize;
    while i + 1 < chars.len() {
        let (idx0, c0) = chars[i];
        let (idx1, c1) = chars[i + 1];
        if idx1 == idx0 + c0.len_utf8() && c0.is_ascii() && c1.is_ascii() {
            let pair = [c0.to_ascii_lowercase(), c1.to_ascii_lowercase()];
            let mut matched = None;
            for (digraph, lower_rep, upper_rep) in DIGRAPHS {
                let bytes = digraph.as_bytes();
                if pair[0] as u8 == bytes[0] && pair[1] as u8 == bytes[1] {
                    if digraph == "ss" {
                        if c0.is_ascii_lowercase() && c1.is_ascii_lowercase() {
                            matched = Some(lower_rep);
                        }
                    } else if c0.is_ascii_uppercase() && c1.is_ascii_lowercase() {
                        matched = Some(upper_rep);
                    } else if c0.is_ascii_lowercase() && c1.is_ascii_lowercase() {
                        matched = Some(lower_rep);
                    }
                    break;
                }
            }
            if let Some(rep) = matched {
                matches.push((idx0, rep));
                i += 2;
                continue;
            }
        }
        i += 1;
    }

    let mut variants = Vec::new();
    if matches.len() > 1 {
        let mut combined = String::with_capacity(name.len());
        let mut cursor = 0usize;
        for &(idx, rep) in &matches {
            combined.push_str(&name[cursor..idx]);
            combined.push(rep);
            cursor = idx + 2;
        }
        combined.push_str(&name[cursor..]);
        variants.push(combined);
    }
    for &(idx, rep) in &matches {
        let mut candidate = String::with_capacity(name.len());
        candidate.push_str(&name[..idx]);
        candidate.push(rep);
        candidate.push_str(&name[idx + 2..]);
        if !variants.contains(&candidate) {
            variants.push(candidate);
        }
    }
    variants.truncate(8);
    variants
}

const GENERATIONAL_SUFFIXES: &[&str] = &["jr", "sr", "ii", "iii", "iv"];

/// Split a PGN name into primary given, primary surname, and ordered tokens,
/// putting `Surname, Given` back into given-name order.
fn split_primary_parts(name: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let name = name.trim();
    if let Some((surname, given)) = name.split_once(',') {
        let given_tokens = name_tokens(given);
        let surname_tokens = name_tokens(surname);
        let primary_given = given_tokens.first().cloned();
        let primary_surname = surname_tokens.first().cloned();
        let mut all = given_tokens;
        all.extend(surname_tokens);
        (primary_given, primary_surname, all)
    } else {
        let all = name_tokens(name);
        let primary_given = all.first().cloned();
        let primary_surname = all.last().cloned();
        (primary_given, primary_surname, all)
    }
}

/// Order-independent match used when validating cached `player_key -> title`
/// entries whose tokens are sorted alphabetically.
fn title_matches_query_key(key: &str, title: &str) -> bool {
    let query = name_tokens(key);
    let title = title.split('(').next().unwrap_or(title);
    let title_tokens = name_tokens(title);
    match_tokens(&query, &title_tokens, None, None)
}

fn match_tokens(
    query: &[String],
    title_tokens: &[String],
    primary_given: Option<&String>,
    primary_surname: Option<&String>,
) -> bool {
    if query.is_empty() || title_tokens.is_empty() {
        return false;
    }
    if query.len() < 2 && title_tokens.len() > 1 {
        return false;
    }
    if query.iter().any(|token| {
        GENERATIONAL_SUFFIXES.contains(&token.as_str())
            && !title_tokens.iter().any(|candidate| candidate == token)
    }) {
        return false;
    }

    let query_multi_count = query
        .iter()
        .filter(|token| token.chars().count() > 1)
        .count();
    let title_multi_count = title_tokens
        .iter()
        .filter(|token| token.chars().count() > 1)
        .count();

    let mut used = vec![false; title_tokens.len()];
    let mut forward_ok = query_multi_count > 0;
    for token in query {
        let is_initial = token.chars().count() == 1;
        if is_initial && query_multi_count >= 2 {
            continue;
        }
        let matched = title_tokens
            .iter()
            .enumerate()
            .find_map(|(index, candidate)| {
                if used[index] {
                    return None;
                }
                let ok = if is_initial {
                    candidate.starts_with(token.as_str())
                } else {
                    candidate == token
                };
                ok.then_some(index)
            });
        if let Some(index) = matched {
            used[index] = true;
        } else {
            forward_ok = false;
            break;
        }
    }
    if forward_ok {
        return true;
    }

    // Handle 3+ multi-letter query tokens (middle names, patronymics, compound
    // surnames) when Wikipedia uses the shorter canonical 2+ token name.
    if query_multi_count >= 3 && title_multi_count >= 2 {
        let mut query_used = vec![false; query.len()];
        for title_token in title_tokens {
            let Some(q_idx) = query.iter().enumerate().find_map(|(index, q_token)| {
                (!query_used[index] && q_token == title_token).then_some(index)
            }) else {
                return false;
            };
            query_used[q_idx] = true;
        }
        if let Some(given) = primary_given
            && !title_tokens.iter().any(|t| t == given)
        {
            return false;
        }
        if let Some(surname) = primary_surname
            && !title_tokens.iter().any(|t| t == surname)
        {
            return false;
        }
        return true;
    }

    false
}

/// True when an article title names the queried player.
///
/// Multi-letter query tokens must match distinct tokens in the article title
/// (or, for 3+ multi-letter tokens like middle names and compound surnames,
/// all title tokens must match distinct query tokens including the primary
/// given and surname tokens). Single-letter initials must match an unused
/// title token when fewer than two multi-letter tokens are present.
fn title_matches_name(name: &str, title: &str) -> bool {
    let (primary_given, primary_surname, query) = split_primary_parts(name);
    let title = title.split('(').next().unwrap_or(title);
    let title_tokens = name_tokens(title);
    let check_given = primary_given.as_ref();
    let check_surname = name
        .contains(',')
        .then_some(primary_surname.as_ref())
        .flatten();
    match_tokens(&query, &title_tokens, check_given, check_surname)
}

fn is_generational_suffix_word(word: &str) -> bool {
    let clean = word.trim_matches(|ch: char| !ch.is_alphanumeric());
    GENERATIONAL_SUFFIXES
        .iter()
        .any(|s| s.eq_ignore_ascii_case(clean))
}

fn strip_generational_suffix(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if let Some((last, first)) = trimmed.split_once(',') {
        let first_words: Vec<&str> = first
            .split_whitespace()
            .filter(|w| !is_generational_suffix_word(w))
            .collect();
        let last_words: Vec<&str> = last
            .split_whitespace()
            .filter(|w| !is_generational_suffix_word(w))
            .collect();
        let candidate = if first_words.is_empty() || last_words.is_empty() {
            return None;
        } else {
            format!("{}, {}", last_words.join(" "), first_words.join(" "))
        };
        return (candidate != trimmed).then_some(candidate);
    }
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() >= 3 && words.last().is_some_and(|w| is_generational_suffix_word(w)) {
        return Some(words[..words.len() - 1].join(" "));
    }
    None
}

fn summary_matches_name(name: &str, summary: &Summary) -> bool {
    if title_matches_name(name, &summary.title) {
        return true;
    }
    let Some(stripped) = strip_generational_suffix(name) else {
        return false;
    };
    if !title_matches_name(&stripped, &summary.title) {
        return false;
    }
    let lead: String = summary
        .extract
        .chars()
        .take(96)
        .collect::<String>()
        .to_ascii_lowercase();
    let extract_has_suffix = name
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| is_generational_suffix_word(w))
        .any(|s| has_word(&lead, &s.to_ascii_lowercase()));
    extract_has_suffix || name.contains(',')
}

/// Normalized identity used for the cache's name index.
pub fn query_key(name: &str) -> Option<String> {
    crate::players::player_key(name)
}

/// Wikipedia search page for a name that could not be matched confidently.
pub fn search_url(name: &str) -> String {
    format!(
        "{SEARCH_PAGE}?search={}",
        encode_component(&search_name(name), "%20")
    )
}

fn search_api_url(name: &str) -> String {
    format!(
        "{SEARCH_ENDPOINT}?q={}&limit=5",
        encode_component(&search_name(name), "%20")
    )
}

fn clean_search_words(part: &str) -> Vec<String> {
    part.split_whitespace()
        .filter_map(|word| {
            let trimmed = word.trim_end_matches(|ch: char| ch.is_ascii_digit());
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect()
}

/// PGN tags often use `Last, First` (and ChessBase sometimes appends a numeric
/// disambiguator like `Vladimir1`); Wikipedia search and direct summary URLs
/// expect `First Last` without trailing digits.
fn search_name(name: &str) -> String {
    let name = name.trim();
    if let Some((last, first)) = name.split_once(',') {
        let last_words = clean_search_words(last.trim());
        let first_all = clean_search_words(first.trim());
        let mut given_words = Vec::new();
        let mut suffix_words = Vec::new();
        for w in first_all {
            if is_generational_suffix_word(&w) {
                suffix_words.push(w);
            } else {
                given_words.push(w);
            }
        }
        if !last_words.is_empty() && !given_words.is_empty() && !first.contains(',') {
            let mut ordered = Vec::new();
            ordered.extend(given_words);
            ordered.extend(last_words);
            ordered.extend(suffix_words);
            return ordered.join(" ");
        }
    }
    let words = clean_search_words(name);
    if words.is_empty() {
        name.to_string()
    } else {
        words.join(" ")
    }
}

fn summary_url(name: &str) -> String {
    summary_url_for_title(&search_name(name))
}

fn summary_url_for_title(title: &str) -> String {
    format!("{SUMMARY_ENDPOINT}{}", encode_component(title.trim(), "_"))
}

fn article_url(title: &str) -> String {
    format!(
        "https://en.wikipedia.org/wiki/{}",
        encode_component(title, "_")
    )
}

fn encode_component(value: &str, space: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push_str(space),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn clean_text(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    decode_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find(';')
            && let Some(ch) = entity_char(&after[..end])
        {
            out.push(ch);
            rest = &after[end + 1..];
            continue;
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn entity_char(entity: &str) -> Option<char> {
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
    }
    if let Some(decimal) = entity.strip_prefix('#') {
        return decimal.parse::<u32>().ok().and_then(char::from_u32);
    }
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "aacute" => 'á',
        "Aacute" => 'Á',
        "agrave" => 'à',
        "Agrave" => 'À',
        "aring" => 'å',
        "Aring" => 'Å',
        "auml" => 'ä',
        "Auml" => 'Ä',
        "aelig" => 'æ',
        "AElig" => 'Æ',
        "ccedil" => 'ç',
        "Ccedil" => 'Ç',
        "eacute" => 'é',
        "Eacute" => 'É',
        "egrave" => 'è',
        "Egrave" => 'È',
        "iacute" => 'í',
        "Iacute" => 'Í',
        "ntilde" => 'ñ',
        "Ntilde" => 'Ñ',
        "oacute" => 'ó',
        "Oacute" => 'Ó',
        "oelig" => 'œ',
        "OElig" => 'Œ',
        "oslash" => 'ø',
        "Oslash" => 'Ø',
        "ouml" => 'ö',
        "Ouml" => 'Ö',
        "szlig" => 'ß',
        "uacute" => 'ú',
        "Uacute" => 'Ú',
        "uuml" => 'ü',
        "Uuml" => 'Ü',
        _ => return None,
    })
}

fn cache_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        Some(
            std::env::temp_dir()
                .join(format!("brett-test-wiki-{}", std::process::id()))
                .join("wikipedia.json"),
        )
    }
    #[cfg(not(test))]
    {
        if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
            return Some(PathBuf::from(dir).join("brett").join("wikipedia.json"));
        }
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".cache")
                .join("brett")
                .join("wikipedia.json")
        })
    }
}

fn title_history_cache_path() -> Option<PathBuf> {
    let mut path = cache_path()?;
    path.set_file_name("wikipedia_titles.json");
    Some(path)
}

/// Load the compact, unbounded title-history cache. Article summaries remain
/// capped separately; a batch can enrich more players than the article cache
/// is intended to retain.
pub fn load_title_history_cache() -> BTreeMap<String, Vec<TitleAward>> {
    let Some(path) = title_history_cache_path() else {
        return BTreeMap::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    title_history_cache_from_json(&text)
}

/// Parse stored title histories, skipping malformed entries.
pub fn title_history_cache_from_json(text: &str) -> BTreeMap<String, Vec<TitleAward>> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return BTreeMap::new();
    };
    let Some(histories) = value.get("histories").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    histories
        .iter()
        .filter_map(|(key, value)| {
            let mut awards = parse_title_history_entries(Some(value));
            sort_title_history(&mut awards);
            (!key.is_empty() && !awards.is_empty()).then(|| (key.clone(), awards))
        })
        .collect()
}

/// Serialize player-keyed title histories deterministically.
pub fn title_history_cache_to_json(histories: &BTreeMap<String, Vec<TitleAward>>) -> String {
    let entries: serde_json::Map<String, Value> = histories
        .iter()
        .map(|(key, awards)| {
            (
                key.clone(),
                Value::Array(
                    awards
                        .iter()
                        .map(|award| {
                            json!({
                                "title": award.title.short(),
                                "year": award.year,
                            })
                        })
                        .collect(),
                ),
            )
        })
        .collect();
    json!({ "version": 1, "histories": entries }).to_string()
}

fn merge_title_history_caches(
    mut existing: BTreeMap<String, Vec<TitleAward>>,
    updates: &BTreeMap<String, Vec<TitleAward>>,
) -> BTreeMap<String, Vec<TitleAward>> {
    for (key, awards) in updates {
        let entry = existing.entry(key.clone()).or_default();
        entry.extend(awards.iter().copied());
        sort_title_history(entry);
    }
    existing
}

/// Persist the compact title cache atomically after batch enrichment.
pub fn save_title_history_cache(histories: &BTreeMap<String, Vec<TitleAward>>) -> Result<()> {
    let _guard = TITLE_HISTORY_SAVE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = title_history_cache_path().context("no XDG cache or HOME directory is available")?;
    // Batch cancellation/restart can queue two background saves. Merge under
    // the lock so a late older snapshot cannot discard entries from a newer
    // batch; title awards are additive and de-duplicate by title/year.
    let merged = merge_title_history_caches(load_title_history_cache(), histories);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("creating Wikipedia title cache folder {}", parent.display())
        })?;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), stamp));
    fs::write(&temporary, title_history_cache_to_json(&merged))
        .with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_summary(title: &str, description: &str, extract: &str) -> Summary {
        Summary {
            title: title.to_string(),
            description: description.to_string(),
            extract: extract.to_string(),
            url: article_url(title),
            countries: countries_from_texts(description, extract),
            country_periods: country_periods_from_texts(description, extract),
            fide_id: None,
            birth_year: None,
            death_year: None,
            peak_elo: None,
            latest_elo: None,
            latest_elo_date: None,
            fide_standard: None,
            fide_rapid: None,
            fide_blitz: None,
            title_history: title_awards_from_texts(description, extract),
        }
    }

    fn summary(title: &str, description: &str, extract: &str) -> Summary {
        make_summary(title, description, extract)
    }

    fn summary_json(title: &str, description: &str, extract: &str) -> Value {
        json!({
            "type": "standard",
            "title": title,
            "description": description,
            "extract": extract,
            "content_urls": {
                "desktop": {
                    "page": article_url(title),
                }
            }
        })
    }

    #[test]
    fn summary_parser_cleans_html_and_entities() {
        let value = summary_json(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus &Oslash;en Carlsen is a <b>Norwegian</b> chess grandmaster &amp; former World Chess Champion.",
        );
        let summary = Summary::from_value(&value).unwrap();
        assert_eq!(
            summary.extract,
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster & former World Chess Champion."
        );
        assert_eq!(summary.url, "https://en.wikipedia.org/wiki/Magnus_Carlsen");
        assert!(is_confident(&summary));
    }

    #[test]
    fn disambiguation_and_non_player_articles_are_not_confident() {
        let mut disambiguation = summary_json("Carlsen", "", "Carlsen may refer to:");
        disambiguation["type"] = Value::String("disambiguation".to_string());
        assert!(Summary::from_value(&disambiguation).is_none());

        let footballer = summary(
            "Magnus Carlsen",
            "Norwegian footballer",
            "He plays football.",
        );
        assert!(!is_confident(&footballer));

        let tournament = summary(
            "World Chess Championship",
            "Chess competition",
            "The World Chess Championship is a chess competition to determine the World Champion.",
        );
        assert!(!is_confident(&tournament));

        let basketball_player = summary(
            "John Smith",
            "American basketball player",
            "John Smith is an American basketball player who also enjoys playing chess.",
        );
        assert!(
            !is_confident(&basketball_player),
            "non-chess athlete with passing chess mention must not be accepted"
        );

        let musician = summary(
            "Bo Diddley",
            "American singer and musician",
            "Recorded on Chess Records in Chicago.",
        );
        assert!(
            !is_confident(&musician),
            "musician with Chess Records mention must not be accepted"
        );

        let player = summary(
            "Judit Polgár",
            "Hungarian chess grandmaster",
            "Judit Polgár is a Hungarian chess grandmaster.",
        );
        assert!(is_confident(&player));
    }

    #[test]
    fn lookup_falls_back_to_search_and_returns_a_confident_summary() {
        let result = lookup_with("Carlsen, Magnus", |url| {
            if url.contains("/summary/Carlsen%2C_Magnus") {
                return Err(FetchError::NotFound);
            }
            if url.contains("/search/page") {
                return Ok(json!({
                    "pages": [{
                        "title": "Magnus Carlsen",
                        "description": "Norwegian chess grandmaster",
                    }]
                }));
            }
            if url.contains("/summary/Magnus_Carlsen") {
                return Ok(summary_json(
                    "Magnus Carlsen",
                    "Norwegian chess grandmaster",
                    "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
                ));
            }
            Err(FetchError::NotFound)
        })
        .unwrap();
        match result {
            Lookup::Summary(summary) => assert_eq!(summary.title, "Magnus Carlsen"),
            Lookup::Ambiguous { .. } => panic!("expected a confident summary"),
        }
    }

    #[test]
    fn abbreviated_names_match_their_expanded_article_titles() {
        assert!(title_matches_name("Bacrot, E", "Étienne Bacrot"));
        assert!(title_matches_name("Svane, R", "Rasmus Svane"));
        assert!(title_matches_name("Buhmann,R", "Rainer Buhmann"));
        assert!(title_matches_name("Carlsen, Magnus", "Magnus Carlsen"));
        assert!(!title_matches_name("Bacrot, E", "Magnus Carlsen"));
        assert!(!title_matches_name("Bacrot, E", "World Chess Championship"));
    }

    #[test]
    fn german_umlaut_digraph_names_match_wikipedia_titles() {
        assert!(title_matches_name("Bluebaum, Matthias", "Matthias Blübaum"));
        assert!(title_matches_name("Mueller, Johannes", "Johannes Müller"));
        assert!(title_matches_name("Grosse, Franz", "Franz Große"));
        assert!(umlaut_variants("Bluebaum, Matthias").contains(&"Blübaum, Matthias".to_string()));
    }

    #[test]
    fn lookup_retries_umlaut_variant_when_ascii_name_fails() {
        let result = lookup_with("Bluebaum, Matthias", |url| {
            // The ASCII spelling 404s consistently.
            if url.contains("Bluebaum") {
                return Err(FetchError::NotFound);
            }
            // The umlaut-restored spelling resolves.
            if url.contains("Bl") && url.contains("baum") {
                return Ok(summary_json(
                    "Matthias Blübaum",
                    "German chess grandmaster",
                    "Matthias Blübaum is a German chess grandmaster.",
                ));
            }
            Err(FetchError::NotFound)
        });
        match result {
            Ok(Lookup::Summary(summary)) => assert_eq!(summary.title, "Matthias Blübaum"),
            other => panic!("expected the umlaut variant to resolve, got {other:?}"),
        }
    }

    #[test]
    fn lookup_skips_a_confident_chess_player_with_a_different_name() {
        let result = lookup_with("Bacrot, E", |url| {
            if url.contains("/summary/Bacrot%2C_E") {
                return Err(FetchError::NotFound);
            }
            if url.contains("/search/page") {
                return Ok(json!({
                    "pages": [
                        {
                            "title": "Magnus Carlsen",
                            "description": "Norwegian chess grandmaster",
                        },
                        {
                            "title": "Étienne Bacrot",
                            "description": "French chess grandmaster",
                        }
                    ]
                }));
            }
            if url.contains("tienne_Bacrot") {
                return Ok(summary_json(
                    "Étienne Bacrot",
                    "French chess grandmaster",
                    "Étienne Bacrot is a French chess grandmaster.",
                ));
            }
            if url.contains("/summary/Magnus_Carlsen") {
                return Ok(summary_json(
                    "Magnus Carlsen",
                    "Norwegian chess grandmaster",
                    "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
                ));
            }
            Err(FetchError::NotFound)
        })
        .unwrap();
        match result {
            Lookup::Summary(summary) => assert_eq!(summary.title, "Étienne Bacrot"),
            Lookup::Ambiguous { .. } => panic!("expected the matching player"),
        }
    }

    #[test]
    fn cache_ignores_a_saved_summary_for_a_different_player() {
        let summary = summary_json(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
        );
        let cache = Cache::from_json(
            &json!({
                "summaries": { "Magnus Carlsen": summary },
                "queries": { "bacrot e": "Magnus Carlsen" },
                "ambiguous": {},
            })
            .to_string(),
        );
        assert_eq!(
            cache.get("Bacrot, E"),
            None,
            "a stale mismatched mapping must be ignored"
        );
    }

    #[test]
    fn lookup_returns_a_search_link_for_an_ambiguous_name() {
        let result = lookup_with("Magnus Carlsen", |url| {
            if url.contains("/summary/Magnus_Carlsen") {
                return Ok(summary_json(
                    "Magnus Carlsen",
                    "Norwegian footballer",
                    "Magnus Carlsen is a Norwegian footballer.",
                ));
            }
            if url.contains("/search/page") {
                return Ok(json!({
                    "pages": [{
                        "title": "Magnus Carlsen",
                        "description": "Norwegian footballer",
                    }]
                }));
            }
            Err(FetchError::NotFound)
        })
        .unwrap();
        match result {
            Lookup::Ambiguous { search_url } => assert_eq!(
                search_url,
                "https://en.wikipedia.org/w/index.php?search=Magnus%20Carlsen"
            ),
            Lookup::Summary(_) => panic!("expected an ambiguous result"),
        }
    }

    #[test]
    fn lookup_reports_network_errors() {
        let err = lookup_with("Magnus Carlsen", |_| {
            Err(FetchError::Other("offline".to_string()))
        })
        .unwrap_err();
        assert!(err.contains("offline"));
    }

    #[test]
    fn cache_is_keyed_by_article_title_and_round_trips() {
        let summary = summary(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
        );
        let mut cache = Cache::default();
        cache.insert("Carlsen, Magnus", &Lookup::Summary(summary.clone()));
        assert_eq!(
            cache.get("Magnus Carlsen"),
            Some(Lookup::Summary(summary.clone()))
        );
        assert!(cache.get("Carlsen, Magnus").is_some());

        let restored = Cache::from_json(&cache.to_json());
        assert_eq!(
            restored.get("Magnus Carlsen"),
            Some(Lookup::Summary(summary))
        );
        assert_eq!(restored.len(), 1);
    }

    #[test]
    fn cache_remembers_ambiguous_names() {
        let mut cache = Cache::default();
        let lookup = Lookup::Ambiguous {
            search_url: search_url("Magnus Carlsen"),
        };
        cache.insert("Magnus Carlsen", &lookup);
        assert_eq!(cache.get("Carlsen, Magnus"), Some(lookup.clone()));

        let restored = Cache::from_json(&cache.to_json());
        assert_eq!(restored.get("Carlsen, Magnus"), Some(lookup));
    }

    #[test]
    fn excerpt_stops_at_a_word_boundary() {
        let summary = summary(
            "Test",
            "",
            "one two three four five six seven eight nine ten eleven twelve",
        );
        let excerpt = summary.excerpt(20);
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.chars().count() <= 21);
        assert!(excerpt.starts_with("one two three"));
    }

    #[test]
    fn search_url_encodes_spaces_and_accents() {
        assert_eq!(
            search_url("José Raúl Capablanca"),
            "https://en.wikipedia.org/w/index.php?search=Jos%C3%A9%20Ra%C3%BAl%20Capablanca"
        );
        assert_eq!(
            search_url("Capablanca, José Raúl"),
            "https://en.wikipedia.org/w/index.php?search=Jos%C3%A9%20Ra%C3%BAl%20Capablanca"
        );
    }

    #[test]
    fn cache_save_and_load_round_trip() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brett-wikipedia-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut cache = Cache::default();
        cache.insert(
            "Magnus Carlsen",
            &Lookup::Summary(summary(
                "Magnus Carlsen",
                "Norwegian chess grandmaster",
                "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
            )),
        );
        cache.save_to(&path).unwrap();
        let restored = Cache::load_from(&path);
        assert_eq!(restored.len(), 1);
        assert!(restored.get("Carlsen, Magnus").is_some());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn demonym_parsing_orders_oldest_to_latest() {
        assert_eq!(
            demonym_countries("Iranian-born French chess grandmaster"),
            vec!["IRI".to_string(), "FRA".to_string()]
        );
        assert_eq!(
            demonym_countries("Soviet and Russian chess grandmaster"),
            vec!["URS".to_string(), "RUS".to_string()]
        );
        assert_eq!(
            demonym_countries("Norwegian chess grandmaster"),
            vec!["NOR".to_string()]
        );
        assert_eq!(
            demonym_countries("American chess player"),
            vec!["USA".to_string()]
        );
        assert!(demonym_countries("chess grandmaster").is_empty());
        assert!(demonym_countries("").is_empty());
        // Deduplication preserves first appearance.
        assert_eq!(
            demonym_countries("Norwegian chess grandmaster, Norway"),
            vec!["NOR".to_string()]
        );
    }

    #[test]
    fn demonym_parsing_uses_lead_sentence_when_description_is_generic() {
        let value = summary_json(
            "Alireza Firouzja",
            "chess grandmaster",
            "Alireza Firouzja is an Iranian-born French chess grandmaster.",
        );
        let summary = Summary::from_value(&value).unwrap();
        assert_eq!(
            summary.countries,
            vec!["IRI".to_string(), "FRA".to_string()]
        );
        assert_eq!(summary.latest_country().map(|c| c.code), Some("FRA"));
        assert_eq!(
            summary.all_countries(),
            vec![
                country::normalize("IRI").unwrap(),
                country::normalize("FRA").unwrap(),
            ]
        );
    }

    #[test]
    fn cached_country_lists_are_repaired_from_current_summary_text() {
        let mut value = summary_json(
            "Levon Aronian",
            "Armenian-American chess grandmaster (born 1982)",
            "Levon Aronian is an Armenian chess grandmaster who has represented the United States since 2021.",
        );
        value["countries"] = json!(["GER", "USA"]);
        let summary = Summary::from_value(&value).unwrap();
        assert_eq!(summary.countries, vec!["ARM", "USA"]);
        assert_eq!(summary.country_periods[0].code, "ARM");
        assert_eq!(summary.country_periods[0].end_year, Some(2021));
        assert_eq!(summary.country_periods[1].start_year, Some(2021));
    }

    #[test]
    fn player_summaries_retain_soviet_russian_and_wartime_federations() {
        let karpov = Summary::from_value(&summary_json(
            "Anatoly Karpov",
            "Russian chess grandmaster (born 1951)",
            "Anatoly Karpov is a Russian and former Soviet chess grandmaster.",
        ))
        .unwrap();
        assert_eq!(karpov.countries, vec!["URS", "RUS"]);

        let keres = Summary::from_value(&summary_json(
            "Paul Keres",
            "Estonian chess grandmaster (1916–1975)",
            "Paul Keres was an Estonian chess grandmaster. He was forced to represent the Soviet Union and Nazi Germany (1941–1944) in international tournaments.",
        ))
        .unwrap();
        assert_eq!(keres.countries, vec!["EST", "GER", "URS"]);
        assert_eq!(
            keres.country_periods[1],
            CountryPeriod {
                code: "GER".to_string(),
                start_year: Some(1941),
                end_year: Some(1944),
                context: Some("Nazi Germany".to_string()),
            }
        );
    }

    #[test]
    fn american_born_canadian_and_wikidata_p1532_resolve_current_federation_last() {
        let mut hansen_json = summary_json(
            "Eric Hansen (chess player)",
            "Canadian chess grandmaster (born 1992)",
            "Eric Hansen is an American-born Canadian chess grandmaster and Twitch streamer. Hansen has represented Canada in the Chess Olympiad.",
        );
        // Heal stale cached `["CAN", "USA"]` ordering as well.
        hansen_json["countries"] = json!(["CAN", "USA"]);
        let hansen = Summary::from_value(&hansen_json).unwrap();
        assert_eq!(hansen.countries, vec!["USA", "CAN"]);
        assert_eq!(hansen.latest_country().map(|c| c.code), Some("CAN"));

        let fields = parse_wikidata_value(&json!({
            "claims": {
                "P1532": [
                    {"mainsnak": {"datavalue": {"value": {"id": "Q16"}}}}
                ],
                "P27": [
                    {"mainsnak": {"datavalue": {"value": {"id": "Q30"}}}},
                    {"mainsnak": {"datavalue": {"value": {"id": "Q16"}}}}
                ]
            }
        }));
        assert_eq!(fields.countries, vec!["USA", "CAN"]);
    }

    #[test]
    fn wikidata_federation_tenures_exclude_temporary_home_country() {
        let mut summary = Summary::from_value(&summary_json(
            "Levon Aronian",
            "Armenian-American chess grandmaster",
            "Levon Aronian is an Armenian chess grandmaster who has represented the United States since 2021.",
        ))
        .unwrap();
        let time = |year: i32| {
            json!([{
                "mainsnak": {
                    "datavalue": {
                        "value": { "time": format!("+{year:04}-01-01T00:00:00Z") }
                    }
                }
            }])
        };
        apply_wikidata_value(
            &mut summary,
            &json!({
                "claims": {
                    "P1532": [
                        {"mainsnak": {"datavalue": {"value": {"id": "Q399"}}}, "qualifiers": {"P582": time(2003)}},
                        {"mainsnak": {"datavalue": {"value": {"id": "Q183"}}}, "qualifiers": {"P580": time(2003), "P582": time(2004)}},
                        {"mainsnak": {"datavalue": {"value": {"id": "Q399"}}}, "qualifiers": {"P580": time(2004), "P582": time(2021)}},
                        {"mainsnak": {"datavalue": {"value": {"id": "Q30"}}}, "rank": "preferred", "qualifiers": {"P580": time(2021)}}
                    ]
                }
            }),
        );
        assert_eq!(summary.countries, vec!["ARM", "USA"]);
        assert!(
            !summary
                .country_periods
                .iter()
                .any(|period| period.code == "GER")
        );
        assert_eq!(
            Summary::country_period_label(&summary.country_periods[0]),
            "Armenia (until 2003)"
        );
        let restored = Summary::from_value(&summary.to_value()).unwrap();
        assert_eq!(restored.country_periods, summary.country_periods);
    }

    #[test]
    fn wikidata_claim_parsing_extracts_all_fields() {
        let payload = json!({
            "entities": {
                "Q12345": {
                    "id": "Q12345",
                    "claims": {
                        "P1440": [{
                            "rank": "normal",
                            "mainsnak": {
                                "datavalue": { "value": "1503014", "type": "string" },
                                "datatype": "string",
                                "snaktype": "value"
                            }
                        }],
                        "P1532": [
                            {
                                "rank": "normal",
                                "mainsnak": {"datavalue": {"value": {"id": "Q15180", "entity-type": "item", "numeric-id": 15180}, "type": "wikibase-entityid"}, "datatype": "wikibase-item", "snaktype": "value"},
                                "qualifiers": {"P582": [{"mainsnak": {"datavalue": {"value": {"time": "+1991-09-06T00:00:00Z"}, "type": "time"}, "datatype": "time", "snaktype": "value"}}]}
                            },
                            {
                                "rank": "preferred",
                                "mainsnak": {"datavalue": {"value": {"id": "Q159", "entity-type": "item", "numeric-id": 159}, "type": "wikibase-entityid"}, "datatype": "wikibase-item", "snaktype": "value"}
                            }
                        ],
                        "P569": [{
                            "rank": "normal",
                            "mainsnak": {"datavalue": {"value": {"time": "+1963-12-09T00:00:00Z", "type": "time"}, "type": "time"}, "datatype": "time", "snaktype": "value"}
                        }],
                        "P570": [{
                            "rank": "normal",
                            "mainsnak": {"datavalue": {"value": {"time": "+2040-01-01T00:00:00Z", "type": "time"}, "type": "time"}, "datatype": "time", "snaktype": "value"}
                        }],
                        "P1087": [{
                            "rank": "normal",
                            "mainsnak": {"datavalue": {"value": {"amount": "+2850"}, "type": "quantity"}, "datatype": "quantity", "snaktype": "value"}
                        }]
                    }
                }
            }
        });
        let fields = parse_wikidata_value(&payload);
        assert_eq!(fields.fide_id.as_deref(), Some("1503014"));
        assert_eq!(fields.birth_year, Some(1963));
        assert_eq!(fields.death_year, Some(2040));
        assert_eq!(fields.peak_elo, Some(2850));
        assert_eq!(
            fields.countries,
            vec!["URS".to_string(), "RUS".to_string()],
            "terminated URS must sort before current RUS"
        );
    }

    #[test]
    fn lifespan_text_fallback_parses_lead_dates_and_ignores_post_copula_years() {
        let keres = Summary::from_value(&summary_json(
            "Paul Keres",
            "Estonian chess grandmaster (1916–1975)",
            "Paul Keres was an Estonian chess grandmaster. He represented the Soviet Union and Germany (1941–1944).",
        ))
        .unwrap();
        assert_eq!(keres.birth_year, Some(1916));
        assert_eq!(keres.death_year, Some(1975));

        let alekhine = Summary::from_value(&summary_json(
            "Alexander Alekhine",
            "Russian and French chess player",
            "Alexander Alekhine (31 October 1892 – 24 March 1946) was a chess grandmaster.",
        ))
        .unwrap();
        assert_eq!(alekhine.birth_year, Some(1892));
        assert_eq!(alekhine.death_year, Some(1946));

        let fischer = Summary::from_value(&summary_json(
            "Bobby Fischer",
            "American chess grandmaster",
            "Robert James Fischer (March 9, 1943 – January 17, 2008) was an American chess grandmaster.",
        ))
        .unwrap();
        assert_eq!(fischer.birth_year, Some(1943));
        assert_eq!(fischer.death_year, Some(2008));
    }

    #[test]
    fn wikidata_latest_country_ordering_prefers_no_end_date() {
        // Order in the payload is RUS first, then URS with an end date;
        // the parser must still emit URS,RUS (oldest → latest).
        let payload = json!({
            "entities": {
                "Q9": {
                    "claims": {
                        "P27": [
                            {"rank": "normal", "mainsnak": {"datavalue": {"value": {"id": "Q159"}}}},
                            {"rank": "normal", "mainsnak": {"datavalue": {"value": {"id": "Q15180"}}}, "qualifiers": {"P582": [{"mainsnak": {"datavalue": {"value": {"time": "+1991-01-01T00:00:00Z"}}}}]}}
                        ]
                    }
                }
            }
        });
        let fields = parse_wikidata_value(&payload);
        assert_eq!(fields.countries, vec!["URS".to_string(), "RUS".to_string()]);
    }

    #[test]
    fn wikidata_fail_soft_on_malformed_payload() {
        let fields = parse_wikidata_value(&json!({"entities": {}}));
        assert!(fields.countries.is_empty());
        assert!(fields.fide_id.is_none());
        let mut summary = summary(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
        );
        apply_wikidata_value(&mut summary, &json!({"foo": "bar"}));
        assert_eq!(summary.countries, vec!["NOR".to_string()]);
        assert!(summary.fide_id.is_none());
    }

    #[test]
    fn lookup_enriches_from_wikidata_when_wikibase_item_present() {
        let mut wiki = summary_json(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
        );
        wiki["wikibase_item"] = Value::String("Q123".to_string());
        let result = lookup_with("Magnus Carlsen", |url| {
            if url.contains("/summary/Magnus_Carlsen") {
                return Ok(wiki.clone());
            }
            if url.contains("wikidata") && url.contains("Q123") {
                return Ok(json!({
                    "entities": {
                        "Q123": {
                            "claims": {
                                "P1440": [{"mainsnak": {"datavalue": {"value": "1503014", "type": "string"}}}],
                                "P569": [{"mainsnak": {"datavalue": {"value": {"time": "+1990-11-30T00:00:00Z"}, "type": "time"}}}],
                                "P1087": [{"mainsnak": {"datavalue": {"value": {"amount": "+2863"}, "type": "quantity"}}}],
                                "P27": [{"mainsnak": {"datavalue": {"value": {"id": "Q20"}}}}]
                            }
                        }
                    }
                }));
            }
            Err(FetchError::NotFound)
        })
        .unwrap();
        match result {
            Lookup::Summary(summary) => {
                assert_eq!(summary.fide_id.as_deref(), Some("1503014"));
                assert_eq!(summary.birth_year, Some(1990));
                assert_eq!(summary.peak_elo, Some(2863));
                assert_eq!(summary.countries, vec!["NOR".to_string()]);
            }
            Lookup::Ambiguous { .. } => panic!("expected a confident summary"),
        }
    }

    #[test]
    fn lookup_stays_confident_when_wikidata_is_missing() {
        let mut wiki = summary_json(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
        );
        wiki["wikibase_item"] = Value::String("Q123".to_string());
        let result = lookup_with("Magnus Carlsen", |url| {
            if url.contains("/summary/Magnus_Carlsen") {
                return Ok(wiki.clone());
            }
            if url.contains("wikidata") {
                return Err(FetchError::Other("offline".to_string()));
            }
            Err(FetchError::NotFound)
        })
        .unwrap();
        match result {
            Lookup::Summary(summary) => {
                assert_eq!(summary.countries, vec!["NOR".to_string()]);
                assert!(summary.fide_id.is_none());
            }
            Lookup::Ambiguous { .. } => panic!("expected a confident summary"),
        }
    }

    #[test]
    fn cache_round_trip_preserves_enrichment_and_defaults_old_entries() {
        // Old-style entry without new fields must still load.
        let old = json!({
            "summaries": {
                "Magnus Carlsen": {
                    "title": "Magnus Carlsen",
                    "description": "Norwegian chess grandmaster",
                    "extract": "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
                    "url": "https://en.wikipedia.org/wiki/Magnus_Carlsen"
                }
            },
            "queries": {"carlsen magnus": "Magnus Carlsen"},
            "ambiguous": {}
        })
        .to_string();
        let restored = Cache::from_json(&old);
        let lookup = restored.get("Magnus Carlsen").unwrap();
        let Lookup::Summary(summary) = lookup else {
            panic!("expected summary");
        };
        assert_eq!(summary.countries, vec!["NOR".to_string()]);
        assert!(summary.fide_id.is_none());
        assert!(summary.birth_year.is_none());
        assert!(summary.death_year.is_none());
        assert!(summary.peak_elo.is_none());

        // New entries round-trip the enrichment fields.
        let mut enriched = make_summary(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
        );
        enriched.fide_id = Some("1503014".to_string());
        enriched.birth_year = Some(1990);
        enriched.death_year = Some(2077);
        enriched.peak_elo = Some(2863);
        let mut cache = Cache::default();
        cache.insert("Magnus Carlsen", &Lookup::Summary(enriched));
        let restored = Cache::from_json(&cache.to_json());
        let Lookup::Summary(round_tripped) = restored.get("Magnus Carlsen").unwrap() else {
            panic!("expected summary");
        };
        assert_eq!(round_tripped.fide_id.as_deref(), Some("1503014"));
        assert_eq!(round_tripped.birth_year, Some(1990));
        assert_eq!(round_tripped.death_year, Some(2077));
        assert_eq!(round_tripped.peak_elo, Some(2863));
        assert_eq!(round_tripped.countries, vec!["NOR".to_string()]);
    }

    #[test]
    fn wikidata_title_history_keeps_award_years() {
        let time = |year: i32| {
            json!([{
                "mainsnak": {
                    "datavalue": {
                        "value": { "time": format!("+{year:04}-01-01T00:00:00Z") }
                    }
                }
            }])
        };
        let mut summary = make_summary(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
        );
        apply_wikidata_value(
            &mut summary,
            &json!({
                "claims": {
                    "P2962": [
                        {"mainsnak": {"datavalue": {"value": {"id": "Q752119"}}}, "qualifiers": {"P580": time(2003)}},
                        {"mainsnak": {"datavalue": {"value": {"id": "Q105269"}}}, "rank": "preferred", "qualifiers": {"P580": time(2004)}},
                        {"mainsnak": {"datavalue": {"value": {"id": "Q20042649"}}}, "qualifiers": {"P580": time(2009)}}
                    ]
                }
            }),
        );
        assert_eq!(
            summary.title_history,
            vec![
                TitleAward {
                    title: PlayerTitle::Im,
                    year: Some(2003)
                },
                TitleAward {
                    title: PlayerTitle::Gm,
                    year: Some(2004)
                },
                TitleAward {
                    title: PlayerTitle::Gm,
                    year: None
                },
            ]
        );
        assert_eq!(summary.current_title(), Some(PlayerTitle::Gm));
        assert_eq!(summary.title_at(2000), None);
        assert_eq!(summary.title_at(2003), Some(PlayerTitle::Im));
        assert_eq!(summary.title_at(2010), Some(PlayerTitle::Gm));
    }

    #[test]
    fn wikidata_title_mapping_accepts_fide_player_titles_only() {
        for (qid, title) in [
            ("Q105269", PlayerTitle::Gm),
            ("Q752119", PlayerTitle::Im),
            ("Q1049520", PlayerTitle::Fm),
            ("Q1973600", PlayerTitle::Cm),
            ("Q3417060", PlayerTitle::Wgm),
            ("Q3314851", PlayerTitle::Wim),
            ("Q3352252", PlayerTitle::Wfm),
            ("Q11982685", PlayerTitle::Wcm),
        ] {
            assert_eq!(title_from_wikidata_qid(qid), Some(title), "{qid}");
        }
        // An honorary grandmaster title is not the same as the FIDE GM title.
        assert_eq!(title_from_wikidata_qid("Q20042649"), None);
        assert_eq!(title_from_wikidata_qid("Q3818700"), None);
    }

    #[test]
    fn title_text_fallback_is_conservative() {
        let summary = Summary::from_value(&summary_json(
            "Example Player",
            "Chess player",
            "Example Player is a German chess grandmaster who also writes about the game.",
        ))
        .unwrap();
        assert_eq!(summary.current_title(), Some(PlayerTitle::Gm));

        let both = Summary::from_value(&summary_json(
            "Example Player",
            "Chess player",
            "Example Player is a Georgian chess grandmaster and the first woman to be awarded the FIDE title Woman Grandmaster.",
        ))
        .unwrap();
        assert_eq!(both.current_title(), Some(PlayerTitle::Gm));

        let not_chess = Summary::from_value(&summary_json(
            "Example Player",
            "Writer",
            "Example Player is a grandmaster of the local lodge.",
        ))
        .unwrap();
        assert_eq!(not_chess.current_title(), None);
    }

    #[test]
    fn incomplete_wikidata_history_does_not_downgrade_lead_title() {
        let mut summary = make_summary(
            "Example Player",
            "Chess grandmaster",
            "Example Player is a chess grandmaster.",
        );
        apply_wikidata_value(
            &mut summary,
            &json!({
                "claims": {
                    "P2962": [
                        {"mainsnak": {"datavalue": {"value": {"id": "Q752119"}}}, "qualifiers": {
                            "P580": [{"mainsnak": {"datavalue": {"value": {"time": "+2018-01-01T00:00:00Z"}}}}]
                        }}
                    ]
                }
            }),
        );
        assert_eq!(summary.current_title(), Some(PlayerTitle::Gm));
        assert_eq!(summary.title_at(2019), Some(PlayerTitle::Im));
    }

    #[test]
    fn title_history_round_trips_through_the_cache() {
        let mut summary = make_summary(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
        );
        summary.title_history = vec![
            TitleAward {
                title: PlayerTitle::Im,
                year: Some(2003),
            },
            TitleAward {
                title: PlayerTitle::Gm,
                year: Some(2004),
            },
        ];
        assert_eq!(summary.current_title(), Some(PlayerTitle::Gm));
        let restored = Summary::from_value(&summary.to_value()).unwrap();
        assert_eq!(restored.title_history, summary.title_history);
        assert_eq!(restored.title_at(2003), Some(PlayerTitle::Im));
        assert_eq!(restored.title_at(1999), None);
    }

    #[test]
    fn compact_title_history_cache_round_trips_player_keys() {
        let histories = BTreeMap::from([
            (
                "carlsen magnus".to_string(),
                vec![
                    TitleAward {
                        title: PlayerTitle::Im,
                        year: Some(2003),
                    },
                    TitleAward {
                        title: PlayerTitle::Gm,
                        year: Some(2004),
                    },
                ],
            ),
            (
                "polgar judit".to_string(),
                vec![TitleAward {
                    title: PlayerTitle::Gm,
                    year: None,
                }],
            ),
        ]);
        let restored = title_history_cache_from_json(&title_history_cache_to_json(&histories));
        assert_eq!(restored, histories);
    }

    #[test]
    fn compact_title_cache_merge_preserves_other_players_and_adds_awards() {
        let existing = BTreeMap::from([
            (
                "existing player".to_string(),
                vec![TitleAward {
                    title: PlayerTitle::Im,
                    year: Some(2000),
                }],
            ),
            (
                "same player".to_string(),
                vec![TitleAward {
                    title: PlayerTitle::Im,
                    year: Some(2003),
                }],
            ),
        ]);
        let updates = BTreeMap::from([(
            "same player".to_string(),
            vec![TitleAward {
                title: PlayerTitle::Gm,
                year: Some(2004),
            }],
        )]);

        let merged = merge_title_history_caches(existing, &updates);
        assert!(merged.contains_key("existing player"));
        assert_eq!(
            merged["same player"],
            vec![
                TitleAward {
                    title: PlayerTitle::Im,
                    year: Some(2003),
                },
                TitleAward {
                    title: PlayerTitle::Gm,
                    year: Some(2004),
                },
            ]
        );
    }

    #[test]
    fn title_matching_handles_diacritics_compound_names_and_rejects_false_positives() {
        assert!(title_matches_name("Bluebaum, Matthias", "Matthias Blübaum"));
        assert!(title_matches_name("Indjic, Aleksandar", "Aleksandar Inđić"));
        assert!(title_matches_name("Dreev, Alexey", "Aleksey Dreev"));
        assert!(title_matches_name(
            "Mekhitarian, Krikor Sevag",
            "Krikor Mekhitarian"
        ));
        assert!(title_matches_name(
            "Martinez Alcantara, Jose Eduardo",
            "José Martínez Alcántara"
        ));
        assert!(title_matches_name(
            "Erigaisi, Arjun Kumar",
            "Arjun Erigaisi"
        ));
        assert!(title_matches_name(
            "Iturrizaga Bonelli, Eduardo",
            "Eduardo Iturrizaga"
        ));
        assert!(title_matches_name(
            "Aravindh, Chithambaram VR",
            "Aravindh Chithambaram"
        ));
        assert!(title_matches_name("Gawain C B Jones", "Gawain Jones"));
        assert!(title_matches_name(
            "Fedoseev, Vladimir1",
            "Vladimir Fedoseev"
        ));

        // False-positive guardrails: prefer no match over a wrong match.
        assert!(!title_matches_name("Bacrot, M", "Étienne Bacrot"));
        assert!(!title_matches_name("Carlsen, H", "Magnus Carlsen"));
        assert!(!title_matches_name("Magnus Carlsen Jr", "Magnus Carlsen"));
        assert!(!title_matches_name("Carlsen", "Magnus Carlsen"));
        assert!(!title_matches_name("Smith, John Paul", "Paul Smith"));
        assert!(!title_matches_name("Krause, Chris", "Christian Krause"));
    }

    #[test]
    fn title_matching_handles_anglicised_slavic_and_german_transliterations() {
        assert!(title_matches_name("Ivanchuk, Vassily", "Vasyl Ivanchuk"));
        assert!(title_matches_name(
            "Kortschnoj, Viktor Lvovich",
            "Viktor Korchnoi"
        ));
        assert!(title_matches_name("Kortschnoj, Viktor", "Viktor Korchnoi"));
        assert!(title_matches_name("Kasparow, Garri", "Garry Kasparov"));
        assert!(title_matches_name("Tal, Michail", "Mikhail Tal"));
        assert!(title_matches_name("Kramnik, Wladimir", "Vladimir Kramnik"));
    }

    #[test]
    fn wikidata_p1087_extracts_peak_and_latest_dated_elo() {
        let payload = json!({
            "entities": {
                "Q337223": {
                    "claims": {
                        "P1087": [
                            {
                                "rank": "normal",
                                "mainsnak": {"datavalue": {"value": {"amount": "+2816"}, "type": "quantity"}},
                                "qualifiers": {
                                    "P585": [{"mainsnak": {"datavalue": {"value": {"time": "+2015-10-01T00:00:00Z"}, "type": "time"}}}]
                                }
                            },
                            {
                                "rank": "preferred",
                                "mainsnak": {"datavalue": {"value": {"amount": "+2792"}, "type": "quantity"}},
                                "qualifiers": {
                                    "P585": [{"mainsnak": {"datavalue": {"value": {"time": "+2026-10-01T00:00:00Z"}, "type": "time"}}}]
                                }
                            }
                        ]
                    }
                }
            }
        });
        let fields = parse_wikidata_value(&payload);
        assert_eq!(fields.peak_elo, Some(2816));
        assert_eq!(fields.latest_elo, Some(2792));
        assert_eq!(fields.latest_elo_date, Some(20261001));
    }

    #[test]
    fn search_name_and_umlaut_variants_normalize_database_spellings() {
        assert_eq!(search_name("Fedoseev, Vladimir1"), "Vladimir Fedoseev");
        assert_eq!(search_name("Bluebaum, Matthias"), "Matthias Bluebaum");
        assert_eq!(
            summary_url("Bluebaum, Matthias"),
            format!("{SUMMARY_ENDPOINT}Matthias_Bluebaum")
        );

        let variants = umlaut_variants("Ueber, Goetze");
        assert!(variants.contains(&"Über, Götze".to_string()));
        assert!(variants.contains(&"Über, Goetze".to_string()));
        assert!(variants.contains(&"Ueber, Götze".to_string()));
        assert!(umlaut_variants("Grosse, Franz").contains(&"Große, Franz".to_string()));
    }

    #[test]
    fn unversioned_ambiguous_cache_is_invalidated_and_remove_clears_entries() {
        let old = json!({
            "summaries": {
                "Magnus Carlsen": summary_json(
                    "Magnus Carlsen",
                    "Norwegian chess grandmaster",
                    "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
                )
            },
            "queries": {
                "bacrot e": "Magnus Carlsen",
                "carlsen magnus": "Magnus Carlsen"
            },
            "ambiguous": {
                "bluebaum matthias": "https://en.wikipedia.org/w/index.php?search=Matthias%20Bluebaum"
            }
        })
        .to_string();
        let mut cache = Cache::from_json(&old);
        assert!(cache.get("Carlsen, Magnus").is_some());
        assert_eq!(cache.get("Bacrot, E"), None);
        assert_eq!(
            cache.get("Bluebaum, Matthias"),
            None,
            "stale unversioned ambiguous entry should be discarded for retry"
        );

        let lookup = Lookup::Ambiguous {
            search_url: search_url("Some Player"),
        };
        cache.insert("Some Player", &lookup);
        let round_trip = Cache::from_json(&cache.to_json());
        assert_eq!(round_trip.get("Some Player"), Some(lookup));

        let mut mutable = round_trip;
        mutable.remove("Some Player");
        assert_eq!(mutable.get("Some Player"), None);
    }

    #[test]
    fn lookup_does_not_retry_umlauts_after_network_error() {
        let mut calls = 0usize;
        let err = lookup_with("Bluebaum, Matthias", |_| {
            calls += 1;
            Err(FetchError::Other("HTTP 429".to_string()))
        })
        .unwrap_err();
        assert!(err.contains("429"));
        assert_eq!(
            calls, 2,
            "only direct summary and search for primary should run on error"
        );
    }

    #[test]
    fn bortnyk_and_paravyan_extracts_and_descriptions_resolve_gm_title_and_year() {
        let paravyan = Summary::from_value(&summary_json(
            "David Paravyan",
            "Russian chess grandmaster (born 1998)",
            "David Arturovich Paravyan is a Russian chess player of Armenian descent, who was awarded the title of grandmaster by FIDE in 2017.",
        ))
        .unwrap();
        assert_eq!(paravyan.current_title(), Some(PlayerTitle::Gm));
        assert!(paravyan.title_history.contains(&TitleAward {
            title: PlayerTitle::Gm,
            year: Some(2017),
        }));

        let bortnyk = Summary::from_value(&summary_json(
            "Olexandr Bortnyk",
            "Ukrainian chess grandmaster (born 1996)",
            "Olexandr Mykolayovych Bortnyk is a Ukrainian chess player. He was awarded the Grandmaster title in 2015, at the age of 19.",
        ))
        .unwrap();
        assert_eq!(bortnyk.current_title(), Some(PlayerTitle::Gm));
        assert!(bortnyk.title_history.contains(&TitleAward {
            title: PlayerTitle::Gm,
            year: Some(2015),
        }));

        let woman_gm = Summary::from_value(&summary_json(
            "Example Woman Player",
            "Georgian chess player",
            "Example Woman Player is a chess player who earned the Woman Grandmaster title in 2018.",
        ))
        .unwrap();
        assert_eq!(woman_gm.current_title(), Some(PlayerTitle::Wgm));
    }

    #[test]
    fn generational_suffix_moroni_jr_resolves_and_matches() {
        assert_eq!(search_name("Moroni, Luca Jr"), "Luca Moroni Jr");
        let summary = Summary::from_value(&summary_json(
            "Luca Moroni",
            "Italian chess grandmaster",
            "Luca Moroni Jr. (born 1 July 2000) is an Italian chess grandmaster.",
        ))
        .unwrap();
        assert!(summary_matches_name("Moroni, Luca Jr", &summary));

        let lookup = lookup_with("Moroni, Luca Jr", |url| {
            if url.ends_with("/Luca_Moroni") {
                Ok(summary_json(
                    "Luca Moroni",
                    "Italian chess grandmaster",
                    "Luca Moroni Jr. (born 1 July 2000) is an Italian chess grandmaster.",
                ))
            } else {
                Err(FetchError::NotFound)
            }
        })
        .unwrap();
        match lookup {
            Lookup::Summary(found) => {
                assert_eq!(found.title, "Luca Moroni");
                assert_eq!(found.countries, vec!["ITA".to_string()]);
            }
            other => panic!("expected Summary for Moroni, Luca Jr, got {other:?}"),
        }
    }

    #[test]
    fn steinitz_william_resolves_as_wilhelm_steinitz_with_historical_flags() {
        let lookup = lookup_with("Steinitz, William", |url| {
            if url.ends_with("/Wilhelm_Steinitz") {
                Ok(summary_json(
                    "Wilhelm Steinitz",
                    "Austrian and American chess player (1836–1900)",
                    "Wilhelm Steinitz (born in Prague, Kingdom of Bohemia, Austrian Empire; later William Steinitz; 1836–1900) was an Austrian and later American chess player and the first World Chess Champion.",
                ))
            } else {
                Err(FetchError::NotFound)
            }
        })
        .unwrap();
        match lookup {
            Lookup::Summary(found) => {
                assert_eq!(found.title, "Wilhelm Steinitz");
                assert!(found.countries.contains(&"BOH".to_string()));
                assert!(found.countries.contains(&"USA".to_string()));
            }
            other => panic!("expected Summary for Steinitz, William, got {other:?}"),
        }
    }

    #[test]
    fn hort_keeps_czechoslovakia_and_germany_and_repairs_stale_cache() {
        let hort = Summary::from_value(&summary_json(
            "Vlastimil Hort",
            "Czech and German chess grandmaster",
            "Vlastimil Hort (12 January 1944 – 12 May 2025) was a Czechoslovak and German chess grandmaster.",
        ))
        .unwrap();
        assert_eq!(
            hort.countries,
            vec!["TCH".to_string(), "GER".to_string()]
        );

        let stale = json!({
            "title": "Vlastimil Hort",
            "description": "Czech and German chess grandmaster",
            "extract": "Vlastimil Hort was a Czechoslovak and German chess grandmaster.",
            "url": "https://en.wikipedia.org/wiki/Vlastimil_Hort",
            "countries": ["GER"],
            "country_periods": [
                {"code": "GER", "start_year": null, "end_year": null, "context": null}
            ],
            "fide_id": null,
            "birth_year": 1944,
            "death_year": 2025,
            "latest_elo": null,
            "latest_elo_date": null,
            "peak_elo": null,
            "fide_standard": null,
            "fide_rapid": null,
            "fide_blitz": null,
            "title_history": []
        });
        let repaired = Summary::from_value(&stale).unwrap();
        assert_eq!(
            repaired.countries,
            vec!["TCH".to_string(), "GER".to_string()]
        );
    }

    #[test]
    fn historical_1800_to_present_states_resolve_without_modern_leaks() {
        let anderssen = Summary::from_value(&summary_json(
            "Adolf Anderssen",
            "German chess master (1818–1879)",
            "Karl Ernst Adolf Anderssen (born in Breslau, Kingdom of Prussia; 1818–1879) was a German chess master.",
        ))
        .unwrap();
        assert!(anderssen.countries.contains(&"PRU".to_string()));
        assert!(!anderssen.countries.contains(&"GER".to_string()));

        let chigorin = Summary::from_value(&summary_json(
            "Mikhail Chigorin",
            "Russian chess player (1850–1908)",
            "Mikhail Ivanovich Chigorin (1850–1908) was a leading Russian chess player in the Russian Empire.",
        ))
        .unwrap();
        assert!(chigorin.countries.contains(&"RUE".to_string()));
        assert!(!chigorin.countries.contains(&"RUS".to_string()));
    }
}
