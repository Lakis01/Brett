//! PGN export.
//!
//! The reader lives in [`crate::domain`]. This module writes the in-memory
//! tree back to standard PGN, including headers, comments, clocks, NAGs, and
//! nested variations. Export is streaming-friendly: one game is formatted at a
//! time so a large database does not need to be held in memory twice.

use std::io::{self, Write};

use shakmaty::fen::Fen;
use shakmaty::{Chess, Color, EnPassantMode};

use crate::domain::{Clock, Game};

const WIDTH: usize = 80;
const SEVEN_TAGS: [&str; 7] = ["Event", "Site", "Date", "Round", "White", "Black", "Result"];

/// Format one game as a complete PGN record, including the trailing blank line.
pub fn game_to_pgn(game: &Game) -> String {
    let mut out = String::new();
    write_headers(&mut out, game);
    out.push('\n');

    let mut movetext = Movetext::default();
    write_root_comment(&mut movetext, game);
    write_line(game, 0, false, &mut movetext);
    movetext.token(result_token(game));
    out.push_str(movetext.out.trim_end());
    out.push_str("\n\n");
    out
}

/// Write one game to a stream.
pub fn write_game(mut writer: impl Write, game: &Game) -> io::Result<()> {
    writer.write_all(game_to_pgn(game).as_bytes())
}

fn write_headers(out: &mut String, game: &Game) {
    let mut headers = game.headers.clone();
    normalize_round_board(&mut headers);
    let has = |headers: &[(String, String)], name: &str| {
        headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case(name))
    };

    if is_custom_start(game) && !has(&headers, "FEN") {
        headers.push(("SetUp".to_string(), "1".to_string()));
        headers.push(("FEN".to_string(), root_fen(game)));
    } else if has(&headers, "FEN") && !has(&headers, "SetUp") {
        headers.push(("SetUp".to_string(), "1".to_string()));
    }

    for tag in SEVEN_TAGS {
        if !has(&headers, tag) {
            headers.push((tag.to_string(), default_tag(tag).to_string()));
        }
    }

    let mut seen = std::collections::HashSet::new();
    for (name, value) in headers {
        let name = sanitize_tag_name(&name);
        if name.is_empty() || !seen.insert(name.to_ascii_lowercase()) {
            continue;
        }
        out.push('[');
        out.push_str(&name);
        out.push_str(" \"");
        out.push_str(&escape_tag_value(&value));
        out.push_str("\"]\n");
    }
}

/// ChessBase and other PGN writers encode the board as `round.board` inside
/// the Round tag. If an imported or manually added `Board` tag is present,
/// fold it into Round and drop the separate header.
fn normalize_round_board(headers: &mut Vec<(String, String)>) {
    let Some(board_index) = headers
        .iter()
        .position(|(name, _)| name.eq_ignore_ascii_case("Board"))
    else {
        return;
    };
    let (_, board) = headers.remove(board_index);
    let board = board.trim();
    if board.is_empty() {
        return;
    }
    if let Some((_, round)) = headers
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case("Round"))
    {
        let base = round
            .split_once('.')
            .map(|(round, _)| round)
            .unwrap_or(round)
            .trim()
            .to_string();
        *round = if base.is_empty() {
            format!("?.{board}")
        } else {
            format!("{base}.{board}")
        };
    } else {
        headers.push(("Round".to_string(), format!("?.{board}")));
    }
}

fn default_tag(tag: &str) -> &'static str {
    match tag {
        "Date" => "????.??.??",
        "Result" => "*",
        _ => "?",
    }
}

fn sanitize_tag_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect()
}

fn escape_tag_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

fn is_custom_start(game: &Game) -> bool {
    let Some(root) = game.nodes.first() else {
        return false;
    };
    let start = Fen::from_position(&Chess::default(), EnPassantMode::Legal).to_string();
    let root = Fen::from_position(&root.pos, EnPassantMode::Legal).to_string();
    root != start
}

