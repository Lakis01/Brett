//! Clean-room The Week In Chess (TWIC) weekly issue and tournament archive
//! downloader and ZIP-to-PGN extractor.

use std::collections::{BTreeSet, HashSet};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::DeflateDecoder;

use crate::domain::{Game, parse_pgn};

pub const TWIC_BASE_URL: &str = "https://theweekinchess.com";
pub const TWIC_INDEX_URL: &str = "https://theweekinchess.com/twic";
pub const TWIC_YEAR_ARCHIVE_URL: &str = "https://theweekinchess.com/a-year-of-pgn-game-files";

const USER_AGENT: &str = "Brett/0.2 (+https://github.com/Lakis01/Brett)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TwicIssue {
    pub number: u32,
    pub date: Option<String>,
    pub date_yyyymmdd: Option<u32>,
    pub pgn_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TwicTournament {
    pub year: u32,
    pub title: String,
    pub pgn_url: String,
}

#[must_use]
pub fn twic_issue_pgn_url(issue: u32) -> String {
    format!("{TWIC_BASE_URL}/zips/twic{issue}g.zip")
}

/// Parse an issue specification such as `"645, 653, 664-672"` into sorted,
/// deduplicated TWIC issue numbers.
pub fn parse_issue_spec(spec: &str) -> Result<Vec<u32>> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        bail!("Enter at least one issue number or range (for example: 645, 653, 664-672).");
    }
    let mut set = BTreeSet::new();
    for raw_token in trimmed.split(|c: char| c == ',' || c.is_whitespace()) {
        let token = raw_token.trim();
        if token.is_empty() {
            continue;
        }
        if let Some((start_s, end_s)) = token.split_once('-') {
            let start: u32 = start_s
                .trim()
                .parse()
                .with_context(|| format!("invalid range start in '{token}'"))?;
            let end: u32 = end_s
                .trim()
                .parse()
                .with_context(|| format!("invalid range end in '{token}'"))?;
            if start == 0 || end == 0 {
                bail!("TWIC issue numbers must be >= 1 (got '{token}').");
            }
            if start > end {
                bail!("Inverted issue range '{token}' (start > end).");
            }
            if end - start > 5000 {
                bail!("Issue range '{token}' is too large (max 5000 issues at once).");
            }
            for n in start..=end {
                set.insert(n);
            }
        } else {
            let n: u32 = token
                .parse()
                .with_context(|| format!("invalid issue number '{token}'"))?;
            if n == 0 {
                bail!("TWIC issue numbers must be >= 1 (got '{token}').");
            }
            set.insert(n);
        }
    }
    if set.is_empty() {
        bail!("No valid TWIC issue numbers found in '{spec}'.");
    }
    Ok(set.into_iter().collect())
}

/// Filter TWIC issues matching `[from_yyyymmdd, to_yyyymmdd]` inclusive.
#[must_use]
pub fn issues_for_date_range(
    issues: &[TwicIssue],
    from_yyyymmdd: Option<u32>,
    to_yyyymmdd: Option<u32>,
) -> Vec<u32> {
    let mut out = BTreeSet::new();
    for issue in issues {
        let Some(d) = issue.date_yyyymmdd else {
            continue;
        };
        if let Some(min_d) = from_yyyymmdd
            && d < min_d
        {
            continue;
        }
        if let Some(max_d) = to_yyyymmdd
            && d > max_d
        {
            continue;
        }
        out.insert(issue.number);
    }
    out.into_iter().collect()
}

/// Filter archived TWIC tournaments by optional year and search text.
#[must_use]
pub fn filter_tournaments<'a>(
    items: &'a [TwicTournament],
    year: Option<u32>,
    query: &str,
) -> Vec<&'a TwicTournament> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    items
        .iter()
        .filter(|item| year.is_none_or(|y| item.year == y))
        .filter(|item| {
            if terms.is_empty() {
                return true;
            }
            let hay = format!("{} {} {}", item.year, item.title, item.pgn_url).to_ascii_lowercase();
            terms.iter().all(|term| hay.contains(term))
        })
        .collect()
}

