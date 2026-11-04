//! Decode the `.cba` annotation record that belongs to one classic game.
//!
//! A record starts with a 14-byte head (game id, marker, item count plus one,
//! record size) followed by self-sized items.  An item names a position in
//! stored move order, a type and a payload.  Brett needs comments, NAGs and
//! the compact PGN drawing tokens ChessBase's own export writes:
//!
//! * text comments (`0x02` after the move, `0x82` before it),
//! * NAGs (`0x03`),
//! * coloured squares (`0x04`) and arrows (`0x05`), written as
//!   `[%csl …]` / `[%cal …]`,
//! * time spent (`0x07`), `[%emt …]`,
//! * engine evaluations (`0x21`), `[%eval …]`,
//! * medals (`0x22`), `[%mdl …]`.
//!
//! Other kinds (quotations, clocks, media) are skipped by their own size,
//! exactly as a reader that does not render them should.
//!
//! Annotation positions count every node in stored depth-first order: the
//! root is `-1`, the first stored move is `0`, and so on.  That is the order
//! [`super::moves`] appends, deliberately without merging equal moves.

use anyhow::{Context, Result, bail};

use super::CbgRecord;
use crate::chess::{File, Rank, Square};
use crate::domain::{Game, Shape, ShapeColor};

const HEAD_LEN: usize = 14;
const ITEM_HEAD_LEN: usize = 6;
const MARK: [u8; 4] = [1, 0, 0x0e, 0x0e];

enum Parsed {
    Text {
        before: bool,
        text: String,
    },
    Nags {
        prefix: u8,
        on_move: u8,
        on_position: u8,
    },
    Squares(Vec<(u8, u8)>),
    Arrows(Vec<(u8, u8, u8)>),
    Evaluation(String),
    Time(String),
    Medal(String),
}

struct Item {
    position: i32,
    parsed: Parsed,
}

#[derive(Default)]
struct Note {
    before: Vec<String>,
    evaluations: Vec<String>,
    times: Vec<String>,
    squares: Vec<(u8, u8)>,
    arrows: Vec<(u8, u8, u8)>,
    medals: Vec<String>,
    texts: Vec<String>,
}

impl Note {
    /// The comment after the move, in ChessBase's export order: engine
    /// evaluations, times, drawing tokens, medals, then texts.
    fn after(&self) -> String {
        let mut parts: Vec<(bool, String)> = Vec::new();
        for evaluation in &self.evaluations {
            parts.push((false, evaluation.clone()));
        }
        for time in &self.times {
            parts.push((true, time.clone()));
        }
        for medal in &self.medals {
            parts.push((false, medal.clone()));
        }
        for text in &self.texts {
            parts.push((true, text.clone()));
        }

        let mut out = String::new();
        for (separator, part) in parts {
            if separator && !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&part);
        }
        out
    }
}