fn root_fen(game: &Game) -> String {
    game.nodes
        .first()
        .map(|node| Fen::from_position(&node.pos, EnPassantMode::Legal).to_string())
        .unwrap_or_else(|| Fen::from_position(&Chess::default(), EnPassantMode::Legal).to_string())
}

fn result_token(game: &Game) -> &str {
    match game.header("Result").unwrap_or("*") {
        "1-0" | "0-1" | "1/2-1/2" | "*" => game.header("Result").unwrap_or("*"),
        _ => "*",
    }
}

fn write_root_comment(out: &mut Movetext, game: &Game) {
    let Some(root) = game.nodes.first() else {
        return;
    };
    if let Some(comment) = format_comment(&root.comment, root.clock) {
        out.token(&comment);
    }
}

/// Write the mainline below `node`, plus every alternative branch.
fn write_line(game: &Game, node: usize, force_number: bool, out: &mut Movetext) {
    let Some(&first) = game.nodes[node].children.first() else {
        return;
    };

    write_move(game, first, force_number, out);
    for &alternative in game.nodes[node].children.iter().skip(1) {
        out.token("(");
        write_move(game, alternative, true, out);
        write_line(game, alternative, false, out);
        out.token(")");
    }
    write_line(game, first, false, out);
}

fn write_move(game: &Game, node: usize, force_number: bool, out: &mut Movetext) {
    let Some(ply) = game.nodes[node].mv.as_ref() else {
        return;
    };

    match ply.color {
        Color::White => out.token(&format!("{}.", ply.number)),
        Color::Black if force_number => out.token(&format!("{}...", ply.number)),
        Color::Black => {}
    }
    out.token(&ply.san);

    for nag in &game.nodes[node].nags {
        if *nag > 0 {
            out.token(&format!("${nag}"));
        }
    }

    if let Some(comment) = format_comment(&game.nodes[node].comment, game.nodes[node].clock) {
        out.token(&comment);
    }
}

fn format_comment(comment: &str, clock: Option<Clock>) -> Option<String> {
    let text = comment
        .replace(['\n', '\r'], " ")
        .replace('{', "(")
        .replace('}', ")");
    let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(clock) = clock {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str("[%clk ");
        text.push_str(&clock_text(clock));
        text.push(']');
    }
    (!text.is_empty()).then(|| format!("{{{text}}}"))
}

fn clock_text(clock: Clock) -> String {
    let total = clock.centis;
    let hours = total / 360_000;
    let minutes = (total % 360_000) / 6_000;
    let seconds = (total % 6_000) / 100;
    let centis = total % 100;
    let fraction = if centis > 0 {
        format!(".{centis:02}")
    } else {
        String::new()
    };
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}{fraction}")
    } else {
        format!("{minutes}:{seconds:02}{fraction}")
    }
}

#[derive(Default)]
struct Movetext {
    out: String,
    column: usize,
}

