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

use crate::country::{self, Country};

const USER_AGENT: &str =
    "Brett/0.1 (native Linux chess preparation app; Wikipedia player summaries)";
const SUMMARY_ENDPOINT: &str = "https://en.wikipedia.org/api/rest_v1/page/summary/";
const SEARCH_ENDPOINT: &str = "https://en.wikipedia.org/w/rest.php/v1/search/page";
const SEARCH_PAGE: &str = "https://en.wikipedia.org/w/index.php";
const WIKIDATA_ENDPOINT: &str = "https://www.wikidata.org/wiki/Special:EntityData/";
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
    /// Canonical 3-letter FIDE codes ordered oldest → latest; last is the
    /// latest/current federation. Empty when no nationality was detected.
    pub countries: Vec<String>,
    /// FIDE id from Wikidata `P1440`, when available.
    pub fide_id: Option<String>,
    /// Birth year from Wikidata `P569`, when available.
    pub birth_year: Option<i32>,
    /// Peak Elo from Wikidata `P1087`, when available.
    pub peak_elo: Option<u16>,
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

    /// All resolved federations ordered oldest → latest.
    pub fn all_countries(&self) -> Vec<Country> {
        self.countries
            .iter()
            .filter_map(|code| country::normalize(code))
            .collect()
    }

    fn to_value(&self) -> Value {
        json!({
            "title": self.title,
            "description": self.description,
            "extract": self.extract,
            "url": self.url,
            "countries": self.countries,
            "fide_id": self.fide_id,
            "birth_year": self.birth_year,
            "peak_elo": self.peak_elo,
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
        // Cached entries from before enrichment may lack the new fields;
        // default them (empty / None) instead of dropping the summary.
        // Stored countries win; otherwise re-derive from lead text.
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
        let countries = if stored_countries.is_empty() {
            // Fresh Wikipedia payload: derive from description + lead sentence.
            // When loading a cache entry without stored countries, the same
            // derivation runs against the cached description/extract, which
            // is harmless and keeps old caches enriched.
            countries_from_texts(&description, &extract)
        } else {
            stored_countries
        };
        let fide_id = value
            .get("fide_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        let birth_year = value
            .get("birth_year")
            .and_then(Value::as_i64)
            .filter(|year| *year >= 0 && *year <= 2100)
            .map(|year| year as i32)
            .or_else(|| {
                value
                    .get("birth_year")
                    .and_then(Value::as_u64)
                    .filter(|year| *year <= 2100)
                    .map(|year| year as i32)
            });
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
            });
        Some(Self {
            title: title.to_string(),
            description,
            extract,
            url,
            countries,
            fide_id,
            birth_year,
            peak_elo,
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
/// extract, oldest → latest. The description order wins; lead-sentence
/// codes not already seen are appended in order.
fn countries_from_texts(description: &str, extract: &str) -> Vec<String> {
    let mut out = demonym_countries(description);
    let lead = extract.split('.').next().unwrap_or(extract);
    for code in demonym_countries(lead) {
        if !out.iter().any(|existing| existing == &code) {
            out.push(code);
        }
    }
    // Include the remainder of the extract only when neither the
    // description nor the lead sentence named a nationality — avoids
    // picking up incidental country mentions later in the article.
    if out.is_empty() {
        out = demonym_countries(extract);
    }
    out
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

fn claim_has_end_date(claim: &Value) -> bool {
    claim
        .pointer("/qualifiers/P582")
        .and_then(Value::as_array)
        .is_some_and(|ends| !ends.is_empty())
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
    pub fide_id: Option<String>,
    pub birth_year: Option<i32>,
    pub peak_elo: Option<u16>,
}

/// Parse Wikidata `EntityData` claims: `P1440` → `fide_id`, `P1532`/`P27`
/// → `countries` (oldest → latest), `P569` → `birth_year`, `P1087` →
/// `peak_elo`. Malformed payloads yield defaults; this never panics.
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
    // P569 → birth_year.
    if let Some(snatched) = claims.get("P569").and_then(Value::as_array) {
        for claim in snatched {
            if claim_rank(claim) == "deprecated" {
                continue;
            }
            if let Some(year) = parse_time_year(claim) {
                fields.birth_year = Some(year);
                break;
            }
        }
    }
    // P1087 → peak_elo (max across non-deprecated claims).
    if let Some(snatched) = claims.get("P1087").and_then(Value::as_array) {
        for claim in snatched {
            if claim_rank(claim) == "deprecated" {
                continue;
            }
            if let Some(elo) = parse_quantity_elo(claim) {
                fields.peak_elo = Some(match fields.peak_elo {
                    Some(current) => current.max(elo),
                    None => elo,
                });
            }
        }
    }
    // P1532 / P27 → countries via country::from_wikidata_qid.
    // Order oldest → latest: terminated claims first, unterminated /
    // preferred next, so the current federation sorts last.
    let mut sourced: Vec<(String, bool, bool)> = Vec::new();
    for prop in ["P1532", "P27"] {
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
                    country.code.to_string(),
                    claim_has_end_date(claim),
                    claim_rank(claim) == "preferred",
                ));
            }
        }
    }
    sourced.sort_by_key(|(_, has_end, is_preferred)| {
        if *is_preferred {
            2
        } else if !*has_end {
            1
        } else {
            0
        }
    });
    for (code, _, _) in sourced {
        if !fields.countries.iter().any(|existing| existing == &code) {
            fields.countries.push(code);
        }
    }
    fields
}