pub(super) fn apply(record: &CbgRecord, id: u32, game: &mut Game) -> Result<()> {
    if record.head_len < HEAD_LEN {
        bail!("annotation record is shorter than its 14-byte head");
    }
    let head = &record.head[..HEAD_LEN];
    if be_u24(&head[0..3]) != id {
        bail!(
            "annotation head names game {}, not {id}",
            be_u24(&head[0..3])
        );
    }
    if head[3..7] != MARK {
        bail!("annotation head has unexpected marker bytes");
    }
    let stated_size = be_u32(&head[10..14]) as usize;
    let actual_size = HEAD_LEN + record.bytes.len();
    if stated_size != actual_size {
        bail!("annotation head states {stated_size} bytes but the record has {actual_size}");
    }

    let mut items = Vec::new();
    let mut walked = 0u32;
    let mut at = 0usize;
    while at < record.bytes.len() {
        let rest = &record.bytes[at..];
        if rest.len() < ITEM_HEAD_LEN {
            bail!("annotation item head at +{at} runs past the record");
        }
        let position = int24(be_u24(&rest[0..3]));
        if position < -1 {
            bail!("annotation position {position} is below the game root");
        }
        let kind = rest[3];
        let size = be_u16(&rest[4..6]) as usize;
        if size < ITEM_HEAD_LEN || size > rest.len() {
            bail!("annotation item at +{at} has invalid size {size}");
        }
        let data = &rest[ITEM_HEAD_LEN..size];
        let parsed = match kind {
            0x02 | 0x82 => {
                if data.len() < 2 {
                    bail!("text annotation at +{at} has no language byte");
                }
                let language = data[1];
                let text = clean(&super::decode_text(&data[2..]), language != 7);
                if text.is_empty() {
                    None
                } else {
                    Some(Parsed::Text {
                        before: kind == 0x82,
                        text,
                    })
                }
            }
            0x03 => {
                if data.is_empty() || data.len() > 3 {
                    bail!("NAG annotation at +{at} has {} bytes", data.len());
                }
                let nag = |index: usize| data.get(index).copied().unwrap_or(0);
                Some(Parsed::Nags {
                    on_move: nag(0),
                    on_position: nag(1),
                    prefix: nag(2),
                })
            }
            0x04 => Some(Parsed::Squares(
                data.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| (pair[0], pair[1]))
                    .collect(),
            )),
            0x05 => Some(Parsed::Arrows(
                data.as_chunks::<3>()
                    .0
                    .iter()
                    .map(|triple| (triple[0], triple[1], triple[2]))
                    .collect(),
            )),
            0x07 => time_token(data).map(Parsed::Time),
            0x21 => evaluation_token(data).map(Parsed::Evaluation),
            0x22 => medal_token(data).map(Parsed::Medal),
            _ => None,
        };
        if let Some(parsed) = parsed {
            items.push(Item { position, parsed });
        }
        walked += 1;
        at += size;
    }
    let stated_count = be_u24(&head[7..10]);
    if stated_count == 0 || stated_count - 1 != walked {
        bail!(
            "annotation head counts {} items but the record holds {walked}",
            stated_count.saturating_sub(1)
        );
    }

    let total_moves = game.nodes.len().saturating_sub(1);
    let mainline_last = mainline_last(game);
    let mut notes: Vec<Option<Note>> = (0..game.nodes.len()).map(|_| None).collect();

    for item in items {
        let node = node_for(item.position, total_moves, mainline_last)?;
        match item.parsed {
            Parsed::Text { before, text } => {
                let note = notes[node].get_or_insert_with(Note::default);
                if before {
                    note.before.push(text);
                } else {
                    note.texts.push(text);
                }
            }
            Parsed::Nags {
                prefix,
                on_move,
                on_position,
            } => {
                // ChessBase's export writes the prefix first, then the move's
                // NAG and finally the NAG on the position.
                for nag in [prefix, on_move, on_position] {
                    if nag != 0 {
                        game.nodes[node].nags.push(nag);
                    }
                }
            }
            Parsed::Squares(squares) => {
                notes[node]
                    .get_or_insert_with(Note::default)
                    .squares
                    .extend(squares);
            }
            Parsed::Arrows(arrows) => {
                notes[node]
                    .get_or_insert_with(Note::default)
                    .arrows
                    .extend(arrows);
            }
            Parsed::Evaluation(evaluation) => {
                notes[node]
                    .get_or_insert_with(Note::default)
                    .evaluations
                    .push(evaluation);
            }
            Parsed::Time(time) => {
                notes[node]
                    .get_or_insert_with(Note::default)
                    .times
                    .push(time);
            }
            Parsed::Medal(medal) => {
                notes[node]
                    .get_or_insert_with(Note::default)
                    .medals
                    .push(medal);
            }
        }
    }

    for (node, note) in notes.into_iter().enumerate() {
        let Some(note) = note else {
            continue;
        };
        for &(color, square) in &note.squares {
            if let (Some(color), Some(square)) = (shape_color(color), shape_square(square))
                && !game.nodes[node]
                    .shapes
                    .contains(&Shape::Square { color, square })
            {
                game.nodes[node]
                    .shapes
                    .push(Shape::Square { color, square });
            }
        }
        for &(color, from, to) in &note.arrows {
            if let (Some(color), Some(from), Some(to)) =
                (shape_color(color), shape_square(from), shape_square(to))
                && !game.nodes[node]
                    .shapes
                    .contains(&Shape::Arrow { color, from, to })
            {
                game.nodes[node]
                    .shapes
                    .push(Shape::Arrow { color, from, to });
            }
        }
        let mut text = note.before.join(" ");
        let after = note.after();
        if !after.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&after);
        }
        if text.is_empty() {
            continue;
        }
        if !game.nodes[node].comment.is_empty() {
            game.nodes[node].comment.push(' ');
        }
        game.nodes[node].comment.push_str(&text);
    }
    Ok(())
}