/// Clean-room ZIP reader extracting and concatenating all `.pgn` members.
/// Supports Method 0 (Stored) and Method 8 (Deflate), reading exact member
/// lengths from the Central Directory so ZIPs written with data descriptors
/// decode reliably.
pub fn extract_pgn_from_zip(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 22 {
        bail!("ZIP archive is truncated ({} bytes)", bytes.len());
    }
    let eocd_sig = [0x50, 0x4b, 0x05, 0x06];
    let search_start = bytes.len().saturating_sub(22 + 65_535);
    let mut eocd_pos = None;
    for idx in (search_start..=bytes.len() - 22).rev() {
        if bytes[idx..idx + 4] == eocd_sig {
            let comment_len = u16_le(bytes, idx + 20) as usize;
            if idx + 22 + comment_len == bytes.len() {
                eocd_pos = Some(idx);
                break;
            }
        }
    }
    let eocd = eocd_pos.ok_or_else(|| anyhow!("Not a valid ZIP archive (EOCD record not found)"))?;
    let total_entries = u16_le(bytes, eocd + 10) as usize;
    let cd_size = u32_le(bytes, eocd + 12) as usize;
    let cd_offset = u32_le(bytes, eocd + 16) as usize;
    if cd_offset.saturating_add(cd_size) > eocd {
        bail!("ZIP central directory extends past end of file");
    }

    let mut out = Vec::new();
    let mut cursor = cd_offset;
    let mut pgn_entries = 0usize;
    for _ in 0..total_entries {
        if cursor + 46 > cd_offset + cd_size {
            bail!("Truncated ZIP central directory header");
        }
        if bytes[cursor..cursor + 4] != [0x50, 0x4b, 0x01, 0x02] {
            bail!("Invalid ZIP central directory signature at offset {cursor}");
        }
        let method = u16_le(bytes, cursor + 10);
        let compressed_size = u32_le(bytes, cursor + 20) as usize;
        let name_len = u16_le(bytes, cursor + 28) as usize;
        let extra_len = u16_le(bytes, cursor + 30) as usize;
        let comment_len = u16_le(bytes, cursor + 32) as usize;
        let local_offset = u32_le(bytes, cursor + 42) as usize;

        let name_start = cursor + 46;
        let name_end = name_start + name_len;
        let next_cursor = name_end + extra_len + comment_len;
        if next_cursor > cd_offset + cd_size {
            bail!("Truncated ZIP central directory entry metadata");
        }
        let name = String::from_utf8_lossy(&bytes[name_start..name_end]);
        cursor = next_cursor;

        if name.ends_with('/') || !name.to_ascii_lowercase().ends_with(".pgn") {
            continue;
        }
        if local_offset + 30 > bytes.len() {
            bail!("ZIP local header offset {local_offset} out of bounds for '{name}'");
        }
        if bytes[local_offset..local_offset + 4] != [0x50, 0x4b, 0x03, 0x04] {
            bail!("Invalid ZIP local file header for '{name}'");
        }
        let local_name_len = u16_le(bytes, local_offset + 26) as usize;
        let local_extra_len = u16_le(bytes, local_offset + 28) as usize;
        let data_start = local_offset + 30 + local_name_len + local_extra_len;
        let data_end = data_start
            .checked_add(compressed_size)
            .ok_or_else(|| anyhow!("ZIP entry '{name}' compressed size overflow"))?;
        if data_end > bytes.len() {
            bail!("ZIP entry '{name}' payload extends past archive end");
        }
        let payload = &bytes[data_start..data_end];
        let mut entry_bytes = Vec::new();
        match method {
            0 => entry_bytes.extend_from_slice(payload),
            8 => {
                let mut decoder = DeflateDecoder::new(payload);
                decoder
                    .read_to_end(&mut entry_bytes)
                    .with_context(|| format!("decompressing '{name}'"))?;
            }
            other => bail!("Unsupported ZIP compression method {other} for '{name}'"),
        }
        if !entry_bytes.is_empty() {
            if !out.is_empty() && !out.ends_with(b"\n\n") {
                if out.ends_with(b"\n") {
                    out.push(b'\n');
                } else {
                    out.extend_from_slice(b"\n\n");
                }
            }
            out.extend_from_slice(&entry_bytes);
            pgn_entries += 1;
        }
    }
    if pgn_entries == 0 {
        bail!("ZIP archive did not contain any non-empty .pgn entries");
    }
    Ok(out)
}

