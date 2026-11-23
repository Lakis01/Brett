//! Bridge for importing PGN games from German, regional, international, and
//! custom chess league/tournament providers.

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::domain::{Game, parse_pgn};

pub const LIGAMANAGER_BASE_URL: &str = "https://ligamanager.schachbund-bayern.de";
pub const DSB_BASE_URL: &str = "https://ergebnisdienst.schachbund.de";

const USER_AGENT: &str = "Brett/0.3 (+https://github.com/Lakis01/Brett)";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LeaguePortal {
    #[default]
    LigaManagerBayern,
    DsbErgebnisdienst,
}

impl LeaguePortal {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LigaManagerBayern => "LigaManager Bayern",
            Self::DsbErgebnisdienst => "DSB Ergebnisdienst",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    LigaManagerBayern,
    DsbErgebnisdienst,
    LichessBroadcast,
    RegionalHtmlPortal,
    CustomUrl,
}

#[derive(Clone, Copy, Debug)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub region: &'static str,
    pub kind: ProviderKind,
    pub url: &'static str,
    pub keywords: &'static str,
}

pub static PROVIDER_CATALOG: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: "ligamanager-bayern",
        name: "LigaManager Bayern",
        region: "Bavaria (BSB)",
        kind: ProviderKind::LigaManagerBayern,
        url: "https://ligamanager.schachbund-bayern.de",
        keywords: "bayern bavaria bsb muenchen nuernberg oberliga bezirksliga kreisliga erlangen passau",
    },
    ProviderDescriptor {
        id: "dsb-ergebnisdienst",
        name: "DSB Ergebnisdienst (Bundesliga / Oberligen)",
        region: "Germany (DSB)",
        kind: ProviderKind::DsbErgebnisdienst,
        url: "https://ergebnisdienst.schachbund.de",
        keywords: "dsb schachbund bundesliga oberliga deutschland germany bedh",
    },
    ProviderDescriptor {
        id: "schachbundesliga",
        name: "Schachbundesliga",
        region: "Germany (1st League)",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.schachbundesliga.de",
        keywords: "bundesliga schachbundesliga pgn baden-baden viernheim osg",
    },
    ProviderDescriptor {
        id: "lichess-broadcasts",
        name: "Lichess Broadcasts (OTB & Tournaments)",
        region: "International",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "lichess broadcast tournament otb tata steel candidates olympiad open world cup",
    },
    ProviderDescriptor {
        id: "chess-results",
        name: "Chess-Results.com (Portal / PGN links)",
        region: "International",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://chess-results.com",
        keywords: "chess-results swiss open fide austria germany club pgn",
    },
    ProviderDescriptor {
        id: "nrw-portal",
        name: "Schachbund NRW (NRW Ergebnisportal)",
        region: "North Rhine-Westphalia",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.schachbund-nrw.de",
        keywords: "nrw nordrhein westfalen koeln duesseldorf dortmund essen bochum",
    },
    ProviderDescriptor {
        id: "bsv-baden",
        name: "Badischer Schachverband (BSV)",
        region: "Baden",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.badischer-schachverband.de",
        keywords: "baden karlsruhe mannheim heidelberg freiburg bsv",
    },
    ProviderDescriptor {
        id: "svw-wuerttemberg",
        name: "Schachverband Württemberg (SVW)",
        region: "Württemberg",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.svw.info",
        keywords: "wuerttemberg stuttgart ulm tuebingen svw baden-wuerttemberg",
    },
    ProviderDescriptor {
        id: "hsv-hessen",
        name: "Hessischer Schachverband (HSV)",
        region: "Hesse",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.hessischer-schachverband.de",
        keywords: "hessen hesse frankfurt wiesbaden kassel darmstadt hsv",
    },
    ProviderDescriptor {
        id: "nsv-niedersachsen",
        name: "Niedersächsischer Schachverband (NSV)",
        region: "Lower Saxony",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.nsv-online.de",
        keywords: "niedersachsen hannover braunschweig goettingen bremen nsv",
    },
    ProviderDescriptor {
        id: "svs-sachsen",
        name: "Schachverband Sachsen (SVS)",
        region: "Saxony",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.schachverband-sachsen.de",
        keywords: "sachsen dresden leipzig chemnitz svs",
    },
    ProviderDescriptor {
        id: "bsv-berlin",
        name: "Berliner Schachverband (BMM)",
        region: "Berlin",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.berlinerschachverband.de",
        keywords: "berlin bmm berliner mannschaftsmeisterschaft",
    },
    ProviderDescriptor {
        id: "sbrp-rlp",
        name: "Schachbund Rheinland-Pfalz",
        region: "Rhineland-Palatinate",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.sbrp.de",
        keywords: "rheinland pfalz mainz koblenz trier sbrp",
    },
    ProviderDescriptor {
        id: "oesb-austria",
        name: "Österreichischer Schachbund (ÖSB)",
        region: "Austria",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.chess.at",
        keywords: "oesterreich austria wien graz salzburg bundesliga oesb",
    },
    ProviderDescriptor {
        id: "ssb-switzerland",
        name: "Schweizerischer Schachbund (SSB / SMM)",
        region: "Switzerland",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.swisschess.ch",
        keywords: "schweiz switzerland zurich basel genf smm ssb",
    },
    ProviderDescriptor {
        id: "ffe-france",
        name: "Fédération Française des Échecs (FFE)",
        region: "France",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.echecs.asso.fr",
        keywords: "france paris lyon top16 ffe",
    },
    ProviderDescriptor {
        id: "custom-url",
        name: "Custom URL (.pgn or tournament HTML page)",
        region: "Custom",
        kind: ProviderKind::CustomUrl,
        url: "",
        keywords: "custom url direct pgn link paste html website",
    },
];