impl Movetext {
    fn token(&mut self, token: &str) {
        let width = token.chars().count();
        if self.column > 0 {
            if self.column + 1 + width > WIDTH {
                self.out.push('\n');
                self.column = 0;
            } else {
                self.out.push(' ');
                self.column += 1;
            }
        }
        self.out.push_str(token);
        self.column += width;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::parse_pgn;

    fn parse_one(pgn: &str) -> Game {
        parse_pgn(pgn.as_bytes()).unwrap().remove(0)
    }

    #[test]
    fn round_trips_comments_clocks_nags_and_variations() {
        let source = r#"[Event "Test"]
[Site "Brett"]
[Date "2026.10.24"]
[Round "1"]
[White "Müller, Jörg"]
[Black "Tal, Mikhail"]
[Result "*"]

1. e4 {king pawn [%clk 0:03:00]} $1 (1. d4 d5) e5 2. Nf3 *
"#;
        let game = parse_one(source);
        let text = game_to_pgn(&game);
        let round = parse_one(&text);

        assert_eq!(round.header("White"), Some("Müller, Jörg"));
        assert_eq!(round.header("Black"), Some("Tal, Mikhail"));
        assert_eq!(round.mainline_len(), 3);
        assert_eq!(round.nodes[1].comment, "king pawn");
        assert_eq!(round.nodes[1].clock.map(|clock| clock.centis), Some(18_000));
        assert_eq!(round.nodes[1].nags, vec![1]);
        assert_eq!(round.nodes[0].children, vec![1, 2]);
        assert_eq!(round.nodes[2].san(), Some("d4"));
        assert_eq!(round.nodes[2].children, vec![3]);
        assert_eq!(round.nodes[3].san(), Some("d5"));
        assert_eq!(round.nodes[4].san(), Some("e5"));
        assert_eq!(round.nodes[5].san(), Some("Nf3"));
    }

    #[test]
    fn preserves_a_custom_start_position() {
        let source = r#"[Event "Setup"]
[Site "?"]
[Date "????.??.??"]
[Round "?"]
[White "White"]
[Black "Black"]
[Result "*"]
[SetUp "1"]
[FEN "8/8/8/8/8/8/K7/7k w - - 0 1"]

1. Kb2 *
"#;
        let game = parse_one(source);
        let text = game_to_pgn(&game);
        let round = parse_one(&text);
        assert_eq!(round.header("FEN"), Some("8/8/8/8/8/8/K7/7k w - - 0 1"));
        assert_eq!(round.mainline_len(), 1);
    }

    #[test]
    fn writes_the_result_for_an_empty_game() {
        let game = parse_one(
            "[Event \"Empty\"]\n[Site \"?\"]\n[Date \"????.??.??\"]\n[Round \"?\"]\n[White \"?\"]\n[Black \"?\"]\n[Result \"1/2-1/2\"]\n\n1/2-1/2\n",
        );
        let text = game_to_pgn(&game);
        assert!(text.contains("[Event \"Empty\"]"));
        assert!(text.trim_end().ends_with("1/2-1/2"));
        let round = parse_one(&text);
        assert_eq!(round.mainline_len(), 0);
    }

    #[test]
    fn round_trips_the_fixture_tree() {
        let game = parse_pgn(include_bytes!("../fixtures/opera.pgn"))
            .unwrap()
            .remove(0);
        let text = game_to_pgn(&game);
        let round = parse_one(&text);
        assert_eq!(round.mainline_len(), game.mainline_len());
        assert_eq!(round.nodes.len(), game.nodes.len());
        for (before, after) in game.nodes.iter().zip(&round.nodes) {
            assert_eq!(before.san(), after.san());
            assert_eq!(before.comment, after.comment);
            assert_eq!(before.clock, after.clock);
            assert_eq!(before.nags, after.nags);
            assert_eq!(before.children.len(), after.children.len());
        }
    }

    #[test]
    fn writes_a_board_tag_as_round_dot_board() {
        let game = parse_one(
            "[Event \"Board\"]\n[Site \"?\"]\n[Date \"2026.10.25\"]\n[Round \"3\"]\n[Board \"4\"]\n[White \"White\"]\n[Black \"Black\"]\n[Result \"*\"]\n\n*\n",
        );
        let text = game_to_pgn(&game);
        assert!(text.contains("[Round \"3.4\"]"));
        assert!(!text.contains("[Board "));

        let mut game = game;
        game.set_header("Round", "?".to_string());
        let text = game_to_pgn(&game);
        assert!(text.contains("[Round \"?.4\"]"));
    }

    #[test]
    fn escapes_tag_values() {
        let mut game = Game::default();
        game.set_header("Event", "A \"quoted\" \\ event".to_string());
        let text = game_to_pgn(&game);
        assert!(text.contains(r#"[Event "A \"quoted\" \\ event"]"#));
        let round = parse_one(&text);
        assert_eq!(round.header("Event"), Some("A \"quoted\" \\ event"));
    }
}
