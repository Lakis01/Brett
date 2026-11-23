//! Game tree model and PGN import.
//!
//! The tree is arena-based: every position after a move is a node, and the
//! root node is the starting position. `children[0]` is the mainline
//! continuation; any further children are variations. Navigation is therefore
//! just a `usize` node id, and going "back" is `parent`.

use std::num::NonZeroU32;
use std::ops::ControlFlow;

use crate::chess::{
    CastlingMode, Chess, Color, EnPassantMode, Fen, File, Move, Position, Rank, Role, San, SanPlus,
    Square, UciMove,
};
use crate::pgn::reader::{Reader, Visitor};
use anyhow::{Context, Result};

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

/// A clock value from a PGN `[%clk ...]` directive.
///
/// Stored in centiseconds so fractional seconds from online PGNs survive
/// without turning the UI into a floating-point formatting problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Clock {
    pub centis: u64,
}

impl Clock {
    /// Parse PGN clock syntax such as `0:03:00`, `3:00`, or `0:02:59.8`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() || text.starts_with('-') {
            return None;
        }
        let mut seconds = 0.0f64;
        let mut parts = 0usize;
        for part in text.split(':') {
            parts += 1;
            let value = part.parse::<f64>().ok()?;
            if !value.is_finite() || value < 0.0 {
                return None;
            }
            seconds = seconds * 60.0 + value;
        }
        if parts == 0 || parts > 3 || seconds > 100.0 * 3600.0 {
            return None;
        }
        Some(Self {
            centis: (seconds * 100.0).round() as u64,
        })
    }

    /// Human-readable clock. Hours are only shown when non-zero.
    pub fn display(self) -> String {
        let total = self.centis / 100;
        let hours = total / 3600;
        let minutes = (total % 3600) / 60;
        let seconds = total % 60;
        if hours > 0 {
            format!("{hours}:{minutes:02}:{seconds:02}")
        } else {
            format!("{minutes}:{seconds:02}")
        }
    }
}

/// Evaluation extracted from a PGN `[%eval ...]` comment directive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PgnEval {
    /// Centipawns from White's perspective (`0.15` -> `Some(15)`).
    pub cp: Option<i32>,
    /// Moves to mate from White's perspective (`#3` -> `Some(3)`, `#-3` -> `Some(-3)`).
    pub mate: Option<i32>,
}

impl PgnEval {
    pub fn parse(text: &str) -> Option<Self> {
        let head = text.split(',').next()?.trim();
        if head.is_empty() {
            return None;
        }
        if let Some(mate_str) = head.strip_prefix('#') {
            let mate = mate_str.trim().parse::<i32>().ok()?;
            if mate == 0 {
                return None;
            }
            Some(Self {
                cp: None,
                mate: Some(mate),
            })
        } else {
            let val = head.parse::<f64>().ok()?;
            if !val.is_finite() || val.abs() > 1000.0 {
                return None;
            }
            Some(Self {
                cp: Some((val * 100.0).round() as i32),
                mate: None,
            })
        }
    }

    pub fn pgn_text(self) -> Option<String> {
        if let Some(mate) = self.mate {
            Some(format!("#{mate}"))
        } else {
            self.cp.map(|cp| format!("{:.2}", cp as f64 / 100.0))
        }
    }
}

/// A position in the game tree.
#[derive(Clone, Debug)]
pub struct Node {
    /// `None` only for the root node.
    pub mv: Option<Ply>,
    pub comment: String,
    /// Remaining time for the player who made `mv`, when the PGN has clocks.
    pub clock: Option<Clock>,
    /// Evaluation from a PGN `[%eval ...]` directive, when present.
    pub eval: Option<PgnEval>,
    /// Coloured squares and arrows attached to this position.
    pub shapes: Vec<Shape>,
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

    /// Append a PGN comment, extracting clock, eval, and drawing directives.
    pub fn add_comment(&mut self, text: &str) {
        let directives = parse_comment_directives(text);
        if let Some(clock) = directives.clock {
            self.clock = Some(clock);
        }
        if let Some(eval) = directives.eval {
            self.eval = Some(eval);
        }
        for shape in directives.shapes {
            if !self.shapes.contains(&shape) {
                self.shapes.push(shape);
            }
        }
        if !directives.text.is_empty() {
            if !self.comment.is_empty() {
                self.comment.push(' ');
            }
            self.comment.push_str(&directives.text);
        }
    }

    /// Replace the human comment while preserving clocks, evaluations, and
    /// any shapes that are not explicitly replaced by drawing directives.
    pub fn replace_comment(&mut self, text: &str) {
        let directives = parse_comment_directives(text);
        self.comment = directives.text;
        if let Some(clock) = directives.clock {
            self.clock = Some(clock);
        }
        if let Some(eval) = directives.eval {
            self.eval = Some(eval);
        }
        if !directives.shapes.is_empty() {
            self.shapes = directives.shapes;
        }
    }
}