#[must_use]
pub fn filter_providers(query: &str) -> Vec<&'static ProviderDescriptor> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if terms.is_empty() {
        return PROVIDER_CATALOG.iter().collect();
    }
    PROVIDER_CATALOG
        .iter()
        .filter(|p| {
            let hay = format!("{} {} {} {}", p.name, p.region, p.url, p.keywords).to_lowercase();
            terms.iter().all(|t| hay.contains(t))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LichessBroadcastRound {
    pub id: String,
    pub name: String,
    pub ongoing: bool,
    pub finished: bool,
    pub pgn_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LichessBroadcastTour {
    pub id: String,
    pub name: String,
    pub location: Option<String>,
    pub info: Option<String>,
    pub all_pgn_url: String,
    pub rounds: Vec<LichessBroadcastRound>,
}

fn push_broadcast_value(value: &Value, out: &mut Vec<LichessBroadcastTour>) {
    let tour = value.get("tour").unwrap_or(value);
    let Some(id) = tour.get("id").and_then(Value::as_str).map(str::trim) else {
        return;
    };
    if id.is_empty() {
        return;
    }
    let name = tour
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(id)
        .to_string();
    let info_obj = tour.get("info");
    let location = info_obj
        .and_then(|v| v.get("location"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut info_parts = Vec::new();
    for key in ["format", "tc"] {
        if let Some(val) = info_obj
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            info_parts.push(val);
        }
    }
    let info = (!info_parts.is_empty()).then(|| info_parts.join(" · "));
    let rounds = value
        .get("rounds")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let rid = r.get("id").and_then(Value::as_str).map(str::trim)?;
                    if rid.is_empty() {
                        return None;
                    }
                    let rname = r
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(rid)
                        .to_string();
                    Some(LichessBroadcastRound {
                        id: rid.to_string(),
                        name: rname,
                        ongoing: r.get("ongoing").and_then(Value::as_bool).unwrap_or(false),
                        finished: r.get("finished").and_then(Value::as_bool).unwrap_or(false),
                        pgn_url: format!("https://lichess.org/api/broadcast/round/{rid}.pgn"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    out.push(LichessBroadcastTour {
        id: id.to_string(),
        name,
        location,
        info,
        all_pgn_url: format!("https://lichess.org/api/broadcast/{id}.pgn"),
        rounds,
    });
}

/// Parse Lichess `/api/broadcast` NDJSON (or `/api/broadcast/search` JSON) payloads.
#[must_use]
pub fn parse_lichess_broadcasts_ndjson(body: &str) -> Vec<LichessBroadcastTour> {
    let mut out = Vec::new();
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return out;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(arr) = value.as_array() {
            for item in arr {
                push_broadcast_value(item, &mut out);
            }
            return out;
        }
        if let Some(arr) = value.get("currentPageResults").and_then(Value::as_array) {
            for item in arr {
                push_broadcast_value(item, &mut out);
            }
            return out;
        }
        let mut grouped = false;
        for group in ["active", "upcoming", "past"] {
            if let Some(arr) = value.get(group).and_then(Value::as_array) {
                grouped = true;
                for item in arr {
                    push_broadcast_value(item, &mut out);
                }
            }
        }
        if grouped {
            return out;
        }
        push_broadcast_value(&value, &mut out);
        return out;
    }

    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            push_broadcast_value(&value, &mut out);
        }
    }
    out
}

fn encode_query_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

pub fn fetch_lichess_broadcasts(
    limit: usize,
    search_query: Option<&str>,
) -> Result<Vec<LichessBroadcastTour>> {
    let q = search_query.map(str::trim).unwrap_or("");
    let url = if q.is_empty() {
        format!(
            "https://lichess.org/api/broadcast?nb={}",
            limit.clamp(1, 100)
        )
    } else {
        format!(
            "https://lichess.org/api/broadcast/search?q={}",
            encode_query_component(q)
        )
    };
    let resp = http_agent()
        .get(&url)
        .set("Accept", "application/x-ndjson, application/json")
        .call()
        .with_context(|| format!("fetching {url}"))?;
    let mut reader = resp.into_reader();
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut raw)
        .with_context(|| format!("reading response from {url}"))?;
    let text = String::from_utf8_lossy(&raw);
    Ok(parse_lichess_broadcasts_ndjson(&text))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredPgnLink {
    pub title: String,
    pub pgn_url: String,
    pub source_label: String,
}

fn url_scheme_and_origin(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = url.strip_prefix("http://") {
        ("http", r)
    } else {
        return None;
    };
    let (authority, path_and_query) = match rest.find(['/', '?', '#']) {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return None;
    }
    Some((scheme, authority, path_and_query))
}

#[must_use]
pub fn resolve_generic_url(base_url: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty()
        || href.starts_with('#')
        || href.to_ascii_lowercase().starts_with("javascript:")
        || href.to_ascii_lowercase().starts_with("mailto:")
    {
        return None;
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_string());
    }
    let (scheme, authority, base_tail) = url_scheme_and_origin(base_url.trim())?;
    if href.starts_with("//") {
        return Some(format!("{scheme}:{href}"));
    }
    if href.starts_with('/') {
        return Some(format!("{scheme}://{authority}{href}"));
    }
    let base_path = base_tail.split(['?', '#']).next().unwrap_or("");
    let base_dir = match base_path.rfind('/') {
        Some(idx) => &base_path[..=idx],
        None => "/",
    };
    let (href_path, href_suffix) = match href.find(['?', '#']) {
        Some(idx) => (&href[..idx], &href[idx..]),
        None => (href, ""),
    };
    let mut stack: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in href_path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    let mut resolved = format!("{scheme}://{authority}/{}", stack.join("/"));
    if href_path.ends_with('/') && !resolved.ends_with('/') {
        resolved.push('/');
    }
    resolved.push_str(href_suffix);
    Some(resolved)
}

fn looks_like_pgn_href(resolved_url: &str, anchor_text: &str) -> bool {
    let lower_url = resolved_url.to_ascii_lowercase();
    let path_only = lower_url.split(['?', '#']).next().unwrap_or("");
    if path_only.ends_with(".pgn")
        || lower_url.contains(".pgn?")
        || lower_url.contains(".pgn&")
        || lower_url.contains("format=pgn")
        || lower_url.contains("output=pgn")
        || lower_url.contains("export=pgn")
    {
        return true;
    }
    let text_lower = anchor_text.to_ascii_lowercase();
    text_lower.contains("pgn")
        && (lower_url.contains("download")
            || lower_url.contains("export")
            || lower_url.contains("/pgn"))
        && !path_only.ends_with(".html")
        && !path_only.ends_with(".htm")
        && !path_only.ends_with(".pdf")
}

/// Extract deduplicated `.pgn` download links from an arbitrary portal HTML page.
#[must_use]
pub fn discover_html_pgn_links(html: &str, page_url: &str) -> Vec<DiscoveredPgnLink> {
    let source_label = url_scheme_and_origin(page_url.trim())
        .map(|(_, host, _)| host.to_string())
        .unwrap_or_else(|| page_url.trim().to_string());
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (_, href, text) in extract_anchors(html) {
        let Some(pgn_url) = resolve_generic_url(page_url, &href) else {
            continue;
        };
        if !looks_like_pgn_href(&pgn_url, &text) {
            continue;
        }
        if !seen.insert(pgn_url.clone()) {
            continue;
        }
        let cleaned = text.trim();
        let title = if cleaned.is_empty()
            || cleaned.eq_ignore_ascii_case("pgn")
            || cleaned.eq_ignore_ascii_case("download")
        {
            pgn_url
                .split(['?', '#'])
                .next()
                .and_then(|p| p.rsplit('/').find(|s| !s.is_empty()))
                .unwrap_or("PGN Download")
                .to_string()
        } else {
            cleaned.to_string()
        };
        out.push(DiscoveredPgnLink {
            title,
            pgn_url,
            source_label: source_label.clone(),
        });
    }
    out
}

fn rewrite_direct_portal_pgn_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let clean = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    if clean.to_ascii_lowercase().ends_with(".pgn") {
        return Some(trimmed.to_string());
    }
    let (_, authority, tail) = url_scheme_and_origin(trimmed)?;
    if !authority.eq_ignore_ascii_case("lichess.org")
        && !authority.eq_ignore_ascii_case("www.lichess.org")
    {
        return None;
    }
    let path = tail.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.first().copied() == Some("study") && segs.len() >= 2 {
        return Some(format!("https://lichess.org/api/study/{}.pgn", segs[1]));
    }
    if segs.first().copied() == Some("broadcast") {
        if segs.len() >= 4 {
            return Some(format!(
                "https://lichess.org/api/broadcast/round/{}.pgn",
                segs[3]
            ));
        }
        if segs.len() == 3 {
            return Some(format!("https://lichess.org/api/broadcast/{}.pgn", segs[2]));
        }
    }
    None
}

/// Fetch a portal URL or pasted `.pgn` URL and return all downloadable PGN links found.
pub fn fetch_portal_pgn_links(portal_url: &str) -> Result<Vec<DiscoveredPgnLink>> {
    let trimmed = portal_url.trim();
    if trimmed.is_empty() {
        bail!("Enter a portal or PGN URL first.");
    }
    let normalized = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let source_label = url_scheme_and_origin(&normalized)
        .map(|(_, host, _)| host.to_string())
        .unwrap_or_else(|| normalized.clone());

    if let Some(direct) = rewrite_direct_portal_pgn_url(&normalized) {
        let title = direct
            .split(['?', '#'])
            .next()
            .and_then(|p| p.rsplit('/').find(|s| !s.is_empty()))
            .unwrap_or("Direct PGN")
            .to_string();
        return Ok(vec![DiscoveredPgnLink {
            title,
            pgn_url: direct,
            source_label,
        }]);
    }

    let bytes = fetch_bytes(&normalized)?;
    let text = decode_web_text(&bytes);
    let preview = text.trim_start();
    if preview.starts_with("[Event ")
        || preview.starts_with("[Site ")
        || preview.starts_with("[White ")
        || (preview.starts_with("1.") && !preview.contains("<html"))
    {
        let title = normalized
            .split(['?', '#'])
            .next()
            .and_then(|p| p.rsplit('/').find(|s| !s.is_empty()))
            .unwrap_or("Direct PGN")
            .to_string();
        return Ok(vec![DiscoveredPgnLink {
            title,
            pgn_url: normalized,
            source_label,
        }]);
    }

    let links = discover_html_pgn_links(&text, &normalized);
    Ok(links)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigaManagerLeague {
    pub district: String,
    pub name: String,
    /// Path prefix without trailing `/spielplan`, e.g. `"/bsb/2026-2027/oberliga-2571"`.
    pub path_prefix: String,
    pub teams_searchable: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LigaManagerDirectory {
    pub current_season: String,
    pub seasons: Vec<String>,
    pub leagues: Vec<LigaManagerLeague>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigaManagerRoundLink {
    pub label: String,
    pub games_hint: Option<String>,
    pub pgn_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LigaManagerDownloads {
    pub title: String,
    pub season: String,
    pub all_pgn_url: Option<String>,
    pub rounds: Vec<LigaManagerRoundLink>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DsbLeagueEntry {
    pub code: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DsbSeasonEntry {
    pub label: String,
    /// Prefix relative to DSB root, e.g. `""` for current season or `"saison2025-26/"`.
    pub prefix: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DsbDirectory {
    pub seasons: Vec<DsbSeasonEntry>,
    pub leagues: Vec<DsbLeagueEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DsbLeaguePgn {
    pub league_name: String,
    pub pgn_url: String,
    pub last_modified: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedLeagueUrl {
    DirectPgn {
        portal: Option<LeaguePortal>,
        label: String,
        pgn_url: String,
    },
    LigaManagerLeague {
        path_prefix: String,
    },
    DsbLeague {
        season_prefix: String,
        league_code: String,
    },
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
}

fn fetch_html(url: &str) -> Result<String> {
    let bytes = fetch_bytes(url)?;
    Ok(decode_web_text(&bytes))
}

fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    let resp = http_agent()
        .get(url)
        .call()
        .with_context(|| format!("fetching {url}"))?;
    let mut reader = resp.into_reader();
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut out)
        .with_context(|| format!("reading response from {url}"))?;
    Ok(out)
}

const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

/// Decode raw web/PGN bytes stripping any UTF-8 BOM and converting Windows-1252 / Latin-1 to UTF-8 if needed.
#[must_use]
pub fn decode_pgn_bytes_utf8(raw: &[u8]) -> Vec<u8> {
    let trimmed = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
    if std::str::from_utf8(trimmed).is_ok() {
        return trimmed.to_vec();
    }
    let mut out = String::with_capacity(trimmed.len() + trimmed.len() / 8);
    for &b in trimmed {
        match b {
            0x80..=0x9f => out.push(CP1252_HIGH[(b - 0x80) as usize]),
            _ => out.push(b as char),
        }
    }
    out.into_bytes()
}

#[must_use]
fn decode_web_text(raw: &[u8]) -> String {
    let utf8 = decode_pgn_bytes_utf8(raw);
    String::from_utf8_lossy(&utf8).into_owned()
}

fn decode_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&ndash;", "-")
        .replace("&mdash;", "-")
        .replace("&auml;", "ä")
        .replace("&ouml;", "ö")
        .replace("&uuml;", "ü")
        .replace("&Auml;", "Ä")
        .replace("&Ouml;", "Ö")
        .replace("&Uuml;", "Ü")
        .replace("&szlig;", "ß")
}

fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => {
                in_tag = true;
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn attr_value(tag_inner: &str, attr: &str) -> Option<String> {
    let lower = tag_inner.to_ascii_lowercase();
    let needle = format!("{attr}=");
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find(&needle) {
        let idx = pos + rel;
        let boundary_ok = idx == 0 || lower.as_bytes()[idx - 1].is_ascii_whitespace();
        if !boundary_ok {
            pos = idx + needle.len();
            continue;
        }
        let rest = tag_inner[idx + needle.len()..].trim_start();
        if let Some(after_q) = rest.strip_prefix('"') {
            let end = after_q.find('"')?;
            return Some(decode_entities(&after_q[..end]));
        }
        if let Some(after_q) = rest.strip_prefix('\'') {
            let end = after_q.find('\'')?;
            return Some(decode_entities(&after_q[..end]));
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        return Some(decode_entities(&rest[..end]));
    }
    None
}

fn extract_anchors(html: &str) -> Vec<(usize, String, String)> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find("<a") {
        let start = pos + rel;
        let next_ch = html[start + 2..].chars().next();
        if !next_ch.is_some_and(|c| c.is_whitespace() || c == '>') {
            pos = start + 2;
            continue;
        }
        let Some(tag_close_rel) = html[start..].find('>') else {
            break;
        };
        let tag_end = start + tag_close_rel;
        let tag_inner = &html[start + 2..tag_end];
        let body_start = tag_end + 1;
        let Some(close_rel) = lower[body_start..].find("</a>") else {
            break;
        };
        let body_end = body_start + close_rel;
        if let Some(href) = attr_value(tag_inner, "href") {
            let text = strip_tags(&html[body_start..body_end]);
            out.push((start, href, text));
        }
        pos = body_end + 4;
    }
    out
}

fn is_season_slug(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    bytes.len() == 9
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..].iter().all(u8::is_ascii_digit)
}

#[must_use]
pub fn resolve_ligamanager_url(href: &str) -> String {
    let trimmed = href.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else if trimmed.starts_with('/') {
        format!("{LIGAMANAGER_BASE_URL}{trimmed}")
    } else {
        format!("{LIGAMANAGER_BASE_URL}/{trimmed}")
    }
}

/// Parse LigaManager Bayern `/YYYY-YYYY/gesamt` directory HTML into seasons & leagues.
#[must_use]
pub fn parse_ligamanager_directory(html: &str) -> LigaManagerDirectory {
    let lower = html.to_ascii_lowercase();
    let mut seasons_set = BTreeSet::new();
    let mut current_season = String::new();

    if let Some(canon_at) = lower.find("rel=\"canonical\"")
        && let Some(line_start) = lower[..canon_at].rfind('<')
        && let Some(line_end_rel) = html[line_start..].find('>')
        && let Some(href) = attr_value(&html[line_start..line_start + line_end_rel], "href")
    {
        for part in href.split('/') {
            if is_season_slug(part) {
                current_season = part.to_string();
                seasons_set.insert(part.to_string());
                break;
            }
        }
    }

    for (_, href, _) in extract_anchors(html) {
        if let Some(rest) = href.strip_prefix('/')
            && let Some((seg, tail)) = rest.split_once('/')
            && is_season_slug(seg)
            && tail.starts_with("gesamt")
        {
            seasons_set.insert(seg.to_string());
        }
    }

    // Collect `<h2 ...>` card headers for district/association names.
    let mut card_titles: Vec<(usize, String)> = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find("<h2") {
        let start = pos + rel;
        let Some(gt) = html[start..].find('>') else {
            break;
        };
        let body_start = start + gt + 1;
        let Some(end_rel) = lower[body_start..].find("</h2>") else {
            break;
        };
        let title = strip_tags(&html[body_start..body_start + end_rel]);
        if !title.is_empty() {
            card_titles.push((start, title));
        }
        pos = body_start + end_rel + 5;
    }

    let mut leagues = Vec::new();
    let mut seen_prefixes = BTreeSet::new();
    let mut row_pos = 0usize;
    while let Some(rel) = lower[row_pos..].find("gesamt-league-row") {
        let marker = row_pos + rel;
        let tr_start = lower[..marker].rfind("<tr").unwrap_or(marker);
        let Some(tr_tag_end_rel) = html[tr_start..].find('>') else {
            break;
        };
        let tr_tag = &html[tr_start + 3..tr_start + tr_tag_end_rel];
        let tr_close_rel = lower[tr_start..].find("</tr>").unwrap_or(1024);
        let tr_end = (tr_start + tr_close_rel + 5).min(html.len());
        let row_html = &html[tr_start..tr_end];

        let data_team = attr_value(tr_tag, "data-team").unwrap_or_default();
        let data_league = attr_value(tr_tag, "data-league").unwrap_or_default();
        let district = card_titles
            .iter()
            .rfind(|(hpos, _)| *hpos <= tr_start)
            .map(|(_, title)| title.clone())
            .unwrap_or_default();

        for (_, href, text) in extract_anchors(row_html) {
            if let Some(prefix) = href.strip_suffix("/spielplan")
                && seen_prefixes.insert(prefix.to_string())
            {
                if current_season.is_empty() {
                    for part in prefix.split('/') {
                        if is_season_slug(part) {
                            current_season = part.to_string();
                            break;
                        }
                    }
                }
                let name = if text.is_empty() {
                    data_league.clone()
                } else {
                    text
                };
                leagues.push(LigaManagerLeague {
                    district: district.clone(),
                    name,
                    path_prefix: prefix.to_string(),
                    teams_searchable: data_team.clone(),
                });
                break;
            }
        }
        row_pos = tr_end.max(marker + 17);
    }

    let seasons: Vec<String> = seasons_set.into_iter().rev().collect();
    if current_season.is_empty()
        && let Some(first) = seasons.first()
    {
        current_season = first.clone();
    }
    LigaManagerDirectory {
        current_season,
        seasons,
        leagues,
    }
}

/// Filter LigaManager leagues by space-separated search terms matching district,
/// league name, URL path, or club/team names.
#[must_use]
pub fn filter_ligamanager_leagues<'a>(
    leagues: &'a [LigaManagerLeague],
    query: &str,
) -> Vec<&'a LigaManagerLeague> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    leagues
        .iter()
        .filter(|item| {
            if terms.is_empty() {
                return true;
            }
            let hay = format!(
                "{} {} {} {}",
                item.district, item.name, item.path_prefix, item.teams_searchable
            )
            .to_lowercase();
            terms.iter().all(|term| hay.contains(term))
        })
        .collect()
}

/// Parse `/{verband}/{season}/{slug}/partien/download` HTML into `alle.pgn` and `runde-N.pgn` links.
#[must_use]
pub fn parse_ligamanager_downloads(html: &str) -> LigaManagerDownloads {
    let lower = html.to_ascii_lowercase();
    let mut title = String::new();
    if let Some(h1_at) = lower.find("<h1")
        && let Some(gt) = html[h1_at..].find('>')
        && let Some(end_rel) = lower[h1_at + gt + 1..].find("</h1>")
    {
        title = strip_tags(&html[h1_at + gt + 1..h1_at + gt + 1 + end_rel]);
        if let Some(stripped) = title.strip_suffix("- Download") {
            title = stripped.trim().to_string();
        }
    }
    let mut season = String::new();
    for part in html.split('/') {
        if is_season_slug(part) {
            season = part.to_string();
            break;
        }
    }

    let mut all_pgn_url = None;
    let mut rounds = Vec::new();
    let mut seen_rounds = BTreeSet::new();

    for (pos, href, _) in extract_anchors(html) {
        let clean = href.split('?').next().unwrap_or(&href);
        if !clean.to_ascii_lowercase().ends_with(".pgn") {
            continue;
        }
        let url = resolve_ligamanager_url(&href);
        if clean.to_ascii_lowercase().ends_with("/alle.pgn") {
            all_pgn_url = Some(url);
            continue;
        }
        if !seen_rounds.insert(url.clone()) {
            continue;
        }
        // Extract label and game count hint from surrounding `<li>...</li>`.
        let li_start = lower[..pos]
            .rfind("<li")
            .unwrap_or_else(|| pos.saturating_sub(300));
        let li_text = strip_tags(&html[li_start..pos]);
        let mut label = clean
            .rsplit('/')
            .next()
            .unwrap_or("Runde")
            .trim_end_matches(".pgn")
            .replace('-', " ");
        let mut games_hint = None;
        if let Some(at) = li_text.find("Runde ") {
            let tail = &li_text[at..];
            let tokens: Vec<&str> = tail.split_whitespace().collect();
            if tokens.len() >= 2 {
                label = format!("{} {}", tokens[0], tokens[1]);
            }
            if let Some(p_idx) = tokens.iter().position(|t| t.starts_with("Part"))
                && p_idx > 0
            {
                games_hint = Some(format!("{} Partien", tokens[p_idx - 1]));
            }
        }
        rounds.push(LigaManagerRoundLink {
            label,
            games_hint,
            pgn_url: url,
        });
    }

    LigaManagerDownloads {
        title,
        season,
        all_pgn_url,
        rounds,
    }
}

fn extract_select_options(html: &str, select_id: &str) -> Vec<(String, String)> {
    let lower = html.to_ascii_lowercase();
    let marker = format!("id=\"{select_id}\"");
    let Some(id_at) = lower
        .find(&marker)
        .or_else(|| lower.find(&format!("name=\"{select_id}\"")))
    else {
        return Vec::new();
    };
    let select_start = lower[..id_at].rfind("<select").unwrap_or(id_at);
    let Some(end_rel) = lower[select_start..].find("</select>") else {
        return Vec::new();
    };
    let block = &html[select_start..select_start + end_rel];
    let block_lower = block.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = block_lower[pos..].find("<option") {
        let start = pos + rel;
        let Some(gt) = block[start..].find('>') else {
            break;
        };
        let attrs = &block[start + 7..start + gt];
        let body_start = start + gt + 1;
        let close_rel = block_lower[body_start..].find("</option>").unwrap_or(0);
        let label = strip_tags(&block[body_start..body_start + close_rel]);
        if let Some(val) = attr_value(attrs, "value")
            && !val.trim().is_empty()
        {
            out.push((val.trim().to_string(), label));
        }
        pos = body_start + close_rel;
    }
    out
}

/// Resolve a relative DSB `href` (such as `./pgn/bl2627.pgn?v=123` or `../pgn/bl2526.pgn?v=123`)
/// against `{DSB_BASE_URL}/{season_prefix}`.
#[must_use]
pub fn resolve_dsb_relative_url(season_prefix: &str, href: &str) -> String {
    let trimmed = href.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }
    if trimmed.starts_with("//") {
        return format!("https:{trimmed}");
    }
    if trimmed.starts_with('/') {
        return format!("{DSB_BASE_URL}{trimmed}");
    }
    let mut base_segments: Vec<&str> = season_prefix
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let mut path_part = trimmed;
    let mut query_part = "";
    if let Some((p, q)) = trimmed.split_once('?') {
        path_part = p;
        query_part = q;
    }
    for seg in path_part.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                base_segments.pop();
            }
            other => base_segments.push(other),
        }
    }
    let joined = base_segments.join("/");
    if query_part.is_empty() {
        format!("{DSB_BASE_URL}/{joined}")
    } else {
        format!("{DSB_BASE_URL}/{joined}?{query_part}")
    }
}

/// Parse DSB Ergebnisdienst `bedh.php` directory HTML into seasons and leagues.
#[must_use]
pub fn parse_dsb_directory(html: &str, current_season_prefix: &str) -> DsbDirectory {
    let mut seasons = Vec::new();
    for (val, label) in extract_select_options(html, "auswahl_saison") {
        let resolved = resolve_dsb_relative_url(current_season_prefix, &val);
        let rel = resolved
            .strip_prefix(DSB_BASE_URL)
            .unwrap_or(&resolved)
            .trim_start_matches('/');
        let prefix = if let Some(idx) = rel.find("bedh.php") {
            rel[..idx].to_string()
        } else {
            String::new()
        };
        if !seasons.iter().any(|s: &DsbSeasonEntry| s.prefix == prefix) {
            seasons.push(DsbSeasonEntry { label, prefix });
        }
    }

    let mut leagues = Vec::new();
    for (val, name) in extract_select_options(html, "auswahl_liga") {
        if let Some((_, query)) = val.split_once('?') {
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=')
                    && k.eq_ignore_ascii_case("liga")
                    && !v.trim().is_empty()
                    && !leagues.iter().any(|l: &DsbLeagueEntry| l.code == v.trim())
                {
                    leagues.push(DsbLeagueEntry {
                        code: v.trim().to_string(),
                        name: name.clone(),
                    });
                }
            }
        }
    }

    DsbDirectory { seasons, leagues }
}

/// Filter DSB leagues by space-separated search terms matching league code or name.
#[must_use]
pub fn filter_dsb_leagues<'a>(
    leagues: &'a [DsbLeagueEntry],
    query: &str,
) -> Vec<&'a DsbLeagueEntry> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    leagues
        .iter()
        .filter(|item| {
            if terms.is_empty() {
                return true;
            }
            let hay = format!("{} {}", item.code, item.name).to_lowercase();
            terms.iter().all(|t| hay.contains(t))
        })
        .collect()
}