fn u16_le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// Resolve a relative or protocol-relative TWIC link against `https://theweekinchess.com`.
fn resolve_twic_url(href: &str) -> String {
    let trimmed = href.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else if trimmed.starts_with('/') {
        format!("{TWIC_BASE_URL}{trimmed}")
    } else {
        format!("{TWIC_BASE_URL}/{trimmed}")
    }
}

fn strip_html_tags(input: &str) -> String {
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
    decode_basic_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_basic_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&ndash;", "-")
        .replace("&mdash;", "-")
}

fn extract_attr_value(tag_inner: &str, attr: &str) -> Option<String> {
    let lower = tag_inner.to_ascii_lowercase();
    let needle = format!("{attr}=");
    let idx = lower.find(&needle)?;
    let rest = tag_inner[idx + needle.len()..].trim_start();
    if let Some(after_quote) = rest.strip_prefix('"') {
        let end = after_quote.find('"')?;
        Some(decode_basic_entities(&after_quote[..end]))
    } else if let Some(after_quote) = rest.strip_prefix('\'') {
        let end = after_quote.find('\'')?;
        Some(decode_basic_entities(&after_quote[..end]))
    } else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        Some(decode_basic_entities(&rest[..end]))
    }
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
        if let Some(href) = extract_attr_value(tag_inner, "href") {
            let text = strip_html_tags(&html[body_start..body_end]);
            out.push((start, href, text));
        }
        pos = body_end + 4;
    }
    out
}

fn month_number(token: &str) -> Option<u32> {
    match token.to_ascii_lowercase().as_str() {
        "jan" | "january" => Some(1),
        "feb" | "february" => Some(2),
        "mar" | "march" => Some(3),
        "apr" | "april" => Some(4),
        "may" => Some(5),
        "jun" | "june" => Some(6),
        "jul" | "july" => Some(7),
        "aug" | "august" => Some(8),
        "sep" | "sept" | "september" => Some(9),
        "oct" | "october" => Some(10),
        "nov" | "november" => Some(11),
        "dec" | "december" => Some(12),
        _ => None,
    }
}

/// Extract a date from text such as `"2025-11-17"`, `"17/11/2025"`, `"17.11.2025"`,
/// or `"17 Nov 2025"`. Returns `(iso_string, yyyymmdd)`.
pub fn parse_date_hint(text: &str) -> Option<(String, u32)> {
    let cleaned = strip_html_tags(text);
    for token in cleaned.split_whitespace() {
        let tok = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '/' && c != '.');
        // YYYY-MM-DD or YYYY.MM.DD
        let parts_dash: Vec<&str> = tok.split('-').collect();
        if parts_dash.len() == 3
            && let (Ok(y), Ok(m), Ok(d)) = (
                parts_dash[0].parse::<u32>(),
                parts_dash[1].parse::<u32>(),
                parts_dash[2].parse::<u32>(),
            )
            && (1990..=2050).contains(&y)
            && (1..=12).contains(&m)
            && (1..=31).contains(&d)
        {
            return Some((format!("{y:04}-{m:02}-{d:02}"), y * 10_000 + m * 100 + d));
        }
        // DD/MM/YYYY or DD.MM.YYYY
        for sep in ['/', '.'] {
            let parts: Vec<&str> = tok.split(sep).collect();
            if parts.len() == 3
                && let (Ok(a), Ok(b), Ok(c)) = (
                    parts[0].parse::<u32>(),
                    parts[1].parse::<u32>(),
                    parts[2].parse::<u32>(),
                )
            {
                if (1990..=2050).contains(&a) && (1..=12).contains(&b) && (1..=31).contains(&c) {
                    return Some((format!("{a:04}-{b:02}-{c:02}"), a * 10_000 + b * 100 + c));
                }
                if (1..=31).contains(&a) && (1..=12).contains(&b) && (1990..=2050).contains(&c) {
                    return Some((format!("{c:04}-{b:02}-{a:02}"), c * 10_000 + b * 100 + a));
                }
            }
        }
    }
    // Check 3 consecutive tokens like "17 Nov 2025" or "Nov 17 2025"
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    for win in words.windows(3) {
        let w0 = win[0].trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let w1 = win[1].trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let w2 = win[2].trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if let (Ok(d), Some(m), Ok(y)) = (w0.parse::<u32>(), month_number(w1), w2.parse::<u32>())
            && (1..=31).contains(&d)
            && (1990..=2050).contains(&y)
        {
            return Some((format!("{y:04}-{m:02}-{d:02}"), y * 10_000 + m * 100 + d));
        }
        if let (Some(m), Ok(d), Ok(y)) = (month_number(w0), w1.parse::<u32>(), w2.parse::<u32>())
            && (1..=31).contains(&d)
            && (1990..=2050).contains(&y)
        {
            return Some((format!("{y:04}-{m:02}-{d:02}"), y * 10_000 + m * 100 + d));
        }
    }
    None
}

