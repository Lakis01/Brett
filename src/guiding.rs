//! Native ChessBase guiding-text documents.
//!
//! Both classic CBH (`.cbg` text records) and 2CBH (`.2cbg` text records)
//! carry the same HTML dialect: prose (`<p>`, `<h1..h4>`), embedded
//! crosstable `<table>`s, `<img src>` photos/crosstable GIFs stored beside
//! the database in a `<stem>.html/` directory, and `CBLink(...)` game links.

use std::path::{Path, PathBuf};

/// One structured content block of a guiding-text document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuidingBlock {
    Heading(String),
    Paragraph(Vec<GuidingSpan>),
    Table(Vec<Vec<String>>),
    Image {
        src: String,
        alt_fen: Option<String>,
    },
}

/// One inline span inside a paragraph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuidingSpan {
    Text(String),
    Link { label: String, target: CbLinkTarget },
}

/// Destination of a `CBLink(...)` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CbLinkTarget {
    GameSet(Vec<u32>),
    Table {
        event: String,
        site: String,
        year: Option<u32>,
    },
    Text {
        title: String,
        event: String,
        site: String,
        year: Option<u32>,
    },
    Game {
        white_last: String,
        black_last: String,
        event: String,
        round: Option<u32>,
    },
}

/// A parsed guiding-text document with its titles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GuidingTextDocument {
    pub titles: Vec<String>,
    pub blocks: Vec<GuidingBlock>,
}

impl GuidingTextDocument {
    pub fn is_empty(&self) -> bool {
        self.titles.is_empty() && self.blocks.is_empty()
    }
}

/// Parse guiding-text HTML into structured blocks.
///
/// Handles `<table>`, `<p>`, `<h1..h4>`, `<img src alt>` and
/// `CBLink(tag_game_set/tag_table/tag_text/tag_game)`. Unknown markup is
/// degraded to plain text; this never panics.
pub fn parse_guiding_html(html: &str) -> GuidingTextDocument {
    let mut doc = GuidingTextDocument::default();
    let mut from = 0usize;
    let lower = html.to_lowercase();
    while from < html.len() {
        let rest = &lower[from..];
        let table_at = rest.find("<table");
        let p_at = rest.find("<p");
        let img_at = rest.find("<img");
        let head_at = find_heading_at(rest);
        let mut best: Option<(&str, usize)> = None;
        for (kind, at) in [
            ("table", table_at),
            ("p", p_at),
            ("img", img_at),
            ("head", head_at),
        ]
        .into_iter()
        {
            if let Some(at) = at {
                let abs = from + at;
                if best.is_none_or(|(_, b)| abs < b) {
                    best = Some((kind, abs));
                }
            }
        }
        let Some((kind, abs)) = best else {
            let tail = html[from..].trim();
            if !tail.is_empty() {
                let spans = parse_spans(tail);
                if spans.iter().any(|span| !span_text(span).trim().is_empty()) {
                    doc.blocks.push(merge_text_block(spans));
                }
            }
            break;
        };
        // Any prose before the next block becomes a paragraph.
        if abs > from {
            let gap = html[from..abs].trim();
            if !gap.is_empty() && !is_tag_only(gap) {
                let text = strip_tags(gap);
                if !text.is_empty() {
                    doc.blocks.push(merge_text_block(parse_spans(gap)));
                }
            }
        }
        match kind {
            "table" => {
                let Some(tag_end) = html[abs..].find('>') else {
                    break;
                };
                let content = abs + tag_end + 1;
                let close = lower[content..]
                    .find("</table>")
                    .map(|i| content + i)
                    .unwrap_or(html.len());
                doc.blocks
                    .push(GuidingBlock::Table(parse_table(&html[content..close])));
                from = close + "</table>".len().min(html.len().saturating_sub(close));
            }
            "p" => {
                let Some(tag_end) = html[abs..].find('>') else {
                    break;
                };
                let content = abs + tag_end + 1;
                let close = lower[content..]
                    .find("</p>")
                    .map(|i| content + i)
                    .unwrap_or(html.len());
                let inner = &html[content..close];
                for (src, alt) in extract_images(inner) {
                    doc.blocks.push(GuidingBlock::Image { src, alt_fen: alt });
                }
                let spans = parse_spans(inner);
                if spans.iter().any(|span| !span_text(span).trim().is_empty()) {
                    doc.blocks.push(merge_text_block(spans));
                }
                from = close + "</p>".len().min(html.len().saturating_sub(close));
            }
            "head" => {
                let Some(tag_end) = html[abs..].find('>') else {
                    break;
                };
                let content = abs + tag_end + 1;
                // Find which level opened so the matching close tag is used.
                let level = heading_level(&lower[abs..]);
                let close_tag = format!("</h{level}>");
                let close = lower[content..]
                    .find(&close_tag)
                    .map(|i| content + i)
                    .unwrap_or(html.len());
                let title = strip_tags(&html[content..close]);
                if !title.is_empty() {
                    doc.blocks.push(GuidingBlock::Heading(title));
                }
                from = close + close_tag.len().min(html.len().saturating_sub(close));
            }
            _ => {
                let Some(tag_end) = html[abs..].find('>') else {
                    break;
                };
                let tag = &html[abs..abs + tag_end + 1];
                let src = attr_value(tag, "src").unwrap_or_default();
                let alt = attr_value(tag, "alt").filter(|alt| !alt.trim().is_empty());
                if !src.is_empty() {
                    doc.blocks.push(GuidingBlock::Image { src, alt_fen: alt });
                }
                from = abs + tag_end + 1;
            }
        }
    }
    doc
}