/// Colour of a board drawing. ChessBase stores these as `G`, `Y`, `R`, `B`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShapeColor {
    Green,
    Yellow,
    Red,
    Blue,
}

impl ShapeColor {
    /// ChessBase/Lichess `%csl`/`%cal` colour letter.
    pub fn code(self) -> char {
        match self {
            Self::Green => 'G',
            Self::Yellow => 'Y',
            Self::Red => 'R',
            Self::Blue => 'B',
        }
    }

    /// Decode a ChessBase/Lichess colour letter.
    pub fn from_code(code: char) -> Option<Self> {
        match code.to_ascii_uppercase() {
            'G' => Some(Self::Green),
            'Y' => Some(Self::Yellow),
            'R' => Some(Self::Red),
            'B' => Some(Self::Blue),
            _ => None,
        }
    }

    /// Decode the colour names used by SCID's `%draw` extension.
    pub fn from_scid_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "green" | "darkgreen" | "lightgreen" | "g" => Some(Self::Green),
            "yellow" | "gold" | "orange" | "y" => Some(Self::Yellow),
            "red" | "darkred" | "lightred" | "r" => Some(Self::Red),
            "blue" | "skyblue" | "cyan" | "lightblue" | "darkblue" | "b" => Some(Self::Blue),
            _ => None,
        }
    }
}

/// A coloured square or arrow attached to one game-tree position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    Square {
        color: ShapeColor,
        square: Square,
    },
    Arrow {
        color: ShapeColor,
        from: Square,
        to: Square,
    },
}

impl Shape {
    /// One entry inside a `%csl` or `%cal` directive, without the brackets.
    pub fn token(self) -> String {
        match self {
            Self::Square { color, square } => {
                format!("{}{square}", color.code())
            }
            Self::Arrow { color, from, to } => {
                format!("{}{from}{to}", color.code())
            }
        }
    }

    /// Parse one `%csl`/`%cal` token such as `Ge4` or `Re2e4`.
    pub fn parse_token(token: &str) -> Option<Self> {
        let mut chars = token.trim().chars();
        let color = ShapeColor::from_code(chars.next()?)?;
        let rest: String = chars.collect();
        match rest.len() {
            2 => Some(Self::Square {
                color,
                square: rest.parse().ok()?,
            }),
            4 => Some(Self::Arrow {
                color,
                from: rest[..2].parse().ok()?,
                to: rest[2..].parse().ok()?,
            }),
            _ => None,
        }
    }
}

/// Render shapes as ChessBase- and Lichess-compatible PGN drawing tokens.
pub fn shape_tokens(shapes: &[Shape]) -> String {
    let mut squares = Vec::new();
    let mut arrows = Vec::new();
    for &shape in shapes {
        match shape {
            Shape::Square { .. } => squares.push(shape.token()),
            Shape::Arrow { .. } => arrows.push(shape.token()),
        }
    }
    let mut out = String::new();
    if !squares.is_empty() {
        out.push_str("[%csl ");
        out.push_str(&squares.join(","));
        out.push(']');
    }
    if !arrows.is_empty() {
        out.push_str("[%cal ");
        out.push_str(&arrows.join(","));
        out.push(']');
    }
    out
}

struct CommentDirectives {
    text: String,
    clock: Option<Clock>,
    eval: Option<PgnEval>,
    shapes: Vec<Shape>,
}

/// Split `[%clk ...]`, `[%eval ...]`, `[%csl ...]`, `[%cal ...]`, and SCID
/// `[%draw ...]` directives out of a PGN comment. Other directives remain text.
fn parse_comment_directives(text: &str) -> CommentDirectives {
    let mut out = String::new();
    let mut rest = text;
    let mut clock = None;
    let mut eval = None;
    let mut shapes = Vec::new();
    loop {
        let Some(start) = rest.find("[%") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let directive = &rest[start + 2..];
        let Some(end) = directive.find(']') else {
            out.push_str(&rest[start..]);
            break;
        };
        let body = directive[..end].trim();
        let (name, arg) = match body.find(|c: char| c.is_whitespace() || c == ':') {
            Some(idx) => (&body[..idx], body[idx + 1..].trim()),
            None => (body, ""),
        };
        let parsed = if name.eq_ignore_ascii_case("clk") {
            if let Some(parsed) = Clock::parse(arg.split_whitespace().next().unwrap_or(arg)) {
                clock = Some(parsed);
            }
            true
        } else if name.eq_ignore_ascii_case("eval") {
            if let Some(parsed) = PgnEval::parse(arg.split_whitespace().next().unwrap_or(arg)) {
                eval = Some(parsed);
            }
            true
        } else if name.eq_ignore_ascii_case("csl") {
            let parsed = parse_csl(arg);
            let kept = !parsed.is_empty();
            shapes.extend(parsed);
            kept
        } else if name.eq_ignore_ascii_case("cal") {
            let parsed = parse_cal(arg);
            let kept = !parsed.is_empty();
            shapes.extend(parsed);
            kept
        } else if name.eq_ignore_ascii_case("draw") {
            let parsed = parse_draw(arg);
            let kept = !parsed.is_empty();
            shapes.extend(parsed);
            kept
        } else {
            false
        };
        if !parsed {
            out.push_str(&rest[start..start + 2 + end + 1]);
        }
        rest = &directive[end + 1..];
    }
    CommentDirectives {
        text: out.split_whitespace().collect::<Vec<_>>().join(" "),
        clock,
        eval,
        shapes,
    }
}