fn node_for(position: i32, total_moves: usize, mainline_last: Option<usize>) -> Result<usize> {
    if position == -1 {
        return Ok(0);
    }
    if total_moves == 0 {
        bail!("annotation at position {position} in a game without moves");
    }
    if position >= 0 && (position as usize) < total_moves {
        return Ok(position as usize + 1);
    }
    mainline_last.context("annotations lie past the last move but the main line has no move")
}

fn mainline_last(game: &Game) -> Option<usize> {
    let mut node = 0usize;
    let mut found = false;
    while let Some(&child) = game.nodes[node].children.first() {
        node = child;
        found = true;
    }
    found.then_some(node)
}

fn clean(text: &str, own_language: bool) -> String {
    let end = text.find('\0').unwrap_or(text.len());
    let mut text = text[..end].to_string();
    if own_language && text.chars().count() > 1 {
        text = text.replace('\u{4}', "[#]");
    }
    while text.ends_with(' ') {
        text.pop();
    }
    text
}

/// ChessBase drawing colour: 2 green, 3 yellow, 4 red, 7 blue. Unknown
/// colours are skipped by the caller.
fn shape_color(color: u8) -> Option<ShapeColor> {
    match color {
        2 => Some(ShapeColor::Green),
        3 => Some(ShapeColor::Yellow),
        4 => Some(ShapeColor::Red),
        7 => Some(ShapeColor::Blue),
        _ => None,
    }
}

/// The drawing payload numbers squares from 1, file by file: `a1` is 1 and
/// `b1` is 9.
fn shape_square(square: u8) -> Option<Square> {
    if !(1..=64).contains(&square) {
        return None;
    }
    let square = square - 1;
    Some(Square::from_coords(
        File::new(u32::from(square / 8)),
        Rank::new(u32::from(square % 8)),
    ))
}

/// `07 1e 00 00` is `[%emt 0:30:00]`; the last byte holds hundredths.
fn time_token(data: &[u8]) -> Option<String> {
    let &[hours, minutes, seconds, hundredths] = data else {
        return None;
    };
    if minutes >= 60 || seconds >= 60 {
        return None;
    }
    let mut out = format!("[%emt {hours}:{minutes:02}:{seconds:02}");
    if hundredths != 0 {
        out.push_str(&format!(".{hundredths:02}"));
    }
    out.push(']');
    Some(out)
}

/// `21` holds the score, kind and depth as little-endian shorts. A mate is
/// written as ±(32767 − n), which is the form ChessBase's export uses.
fn evaluation_token(data: &[u8]) -> Option<String> {
    if data.len() != 6 {
        return None;
    }
    let short = |at: usize| i16::from_le_bytes([data[at], data[at + 1]]);
    let (value, kind, depth) = (short(0), short(2), short(4));
    let score = match (kind, value) {
        (1, value) if value > 0 => 32767 - value,
        (1, value) => -32767 - value,
        (_, value) => value,
    };
    Some(format!("[%eval {score},{depth}]"))
}

fn medal_token(data: &[u8]) -> Option<String> {
    let bytes: [u8; 4] = data.try_into().ok()?;
    Some(format!("[%mdl {}]", u32::from_be_bytes(bytes)))
}

fn int24(value: u32) -> i32 {
    ((value << 8) as i32) >> 8
}

fn be_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be_u24(bytes: &[u8]) -> u32 {
    (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