fn parse_twic_issue_number_from_href(href: &str) -> Option<u32> {
    let lower = href.to_ascii_lowercase();
    let idx = lower.find("twic")?;
    let rest = &lower[idx + 4..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let after = &rest[digits.len()..];
    if after.starts_with("g.zip") || after.starts_with(".pgn") || after.starts_with("c6.zip") {
        let num = digits.parse::<u32>().ok()?;
        (num >= 1).then_some(num)
    } else {
        None
    }
}

/// Parse weekly TWIC issue links and release dates from `https://theweekinchess.com/twic`.
#[must_use]
pub fn parse_twic_index_html(html: &str) -> Vec<TwicIssue> {
    let lower = html.to_ascii_lowercase();
    let anchors = extract_anchors(html);
    let mut by_num: BTreeSet<u32> = BTreeSet::new();
    let mut out = Vec::new();
    for (pos, href, _) in anchors {
        let Some(number) = parse_twic_issue_number_from_href(&href) else {
            continue;
        };
        if !href.to_ascii_lowercase().contains("g.zip")
            && !href.to_ascii_lowercase().ends_with(".pgn")
        {
            continue;
        }
        if !by_num.insert(number) {
            continue;
        }
        // Look inside surrounding <tr>...</tr> if present for release date.
        let tr_start = lower[..pos].rfind("<tr").unwrap_or_else(|| pos.saturating_sub(240));
        let tr_end = lower[pos..]
            .find("</tr>")
            .map(|r| pos + r + 5)
            .unwrap_or_else(|| (pos + 240).min(html.len()));
        let row_slice = &html[tr_start..tr_end];
        let date_pair = parse_date_hint(row_slice);
        out.push(TwicIssue {
            number,
            date: date_pair.as_ref().map(|(s, _)| s.clone()),
            date_yyyymmdd: date_pair.as_ref().map(|(_, d)| *d),
            pgn_url: resolve_twic_url(&href),
        });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.number));
    out
}

fn extract_year_from_text(text: &str) -> Option<u32> {
    for token in text.split(|c: char| !c.is_ascii_digit()) {
        if token.len() == 4
            && let Ok(y) = token.parse::<u32>()
            && (1990..=2050).contains(&y)
        {
            return Some(y);
        }
    }
    None
}

/// Parse tournament archives from `https://theweekinchess.com/a-year-of-pgn-game-files`.
#[must_use]
pub fn parse_year_archive_html(html: &str) -> Vec<TwicTournament> {
    let lower = html.to_ascii_lowercase();
    // Collect heading offsets (`<h1..h4>...`) carrying a 4-digit year.
    let mut headings: Vec<(usize, u32)> = Vec::new();
    for tag in ["<h1", "<h2", "<h3", "<h4"] {
        let end_tag = format!("</{}", &tag[1..]);
        let mut cursor = 0usize;
        while let Some(rel) = lower[cursor..].find(tag) {
            let start = cursor + rel;
            if let Some(gt) = html[start..].find('>')
                && let Some(close_rel) = lower[start + gt + 1..].find(&end_tag)
            {
                let text = strip_html_tags(&html[start + gt + 1..start + gt + 1 + close_rel]);
                if let Some(y) = extract_year_from_text(&text) {
                    headings.push((start, y));
                }
                cursor = start + gt + 1 + close_rel;
            } else {
                break;
            }
        }
    }
    headings.sort_by_key(|(pos, _)| *pos);

    let mut seen_urls = HashSet::new();
    let mut out = Vec::new();
    for (pos, href, text) in extract_anchors(html) {
        let href_lower = href.to_ascii_lowercase();
        if !(href_lower.ends_with(".zip") || href_lower.ends_with(".pgn")) {
            continue;
        }
        // Skip generic weekly twicXXXXg.zip links if they happen to be sidebar links.
        if parse_twic_issue_number_from_href(&href).is_some() {
            continue;
        }
        let url = resolve_twic_url(&href);
        if !seen_urls.insert(url.clone()) {
            continue;
        }
        let title = if text.trim().is_empty() {
            href.rsplit('/')
                .next()
                .unwrap_or(&href)
                .trim_end_matches(".zip")
                .trim_end_matches(".pgn")
                .replace(['_', '-'], " ")
        } else {
            text.trim().to_string()
        };
        let year = headings
            .iter()
            .rfind(|(h_pos, _)| *h_pos <= pos)
            .map(|(_, y)| *y)
            .or_else(|| extract_year_from_text(&title))
            .or_else(|| extract_year_from_text(&href))
            .unwrap_or(0);
        out.push(TwicTournament {
            year,
            title,
            pgn_url: url,
        });
    }
    out
}