/// Parse DSB Ergebnisdienst `bedh.php?liga=...` HTML for the PGN download URL.
#[must_use]
pub fn parse_dsb_league_pgn(html: &str, season_prefix: &str) -> Option<DsbLeaguePgn> {
    let lower = html.to_ascii_lowercase();
    let mut league_name = String::new();
    if let Some(h2_at) = lower.find("<h2")
        && let Some(gt) = html[h2_at..].find('>')
        && let Some(end_rel) = lower[h2_at + gt + 1..].find("</h2>")
    {
        league_name = strip_tags(&html[h2_at + gt + 1..h2_at + gt + 1 + end_rel]);
    }
    if league_name.is_empty() {
        for (val, label) in extract_select_options(html, "auswahl_liga") {
            let idx = lower.find(&val.to_ascii_lowercase());
            let _ = idx;
            if !label.is_empty() && league_name.is_empty() {
                league_name = label;
                break;
            }
        }
    }

    for (pos, href, _) in extract_anchors(html) {
        let path_only = href.split('?').next().unwrap_or(&href);
        if path_only.to_ascii_lowercase().ends_with(".pgn") {
            let pgn_url = resolve_dsb_relative_url(season_prefix, &href);
            let tail_end = (pos + 220).min(html.len());
            let tail_text = strip_tags(&html[pos..tail_end]);
            let last_modified = tail_text
                .find("letzte Änderung")
                .map(|i| tail_text[i..].trim().to_string());
            return Some(DsbLeaguePgn {
                league_name,
                pgn_url,
                last_modified,
            });
        }
    }
    None
}

