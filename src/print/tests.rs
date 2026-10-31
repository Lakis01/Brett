//! Unit tests for the backend-neutral print document.

use std::collections::BTreeSet;

use crate::chess::EnPassantMode;
use crate::domain::{self, Game};
use crate::print::*;

fn meta() -> PrintMeta {
    PrintMeta {
        source: "Test Database".to_string(),
        owner: "Tester".to_string(),
        date: "2026-10-31".to_string(),
    }
}

fn parse(pgn: &str) -> Game {
    let games = domain::parse_pgn(pgn.as_bytes()).unwrap();
    assert_eq!(games.len(), 1);
    games.into_iter().next().unwrap()
}

fn opera() -> Game {
    parse(include_str!("../../fixtures/opera.pgn"))
}

fn moves(items: &[PrintItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            PrintItem::Move(mv) => Some(mv.text.clone()),
            _ => None,
        })
        .collect()
}

fn variation(items: &[PrintItem]) -> &[PrintItem] {
    items
        .iter()
        .find_map(|item| match item {
            PrintItem::Variation(items) => Some(items.as_slice()),
            _ => None,
        })
        .expect("expected a variation")
}

#[test]
fn mainline_moves_are_bold_and_numbered() {
    let game = opera();
    let doc = build_document(&game, meta(), &BTreeSet::new(), false);

    assert_eq!(
        moves(&doc.game.items),
        [
            "1.e4", "e5", "2.Nf3", "d6", "3.d4", "Bg4", "4.dxe5", "Bxf3", "5.Qxf3", "dxe5",
            "6.Bc4", "Nf6", "7.Qb3", "Qe7", "8.Nc3", "c6", "9.Bg5", "b5", "10.Nxb5!", "cxb5",
            "11.Bxb5+", "Nbd7", "12.O-O-O", "Rd8", "13.Rxd7", "Rxd7", "14.Rd1", "Qe6", "15.Bxd7+",
            "Nxd7", "16.Qb8+", "Nxb8", "17.Rd8#",
        ]
    );
    assert!(doc.game.items.iter().all(|item| match item {
        PrintItem::Move(mv) => mv.bold,
        _ => true,
    }));
    assert_eq!(doc.game.result, "1-0");
}

#[test]
fn root_comment_is_printed_before_the_first_move() {
    let game = opera();
    let doc = build_document(&game, meta(), &BTreeSet::new(), false);

    assert!(matches!(
        doc.game.items.first(),
        Some(PrintItem::Comment(text)) if text.contains("Opera Game")
    ));
}

