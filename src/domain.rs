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
