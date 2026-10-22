//! Game tree model and PGN import.
//!
//! The tree is arena-based: every position after a move is a node, and the
//! root node is the starting position. `children[0]` is the mainline
//! continuation; any further children are variations. Navigation is therefore
//! just a `usize` node id, and going "back" is `parent`.

use std::num::NonZeroU32;
use std::ops::ControlFlow;

use anyhow::{Context, Result};
use pgn_reader::{Nag, RawComment, RawTag, Reader, SanPlus, Skip, Visitor};
use shakmaty::fen::Fen;
use shakmaty::san::San;
use shakmaty::uci::UciMove;
use shakmaty::{CastlingMode, Chess, Color, EnPassantMode, FromSetup, Position, Role, Square};

/// One played move, with both notations and the move number it was played on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ply {
    pub san: String,
    pub uci: String,
    pub number: u32,
    pub color: Color,
}

impl Ply {
    pub fn is_white(&self) -> bool {
        self.color == Color::White
    }
}

/// A position in the game tree.
#[derive(Clone, Debug)]
pub struct Node {
    /// `None` only for the root node.
    pub mv: Option<Ply>,
    pub comment: String,
    pub nags: Vec<u8>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Position *after* `mv` (the start position for the root).
    pub pos: Chess,
}

impl Node {
    pub fn san(&self) -> Option<&str> {
        self.mv.as_ref().map(|m| m.san.as_str())
    }
}

/// A single game: headers plus an arena of nodes rooted at index 0.
#[derive(Clone, Debug)]
pub struct Game {
    pub headers: Vec<(String, String)>,
    pub nodes: Vec<Node>,
    /// Non-fatal problems encountered while importing (illegal move, bad FEN).
    pub errors: Vec<String>,
}

impl Default for Game {
    fn default() -> Self {
        Self {
            headers: Vec::new(),
            nodes: vec![Node {
                mv: None,
                comment: String::new(),
                nags: Vec::new(),
                parent: None,
                children: Vec::new(),
                pos: Chess::default(),
            }],
            errors: Vec::new(),
        }
    }
}

impl Game {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    pub fn white(&self) -> &str {
        self.header("White").unwrap_or("?")
    }

    pub fn black(&self) -> &str {
        self.header("Black").unwrap_or("?")
    }

    pub fn result(&self) -> &str {
        self.header("Result").unwrap_or("*")
    }

    pub fn date(&self) -> &str {
        self.header("Date").unwrap_or("????.??.??")
    }

    pub fn event(&self) -> &str {
        self.header("Event").unwrap_or("?")
    }

    pub fn root(&self) -> &Node {
        &self.nodes[0]
    }

    /// Number of plies along the mainline (variations excluded).
    pub fn mainline_len(&self) -> usize {
        let mut n = 0;
        let mut id = 0;
        while let Some(&child) = self.nodes[id].children.first() {
            n += 1;
            id = child;
        }
        n
    }

    /// Node ids along the mainline, excluding the root.
    pub fn mainline(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut id = 0;
        while let Some(&child) = self.nodes[id].children.first() {
            out.push(child);
            id = child;
        }
        out
    }

    /// Final position of the game, following the mainline.
    pub fn final_position(&self) -> &Chess {
        let id = self.mainline().last().copied().unwrap_or(0);
        &self.nodes[id].pos
    }

    /// Append `mv` under `parent`, reusing an existing child with the same move.
    ///
    /// If `parent` already has a mainline child, the new move becomes a
    /// variation; this matches how a user exploring a game expects the tree
    /// to grow.
    pub fn add_move(&mut self, parent: usize, mv: shakmaty::Move) -> usize {
        let uci = UciMove::from_move(mv, CastlingMode::Standard).to_string();

        let existing = self.nodes[parent]
            .children
            .iter()
            .copied()
            .find(|&child| self.nodes[child].mv.as_ref().is_some_and(|p| p.uci == uci));
        if let Some(child) = existing {
            return child;
        }

        let pos = self.nodes[parent].pos.clone();
        let after = pos
            .clone()
            .play(mv)
            .expect("add_move must be called with a legal move");
        let ply = Ply {
            san: SanPlus::from_move(pos.clone(), mv).to_string(),
            uci,
            number: pos.fullmoves().get(),
            color: pos.turn(),
        };

        let id = self.nodes.len();
        self.nodes.push(Node {
            mv: Some(ply),
            comment: String::new(),
            nags: Vec::new(),
            parent: Some(parent),
            children: Vec::new(),
            pos: after,
        });
        self.nodes[parent].children.push(id);
        id
    }
}

