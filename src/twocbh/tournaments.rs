//! Mega Database tournament index decoder (Backlog Task 4b).
//!
//! Slot 1 of the Mega Database is a text record whose `.2cbg` payload is an
//! HTML tournament index. Each entry is a `<li class="forclick">` title
//! followed by a `<ul>` block with `CBLink(tag_game_set,<slot>)` game links
//! (one per game; the singular `tag_game` is ignored) and a
//! `CBLink(tag_table,Event,Site,Year)` table link. Both single and double
//! quotes are accepted.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MegaTournamentEntry {
    pub title: String,
    pub event: Option<String>,
    pub site: Option<String>,
    pub year: Option<u32>,
    pub game_slots: Vec<u32>,
    pub text_slot: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MegaTournamentCategory {
    pub name: String,
    pub entries: Vec<MegaTournamentEntry>,
}

/// Parse the Mega tournament index HTML into categories of entries.
pub fn parse_mega_tournament_index(html: &str) -> Vec<MegaTournamentCategory> {
    let headings = collect_headings(html);
    let mut categories: Vec<MegaTournamentCategory> = Vec::new();

    let mut search_from = 0usize;
    while let Some(entry_at) = find_forclick_li(html, search_from) {
        let (title, li_end) = read_li_title(html, entry_at);
        let ul_block = read_following_ul(html, li_end);
        let (game_slots, event, site, year) = ul_block.map(parse_ul_block).unwrap_or_default();
        let title = strip_tags(&title);
        if !title.is_empty() && (!game_slots.is_empty() || event.is_some()) {
            let category_name = category_for(&headings, entry_at)
                .unwrap_or("Tournaments")
                .to_string();
            let entry = MegaTournamentEntry {
                title,
                event,
                site,
                year,
                game_slots,
                text_slot: None,
            };
            match categories.last_mut() {
                Some(last) if last.name == category_name => last.entries.push(entry),
                _ => categories.push(MegaTournamentCategory {
                    name: category_name,
                    entries: vec![entry],
                }),
            }
        }
        search_from = li_end.max(entry_at + 1);
    }

    categories
}

fn parse_ul_block(block: &str) -> (Vec<u32>, Option<String>, Option<String>, Option<u32>) {
    let mut slots: Vec<u32> = Vec::new();
    let mut event: Option<String> = None;
    let mut site: Option<String> = None;
    let mut year: Option<u32> = None;

    let mut from = 0usize;
    while let Some(call) = block[from..].find("CBLink") {
        let base = from + call;
        let Some(open) = block[base..].find('(') else {
            break;
        };
        let mut pos = base + open + 1;
        let Some((tag, next)) = parse_quoted(block, pos) else {
            from = pos;
            continue;
        };
        pos = next;
        match tag.as_str() {
            "tag_game_set" => {
                pos = skip_ws_comma(block, pos);
                let (num_text, quoted) = parse_number_text(block, pos);
                if let Ok(slot) = num_text.parse::<u32>()
                    && !slots.contains(&slot)
                {
                    slots.push(slot);
                }
                from = if quoted {
                    pos + num_text.len() + 2
                } else {
                    pos + num_text.len()
                };
            }
            "tag_table" => {
                pos = skip_ws_comma(block, pos);
                if let Some((ev, n1)) = parse_quoted(block, pos) {
                    pos = skip_ws_comma(block, n1);
                    if let Some((si, n2)) = parse_quoted(block, pos) {
                        pos = skip_ws_comma(block, n2);
                        let (year_text, _) = parse_number_text(block, pos);
                        event = non_empty(ev);
                        site = non_empty(si);
                        year = year_text.parse::<u32>().ok();
                        from = pos + year_text.len();
                        continue;
                    }
                }
                from = pos;
            }
            _ => {
                // `tag_game` (singular) and anything else is ignored.
                from = pos;
            }
        }
    }

    (slots, event, site, year)
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
    let value = text[start..pos].to_string();
    Some((value, pos + 1))
}