/// Resolve any user-pasted LigaManager Bayern or DSB Ergebnisdienst URL.
pub fn resolve_league_url(input: &str) -> Result<ResolvedLeagueUrl> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("Enter a LigaManager Bayern or DSB Ergebnisdienst URL.");
    }
    let normalized = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let after_scheme = normalized
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&normalized);
    let (host_raw, path_and_query) = match after_scheme.find('/') {
        Some(idx) => (&after_scheme[..idx], &after_scheme[idx..]),
        None => (after_scheme, "/"),
    };
    let host = host_raw.to_ascii_lowercase();
    let (path_only, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));

    let portal = if host.ends_with("ligamanager.schachbund-bayern.de") {
        Some(LeaguePortal::LigaManagerBayern)
    } else if host.ends_with("ergebnisdienst.schachbund.de") {
        Some(LeaguePortal::DsbErgebnisdienst)
    } else {
        None
    };

    if path_only.to_ascii_lowercase().ends_with(".pgn") {
        let label = path_only
            .rsplit('/')
            .next()
            .unwrap_or("league.pgn")
            .to_string();
        return Ok(ResolvedLeagueUrl::DirectPgn {
            portal,
            label,
            pgn_url: normalized,
        });
    }

    match portal {
        Some(LeaguePortal::LigaManagerBayern) => {
            let segs: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();
            if segs.len() >= 3 && is_season_slug(segs[1]) {
                let prefix = format!("/{}/{}/{}", segs[0], segs[1], segs[2]);
                return Ok(ResolvedLeagueUrl::LigaManagerLeague {
                    path_prefix: prefix,
                });
            }
            bail!(
                "Paste a LigaManager league URL such as https://ligamanager.schachbund-bayern.de/bsb/2026-2027/oberliga-2571/spielplan or a direct .pgn link."
            );
        }
        Some(LeaguePortal::DsbErgebnisdienst) => {
            let mut league_code = String::new();
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=')
                    && k.eq_ignore_ascii_case("liga")
                    && !v.trim().is_empty()
                {
                    league_code = v.trim().to_string();
                    break;
                }
            }
            if league_code.is_empty() {
                bail!(
                    "DSB Ergebnisdienst URL must contain `?liga=CODE` (e.g. bedh.php?liga=bl) or end in .pgn."
                );
            }
            let segs: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();
            let season_prefix = if segs.len() >= 2 && segs[0].starts_with("saison") {
                format!("{}/", segs[0])
            } else {
                String::new()
            };
            Ok(ResolvedLeagueUrl::DsbLeague {
                season_prefix,
                league_code,
            })
        }
        None => {
            bail!(
                "Unsupported league URL host '{host}'. Supported portals: ligamanager.schachbund-bayern.de and ergebnisdienst.schachbund.de (or any direct .pgn URL)."
            );
        }
    }
}