/// Parse every game in a PGN byte stream.
pub fn parse_pgn(bytes: &[u8]) -> Result<Vec<Game>> {
    let mut reader = Reader::new(std::io::Cursor::new(bytes));
    let mut visitor = TreeVisitor;
    let mut games = Vec::new();
    while let Some(game) = reader.read_game(&mut visitor).context("reading PGN")? {
        games.push(game);
    }
    Ok(games)
}

pub fn parse_pgn_file(path: &std::path::Path) -> Result<Vec<Game>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    parse_pgn(&bytes)
}

struct TreeVisitor;

impl Visitor for TreeVisitor {
    type Tags = Vec<(String, String)>;
    type Movetext = Builder;
    type Output = Game;

    fn begin_tags(&mut self) -> ControlFlow<Game, Vec<(String, String)>> {
        ControlFlow::Continue(Vec::new())
    }

    fn tag(
        &mut self,
        tags: &mut Vec<(String, String)>,
        name: &[u8],
        value: RawTag<'_>,
    ) -> ControlFlow<Game> {
        tags.push((
            String::from_utf8_lossy(name).into_owned(),
            value.decode_utf8_lossy().into_owned(),
        ));
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, tags: Vec<(String, String)>) -> ControlFlow<Game, Builder> {
        ControlFlow::Continue(Builder::new(tags))
    }

    fn san(&mut self, mt: &mut Builder, san_plus: SanPlus) -> ControlFlow<Game> {
        mt.push(san_plus);
        ControlFlow::Continue(())
    }

    fn nag(&mut self, mt: &mut Builder, nag: Nag) -> ControlFlow<Game> {
        mt.nodes[mt.current].nags.push(nag.0);
        ControlFlow::Continue(())
    }

    fn comment(&mut self, mt: &mut Builder, comment: RawComment<'_>) -> ControlFlow<Game> {
        let text = String::from_utf8_lossy(comment.0);
        let node = &mut mt.nodes[mt.current];
        if !text.is_empty() {
            if !node.comment.is_empty() {
                node.comment.push(' ');
            }
            node.comment.push_str(text.trim());
        }
        ControlFlow::Continue(())
    }

    fn begin_variation(&mut self, mt: &mut Builder) -> ControlFlow<Game, Skip> {
        // A variation replaces the move that was just played, so it starts
        // from the position before that move.
        let before = mt.nodes[mt.current].parent.unwrap_or(0);
        mt.stack.push(mt.current);
        mt.current = before;
        ControlFlow::Continue(Skip(false))
    }

    fn end_variation(&mut self, mt: &mut Builder) -> ControlFlow<Game> {
        mt.current = mt.stack.pop().unwrap_or(0);
        ControlFlow::Continue(())
    }

    fn end_game(&mut self, mt: Builder) -> Game {
        mt.finish()
    }
}

struct Builder {
    headers: Vec<(String, String)>,
    nodes: Vec<Node>,
    current: usize,
    stack: Vec<usize>,
    errors: Vec<String>,
}

impl Builder {
    fn new(headers: Vec<(String, String)>) -> Self {
        let mut errors = Vec::new();
        let start = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("FEN"))
            .and_then(|(_, v)| Fen::from_ascii(v.as_bytes()).ok())
            .and_then(|fen| fen.into_position(CastlingMode::Standard).ok());