fn merge_text_block(spans: Vec<GuidingSpan>) -> GuidingBlock {
    GuidingBlock::Paragraph(spans)
}

fn span_text(span: &GuidingSpan) -> &str {
    match span {
        GuidingSpan::Text(text) => text,
        GuidingSpan::Link { label, .. } => label,
    }
}

fn is_tag_only(gap: &str) -> bool {
    let stripped = strip_tags(gap);
    stripped.trim().is_empty()
}

fn find_heading_at(lower_rest: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    for level in 1..=4 {
        let needle = format!("<h{level}");
        let mut from = 0usize;
        while let Some(rel) = lower_rest[from..].find(&needle) {
            let abs = from + rel;
            let after = lower_rest.as_bytes().get(abs + 3).copied().unwrap_or(b' ');
            // Accept `<h1>`, `<h1 class=...>`; reject `<hr>`, `<html`.
            if after == b'>'
                || after == b' '
                || after == b'\t'
                || after == b'\n'
                || after == b'\r'
                || after == b'/'
            {
                if best.is_none_or(|b| abs < b) {
                    best = Some(abs);
                }
                break;
            }
            from = abs + 1;
            if from >= lower_rest.len() {
                break;
            }
        }
    }
    best
}

fn heading_level(lower_tag: &str) -> u8 {
    lower_tag
        .as_bytes()
        .get(2)
        .copied()
        .filter(|b| (b'1'..=b'4').contains(b))
        .map(|b| b - b'0')
        .unwrap_or(1)
}

fn parse_table(inner: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let lower = inner.to_lowercase();
    let mut from = 0usize;
    while let Some(tr_rel) = lower[from..].find("<tr") {
        let tr_abs = from + tr_rel;
        let Some(tag_end) = inner[tr_abs..].find('>') else {
            break;
        };
        let content = tr_abs + tag_end + 1;
        let Some(close_rel) = lower[content..].find("</tr>") else {
            break;
        };
        let close = content + close_rel;
        rows.push(parse_row(&inner[content..close]));
        from = close + "</tr>".len();
    }
    rows
}