pub fn fetch_ligamanager_directory(season: Option<&str>) -> Result<LigaManagerDirectory> {
    let url = match season.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("{LIGAMANAGER_BASE_URL}/{s}/gesamt"),
        None => format!("{LIGAMANAGER_BASE_URL}/"),
    };
    let html = fetch_html(&url)?;
    let dir = parse_ligamanager_directory(&html);
    if dir.leagues.is_empty() {
        bail!("No leagues found in LigaManager directory at {url}");
    }
    Ok(dir)
}

pub fn fetch_ligamanager_downloads(path_prefix: &str) -> Result<LigaManagerDownloads> {
    let prefix = path_prefix.trim();
    let url = format!(
        "{}/{}/partien/download",
        LIGAMANAGER_BASE_URL,
        prefix.trim_start_matches('/')
    );
    let html = fetch_html(&url)?;
    let dl = parse_ligamanager_downloads(&html);
    if dl.all_pgn_url.is_none() && dl.rounds.is_empty() {
        bail!("No PGN downloads are published yet at {url}");
    }
    Ok(dl)
}

pub fn fetch_dsb_directory(season_prefix: &str) -> Result<DsbDirectory> {
    let prefix = season_prefix.trim_start_matches('/');
    let url = format!("{DSB_BASE_URL}/{prefix}bedh.php?liga=bl");
    let html = fetch_html(&url)?;
    let dir = parse_dsb_directory(&html, prefix);
    if dir.leagues.is_empty() {
        bail!("No leagues found in DSB Ergebnisdienst at {url}");
    }
    Ok(dir)
}