fn parse_csl(arg: &str) -> Vec<Shape> {
    let mut out = Vec::new();
    for item in arg
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let bytes = item.as_bytes();
        if bytes.len() != 3 {
            continue;
        }
        let Some(color) = ShapeColor::from_code(bytes[0] as char) else {
            continue;
        };
        let Some(square) = parse_square(&item[1..]) else {
            continue;
        };
        out.push(Shape::Square { color, square });
    }
    out
}

fn parse_cal(arg: &str) -> Vec<Shape> {
    let mut out = Vec::new();
    for item in arg
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let bytes = item.as_bytes();
        if bytes.len() != 5 {
            continue;
        }
        let Some(color) = ShapeColor::from_code(bytes[0] as char) else {
            continue;
        };
        let (Some(from), Some(to)) = (parse_square(&item[1..3]), parse_square(&item[3..5])) else {
            continue;
        };
        out.push(Shape::Arrow { color, from, to });
    }
    out
}

fn parse_draw(arg: &str) -> Vec<Shape> {
    let fields: Vec<&str> = arg
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    match fields.as_slice() {
        [kind, a, b]
            if kind.eq_ignore_ascii_case("square")
                || kind.eq_ignore_ascii_case("circle")
                || kind.eq_ignore_ascii_case("disk")
                || kind.eq_ignore_ascii_case("mark")
                || kind.eq_ignore_ascii_case("cross") =>
        {
            let square = parse_square(a).or_else(|| parse_square(b));
            let color = ShapeColor::from_scid_name(b).or_else(|| ShapeColor::from_scid_name(a));
            if let (Some(square), Some(color)) = (square, color) {
                vec![Shape::Square { color, square }]
            } else {
                Vec::new()
            }
        }
        [kind, a, b, c]
            if kind.eq_ignore_ascii_case("arrow") || kind.eq_ignore_ascii_case("line") =>
        {
            let (Some(from), Some(to), Some(color)) = (
                parse_square(a),
                parse_square(b),
                ShapeColor::from_scid_name(c),
            ) else {
                return Vec::new();
            };
            vec![Shape::Arrow { color, from, to }]
        }
        _ => Vec::new(),
    }
}