fn parse_number_text(text: &str, mut pos: usize) -> (String, bool) {
    let bytes = text.as_bytes();
    while pos < bytes.len() && (bytes[pos].is_ascii_whitespace() || bytes[pos] == b',') {
        // `skip_ws_comma` already ran; tolerate strays without looping forever.
        if bytes[pos] == b',' {
            pos += 1;
            break;
        }
        pos += 1;
    }
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    let mut quoted = false;
    if pos < bytes.len() && (bytes[pos] == b'\'' || bytes[pos] == b'"') {
        if let Some((value, _)) = parse_quoted(text, pos) {
            return (value, true);
        }
        quoted = true;
    }
    let start = pos;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        pos += 1;
    }
    let _ = quoted;
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

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn collect_headings(html: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for tag in ["h1", "h2", "h3", "h4"] {
        let mut from = 0usize;
        while let Some(start) = html[from..].find(&format!("<{tag}")) {
            let base = from + start;
            let Some(title_start) = html[base..].find('>') else {
                break;
            };
            let content = base + title_start + 1;
            let close = format!("</{tag}>");
            let Some(end_rel) = html[content..].find(&close) else {
                break;
            };
            let name = strip_tags(&html[content..content + end_rel]);
            if !name.is_empty() {
                out.push((base, name));
            }
            from = content + end_rel + close.len();
        }
    }
    out.sort_by_key(|(pos, _)| *pos);
    out
}

fn category_for(headings: &[(usize, String)], entry_at: usize) -> Option<&str> {
    headings
        .iter()
        .rev()
        .find(|(pos, _)| *pos < entry_at)
        .map(|(_, name)| name.as_str())
}

fn find_forclick_li(html: &str, from: usize) -> Option<usize> {
    let lower = html.to_lowercase();
    let mut cursor = from;
    while let Some(rel) = lower[cursor..].find("<li") {
        let base = cursor + rel;
        let tag_end_rel = lower[base..].find('>')?;
        let tag = &lower[base..base + tag_end_rel];
        if tag.contains("forclick") {
            return Some(base);
        }
        cursor = base + tag_end_rel + 1;
    }
    None
}

fn read_li_title(html: &str, li_at: usize) -> (String, usize) {
    let tag_end = html[li_at..]
        .find('>')
        .map(|i| li_at + i + 1)
        .unwrap_or(li_at);
    let close = html[tag_end..].find("</li>").map(|i| tag_end + i);
    match close {
        Some(end) => (html[tag_end..end].to_string(), end + "</li>".len()),
        None => (html[tag_end..].to_string(), html.len()),
    }
}

fn read_following_ul(html: &str, from: usize) -> Option<&str> {
    let tail = html.get(from..)?;
    let lower = tail.to_lowercase();
    let ul_rel = lower.find("<ul")?;
    let ul_base = from + ul_rel;
    let tag_end = html[ul_base..].find('>')? + ul_base + 1;
    let close_rel = html[tag_end..].to_lowercase().find("</ul>")?;
    html.get(tag_end..tag_end + close_rel)
}