pub fn fetch_dsb_league_pgn(season_prefix: &str, league_code: &str) -> Result<DsbLeaguePgn> {
    let prefix = season_prefix.trim_start_matches('/');
    let url = format!("{DSB_BASE_URL}/{prefix}bedh.php?liga={league_code}");
    let html = fetch_html(&url)?;
    parse_dsb_league_pgn(&html, prefix)
        .ok_or_else(|| anyhow!("No PGN download link found on DSB Ergebnisdienst page {url}"))
}

/// Filter parsed league PGN games by optional comma/semicolon-separated player or
/// club/team queries (a game matches if ANY clause has all whitespace terms in
/// `White`, `Black`, `WhiteTeam`, `BlackTeam`, `Event`, or `Site`).
#[must_use]
pub fn filter_league_games(games: Vec<Game>, filter_expr: &str) -> Vec<Game> {
    let clauses: Vec<Vec<String>> = filter_expr
        .split([';', '|'])
        .map(|clause| {
            clause
                .split_whitespace()
                .map(|w| w.trim_matches(',').to_lowercase())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|terms| !terms.is_empty())
        .collect();
    if clauses.is_empty() {
        return games;
    }
    games
        .into_iter()
        .filter(|g| {
            let hay = format!(
                "{} {} {} {} {} {}",
                g.white(),
                g.black(),
                g.header("WhiteTeam").unwrap_or(""),
                g.header("BlackTeam").unwrap_or(""),
                g.event(),
                g.header("Site").unwrap_or(""),
            )
            .to_lowercase();
            clauses
                .iter()
                .any(|terms| terms.iter().all(|term| hay.contains(term)))
        })
        .collect()
}

pub fn parse_league_pgn_bytes(raw: &[u8], filter_expr: &str) -> Result<Vec<Game>> {
    let utf8 = decode_pgn_bytes_utf8(raw);
    let games = parse_pgn(&utf8)?;
    Ok(filter_league_games(games, filter_expr))
}

pub fn download_and_parse_league_pgn(
    pgn_url: &str,
    filter_expr: &str,
) -> Result<(usize, Vec<Game>)> {
    let raw = fetch_bytes(pgn_url)?;
    let utf8 = decode_pgn_bytes_utf8(&raw);
    let all_games = parse_pgn(&utf8)?;
    let total_downloaded = all_games.len();
    let filtered = filter_league_games(all_games, filter_expr);
    Ok((total_downloaded, filtered))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_LM_DIR: &str = r#"<!DOCTYPE html>
<html><head>
<link rel="canonical" href="https://ligamanager.schachbund-bayern.de/2026-2027/gesamt">
</head><body>
<a href="/2026-2027/gesamt">2026/2027</a>
<a href="/2025-2026/gesamt">2025/2026</a>
<div class="liga-card">
  <h2 class="card-title"><a href="/bsb/2026-2027/uebersicht">200 Bayerischer Schachbund e.V.</a></h2>
  <table>
    <tr class="gesamt-league-row" data-league="oberliga" data-team="sc erlangen 1 sk passau 1 fc bayern münchen 2">
      <td><a href="/bsb/2026-2027/oberliga-2571/spielplan">Oberliga</a></td>
    </tr>
  </table>
</div>
<div class="liga-card">
  <h2 class="card-title"><a href="/mfr/2026-2027/uebersicht">Mittelfranken</a></h2>
  <table>
    <tr class="gesamt-league-row" data-league="bezirksliga 1" data-team="sc uffenheim 1 sg fürth 3">
      <td><a href="/mfr/2026-2027/bezirksliga-1-2602/spielplan">Bezirksliga 1</a></td>
    </tr>
  </table>
</div>
</body></html>"#;

    const SAMPLE_LM_DL: &str = r#"<html><body>
<div class="liga-page-heading">
  <h1 class="h2 mb-1">Oberliga - Download</h1>
</div>
<a href="/bsb/2025-2026/oberliga-2486/partien/download/alle.pgn">Alle Partien herunterladen</a>
<ul>
  <li id="runde-1">
    <div><span class="fw-semibold">Runde 1</span><span>40 Partien</span></div>
    <a href="/bsb/2025-2026/oberliga-2486/partien/download/runde-1.pgn">PGN</a>
  </li>
  <li id="runde-2">
    <div><span class="fw-semibold">Runde 2</span><span>38 Partien</span></div>
    <a href="/bsb/2025-2026/oberliga-2486/partien/download/runde-2.pgn">PGN</a>
  </li>
</ul>
</body></html>"#;

    const SAMPLE_DSB_DIR: &str = r#"<html><body>
<h2>Bundesliga</h2>
<select name="auswahl_liga" id="auswahl_liga">
  <option value="bedh.php?liga=bl" selected="selected">Bundesliga</option>
  <option value="bedh.php?liga=olby">Oberliga Bayern</option>
</select>
<select name="auswahl_saison" id="auswahl_saison">
  <option value="./bedh.php?liga=bl" selected="selected">2026/27</option>
  <option value="./saison2025-26/bedh.php?liga=bl">2025/26</option>
</select>
<p><a href="./pgn/bl2627.pgn?v=1795114939">Download der Partien (PGN)</a> - letzte Änderung 19.11.26 20:02</p>
</body></html>"#;

    #[test]
    fn parses_ligamanager_directory_and_filters_by_club() {
        let dir = parse_ligamanager_directory(SAMPLE_LM_DIR);
        assert_eq!(dir.current_season, "2026-2027");
        assert_eq!(dir.seasons, vec!["2026-2027", "2025-2026"]);
        assert_eq!(dir.leagues.len(), 2);
        assert_eq!(dir.leagues[0].path_prefix, "/bsb/2026-2027/oberliga-2571");
        assert_eq!(dir.leagues[1].district, "Mittelfranken");

        let found = filter_ligamanager_leagues(&dir.leagues, "passau");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Oberliga");
    }

    #[test]
    fn parses_ligamanager_downloads_page() {
        let dl = parse_ligamanager_downloads(SAMPLE_LM_DL);
        assert_eq!(dl.title, "Oberliga");
        assert_eq!(dl.season, "2025-2026");
        assert_eq!(
            dl.all_pgn_url.as_deref(),
            Some(
                "https://ligamanager.schachbund-bayern.de/bsb/2025-2026/oberliga-2486/partien/download/alle.pgn"
            )
        );
        assert_eq!(dl.rounds.len(), 2);
        assert_eq!(dl.rounds[0].label, "Runde 1");
        assert_eq!(dl.rounds[0].games_hint.as_deref(), Some("40 Partien"));
    }

    #[test]
    fn parses_dsb_directory_and_relative_pgn_urls() {
        let dir = parse_dsb_directory(SAMPLE_DSB_DIR, "");
        assert_eq!(dir.seasons.len(), 2);
        assert_eq!(dir.seasons[0].prefix, "");
        assert_eq!(dir.seasons[1].prefix, "saison2025-26/");
        assert_eq!(dir.leagues.len(), 2);
        assert_eq!(dir.leagues[1].code, "olby");

        let pgn = parse_dsb_league_pgn(SAMPLE_DSB_DIR, "").unwrap();
        assert_eq!(
            pgn.pgn_url,
            "https://ergebnisdienst.schachbund.de/pgn/bl2627.pgn?v=1795114939"
        );
        assert!(pgn.last_modified.unwrap().contains("19.11.26"));

        let archive_url =
            resolve_dsb_relative_url("saison2025-26/", "../pgn/bl2526.pgn?v=1777241450");
        assert_eq!(
            archive_url,
            "https://ergebnisdienst.schachbund.de/pgn/bl2526.pgn?v=1777241450"
        );
    }

    #[test]
    fn resolves_pasted_league_urls() {
        assert_eq!(
            resolve_league_url(
                "https://ligamanager.schachbund-bayern.de/bsb/2026-2027/oberliga-2571/spielplan"
            )
            .unwrap(),
            ResolvedLeagueUrl::LigaManagerLeague {
                path_prefix: "/bsb/2026-2027/oberliga-2571".to_string()
            }
        );
        assert_eq!(
            resolve_league_url(
                "https://ergebnisdienst.schachbund.de/saison2025-26/bedh.php?liga=olby"
            )
            .unwrap(),
            ResolvedLeagueUrl::DsbLeague {
                season_prefix: "saison2025-26/".to_string(),
                league_code: "olby".to_string()
            }
        );
        assert!(matches!(
            resolve_league_url("https://ergebnisdienst.schachbund.de/pgn/bl2627.pgn").unwrap(),
            ResolvedLeagueUrl::DirectPgn {
                portal: Some(LeaguePortal::DsbErgebnisdienst),
                ..
            }
        ));
    }

    #[test]
    fn strips_bom_and_filters_by_player_or_team() {
        let mut pgn = vec![0xEF, 0xBB, 0xBF];
        pgn.extend_from_slice(
            r#"[Event "SK Passau 1-SF Augsburg 1"]
[White "Hinterreiter, Markus"]
[Black "Vuckovic, Zarko"]
[WhiteTeam "SK Passau 1"]
[BlackTeam "SF Augsburg 1"]
[Result "0-1"]

1. d4 d5 0-1

[Event "FC Bayern München 2-SC Erlangen 1"]
[White "Mustermann, Max"]
[Black "Beispiel, Erika"]
[WhiteTeam "FC Bayern München 2"]
[BlackTeam "SC Erlangen 1"]
[Result "1-0"]

1. e4 e5 1-0
"#
            .as_bytes(),
        );
        let filtered = parse_league_pgn_bytes(&pgn, "Passau").unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].white(), "Hinterreiter, Markus");

        let by_multi_name = parse_league_pgn_bytes(&pgn, "Mustermann; Vuckovic").unwrap();
        assert_eq!(by_multi_name.len(), 2);
    }
}

