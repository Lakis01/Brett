//! Backend-neutral document model for print export.
//!
//! The builder walks Brett's arena-based game tree once and produces a flat,
//! deterministic list of printable items. A renderer (Typst first, LaTeX
//! later) only has to turn that list into typeset source; it never needs to
//! understand chess moves or the game tree.

use std::collections::{BTreeMap, BTreeSet};

use crate::chess::{Chess, Color, EnPassantMode, Fen};
use crate::domain::{self, Game, Node, PgnEval};

mod board;
pub mod typst;

#[cfg(test)]
mod tests;

/// The orthodox starting position, used to decide whether a game needs an
/// explicit start FEN in its printed heading.
const STANDARD_START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

/// Page-level metadata shown in the running header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintMeta {
    pub source: String,
    pub owner: String,
    pub date: String,
}

/// One game, ready to be rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintDocument {
    pub meta: PrintMeta,
    pub game: PrintGame,
    /// Every requested diagram, sorted by node id. Item references are
    /// indexes into this vector.
    pub diagrams: Vec<Diagram>,
}

/// The printable parts of one game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintGame {
    pub headers: Vec<(String, String)>,
    /// Present only for custom starts and Chess960 positions.
    pub start_fen: Option<String>,
    pub variant: Option<String>,
    pub items: Vec<PrintItem>,
    pub result: String,
}

/// A piece of printed game content, in reading order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintItem {
    Move(PrintMove),
    /// A human comment, printed as an indented block.
    Comment(String),
    /// Clock and/or evaluation data, printed as a compact bracketed note.
    Annotation(String),
    /// An indented variation line.
    Variation(Vec<PrintItem>),
    /// A reference into [`PrintDocument::diagrams`].
    Diagram(usize),
}

/// One move token in the printed move list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintMove {
    pub text: String,
    /// Mainline moves are bold; variation moves are not.
    pub bold: bool,
}

/// A board diagram at one node of the game tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagram {
    pub node: usize,
    pub fen: String,
    pub caption: String,
    /// SVG source. Filled in by the board renderer.
    pub svg: String,
}

/// Build the printable representation of one game.
///
/// `selected` contains node ids chosen by the user. Invalid ids are ignored.
/// When `include_final` is set, the final mainline position is added (and
/// deduplicated against the selection).
pub fn build_document(
    game: &Game,
    meta: PrintMeta,
    selected: &BTreeSet<usize>,
    include_final: bool,
) -> PrintDocument {
    build_document_with_depth(game, meta, selected, include_final, usize::MAX)
}

/// Build the printable representation with an explicit maximum variation depth.
///
/// - `max_variation_depth = 0`: mainline only (no variations).
/// - `max_variation_depth = 1`: direct variations only (no nested variations).
/// - `max_variation_depth >= 2`: all variations.
pub fn build_document_with_depth(
    game: &Game,
    meta: PrintMeta,
    selected: &BTreeSet<usize>,
    include_final: bool,
    max_variation_depth: usize,
) -> PrintDocument {
    let final_node = game.mainline().last().copied().unwrap_or(0);

    let mut nodes: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|node| game.nodes.get(*node).is_some())
        .collect();
    if include_final {
        nodes.push(final_node);
    }
    nodes.sort_unstable();
    nodes.dedup();

    let diagrams: Vec<Diagram> = nodes
        .iter()
        .map(|&node| {
            let (fen, svg) = match game.nodes.get(node) {
                Some(node) => (
                    Fen::from_position(&node.pos, EnPassantMode::Legal).to_string(),
                    board::board_svg(&node.pos),
                ),
                // A malformed empty game still gets a well-defined board.
                None => {
                    let start = Chess::default();
                    (
                        Fen::from_position(&start, EnPassantMode::Legal).to_string(),
                        board::board_svg(&start),
                    )
                }
            };
            Diagram {
                node,
                fen,
                caption: diagram_caption(game, node, final_node),
                svg,
            }
        })
        .collect();

    let diagram_at: BTreeMap<usize, usize> = diagrams
        .iter()
        .enumerate()
        .map(|(index, diagram)| (diagram.node, index))
        .collect();

    let mut items = Vec::new();
    if let Some(root) = game.nodes.first() {
        if !root.comment.is_empty() {
            items.push(PrintItem::Comment(root.comment.clone()));
        }
        if let Some(annotation) = annotation_text(root) {
            items.push(PrintItem::Annotation(annotation));
        }
        // A start-position diagram is only placed when there is a move list
        // for it to introduce. An empty game still carries the diagram in
        // `diagrams`, and the renderer can print it after the heading.
        if !root.children.is_empty()
            && let Some(&index) = diagram_at.get(&0)
        {
            items.push(PrintItem::Diagram(index));
        }
    }

    let ctx = PrintContext {
        game,
        diagram_at: &diagram_at,
        max_variation_depth,
        final_node,
    };

    emit_sequence(&ctx, 0, 0, true, &mut items);

    PrintDocument {
        meta,
        game: PrintGame {
            headers: game.headers.clone(),
            start_fen: start_fen(game),
            variant: game.header("Variant").map(str::to_string),
            items,
            result: game.result().to_string(),
        },
        diagrams,
    }
}