fn parse_square(text: &str) -> Option<Square> {
    let bytes = text.as_bytes();
    if bytes.len() != 2 {
        return None;
    }
    let file = bytes[0].to_ascii_lowercase();
    if !(b'a'..=b'h').contains(&file) || !(b'1'..=b'8').contains(&bytes[1]) {
        return None;
    }
    Some(Square::from_coords(
        File::new(u32::from(file - b'a')),
        Rank::new(u32::from(bytes[1] - b'1')),
    ))
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
                clock: None,
                eval: None,
                shapes: Vec::new(),
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

    pub fn site(&self) -> &str {
        self.header("Site").unwrap_or("?")
    }

    pub fn set_header(&mut self, key: &str, value: String) {
        if let Some((_, current)) = self
            .headers
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
        {
            *current = value;
        } else {
            self.headers.push((key.to_string(), value));
        }
    }

    pub fn remove_header(&mut self, key: &str) {
        self.headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case(key));
    }

    pub fn has_clocks(&self) -> bool {
        self.nodes.iter().any(|node| node.clock.is_some())
    }

    /// Most recent clock for `color` at or before `node`.
    ///
    /// Clock comments in PGN belong to the player who just moved, so this
    /// walks the parent chain to find that player's latest known time.
    pub fn clock_for(&self, node: usize, color: Color) -> Option<Clock> {
        let mut current = Some(node);
        while let Some(id) = current {
            let node = self.nodes.get(id)?;
            if node.mv.as_ref().is_some_and(|ply| ply.color == color)
                && let Some(clock) = node.clock
            {
                return Some(clock);
            }
            current = node.parent;
        }
        None
    }

    /// Remaining time for the player whose turn it is at `node`, if the PGN
    /// supplied a clock for that player's previous move.
    pub fn side_to_move_clock(&self, node: usize) -> Option<(Color, Clock)> {
        let turn = self.nodes.get(node)?.pos.turn();
        self.clock_for(node, turn).map(|clock| (turn, clock))
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

    /// Truncate every branch at `max_plies`, dropping deeper moves and their
    /// annotations. The root position is depth zero. This rebuilds the arena so
    /// unreachable nodes do not linger after export.
    pub fn truncate_mainline(&mut self, max_plies: usize) {
        if self.nodes.is_empty() || tree_depth(self, 0, 0) <= max_plies {
            return;
        }
        let old = std::mem::take(&mut self.nodes);
        let mut new = Vec::with_capacity(old.len());
        copy_subtree(&old, 0, None, 0, max_plies, &mut new);
        self.nodes = new;
        let mainline_len = self.mainline_len().to_string();
        if let Some((_, value)) = self
            .headers
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case("PlyCount"))
        {
            *value = mainline_len;
        }
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
    pub fn add_move(&mut self, parent: usize, mv: Move) -> usize {
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
            clock: None,
            eval: None,
            shapes: Vec::new(),
            nags: Vec::new(),
            parent: Some(parent),
            children: Vec::new(),
            pos: after,
        });
        self.nodes[parent].children.push(id);
        id
    }

    /// Root node of the variation containing `node`, or `None` when `node` lies
    /// on the top-level mainline (or is the root).
    pub fn variation_root(&self, mut node: usize) -> Option<usize> {
        if node >= self.nodes.len() {
            return None;
        }
        while let Some(parent) = self.nodes[node].parent {
            if self.nodes[parent].children.first().copied() != Some(node) {
                return Some(node);
            }
            node = parent;
        }
        None
    }

    pub fn can_promote_variation(&self, node: usize) -> bool {
        self.variation_root(node).is_some()
    }

    pub fn can_move_variation_up(&self, node: usize) -> bool {
        let Some(var_root) = self.variation_root(node) else {
            return false;
        };
        let Some(parent) = self.nodes[var_root].parent else {
            return false;
        };
        self.nodes[parent]
            .children
            .iter()
            .position(|&c| c == var_root)
            .is_some_and(|idx| idx >= 2)
    }

    pub fn can_move_variation_down(&self, node: usize) -> bool {
        let Some(var_root) = self.variation_root(node) else {
            return false;
        };
        let Some(parent) = self.nodes[var_root].parent else {
            return false;
        };
        let children = &self.nodes[parent].children;
        children
            .iter()
            .position(|&c| c == var_root)
            .is_some_and(|idx| idx >= 1 && idx + 1 < children.len())
    }

    fn sync_ply_count_header(&mut self) {
        let mainline_len = self.mainline_len().to_string();
        if let Some((_, value)) = self
            .headers
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case("PlyCount"))
        {
            *value = mainline_len;
        }
    }

    /// Promote the innermost variation containing `node` one level up (it
    /// becomes the primary continuation at its branch point, while the previous
    /// primary continuation becomes variation 1).
    pub fn promote_variation(&mut self, node: usize) -> bool {
        let Some(var_root) = self.variation_root(node) else {
            return false;
        };
        let Some(parent) = self.nodes[var_root].parent else {
            return false;
        };
        let children = &mut self.nodes[parent].children;
        let Some(pos) = children.iter().position(|&c| c == var_root) else {
            return false;
        };
        if pos == 0 {
            return false;
        }
        let item = children.remove(pos);
        children.insert(0, item);
        self.sync_ply_count_header();
        true
    }

    /// Promote `node` through every enclosing branch point all the way up to
    /// the top-level mainline.
    pub fn promote_to_mainline(&mut self, node: usize) -> bool {
        let mut changed = false;
        while self.promote_variation(node) {
            changed = true;
        }
        changed
    }

    /// Swap the variation containing `node` with the sibling variation before it.
    pub fn move_variation_up(&mut self, node: usize) -> bool {
        let Some(var_root) = self.variation_root(node) else {
            return false;
        };
        let Some(parent) = self.nodes[var_root].parent else {
            return false;
        };
        let children = &mut self.nodes[parent].children;
        let Some(pos) = children.iter().position(|&c| c == var_root) else {
            return false;
        };
        if pos < 2 {
            return false;
        }
        children.swap(pos, pos - 1);
        true
    }

    /// Swap the variation containing `node` with the sibling variation after it.
    pub fn move_variation_down(&mut self, node: usize) -> bool {
        let Some(var_root) = self.variation_root(node) else {
            return false;
        };
        let Some(parent) = self.nodes[var_root].parent else {
            return false;
        };
        let children = &mut self.nodes[parent].children;
        let Some(pos) = children.iter().position(|&c| c == var_root) else {
            return false;
        };
        if pos == 0 || pos + 1 >= children.len() {
            return false;
        }
        children.swap(pos, pos + 1);
        true
    }

    fn compact_reachable(&mut self) -> Vec<Option<usize>> {
        let old = std::mem::take(&mut self.nodes);
        let mut new = Vec::with_capacity(old.len());
        let mut map = vec![None; old.len()];
        if !old.is_empty() {
            copy_subtree_mapped(&old, 0, None, &mut new, &mut map);
        }
        self.nodes = new;
        self.sync_ply_count_header();
        map
    }

    /// Delete the variation containing `node`. Returns the remapped branch parent
    /// node id and the full `old_node -> Option<new_node>` index table.
    pub fn delete_variation(&mut self, node: usize) -> Option<(usize, Vec<Option<usize>>)> {
        let var_root = self.variation_root(node)?;
        let parent = self.nodes[var_root].parent?;
        self.nodes[parent].children.retain(|&c| c != var_root);
        let map = self.compact_reachable();
        let new_parent = map.get(parent).and_then(|&x| x).unwrap_or(0);
        Some((new_parent, map))
    }

    /// Remove all continuations after `node`, returning the compacted node
    /// index mapping when any children were removed.
    pub fn truncate_after(&mut self, node: usize) -> Option<Vec<Option<usize>>> {
        if node >= self.nodes.len() || self.nodes[node].children.is_empty() {
            return None;
        }
        self.nodes[node].children.clear();
        Some(self.compact_reachable())
    }
}