#[test]
fn comments_clock_eval_and_nags_are_preserved() {
    let game = parse(
        "[Event \"Annotated\"]\n[Site \"?\"]\n[Date \"2026.01.01\"]\n[Round \"1\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"*\"]\n\n1. e4! {Opening [%clk 0:03:00] [%eval 0.35]} e5? {[%clk 0:02:59] [%eval -0.20]} *\n",
    );
    let doc = build_document(&game, meta(), &BTreeSet::new(), false);

    assert_eq!(moves(&doc.game.items), ["1.e4!", "e5?"]);
    let comment = doc
        .game
        .items
        .iter()
        .find_map(|item| match item {
            PrintItem::Comment(text) => Some(text.as_str()),
            _ => None,
        })
        .unwrap();
    assert_eq!(comment, "Opening");
    let annotations: Vec<&str> = doc
        .game
        .items
        .iter()
        .filter_map(|item| match item {
            PrintItem::Annotation(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        annotations,
        ["[clk 3:00; eval +0.35]", "[clk 2:59; eval -0.20]"]
    );
}

#[test]
fn nested_variations_keep_their_move_numbers_and_order() {
    let game = parse(
        "[Event \"Nested\"]\n[Site \"?\"]\n[Date \"2026.01.01\"]\n[Round \"1\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"*\"]\n\n1. e4 e5 (1... c5 2. Nf3 Nc6 (2... d6 3. d4) 3. Bb5) 2. Nf3 Nc6 *\n",
    );
    let doc = build_document(&game, meta(), &BTreeSet::new(), false);

    assert_eq!(moves(&doc.game.items), ["1.e4", "e5", "2.Nf3", "Nc6"]);
    let first = variation(&doc.game.items);
    assert_eq!(moves(first), ["1...c5", "2.Nf3", "Nc6", "3.Bb5"]);
    let second = variation(first);
    assert_eq!(moves(second), ["2...d6", "3.d4"]);
}

#[test]
fn a_black_continuation_after_a_variation_gets_its_move_number() {
    let game = parse(
        "[Event \"After variation\"]\n[Site \"?\"]\n[Date \"2026.01.01\"]\n[Round \"1\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"*\"]\n\n1. e4 e5 2. Nf3 (2. Bc4) 2... Nc6 *\n",
    );
    let doc = build_document(&game, meta(), &BTreeSet::new(), false);

    assert_eq!(moves(&doc.game.items), ["1.e4", "e5", "2.Nf3", "2...Nc6"]);
}

#[test]
fn standard_start_is_omitted_and_custom_start_is_preserved() {
    let standard = opera();
    let doc = build_document(&standard, meta(), &BTreeSet::new(), false);
    assert_eq!(doc.game.start_fen, None);
    assert_eq!(doc.game.variant, None);

    let chess960 = parse(
        "[Event \"960\"]\n[Site \"?\"]\n[Date \"2026.01.01\"]\n[Round \"1\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"*\"]\n[FEN \"qnbnrkrb/pppppppp/8/8/8/8/PPPPPPPP/QNBNRKRB w GEge - 0 1\"]\n[Variant \"Fischerandom\"]\n[SetUp \"1\"]\n\n1. d4 d5 2. Nbc3 b6 3. O-O Ne6 *\n",
    );
    let doc = build_document(&chess960, meta(), &BTreeSet::new(), false);
    let fen = doc.game.start_fen.as_deref().unwrap();
    assert!(fen.contains("QNBNRKRB"));
    assert_eq!(doc.game.variant.as_deref(), Some("Fischerandom"));
    assert_eq!(
        moves(&doc.game.items),
        ["1.d4", "d5", "2.Nbc3", "b6", "3.O-O", "Ne6"]
    );
}

#[test]
fn selected_diagrams_are_sorted_and_the_final_position_is_the_default() {
    let game = opera();
    let mainline = game.mainline();
    let final_node = *mainline.last().unwrap();

    let default = build_document(&game, meta(), &BTreeSet::new(), true);
    assert_eq!(default.diagrams.len(), 1);
    assert_eq!(default.diagrams[0].node, final_node);
    assert_eq!(default.diagrams[0].caption, "final position");

    let selected = BTreeSet::from([mainline[2], final_node, 9_999]);
    let doc = build_document(&game, meta(), &selected, true);
    assert_eq!(
        doc.diagrams
            .iter()
            .map(|diagram| diagram.node)
            .collect::<Vec<_>>(),
        [mainline[2], final_node]
    );
    assert!(doc.diagrams[0].caption.starts_with("after "));
    assert_eq!(
        doc.diagrams[0].fen,
        crate::chess::Fen::from_position(&game.nodes[mainline[2]].pos, EnPassantMode::Legal)
            .to_string()
    );
}

#[test]
fn an_empty_game_keeps_its_start_diagram_and_result() {
    let game = Game::default();
    let doc = build_document(&game, meta(), &BTreeSet::new(), true);

    assert!(doc.game.items.is_empty());
    assert_eq!(doc.game.result, "*");
    assert_eq!(doc.diagrams.len(), 1);
    assert_eq!(doc.diagrams[0].node, 0);
    assert_eq!(doc.diagrams[0].caption, "start position");

    let no_diagram = build_document(&game, meta(), &BTreeSet::new(), false);
    assert!(no_diagram.diagrams.is_empty());
}

#[test]
fn output_is_deterministic() {
    let game = opera();
    let selected = BTreeSet::from([4, 12, 20]);
    let first = build_document(&game, meta(), &selected, true);
    let second = build_document(&game, meta(), &selected, true);
    assert_eq!(first, second);
}