#[cfg(test)]
mod provider_portal_tests {
    use super::*;

    #[test]
    fn catalog_filters_regional_and_international_providers() {
        let all = filter_providers("");
        assert!(all.iter().any(|p| p.id == "ligamanager-bayern"));
        assert!(all.iter().any(|p| p.id == "dsb-ergebnisdienst"));
        assert!(all.iter().any(|p| p.id == "lichess-broadcasts"));
        assert!(all.iter().any(|p| p.id == "custom-url"));

        let bsb = filter_providers("bayern bsb");
        assert_eq!(bsb.len(), 1);
        assert_eq!(bsb[0].id, "ligamanager-bayern");

        let bundesliga = filter_providers("bundesliga");
        assert!(bundesliga.iter().any(|p| p.id == "dsb-ergebnisdienst"));
        assert!(bundesliga.iter().any(|p| p.id == "schachbundesliga"));

        let austria = filter_providers("austria");
        assert!(austria.iter().any(|p| p.id == "oesb-austria"));
    }

    #[test]
    fn parses_lichess_broadcasts_ndjson_and_search_results() {
        let ndjson = concat!(
            r#"{"tour":{"id":"tour1111","name":"Tata Steel 2026","info":{"location":"Wijk aan Zee","format":"14-player RR","tc":"Classical"}},"rounds":[{"id":"rnd00001","name":"Round 1","ongoing":false,"finished":true},{"id":"rnd00002","name":"Round 2","ongoing":true,"finished":false}]}"#,
            "\n\n",
            r#"{"tour":{"id":"tour2222","name":"FIDE Candidates"},"rounds":[]}"#
        );
        let tours = parse_lichess_broadcasts_ndjson(ndjson);
        assert_eq!(tours.len(), 2);
        assert_eq!(tours[0].id, "tour1111");
        assert_eq!(tours[0].name, "Tata Steel 2026");
        assert_eq!(tours[0].location.as_deref(), Some("Wijk aan Zee"));
        assert_eq!(tours[0].info.as_deref(), Some("14-player RR · Classical"));
        assert_eq!(
            tours[0].all_pgn_url,
            "https://lichess.org/api/broadcast/tour1111.pgn"
        );
        assert_eq!(tours[0].rounds.len(), 2);
        assert_eq!(
            tours[0].rounds[0].pgn_url,
            "https://lichess.org/api/broadcast/round/rnd00001.pgn"
        );
        assert!(tours[0].rounds[0].finished);
        assert!(!tours[0].rounds[0].ongoing);
        assert!(tours[0].rounds[1].ongoing);
        assert_eq!(tours[1].name, "FIDE Candidates");

        let search_json = r#"{"currentPageResults":[{"tour":{"id":"srch1","name":"Bundesliga"},"rounds":[{"id":"srchr1","name":"Runde 1"}]}]}"#;
        let search_tours = parse_lichess_broadcasts_ndjson(search_json);
        assert_eq!(search_tours.len(), 1);
        assert_eq!(search_tours[0].rounds[0].name, "Runde 1");
    }