fn copy_subtree_mapped(
    old: &[Node],
    id: usize,
    parent: Option<usize>,
    out: &mut Vec<Node>,
    map: &mut [Option<usize>],
) -> usize {
    let new_id = out.len();
    map[id] = Some(new_id);
    let mut node = old[id].clone();
    node.parent = parent;
    node.children.clear();
    out.push(node);
    for &child in &old[id].children {
        let child_id = copy_subtree_mapped(old, child, Some(new_id), out, map);
        out[new_id].children.push(child_id);
    }
    new_id
}

/// Deepest node below `node`, measured in plies from the root.
fn tree_depth(game: &Game, node: usize, depth: usize) -> usize {
    game.nodes[node]
        .children
        .iter()
        .map(|&child| tree_depth(game, child, depth + 1))
        .max()
        .unwrap_or(depth)
}

/// Clone a subtree into a fresh arena, stopping at `max_plies`.
fn copy_subtree(
    old: &[Node],
    id: usize,
    parent: Option<usize>,
    depth: usize,
    max_plies: usize,
    out: &mut Vec<Node>,
) -> usize {
    let new_id = out.len();
    let mut node = old[id].clone();
    node.parent = parent;
    node.children.clear();
    out.push(node);
    if depth < max_plies {
        for &child in &old[id].children {
            let child_id = copy_subtree(old, child, Some(new_id), depth + 1, max_plies, out);
            out[new_id].children.push(child_id);
        }
    }
    new_id
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
        name: &str,
        value: &str,
    ) -> ControlFlow<Game> {
        tags.push((name.to_string(), value.to_string()));
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, tags: Vec<(String, String)>) -> ControlFlow<Game, Builder> {
        ControlFlow::Continue(Builder::new(tags))
    }

    fn san(&mut self, mt: &mut Builder, san_plus: SanPlus) -> ControlFlow<Game> {
        mt.push(san_plus);
        ControlFlow::Continue(())
    }

    fn nag(&mut self, mt: &mut Builder, nag: u8) -> ControlFlow<Game> {
        mt.nodes[mt.current].nags.push(nag);
        ControlFlow::Continue(())
    }

    fn comment(&mut self, mt: &mut Builder, comment: &str) -> ControlFlow<Game> {
        mt.nodes[mt.current].add_comment(comment);
        ControlFlow::Continue(())
    }

    fn begin_variation(&mut self, mt: &mut Builder) -> ControlFlow<Game> {
        // A variation replaces the move that was just played, so it starts
        // from the position before that move.
        let before = mt.nodes[mt.current].parent.unwrap_or(0);
        mt.stack.push((mt.current, mt.broken));
        if !mt.broken {
            mt.current = before;
        }
        ControlFlow::Continue(())
    }

    fn end_variation(&mut self, mt: &mut Builder) -> ControlFlow<Game> {
        let (current, broken) = mt.stack.pop().unwrap_or((0, false));
        mt.current = current;
        mt.broken = broken;
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
    broken: bool,
    stack: Vec<(usize, bool)>,
    errors: Vec<String>,
}