fn parse_row(inner: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let lower = inner.to_lowercase();
    let mut from = 0usize;
    while from < inner.len() {
        let td = lower[from..].find("<td");
        let th = lower[from..].find("<th");
        let (at, tag) = match (td, th) {
            (Some(a), Some(b)) if a <= b => (from + a, "td"),
            (Some(_), Some(b)) => (from + b, "th"),
            (Some(a), None) => (from + a, "td"),
            (None, Some(b)) => (from + b, "th"),
            (None, None) => break,
        };
        let Some(tag_end) = inner[at..].find('>') else {
            break;
        };
        let content = at + tag_end + 1;
        let close_tag = format!("</{tag}>");
        let Some(close_rel) = lower[content..].find(&close_tag) else {
            break;
        };
        let close = content + close_rel;
        cells.push(strip_tags(&inner[content..close]));
        from = close + close_tag.len();
    }
    cells
}

/// Split a prose fragment into text/link spans via `CBLink(...)` calls.
fn parse_spans(fragment: &str) -> Vec<GuidingSpan> {
    let mut spans = Vec::new();
    let mut from = 0usize;
    while let Some(call_at) = fragment[from..].find("CBLink") {
        let base = from + call_at;
        let Some(open) = fragment[base..].find('(') else {
            break;
        };
        let args_at = base + open + 1;
        // The label is the anchor text preceding the call when inside <a>.
        let anchor_label = anchor_label_before(fragment, base);
        let (tag, mut pos) = match parse_quoted(fragment, args_at) {
            Some(pair) => pair,
            None => {
                from = args_at;
                continue;
            }
        };
        let target = match tag.as_str() {
            "tag_game_set" => {
                let mut slots = Vec::new();
                loop {
                    pos = skip_ws_comma(fragment, pos);
                    if fragment[pos..].starts_with(')') {
                        break;
                    }
                    let (num, quoted) = parse_number_text(fragment, pos);
                    if num.is_empty() {
                        break;
                    }
                    if let Ok(slot) = num.parse::<u32>()
                        && !slots.contains(&slot)
                    {
                        slots.push(slot);
                    }
                    pos += if quoted { num.len() + 2 } else { num.len() };
                    if fragment[pos..].starts_with(')') {
                        break;
                    }
                    if fragment[pos..].starts_with(',') {
                        pos += 1;
                    } else {
                        pos = skip_ws_comma(fragment, pos);
                        if !fragment[pos..].starts_with(')') && !fragment[pos..].starts_with(',') {
                            break;
                        }
                    }
                }
                (!slots.is_empty()).then_some(CbLinkTarget::GameSet(slots))
            }
            "tag_table" => {
                pos = skip_ws_comma(fragment, pos);
                let (event, n1) = match parse_quoted(fragment, pos) {
                    Some(pair) => pair,
                    None => (String::new(), pos),
                };
                pos = skip_ws_comma(fragment, n1);
                let (site, n2) = match parse_quoted(fragment, pos) {
                    Some(pair) => pair,
                    None => (String::new(), pos),
                };
                pos = skip_ws_comma(fragment, n2);
                let (year_text, _) = parse_number_text(fragment, pos);
                (!event.is_empty()).then_some(CbLinkTarget::Table {
                    event,
                    site,
                    year: year_text.parse::<u32>().ok(),
                })
            }
            "tag_text" => {
                pos = skip_ws_comma(fragment, pos);
                let (title, n1) = match parse_quoted(fragment, pos) {
                    Some(pair) => pair,
                    None => (String::new(), pos),
                };
                pos = skip_ws_comma(fragment, n1);
                let (event, n2) = match parse_quoted(fragment, pos) {
                    Some(pair) => pair,
                    None => (String::new(), pos),
                };
                pos = skip_ws_comma(fragment, n2);
                let (site, n3) = match parse_quoted(fragment, pos) {
                    Some(pair) => pair,
                    None => (String::new(), pos),
                };
                pos = skip_ws_comma(fragment, n3);
                let (year_text, _) = parse_number_text(fragment, pos);
                (!title.is_empty()).then_some(CbLinkTarget::Text {
                    title,
                    event,
                    site,
                    year: year_text.parse::<u32>().ok(),
                })
            }
            "tag_game" => {
                pos = skip_ws_comma(fragment, pos);
                let (white_last, n1) = match parse_quoted(fragment, pos) {
                    Some(pair) => pair,
                    None => (String::new(), pos),
                };
                pos = skip_ws_comma(fragment, n1);
                let (black_last, n2) = match parse_quoted(fragment, pos) {
                    Some(pair) => pair,
                    None => (String::new(), pos),
                };
                pos = skip_ws_comma(fragment, n2);
                let (event, n3) = match parse_quoted(fragment, pos) {
                    Some(pair) => pair,
                    None => (String::new(), pos),
                };
                pos = skip_ws_comma(fragment, n3);
                let (round_text, _) = parse_number_text(fragment, pos);
                (!white_last.is_empty()).then_some(CbLinkTarget::Game {
                    white_last,
                    black_last,
                    event,
                    round: round_text.parse::<u32>().ok(),
                })
            }
            _ => None,
        };
        // Emit text before the link's anchor (or the call itself).
        let anchor_start = anchor_label
            .as_ref()
            .map(|(_, start)| *start)
            .unwrap_or(base);
        if anchor_start > from {
            let text = strip_tags(&fragment[from..anchor_start]);
            if !text.is_empty() {
                spans.push(GuidingSpan::Text(text));
            }
        }
        if let Some(target) = target {
            let label = anchor_label
                .map(|(label, _)| label)
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| target.fallback_label());
            spans.push(GuidingSpan::Link { label, target });
        }
        // Continue after the closing ')'.
        let close = fragment[args_at..]
            .find(')')
            .map(|i| args_at + i + 1)
            .unwrap_or(args_at + 1);
        // Skip a closing </a> when the link came from an anchor.
        let after = if fragment[close..].to_lowercase().starts_with("</a") {
            fragment[close..]
                .find('>')
                .map(|i| close + i + 1)
                .unwrap_or(close)
        } else {
            close
        };
        from = after.max(close);
    }
    if from < fragment.len() {
        let text = strip_tags(&fragment[from..]);
        if !text.is_empty() {
            spans.push(GuidingSpan::Text(text));
        }
    }
    spans
}