        let pos = match start {
            Some(pos) => pos,
            None => {
                if headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("FEN")) {
                    errors.push("invalid FEN tag, using the start position".to_string());
                }
                Chess::default()
            }
        };

        Self {
            headers,
            nodes: vec![Node {
                mv: None,
                comment: String::new(),
                nags: Vec::new(),
                parent: None,
                children: Vec::new(),
                pos,
            }],
            current: 0,
            stack: Vec::new(),
            errors,
        }
    }

    fn push(&mut self, san_plus: SanPlus) {
        let parent = self.current;
        let pos = self.nodes[parent].pos.clone();
        let number = pos.fullmoves().get();
        let color = pos.turn();
        let fen = || Fen::from_position(&pos, EnPassantMode::Legal).to_string();

        if matches!(san_plus.san, San::Null) {
            // Analysis null move: the side to move passes. The position keeps
            // its pieces but the turn flips, and en passant rights are gone.
            let mut setup = Fen::from_position(&pos, EnPassantMode::Legal).into_setup();
            setup.swap_turn();
            setup.halfmoves = 0;
            if color == Color::Black {
                setup.fullmoves = NonZeroU32::new(setup.fullmoves.get() + 1).unwrap();
            }
            match Chess::from_setup(setup, CastlingMode::Standard) {
                Ok(after) => {
                    let ply = Ply {
                        san: "--".to_string(),
                        uci: "0000".to_string(),
                        number,
                        color,
                    };
                    let id = self.insert(parent, ply, after);
                    self.current = id;
                }
                Err(err) => {
                    self.errors
                        .push(format!("cannot apply null move in {}: {err}", fen()));
                }
            }
            return;
        }

        let mv = match san_plus.san.to_move(&pos) {
            Ok(mv) => mv,
            Err(err) => {
                self.errors
                    .push(format!("illegal move {} in {}: {err}", san_plus, fen()));
                return;
            }
        };

        let after = match pos.clone().play(mv) {
            Ok(after) => after,
            Err(err) => {
                self.errors
                    .push(format!("illegal move {} in {}: {err}", san_plus, fen()));
                return;
            }
        };

        let ply = Ply {
            san: san_plus.to_string(),
            uci: UciMove::from_move(mv, CastlingMode::Standard).to_string(),
            number,
            color,
        };
        let id = self.insert(parent, ply, after);
        self.current = id;
    }

    fn insert(&mut self, parent: usize, ply: Ply, after: Chess) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            mv: Some(ply),
            comment: String::new(),
            nags: Vec::new(),
            parent: Some(parent),
            children: Vec::new(),
            pos: after,
        });
        self.nodes[parent].children.push(id);
        id
    }

    fn finish(self) -> Game {
        Game {
            headers: self.headers,
            nodes: self.nodes,
            errors: self.errors,
        }
    }
}

/// A display token for the move list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Move {
        node: usize,
        number: u32,
        color: Color,
        san: String,
        line_start: bool,
    },
    OpenParen,
    CloseParen,
}

/// One visually distinct line of PGN movetext.
///
/// Top-level variations are placed on their own indented lines. Nested
/// variations stay inline inside their parent variation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveLine {
    pub depth: usize,
    pub tokens: Vec<Token>,
}

/// Split a game into readable PGN lines.
///
/// The mainline remains continuous until a position has an alternative move.
/// At that point the mainline line is flushed, each alternative is rendered on
/// an indented line, and the mainline resumes on a fresh line. This keeps long
/// annotated games readable without losing PGN ordering.
pub fn move_lines(game: &Game) -> Vec<MoveLine> {
    let mut lines = Vec::new();
    let mut current = Vec::new();
    walk_lines(game, 0, 0, &mut current, &mut lines);
    flush_line(0, current, &mut lines);
    lines
}

fn flush_line(depth: usize, tokens: Vec<Token>, lines: &mut Vec<MoveLine>) {
    if !tokens.is_empty() {
        lines.push(MoveLine { depth, tokens });
    }
}

fn walk_lines(
    game: &Game,
    node: usize,
    depth: usize,
    current: &mut Vec<Token>,
    lines: &mut Vec<MoveLine>,
) {
    let Some(&first) = game.nodes[node].children.first() else {
        return;
    };

    push_move(game, first, current.is_empty(), current);

    if game.nodes[node].children.len() > 1 {
        flush_line(depth, std::mem::take(current), lines);
        for &alternative in game.nodes[node].children.iter().skip(1) {
            let mut variation = vec![Token::OpenParen];
            push_move(game, alternative, true, &mut variation);
            walk_inline(game, alternative, &mut variation);
            variation.push(Token::CloseParen);
            lines.push(MoveLine {
                depth: depth + 1,
                tokens: variation,
            });
        }
    }

    walk_lines(game, first, depth, current, lines);
}

/// Flatten an alternative line, including its own nested variations inline.
fn walk_inline(game: &Game, node: usize, out: &mut Vec<Token>) {
    let Some(&first) = game.nodes[node].children.first() else {
        return;
    };

    push_move(game, first, false, out);
    for &alternative in game.nodes[node].children.iter().skip(1) {
        out.push(Token::OpenParen);
        push_move(game, alternative, true, out);
        walk_inline(game, alternative, out);
        out.push(Token::CloseParen);
    }
    walk_inline(game, first, out);
}