impl Builder {
    fn new(headers: Vec<(String, String)>) -> Self {
        let mut errors = Vec::new();
        let mode = if headers.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("Variant") && {
                let value = value.to_ascii_lowercase();
                value.contains("chess960") || value.contains("fischer")
            }
        }) {
            CastlingMode::Chess960
        } else {
            CastlingMode::Standard
        };
        let start = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("FEN"))
            .and_then(|(_, v)| Fen::from_ascii(v.as_bytes()).ok())
            .and_then(|fen| fen.into_position(mode).ok());

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
                clock: None,
                eval: None,
                shapes: Vec::new(),
                nags: Vec::new(),
                parent: None,
                children: Vec::new(),
                pos,
            }],
            current: 0,
            broken: false,
            stack: Vec::new(),
            errors,
        }
    }

    fn push(&mut self, san_plus: SanPlus) {
        if self.broken {
            return;
        }
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
            match Chess::from_setup(setup, pos.castling_mode()) {
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
                    self.broken = true;
                }
            }
            return;
        }

        let mv = match san_plus.san.to_move(&pos) {
            Ok(mv) => mv,
            Err(err) => {
                self.errors
                    .push(format!("illegal move {} in {}: {err}", san_plus, fen()));
                self.broken = true;
                return;
            }
        };

        let after = match pos.clone().play(mv) {
            Ok(after) => after,
            Err(err) => {
                self.errors
                    .push(format!("illegal move {} in {}: {err}", san_plus, fen()));
                self.broken = true;
                return;
            }
        };

        let ply = Ply {
            san: san_plus.to_string(),
            uci: UciMove::from_move(mv, pos.castling_mode()).to_string(),
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
            clock: None,
            eval: None,
            shapes: Vec::new(),
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
        7 => "□",
        8 => "◇",
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
                symbol: "◇",
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

/// Destination of the king in a castling move.
///
/// [`Move::Castle`]'s `to()` is the rook's square, which is useful for
/// UCI but not what a player drags the king onto. Standard chess puts the king
/// on the G-file after kingside castling and the C-file after queenside.
fn castle_king_destination(mv: &Move) -> Option<Square> {
    let Move::Castle { king, rook } = mv else {
        return None;
    };
    let file = if rook.file() > king.file() {
        File::G
    } else {
        File::C
    };
    Some(Square::from_coords(file, king.rank()))
}

/// All legal destination squares from `from`, with the move that gets there.
///
/// Castling exposes both the king's destination and the rook's square as
/// targets, so a king can be clicked or dragged onto either one.
pub fn legal_targets(pos: &Chess, from: Square) -> Vec<(Square, Move)> {
    let mut targets = Vec::new();
    for mv in pos.legal_moves().iter().filter(|m| m.from() == Some(from)) {
        targets.push((mv.to(), *mv));
        if let Some(destination) = castle_king_destination(mv)
            && destination != mv.to()
        {
            targets.push((destination, *mv));
        }
    }
    targets
}

/// Choose a promotion piece when several moves share from/to (default: queen).
///
/// For castling, `to` may be the king's destination (`g1`/`c1`) rather than
/// the rook's square that `Move::to` reports.
pub fn pick_move(pos: &Chess, from: Square, to: Square) -> Option<Move> {
    let candidates: Vec<Move> = pos
        .legal_moves()
        .iter()
        .filter(|m| m.from() == Some(from) && m.to() == to)
        .copied()
        .collect();
    if let Some(mv) = candidates
        .iter()
        .find(|m| m.promotion() == Some(Role::Queen))
        .or_else(|| candidates.first())
    {
        return Some(*mv);
    }

    pos.legal_moves()
        .iter()
        .find(|m| {
            m.from() == Some(from)
                && castle_king_destination(m).is_some_and(|destination| destination == to)
        })
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_pgn_clocks() {
        assert_eq!(Clock::parse("0:03:00").unwrap().display(), "3:00");
        assert_eq!(Clock::parse("1:02:03").unwrap().display(), "1:02:03");
        assert_eq!(Clock::parse("0:02:59.8").unwrap().display(), "2:59");
        assert!(Clock::parse("hello").is_none());
        assert!(Clock::parse("-1:00").is_none());
    }

    #[test]
    fn clock_comments_attach_to_the_move_and_hide_from_text() {
        let games = parse_pgn(
            br#"[Event "Clock test"]

1. e4 {[%clk 0:05:00] King pawn.} e5 {[%clk 0:04:58]} *
"#,
        )
        .unwrap();
        let game = &games[0];
        assert_eq!(game.nodes[1].comment, "King pawn.");
        assert_eq!(game.nodes[1].clock.unwrap().display(), "5:00");
        assert_eq!(game.clock_for(2, Color::White).unwrap().display(), "5:00");
        assert_eq!(game.clock_for(2, Color::Black).unwrap().display(), "4:58");
        assert!(game.clock_for(1, Color::Black).is_none());
        assert_eq!(
            game.side_to_move_clock(2),
            Some((Color::White, Clock { centis: 30_000 }))
        );
        assert!(game.side_to_move_clock(0).is_none());
    }

    #[test]
    fn eval_and_clock_directives_decode_from_lichess_broadcast_comments() {
        let games = parse_pgn(
            br#"[Event "Broadcast"]

1. d4 { [%eval 0.15] [%clk 1:30:56] } d5 { [%eval 0.25,20] [%clk 1:40:54] } 2. c4 { Good move! [%eval #3] } e6 { [%eval #-2] } 3. Nc3 { [%eval bogus] [%emt 0:01:12] Still text. } *
"#,
        )
        .unwrap();
        let game = &games[0];

        assert!(game.nodes[1].comment.is_empty());
        assert_eq!(game.nodes[1].clock, Some(Clock { centis: 545_600 }));
        assert_eq!(
            game.nodes[1].eval,
            Some(PgnEval {
                cp: Some(15),
                mate: None,
            })
        );

        assert!(game.nodes[2].comment.is_empty());
        assert_eq!(game.nodes[2].clock, Some(Clock { centis: 605_400 }));
        assert_eq!(
            game.nodes[2].eval,
            Some(PgnEval {
                cp: Some(25),
                mate: None,
            })
        );

        assert_eq!(game.nodes[3].comment, "Good move!");
        assert_eq!(
            game.nodes[3].eval,
            Some(PgnEval {
                cp: None,
                mate: Some(3),
            })
        );

        assert!(game.nodes[4].comment.is_empty());
        assert_eq!(
            game.nodes[4].eval,
            Some(PgnEval {
                cp: None,
                mate: Some(-2),
            })
        );

        assert_eq!(game.nodes[5].comment, "[%emt 0:01:12] Still text.");
        assert_eq!(game.nodes[5].eval, None);

        let mut edited = game.nodes[1].clone();
        edited.replace_comment("Human note");
        assert_eq!(edited.comment, "Human note");
        assert_eq!(edited.clock, Some(Clock { centis: 545_600 }));
        assert_eq!(
            edited.eval,
            Some(PgnEval {
                cp: Some(15),
                mate: None,
            })
        );
    }

    #[test]
    fn parses_coloured_squares_and_arrows_from_comments() {
        let games = parse_pgn(
            br#"[Event "Shapes"]

1. e4 {[%csl Ge4,Re5][%cal Gg1f3,Ye2e4] Key squares.} e5 *
"#,
        )
        .unwrap();
        let node = &games[0].nodes[1];
        assert_eq!(
            node.shapes,
            vec![
                Shape::Square {
                    color: ShapeColor::Green,
                    square: Square::E4,
                },
                Shape::Square {
                    color: ShapeColor::Red,
                    square: Square::E5,
                },
                Shape::Arrow {
                    color: ShapeColor::Green,
                    from: Square::G1,
                    to: Square::F3,
                },
                Shape::Arrow {
                    color: ShapeColor::Yellow,
                    from: Square::E2,
                    to: Square::E4,
                },
            ]
        );
        assert_eq!(node.comment, "Key squares.");
    }

    #[test]
    fn parses_scid_draw_annotations() {
        let games = parse_pgn(
            br#"[Event "SCID drawing"]

1. e4 {[%draw arrow,e2,h5,skyBlue][%draw square,d4,red][%draw circle,c5,red] Threat.} e5 *
"#,
        )
        .unwrap();
        let node = &games[0].nodes[1];
        assert_eq!(
            node.shapes,
            vec![
                Shape::Arrow {
                    color: ShapeColor::Blue,
                    from: Square::E2,
                    to: Square::H5,
                },
                Shape::Square {
                    color: ShapeColor::Red,
                    square: Square::D4,
                },
                Shape::Square {
                    color: ShapeColor::Red,
                    square: Square::C5,
                },
            ]
        );
        assert_eq!(node.comment, "Threat.");
    }

    #[test]
    fn editing_a_comment_keeps_existing_shapes() {
        let mut game = parse_pgn(
            br#"[Event "Shapes"]

1. e4 {[%cal Gg1f3]} e5 *
"#,
        )
        .unwrap()
        .remove(0);
        assert_eq!(game.nodes[1].shapes.len(), 1);

        game.nodes[1].replace_comment("A human note");
        assert_eq!(game.nodes[1].comment, "A human note");
        assert_eq!(game.nodes[1].shapes.len(), 1);

        game.nodes[1].add_comment("[%csl Gd4]");
        assert_eq!(game.nodes[1].comment, "A human note");
        assert_eq!(game.nodes[1].shapes.len(), 2);
    }

    #[test]
    fn truncates_every_branch_at_the_ply_limit() {
        let mut game = parse_pgn(
            br#"[Event "Long"]
[PlyCount "9"]

1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 (3... Nf6 4. O-O Nxe4 5. d4) 5. O-O *
"#,
        )
        .unwrap()
        .remove(0);
        assert!(game.mainline_len() > 5);

        game.truncate_mainline(3);
        assert_eq!(game.mainline_len(), 3);
        assert_eq!(game.header("PlyCount"), Some("3"));

        fn max_depth(game: &Game, node: usize, depth: usize) -> usize {
            game.nodes[node]
                .children
                .iter()
                .map(|&child| max_depth(game, child, depth + 1))
                .max()
                .unwrap_or(depth)
        }
        assert_eq!(max_depth(&game, 0, 0), 3);
        assert_eq!(game.mainline().len(), 3);
    }

    #[test]
    fn forced_and_only_move_have_distinct_symbols() {
        assert_eq!(nag_symbol(7), Some("□"));
        assert_eq!(nag_symbol(8), Some("◇"));
        assert_ne!(nag_symbol(7), nag_symbol(8));
    }

    #[test]
    fn castling_accepts_king_and_rook_destinations() {
        let pos = Fen::from_ascii(b"r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1")
            .unwrap()
            .into_position(CastlingMode::Standard)
            .unwrap();
        let kingside = pick_move(&pos, Square::E1, Square::G1).unwrap();
        assert_eq!(
            kingside,
            Move::Castle {
                king: Square::E1,
                rook: Square::H1,
            }
        );
        assert_eq!(pick_move(&pos, Square::E1, Square::H1), Some(kingside));
        let queenside = pick_move(&pos, Square::E1, Square::C1).unwrap();
        assert_eq!(pick_move(&pos, Square::E1, Square::A1), Some(queenside));
        let targets = legal_targets(&pos, Square::E1);
        assert!(targets.iter().any(|(square, _)| *square == Square::G1));
        assert!(targets.iter().any(|(square, _)| *square == Square::C1));
    }

    #[test]
    fn nested_variation_promote_reorder_delete_and_truncate() {
        let mut game = parse_pgn(
            br#"[Event "VarTest"]

1. e4 e5 (1... c5 2. Nf3 d6 (2... Nc6 3. Bb5) 3. d4) (1... e6 2. d4) 2. Nf3 *
"#,
        )
        .unwrap()
        .remove(0);

        let e4 = game.mainline()[0];
        let c5 = game.nodes[e4].children[1];
        let e6 = game.nodes[e4].children[2];
        let nf3_sic = game.nodes[c5].children[0];
        let nc6 = game.nodes[nf3_sic].children[1];
        let bb5 = game.nodes[nc6].children[0];

        assert!(!game.can_promote_variation(e4));
        assert!(game.can_promote_variation(bb5));
        assert!(!game.can_move_variation_up(c5));
        assert!(game.can_move_variation_down(c5));
        assert!(game.can_move_variation_up(e6));

        // Promote nested Bb5 one level: Nc6 replaces d6 inside the c5 variation.
        assert!(game.promote_variation(bb5));
        assert_eq!(game.nodes[nf3_sic].children[0], nc6);
        // Top-level mainline is still e5 until promoted again.
        assert_eq!(game.nodes[e4].children.len(), 3);
        assert_ne!(game.nodes[e4].children[0], c5);

        // Make main line promotes all remaining levels up to the root mainline.
        assert!(game.promote_to_mainline(bb5));
        assert_eq!(game.nodes[e4].children[0], c5);
        assert!(!game.can_promote_variation(bb5));

        // Swap remaining variations (previous mainline e5 vs e6) under e4.
        let v1 = game.nodes[e4].children[1];
        let v2 = game.nodes[e4].children[2];
        assert!(game.move_variation_down(v1));
        assert_eq!(game.nodes[e4].children[1], v2);
        assert_eq!(game.nodes[e4].children[2], v1);
        assert!(game.move_variation_up(v1));
        assert_eq!(game.nodes[e4].children[1], v1);

        // Truncate after Nc6 removes Bb5.
        let map = game.truncate_after(nc6).unwrap();
        let nc6_new = map[nc6].unwrap();
        assert!(game.nodes[nc6_new].children.is_empty());

        // Delete one sibling variation under e4.
        let v2_new = map[v2].unwrap();
        let (parent_new, _) = game.delete_variation(v2_new).unwrap();
        assert_eq!(game.nodes[parent_new].children.len(), 2);
    }
}