struct PrintContext<'a> {
    game: &'a Game,
    diagram_at: &'a BTreeMap<usize, usize>,
    max_variation_depth: usize,
    final_node: usize,
}

/// Emit the children of `parent` as one printed line.
///
/// `children[0]` is the continuation; every later child is an alternative to
/// it and becomes an indented variation after the continuation's move.
fn emit_sequence(
    ctx: &PrintContext<'_>,
    parent: usize,
    depth: usize,
    mut line_start: bool,
    items: &mut Vec<PrintItem>,
) {
    let Some(parent_node) = ctx.game.nodes.get(parent) else {
        return;
    };
    let Some(&main) = parent_node.children.first() else {
        return;
    };

    emit_move(ctx.game, main, depth, &mut line_start, items);
    emit_extras(ctx, main, items);

    if depth < ctx.max_variation_depth {
        for &alternative in &parent_node.children[1..] {
            let mut variation = Vec::new();
            emit_variation(ctx, alternative, depth + 1, &mut variation);
            items.push(PrintItem::Variation(variation));
            // A black continuation after a variation needs its move number again.
            line_start = true;
        }
    }

    emit_sequence(ctx, main, depth, line_start, items);
}

/// Emit a variation whose first move is `first`.
fn emit_variation(ctx: &PrintContext<'_>, first: usize, depth: usize, items: &mut Vec<PrintItem>) {
    let mut line_start = true;
    emit_move(ctx.game, first, depth, &mut line_start, items);
    emit_extras(ctx, first, items);
    emit_sequence(ctx, first, depth, line_start, items);
}

fn emit_move(
    game: &Game,
    node: usize,
    depth: usize,
    line_start: &mut bool,
    items: &mut Vec<PrintItem>,
) {
    let Some(node) = game.nodes.get(node) else {
        return;
    };
    let Some(ply) = node.mv.as_ref() else {
        return;
    };

    let mut text = String::new();
    if ply.color == Color::White {
        text.push_str(&ply.number.to_string());
        text.push('.');
    } else if *line_start {
        text.push_str(&ply.number.to_string());
        text.push_str("...");
    }
    text.push_str(&ply.san);
    append_nags(&mut text, &node.nags);

    items.push(PrintItem::Move(PrintMove {
        text,
        bold: depth == 0,
    }));
    *line_start = false;
}

fn emit_extras(ctx: &PrintContext<'_>, node: usize, items: &mut Vec<PrintItem>) {
    let Some(node_ref) = ctx.game.nodes.get(node) else {
        return;
    };
    if node != ctx.final_node && !node_ref.comment.is_empty() {
        items.push(PrintItem::Comment(node_ref.comment.clone()));
    }
    if let Some(annotation) = annotation_text(node_ref) {
        items.push(PrintItem::Annotation(annotation));
    }
    if node != ctx.final_node
        && let Some(&index) = ctx.diagram_at.get(&node)
    {
        items.push(PrintItem::Diagram(index));
    }
}

fn append_nags(text: &mut String, nags: &[u8]) {
    for &nag in nags {
        if let Some(symbol) = domain::nag_symbol(nag) {
            text.push_str(symbol);
        }
    }
}

fn annotation_text(node: &Node) -> Option<String> {
    match (node.clock, node.eval) {
        (None, None) => None,
        (Some(clock), None) => Some(format!("[clk {}]", clock.display())),
        (None, Some(eval)) => Some(format!("[eval {}]", eval_text(eval))),
        (Some(clock), Some(eval)) => Some(format!(
            "[clk {}; eval {}]",
            clock.display(),
            eval_text(eval)
        )),
    }
}

fn eval_text(eval: PgnEval) -> String {
    if let Some(mate) = eval.mate {
        return format!("#{mate}");
    }
    match eval.cp {
        Some(cp) if cp > 0 => format!("+{:.2}", cp as f64 / 100.0),
        Some(cp) => format!("{:.2}", cp as f64 / 100.0),
        None => "?".to_string(),
    }
}

fn start_fen(game: &Game) -> Option<String> {
    let root = game.nodes.first()?;
    let fen = Fen::from_position(&root.pos, EnPassantMode::Legal).to_string();
    (fen != STANDARD_START_FEN).then_some(fen)
}

fn diagram_caption(game: &Game, node: usize, final_node: usize) -> String {
    if node == 0 {
        return "start position".to_string();
    }
    if node == final_node {
        return "final position".to_string();
    }
    match full_move_text(game, node) {
        Some(text) => format!("after {text}"),
        None => format!("after node {node}"),
    }
}

fn full_move_text(game: &Game, node: usize) -> Option<String> {
    let node = game.nodes.get(node)?;
    let ply = node.mv.as_ref()?;
    let mut text = format!(
        "{}{}",
        ply.number,
        if ply.color == Color::White {
            "."
        } else {
            "..."
        }
    );
    text.push_str(&ply.san);
    append_nags(&mut text, &node.nags);
    Some(text)
}
