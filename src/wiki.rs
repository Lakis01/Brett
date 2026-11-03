//! Wikipedia player summaries for the selected player profile.
//!
//! Lookups happen only when a profile is opened, never while a database is
//! indexed or a player list is rendered. A confident chess-player match is
//! shown as a short, attributed summary; anything else becomes a search link.
//! Results are cached locally by article title, so reopening a profile does not
//! repeat the request. No article text is bundled with Brett.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

const USER_AGENT: &str =
    "Brett/0.1 (native Linux chess preparation app; Wikipedia player summaries)";
const SUMMARY_ENDPOINT: &str = "https://en.wikipedia.org/api/rest_v1/page/summary/";
const SEARCH_ENDPOINT: &str = "https://en.wikipedia.org/w/rest.php/v1/search/page";
const SEARCH_PAGE: &str = "https://en.wikipedia.org/w/index.php";
const MAX_SUMMARIES: usize = 256;
const MAX_AMBIGUOUS: usize = 256;

/// A cleaned Wikipedia article summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub title: String,
    pub description: String,
    pub extract: String,
    /// Canonical article URL on Wikipedia.
    pub url: String,
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

    fn to_value(&self) -> Value {
        json!({
            "title": self.title,
            "description": self.description,
            "extract": self.extract,
            "url": self.url,
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
        Some(Self {
            title: title.to_string(),
            description,
            extract,
            url,
        })
    }
}

/// The result of looking up a player name.
#[derive(Clone, Debug, PartialEq, Eq)]
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
            && title_matches_name(name, &summary.title)
        {
            return Some(Lookup::Summary(summary.clone()));
        }
        self.ambiguous
            .get(&key)
            .map(|search_url| Lookup::Ambiguous {
                search_url: search_url.clone(),
            })
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

    pub fn len(&self) -> usize {
        self.summaries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.summaries.is_empty() && self.ambiguous.is_empty()
    }

    pub fn from_json(text: &str) -> Self {
        let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
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
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let ambiguous: BTreeMap<String, String> = value
            .get("ambiguous")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
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

/// Look up a player name on Wikipedia. Network errors are returned as
/// human-readable strings so the profile can offer a retry and a search link.
pub fn lookup(name: &str) -> std::result::Result<Lookup, String> {
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(6))
        .timeout(Duration::from_secs(12))
        .build();
    lookup_with(name, |url| get_json(&agent, url))
}

fn lookup_with(
    name: &str,
    mut fetch: impl FnMut(&str) -> std::result::Result<Value, FetchError>,
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
            if let Some(summary) = Summary::from_value(&value) {
                direct_title = Some(summary.title.clone());
                if is_confident(&summary) && title_matches_name(name, &summary.title) {
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
            if !title_matches_name(name, title) {
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
                if let Some(summary) = Summary::from_value(&value) {
                    parsed_any = true;
                    if is_confident(&summary) && title_matches_name(name, &summary.title) {
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

fn get_json(agent: &ureq::Agent, url: &str) -> std::result::Result<Value, FetchError> {
    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(404, _)) => return Err(FetchError::NotFound),
        Err(ureq::Error::Status(code, _)) => {
            return Err(FetchError::Other(format!(
                "Wikipedia returned HTTP {code}."
            )));
        }
        Err(err) => {
            return Err(FetchError::Other(format!(
                "Wikipedia request failed: {err}"
            )));
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

fn name_tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| deunicode::deunicode(token).to_lowercase())
        .collect()
}

/// Split a PGN name into tokens, putting `Surname, Given` back into
/// given-name order so both spellings match the same article.
fn query_name_tokens(name: &str) -> Vec<String> {
    let name = name.trim();
    if let Some((surname, given)) = name.split_once(',') {
        let mut tokens = name_tokens(given);
        tokens.extend(name_tokens(surname));
        tokens
    } else {
        name_tokens(name)
    }
}

/// True when an article title names the queried player.
///
/// Search results for abbreviated PGN names such as `Bacrot, E` often contain
/// a confident but unrelated chess player. Multi-letter query tokens (the
/// surname and any spelled-out given names) must all appear in the title;
/// single-letter initials may be absent so `Gawain C B Jones` still matches
/// `Gawain Jones`. Prefix matching covers conventional abbreviations:
/// `Svane, R` matches `Rasmus Svane` and `Buhmann,R` matches `Rainer Buhmann`.
fn title_matches_name(name: &str, title: &str) -> bool {
    let query = query_name_tokens(name);
    let title = title.split('(').next().unwrap_or(title);
    let title_tokens = name_tokens(title);
    if query.is_empty() || title_tokens.is_empty() {
        return false;
    }

    let mut used = vec![false; title_tokens.len()];
    let mut required = 0;
    for token in &query {
        if token.chars().count() == 1 {
            continue;
        }
        required += 1;
        let Some(index) = title_tokens
            .iter()
            .enumerate()
            .find_map(|(index, candidate)| {
                (!used[index] && candidate.starts_with(token.as_str())).then_some(index)
            })
        else {
            return false;
        };
        used[index] = true;
    }
    required > 0
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

/// PGN tags often use `Last, First`; Wikipedia search reads `First Last`
/// more naturally. Leave every other spelling alone.
fn search_name(name: &str) -> String {
    let name = name.trim();
    if let Some((last, first)) = name.split_once(',') {
        let last = last.trim();
        let first = first.trim();
        if !last.is_empty() && !first.is_empty() && !first.contains(',') {
            return format!("{first} {last}");
        }
    }
    name.to_string()
}

fn summary_url(name: &str) -> String {
    summary_url_for_title(name)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(title: &str, description: &str, extract: &str) -> Summary {
        Summary {
            title: title.to_string(),
            description: description.to_string(),
            extract: extract.to_string(),
            url: article_url(title),
        }
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
}