fn twic_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
}

pub fn fetch_twic_index() -> Result<Vec<TwicIssue>> {
    let html = twic_agent()
        .get(TWIC_INDEX_URL)
        .call()
        .with_context(|| format!("fetching {TWIC_INDEX_URL}"))?
        .into_string()
        .context("decoding TWIC index page")?;
    let issues = parse_twic_index_html(&html);
    if issues.is_empty() {
        bail!("No TWIC issues found at {TWIC_INDEX_URL}");
    }
    Ok(issues)
}

pub fn fetch_year_archive() -> Result<Vec<TwicTournament>> {
    let html = twic_agent()
        .get(TWIC_YEAR_ARCHIVE_URL)
        .call()
        .with_context(|| format!("fetching {TWIC_YEAR_ARCHIVE_URL}"))?
        .into_string()
        .context("decoding TWIC year archive page")?;
    let items = parse_year_archive_html(&html);
    if items.is_empty() {
        bail!("No tournament archives found at {TWIC_YEAR_ARCHIVE_URL}");
    }
    Ok(items)
}

pub fn download_twic_pgn_urls<F>(
    items: &[(String, String)],
    mut on_progress: F,
    cancel: &AtomicBool,
) -> Result<Vec<Game>>
where
    F: FnMut(usize, usize, &str),
{
    let agent = twic_agent();
    let total = items.len();
    let mut all_games = Vec::new();
    for (idx, (label, url)) in items.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("TWIC download cancelled.");
        }
        on_progress(idx, total, label);
        let resp = agent
            .get(url)
            .call()
            .with_context(|| format!("downloading {url}"))?;
        let mut raw = Vec::new();
        resp.into_reader()
            .take(128 * 1024 * 1024)
            .read_to_end(&mut raw)
            .with_context(|| format!("reading response body for {url}"))?;
        let pgn_bytes = if url.to_ascii_lowercase().ends_with(".zip") {
            extract_pgn_from_zip(&raw).with_context(|| format!("extracting ZIP from {url}"))?
        } else {
            raw
        };
        let mut parsed = parse_pgn(&pgn_bytes).with_context(|| format!("parsing PGN from {url}"))?;
        all_games.append(&mut parsed);
    }
    on_progress(total, total, "Done");
    Ok(all_games)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    /// Minimal test ZIP builder supporting compression method 0 (Stored) and
    /// method 8 (Deflate), optionally zeroing local header sizes to mimic data
    /// descriptors (general purpose bit 3).
    fn build_test_zip(entries: &[(&str, &[u8], bool, bool)]) -> Vec<u8> {
        struct Rec {
            name: String,
            method: u16,
            crc: u32,
            comp_len: u32,
            uncomp_len: u32,
            local_offset: u32,
            flags: u16,
        }
        let mut out = Vec::new();
        let mut recs = Vec::new();
        for &(name, data, deflate, data_descriptor) in entries {
            let local_offset = out.len() as u32;
            let compressed = if deflate {
                let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
                enc.write_all(data).unwrap();
                enc.finish().unwrap()
            } else {
                data.to_vec()
            };
            let method = if deflate { 8u16 } else { 0u16 };
            let flags = if data_descriptor { 0x0008u16 } else { 0u16 };
            let comp_len = compressed.len() as u32;
            let uncomp_len = data.len() as u32;
            let crc = 0x1234_5678u32;

            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&(if data_descriptor { 0 } else { crc }).to_le_bytes());
            out.extend_from_slice(&(if data_descriptor { 0 } else { comp_len }).to_le_bytes());
            out.extend_from_slice(&(if data_descriptor { 0 } else { uncomp_len }).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&compressed);
            if data_descriptor {
                out.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
                out.extend_from_slice(&crc.to_le_bytes());
                out.extend_from_slice(&comp_len.to_le_bytes());
                out.extend_from_slice(&uncomp_len.to_le_bytes());
            }
            recs.push(Rec {
                name: name.to_string(),
                method,
                crc,
                comp_len,
                uncomp_len,
                local_offset,
                flags,
            });
        }
        let cd_offset = out.len() as u32;
        for r in &recs {
            out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&r.flags.to_le_bytes());
            out.extend_from_slice(&r.method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&r.crc.to_le_bytes());
            out.extend_from_slice(&r.comp_len.to_le_bytes());
            out.extend_from_slice(&r.uncomp_len.to_le_bytes());
            out.extend_from_slice(&(r.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&r.local_offset.to_le_bytes());
            out.extend_from_slice(r.name.as_bytes());
        }
        let cd_size = out.len() as u32 - cd_offset;
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(recs.len() as u16).to_le_bytes());
        out.extend_from_slice(&(recs.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn extracts_stored_and_deflated_pgn_from_zip() {
        let pgn1 = b"[White \"A\"]\n[Black \"B\"]\n\n1. e4 e5 *\n";
        let pgn2 = b"[White \"C\"]\n[Black \"D\"]\n\n1. d4 d5 *\n";
        let readme = b"ignore non-pgn text";
        let zip = build_test_zip(&[
            ("readme.txt", readme, false, false),
            ("twic1500.pgn", pgn1, true, false),
            ("extra.PGN", pgn2, false, true),
        ]);
        let extracted = extract_pgn_from_zip(&zip).unwrap();
        let text = String::from_utf8(extracted).unwrap();
        assert!(text.contains("1. e4 e5"));
        assert!(text.contains("1. d4 d5"));
        assert!(!text.contains("ignore non-pgn text"));
    }

    #[test]
    fn parse_issue_spec_handles_lists_ranges_and_errors() {
        let nums = parse_issue_spec("645, 653, 664-667, 653").unwrap();
        assert_eq!(nums, vec![645, 653, 664, 665, 666, 667]);
        assert!(parse_issue_spec("").is_err());
        assert!(parse_issue_spec("670-664").is_err());
        assert!(parse_issue_spec("abc").is_err());
    }

    #[test]
    fn parse_twic_index_and_date_filter() {
        let html = r#"
            <table>
                <tr><td>1565</td><td>11 Nov 2024</td><td><a href="/zips/twic1565g.zip">PGN</a></td></tr>
                <tr><td>1566</td><td>2024-11-18</td><td><a href="https://theweekinchess.com/zips/twic1566g.zip">PGN</a></td></tr>
                <tr><td>1567</td><td>25/11/2024</td><td><a href="zips/twic1567c6.zip">CBV</a></td></tr>
            </table>
        "#;
        let parsed = parse_twic_index_html(html);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].number, 1566);
        assert_eq!(parsed[0].date.as_deref(), Some("2024-11-18"));
        assert_eq!(parsed[1].number, 1565);
        assert_eq!(parsed[1].date.as_deref(), Some("2024-11-11"));
        assert_eq!(
            issues_for_date_range(&parsed, Some(20_241_115), Some(20_241_130)),
            vec![1566]
        );
    }

    #[test]
    fn parse_year_archive_and_filter_tournaments() {
        let html = r#"
            <h2>2024</h2>
            <p><a href="/zips/tata24.zip">Tata Steel Masters &amp; Challengers</a></p>
            <p><a href="/zips/candidates24.zip">FIDE Candidates Toronto</a></p>
            <h2>2023</h2>
            <p><a href="/zips/wijk23.pgn">Wijk aan Zee</a></p>
            <p><a href="/zips/twic1500g.zip">TWIC 1500</a></p>
        "#;
        let items = parse_year_archive_html(html);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].year, 2024);
        assert_eq!(items[0].title, "Tata Steel Masters & Challengers");
        assert_eq!(filter_tournaments(&items, Some(2024), "candidates").len(), 1);
        assert_eq!(filter_tournaments(&items, None, "wijk").len(), 1);
    }
}