impl CbLinkTarget {
    fn fallback_label(&self) -> String {
        match self {
            Self::GameSet(slots) => format!("{} game(s)", slots.len()),
            Self::Table { event, .. } => event.clone(),
            Self::Text { title, .. } => title.clone(),
            Self::Game {
                white_last,
                black_last,
                ..
            } => format!("{white_last} - {black_last}"),
        }
    }
}

/// Anchor text of the `<a ...>` element enclosing a `CBLink` call position.
fn anchor_label_before(fragment: &str, call_at: usize) -> Option<(String, usize)> {
    let open_at = fragment[..call_at].rfind("<a")?;
    let tag_end = fragment[open_at..].find('>')?;
    let content = open_at + tag_end + 1;
    if content > call_at {
        return None;
    }
    let lower = fragment.to_lowercase();
    let after = &lower[call_at..];
    let close_rel = after.find("</a>")?;
    let _ = close_rel;
    // The label runs from the anchor content to the start of the href value;
    // approximate by stripping tags from content..call_at and taking the tail
    // before `href=`.
    let head = &fragment[content..call_at];
    let label = if let Some(href) = head.to_lowercase().rfind("href") {
        strip_tags(&head[..href])
    } else {
        strip_tags(head)
    };
    // Anchor start for span splitting: the `<a` itself.
    Some((label.trim().to_string(), open_at))
}