    #[test]
    fn resolves_urls_and_discovers_html_pgn_links() {
        assert_eq!(
            resolve_generic_url(
                "https://www.schachbund-nrw.de/liga/index.html",
                "pgn/r1.pgn"
            )
            .as_deref(),
            Some("https://www.schachbund-nrw.de/liga/pgn/r1.pgn")
        );
        assert_eq!(
            resolve_generic_url(
                "https://www.chess.at/turniere/open/",
                "../files/open.pgn?v=2"
            )
            .as_deref(),
            Some("https://www.chess.at/turniere/files/open.pgn?v=2")
        );
        assert_eq!(resolve_generic_url("https://example.org/", "#top"), None);
        assert_eq!(
            resolve_generic_url("https://example.org/", "javascript:void(0)"),
            None
        );

        let html = r#"
            <html><body>
                <a href="news.html">News</a>
                <a href="/downloads/oberliga_r1.pgn">Oberliga Round 1</a>
                <a href="/downloads/oberliga_r1.pgn">Duplicate link</a>
                <a href="files/all_games.pgn?dl=1">PGN</a>
                <a href="/export.php?format=pgn&id=7">Export Tournament</a>
                <a href="bulletin.pdf">Bulletin PDF</a>
            </body></html>
        "#;
        let links = discover_html_pgn_links(html, "https://www.example-chess.de/2026/table.html");
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].title, "Oberliga Round 1");
        assert_eq!(
            links[0].pgn_url,
            "https://www.example-chess.de/downloads/oberliga_r1.pgn"
        );
        assert_eq!(links[0].source_label, "www.example-chess.de");
        assert_eq!(links[1].title, "all_games.pgn");
        assert_eq!(
            links[1].pgn_url,
            "https://www.example-chess.de/2026/files/all_games.pgn?dl=1"
        );
        assert_eq!(links[2].title, "Export Tournament");
        assert_eq!(
            links[2].pgn_url,
            "https://www.example-chess.de/export.php?format=pgn&id=7"
        );
    }
}