/// Render SAN with Unicode figurines (`Nf3` → `♘f3`).
///
/// Castling and pawn moves keep their normal notation. Promotion letters are
/// replaced as well (`e8=Q+` → `e8=♕+`).
pub fn figurine_san(san: &str) -> String {
    let mut out = String::with_capacity(san.len() + 3);
    let mut chars = san.chars();
    if let Some(first) = chars.next() {
        if let Some(glyph) = piece_glyph(first) {
            out.push(glyph);
        } else {
            out.push(first);
        }
    }

    let rest: String = chars.collect();
    let mut iter = rest.chars().peekable();
    while let Some(c) = iter.next() {
        if c == '='
            && let Some(glyph) = iter.peek().and_then(|next| piece_glyph(*next))
        {
            out.push('=');
            out.push(glyph);
            iter.next();
        } else {
            out.push(c);
        }
    }
    out
}

fn piece_glyph(letter: char) -> Option<char> {
    match letter {
        'K' => Some('♔'),
        'Q' => Some('♕'),
        'R' => Some('♖'),
        'B' => Some('♗'),
        'N' => Some('♘'),
        _ => None,
    }
}

/// Unicode annotation symbol for a standard PGN NAG.
pub fn nag_symbol(nag: u8) -> Option<&'static str> {
    Some(match nag {
        1 => "!",
        2 => "?",
        3 => "!!",
        4 => "??",
        5 => "!?",
        6 => "?!",
        7 | 8 => "□",
        9 => "??",
        10 | 11 => "=",
        12 | 13 => "∞",
        14 => "+=",
        15 => "=+",
        16 => "±",
        17 => "∓",
        18 => "+−",
        19 => "−+",
        20 => "+−−",
        21 => "−−+",
        22 | 23 => "⨀",
        26 | 35 | 36 => "↑",
        40 | 41 => "→",
        44 => "+=",
        48 | 50 => "⊕",
        54 | 56 | 58 => "→",
        60 | 62 | 64 => "←",
        130..=135 => "⇆",
        136 => "⏱",
        140 => "∆",
        142 => ">",
        144 => "⇄",
        145 => "RR",
        146 => "N",
        147 => "×",
        149 => "≡",
        151 => "BP",
        153 => "OB",
        154 => "SB",
        190 => "≈",
        191 => "2P",
        192 => "SP",
        193 => "UP",
        201 => "D",
        210 => "↗",
        211 => "#",
        212 => "↑",
        213 => "＋",
        214 => "|_",
        215 => "_|",
        _ => return None,
    })
}

/// One selectable annotation in the move context menu.
#[derive(Clone, Copy, Debug)]
pub struct NagChoice {
    pub code: u8,
    pub symbol: &'static str,
    pub label: &'static str,
}