fn parse_quoted(text: &str, mut pos: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    let quote = *bytes.get(pos)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    pos += 1;
    let start = pos;
    while pos < bytes.len() && bytes[pos] != quote {
        pos += 1;
    }
    if pos >= bytes.len() {
        return None;
    }
    Some((text[start..pos].to_string(), pos + 1))
}

fn parse_number_text(text: &str, mut pos: usize) -> (String, bool) {
    let bytes = text.as_bytes();
    while pos < bytes.len() && (bytes[pos].is_ascii_whitespace() || bytes[pos] == b',') {
        if bytes[pos] == b',' {
            pos += 1;
            break;
        }
        pos += 1;
    }
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    if pos < bytes.len()
        && (bytes[pos] == b'\'' || bytes[pos] == b'"')
        && let Some((value, _)) = parse_quoted(text, pos)
    {
        return (value, true);
    }
    let start = pos;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        pos += 1;
    }
    (text[start..pos].to_string(), false)
}

fn skip_ws_comma(text: &str, mut pos: usize) -> usize {
    let bytes = text.as_bytes();
    loop {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos < bytes.len() && bytes[pos] == b',' {
            pos += 1;
            continue;
        }
        break;
    }
    pos
}

/// All `<img src alt>` tags inside a fragment, in order.
fn extract_images(fragment: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let lower = fragment.to_lowercase();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("<img") {
        let base = from + rel;
        let Some(end) = fragment[base..].find('>') else {
            break;
        };
        let tag = &fragment[base..base + end + 1];
        if let Some(src) = attr_value(tag, "src").filter(|src| !src.is_empty()) {
            let alt = attr_value(tag, "alt").filter(|alt| !alt.trim().is_empty());
            out.push((src, alt));
        }
        from = base + end + 1;
    }
    out
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_lowercase();
    let needle = format!("{name}=");
    let rel = lower.find(&needle)?;
    let mut pos = rel + needle.len();
    let bytes = tag.as_bytes();
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    let quote = *bytes.get(pos)?;
    if quote == b'\'' || quote == b'"' {
        let start = pos + 1;
        let end = tag[start..].find(quote as char)? + start;
        Some(tag[start..end].to_string())
    } else {
        let start = pos;
        while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() && bytes[pos] != b'>' {
            pos += 1;
        }
        Some(tag[start..pos].to_string())
    }
}

pub(crate) fn strip_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let decoded = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// File name of an `<img src>` after the last `\` or `/`.
pub fn image_basename(src: &str) -> String {
    src.rsplit(['\\', '/']).next().unwrap_or(src).to_string()
}

/// `<stem>.html/` directory holding a database's guiding-text pictures.
pub fn image_dir_for_db(db_path: &Path) -> PathBuf {
    let stem = db_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    // `Mega Database 2026.2cbh` -> `Mega Database 2026.html`; when the file
    // name itself ends in `.html` (a saved document), use it directly.
    if db_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html"))
    {
        return db_path.to_path_buf();
    }
    let file_name = if stem.is_empty() {
        "database.html".to_string()
    } else {
        format!("{stem}.html")
    };
    match db_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    }
}

/// Load an `<img src>` from the database's `.html/` directory as an egui image.
pub fn load_guiding_image(db_path: &Path, src: &str) -> anyhow::Result<egui::ColorImage> {
    let name = image_basename(src);
    if name.is_empty() || name.contains("..") {
        anyhow::bail!("guiding image {src:?} is not a plain file name");
    }
    let path = image_dir_for_db(db_path).join(&name);
    let image = image::open(&path)
        .map_err(|err| anyhow::anyhow!("opening guiding image {}: {err:#}", path.display()))?;
    let rgba = image.to_rgba8();
    let (width, height) = (rgba.width() as usize, rgba.height() as usize);
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        anyhow::bail!("guiding image {} has size {width}x{height}", path.display());
    }
    let pixels = rgba.into_raw();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [width, height],
        &pixels,
    ))
}