/// Merge Wikidata fields into a summary. Countries from Wikidata replace
/// demonym-derived ones when present; the other fields fill when missing
/// or overwrite with the authoritative value.
pub(crate) fn apply_wikidata_value(summary: &mut Summary, value: &Value) {
    let fields = parse_wikidata_value(value);
    if !fields.countries.is_empty() {
        summary.countries = fields.countries;
    }
    if let Some(fide_id) = fields.fide_id {
        summary.fide_id = Some(fide_id);
    }
    if let Some(birth_year) = fields.birth_year {
        summary.birth_year = Some(birth_year);
    }
    if let Some(peak_elo) = fields.peak_elo {
        summary.peak_elo = Some(peak_elo);
    }
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
            if let Some(mut summary) = Summary::from_value(&value) {
                direct_title = Some(summary.title.clone());
                if is_confident(&summary) && title_matches_name(name, &summary.title) {
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
                if let Some(mut summary) = Summary::from_value(&value) {
                    parsed_any = true;
                    if is_confident(&summary) && title_matches_name(name, &summary.title) {
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

    fn make_summary(title: &str, description: &str, extract: &str) -> Summary {
        Summary {
            title: title.to_string(),
            description: description.to_string(),
            extract: extract.to_string(),
            url: article_url(title),
            countries: countries_from_texts(description, extract),
            fide_id: None,
            birth_year: None,
            peak_elo: None,
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
        assert_eq!(fields.peak_elo, Some(2850));
        assert_eq!(
            fields.countries,
            vec!["URS".to_string(), "RUS".to_string()],
            "terminated URS must sort before current RUS"
        );
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
        assert!(summary.peak_elo.is_none());

        // New entries round-trip the enrichment fields.
        let mut enriched = make_summary(
            "Magnus Carlsen",
            "Norwegian chess grandmaster",
            "Sven Magnus Øen Carlsen is a Norwegian chess grandmaster.",
        );
        enriched.fide_id = Some("1503014".to_string());
        enriched.birth_year = Some(1990);
        enriched.peak_elo = Some(2863);
        let mut cache = Cache::default();
        cache.insert("Magnus Carlsen", &Lookup::Summary(enriched));
        let restored = Cache::from_json(&cache.to_json());
        let Lookup::Summary(round_tripped) = restored.get("Magnus Carlsen").unwrap() else {
            panic!("expected summary");
        };
        assert_eq!(round_tripped.fide_id.as_deref(), Some("1503014"));
        assert_eq!(round_tripped.birth_year, Some(1990));
        assert_eq!(round_tripped.peak_elo, Some(2863));
        assert_eq!(round_tripped.countries, vec!["NOR".to_string()]);
    }
}