fn strip_tags(raw: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cblink_game_sets_and_table_links_from_mega_html() {
        let html = r#"
<h2>World Championships</h2>
<li class="forclick">2024: Ding Liren - Gukesh (Singapore)</li>
<ul>
<li><a href="javascript:CBLink('tag_game_set',11300750)">Game 1</a></li>
<li><a href='javascript:CBLink("tag_game_set",11300751)'>Game 2</a></li>
<li><a href="javascript:CBLink('tag_game_set',11300752)">Game 3</a></li>
<li><a href='javascript:CBLink("tag_game",999)'>Single (ignored)</a></li>
<li><a href="javascript:CBLink('tag_table','World-ch Ding-Gukesh','Singapore',2024)">Table</a></li>
</ul>
"#;
        let cats = parse_mega_tournament_index(html);
        assert_eq!(cats.len(), 1, "expected one category, got {cats:?}");
        assert_eq!(cats[0].name, "World Championships");
        assert_eq!(cats[0].entries.len(), 1);
        let entry = &cats[0].entries[0];
        assert_eq!(entry.title, "2024: Ding Liren - Gukesh (Singapore)");
        assert_eq!(entry.event.as_deref(), Some("World-ch Ding-Gukesh"));
        assert_eq!(entry.site.as_deref(), Some("Singapore"));
        assert_eq!(entry.year, Some(2024));
        assert_eq!(entry.game_slots, vec![11300750, 11300751, 11300752]);
    }

    #[test]
    fn parses_double_quoted_table_links_and_ignores_singular_game_links() {
        let html = r#"
<li class='forclick'>Candidates 2024 (Toronto)</li>
<ul>
<li><a href='javascript:CBLink("tag_game_set",9001)'>G1</a></li>
<li><a href="javascript:CBLink('tag_game',9002)">Solo</a></li>
<li><a href='javascript:CBLink("tag_table","Candidates","Toronto","2024")'>Table</a></li>
</ul>
"#;
        let cats = parse_mega_tournament_index(html);
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].name, "Tournaments");
        let entry = &cats[0].entries[0];
        assert_eq!(entry.game_slots, vec![9001]);
        assert_eq!(entry.event.as_deref(), Some("Candidates"));
        assert_eq!(entry.site.as_deref(), Some("Toronto"));
        assert_eq!(entry.year, Some(2024));
    }

    #[test]
    fn guiding_document_keeps_embedded_table_prose_and_images() {
        let html = r#"
<h2>Ostend 1907</h2>
<p>Final report with <a href="javascript:CBLink('tag_game_set',42)">Goetz - Janowski</a>.</p>
<table>
<tr><th></th><th>GOETZ</th><th>JANOWSKI</th></tr>
<tr><td>1</td><td>1</td><td>0</td></tr>
<tr><td>2</td><td>0</td><td>1</td></tr>
</table>
<p><img src="Mega Database 2026.html\8255890.jpg" alt="rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"></p>
<p><a href="javascript:CBLink('tag_table','Ostend','Ostend',1907)">Table</a>
<a href="javascript:CBLink('tag_text','Report','Ostend','Ostend',1907)">Report</a>
<a href="javascript:CBLink('tag_game','Goetz','Janowski','Ostend',3)">G3</a></p>
"#;
        let doc = crate::guiding::parse_guiding_html(html);
        assert!(
            doc.blocks.iter().any(|block| matches!(
                block,
                crate::guiding::GuidingBlock::Heading(heading) if heading.contains("Ostend")
            )),
            "expected a heading, got {:?}",
            doc.blocks
        );
        let rows: Vec<Vec<String>> = doc
            .blocks
            .iter()
            .find_map(|block| match block {
                crate::guiding::GuidingBlock::Table(rows) => Some(rows.clone()),
                _ => None,
            })
            .expect("expected an embedded table");
        let flat = rows.concat().join(" ");
        assert!(flat.contains("GOETZ"), "table must keep GOETZ row: {flat}");
        assert!(
            flat.contains("JANOWSKI"),
            "table must keep JANOWSKI row: {flat}"
        );
        let image = doc
            .blocks
            .iter()
            .find_map(|block| match block {
                crate::guiding::GuidingBlock::Image { src, alt_fen } => {
                    Some((src.clone(), alt_fen.clone()))
                }
                _ => None,
            })
            .expect("expected an inline image");
        assert_eq!(crate::guiding::image_basename(&image.0), "8255890.jpg");
        assert_eq!(
            image.1.as_deref(),
            Some("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1")
        );
        let mut saw_set = false;
        let mut saw_table = false;
        let mut saw_text = false;
        let mut saw_game = false;
        for block in &doc.blocks {
            let crate::guiding::GuidingBlock::Paragraph(spans) = block else {
                continue;
            };
            for span in spans {
                match span {
                    crate::guiding::GuidingSpan::Link { target, .. } => match target {
                        crate::guiding::CbLinkTarget::GameSet(slots) => {
                            assert_eq!(*slots, vec![42]);
                            saw_set = true;
                        }
                        crate::guiding::CbLinkTarget::Table { event, site, year } => {
                            assert_eq!(event, "Ostend");
                            assert_eq!(site, "Ostend");
                            assert_eq!(*year, Some(1907));
                            saw_table = true;
                        }
                        crate::guiding::CbLinkTarget::Text { title, .. } => {
                            assert_eq!(title, "Report");
                            saw_text = true;
                        }
                        crate::guiding::CbLinkTarget::Game {
                            white_last, round, ..
                        } => {
                            assert_eq!(white_last, "Goetz");
                            assert_eq!(*round, Some(3));
                            saw_game = true;
                        }
                    },
                    crate::guiding::GuidingSpan::Text(_) => {}
                }
            }
        }
        assert!(
            saw_set && saw_table && saw_text && saw_game,
            "all CBLink kinds must parse"
        );
    }

    #[test]
    fn two_lid_text_title_decodes_single_string_block() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "brett-2cbh-title-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("titles.2lid");
        let mut bytes = vec![0u8; 0xB8 + 7 * 0x108A + 0xE82 + 64];
        // Minimal name header so NameBase uses default offsets.
        bytes[0..4].copy_from_slice(&0u32.to_be_bytes());
        bytes[4..8].copy_from_slice(&0u32.to_be_bytes());
        let id = 7u32;
        let base = 0xB8 + id as usize * 0x108A + 0xE82;
        let title = "Ostend 1907 Report";
        bytes[base..base + 4].copy_from_slice(&(title.len() as u32).to_le_bytes());
        bytes[base + 4..base + 4 + title.len()].copy_from_slice(title.as_bytes());
        {
            let mut file = std::fs::File::create(&path).unwrap();
            file.write_all(&bytes).unwrap();
        }
        let lid = crate::twocbh::NameBase::open(&path).unwrap();
        assert_eq!(lid.text_title(id), title);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