/// Common chess annotation symbols, grouped for menus and the annotation
/// editor. The numeric code is the standard NAG stored in the PGN.
pub const NAG_GROUPS: &[(&str, &[NagChoice])] = &[
    (
        "Move quality",
        &[
            NagChoice {
                code: 1,
                symbol: "!",
                label: "Good move",
            },
            NagChoice {
                code: 2,
                symbol: "?",
                label: "Poor move",
            },
            NagChoice {
                code: 3,
                symbol: "!!",
                label: "Excellent move",
            },
            NagChoice {
                code: 4,
                symbol: "??",
                label: "Blunder",
            },
            NagChoice {
                code: 5,
                symbol: "!?",
                label: "Interesting move",
            },
            NagChoice {
                code: 6,
                symbol: "?!",
                label: "Dubious move",
            },
            NagChoice {
                code: 7,
                symbol: "□",
                label: "Forced move",
            },
            NagChoice {
                code: 8,
                symbol: "□",
                label: "Only move",
            },
        ],
    ),
    (
        "Evaluation",
        &[
            NagChoice {
                code: 10,
                symbol: "=",
                label: "Equal position",
            },
            NagChoice {
                code: 12,
                symbol: "∞",
                label: "Unclear position",
            },
            NagChoice {
                code: 14,
                symbol: "+=",
                label: "White is slightly better",
            },
            NagChoice {
                code: 15,
                symbol: "=+",
                label: "Black is slightly better",
            },
            NagChoice {
                code: 16,
                symbol: "±",
                label: "White is better",
            },
            NagChoice {
                code: 17,
                symbol: "∓",
                label: "Black is better",
            },
            NagChoice {
                code: 18,
                symbol: "+−",
                label: "White is winning",
            },
            NagChoice {
                code: 19,
                symbol: "−+",
                label: "Black is winning",
            },
            NagChoice {
                code: 20,
                symbol: "+−−",
                label: "White has a decisive advantage",
            },
            NagChoice {
                code: 21,
                symbol: "−−+",
                label: "Black has a decisive advantage",
            },
        ],
    ),
    (
        "Ideas and structure",
        &[
            NagChoice {
                code: 22,
                symbol: "⨀",
                label: "Zugzwang",
            },
            NagChoice {
                code: 36,
                symbol: "↑",
                label: "With the initiative",
            },
            NagChoice {
                code: 40,
                symbol: "→",
                label: "With an attack",
            },
            NagChoice {
                code: 130,
                symbol: "⇆",
                label: "Counterplay",
            },
            NagChoice {
                code: 140,
                symbol: "∆",
                label: "With the idea",
            },
            NagChoice {
                code: 142,
                symbol: ">",
                label: "Better is",
            },
            NagChoice {
                code: 146,
                symbol: "N",
                label: "Novelty",
            },
            NagChoice {
                code: 201,
                symbol: "D",
                label: "Diagram",
            },
            NagChoice {
                code: 48,
                symbol: "⊕",
                label: "White has a slight centre",
            },
            NagChoice {
                code: 191,
                symbol: "2P",
                label: "Double pawns",
            },
            NagChoice {
                code: 192,
                symbol: "SP",
                label: "Separated pawns",
            },
            NagChoice {
                code: 193,
                symbol: "UP",
                label: "United pawns",
            },
        ],
    ),
];

/// Flatten the tree into PGN-style ordered tokens.
///
/// At every position the mainline move is written first, followed by the
/// variations that replace it, matching PGN convention:
/// `3. d4 Bg4 (3... exd4 4. Nxd4) 4. dxe5`.
pub fn tokens(game: &Game) -> Vec<Token> {
    let mut out = Vec::new();
    walk(game, 0, true, &mut out);
    out
}

fn push_move(game: &Game, id: usize, line_start: bool, out: &mut Vec<Token>) {
    let Some(ply) = &game.nodes[id].mv else {
        return;
    };
    out.push(Token::Move {
        node: id,
        number: ply.number,
        color: ply.color,
        san: ply.san.clone(),
        line_start,
    });
}

/// `node` is a position; emit its mainline continuation and its variations.
fn walk(game: &Game, node: usize, line_start: bool, out: &mut Vec<Token>) {
    let Some(&first) = game.nodes[node].children.first() else {
        return;
    };

    push_move(game, first, line_start, out);

    for &alternative in game.nodes[node].children.iter().skip(1) {
        out.push(Token::OpenParen);
        push_move(game, alternative, true, out);
        walk(game, alternative, false, out);
        out.push(Token::CloseParen);
    }

    walk(game, first, false, out);
}

/// Squares touched by the move that led to `node` (for board highlighting).
pub fn last_move_squares(node: &Node) -> Option<(Square, Square)> {
    let mv = node.mv.as_ref()?;
    let uci = &mv.uci;
    if uci.len() < 4 {
        return None;
    }
    let from = uci[0..2].parse::<Square>().ok()?;
    let to = uci[2..4].parse::<Square>().ok()?;
    Some((from, to))
}

/// All legal destination squares from `from`, with the move that gets there.
pub fn legal_targets(pos: &Chess, from: Square) -> Vec<(Square, shakmaty::Move)> {
    pos.legal_moves()
        .iter()
        .filter(|m| m.from() == Some(from))
        .map(|m| (m.to(), *m))
        .collect()
}

/// Choose a promotion piece when several moves share from/to (default: queen).
pub fn pick_move(pos: &Chess, from: Square, to: Square) -> Option<shakmaty::Move> {
    let candidates: Vec<shakmaty::Move> = pos
        .legal_moves()
        .iter()
        .filter(|m| m.from() == Some(from) && m.to() == to)
        .copied()
        .collect();

    candidates
        .iter()
        .find(|m| m.promotion() == Some(Role::Queen))
        .or_else(|| candidates.first())
        .copied()
}
