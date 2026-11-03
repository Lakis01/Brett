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
    parse(crate::test_data::OPERA_PGN)
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

#[test]
fn board_svg_contains_squares_coordinates_pieces_and_attribution() {
    let svg = crate::print::board::board_svg(&crate::chess::Chess::default());

    assert_eq!(svg.matches("class=\"square\"").count(), 64);
    for file in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        assert_eq!(
            svg.matches(&format!(">{file}<")).count(),
            2,
            "file {file} should be labelled above and below the board"
        );
    }
    for rank in ["1", "2", "3", "4", "5", "6", "7", "8"] {
        assert_eq!(
            svg.matches(&format!(">{rank}<")).count(),
            2,
            "rank {rank} should be labelled on both sides"
        );
    }
    assert_eq!(svg.matches("class=\"piece\"").count(), 32);
    assert!(svg.contains("class=\"side-to-move\""));
    assert!(svg.contains("data-side-to-move=\"white\""));
    assert!(
        svg.contains("M22.5 11.63V6"),
        "the cburnett king artwork should be embedded"
    );
    assert!(svg.contains("Colin M. L. Burnett"));
    assert!(svg.contains("CC BY-SA 3.0"));
    assert!(svg.contains("viewBox=\"0 0 420 420\""));
}

#[test]
fn side_to_move_marker_follows_the_position() {
    let game = parse(
        "[Event \"Marker\"]\n[Site \"?\"]\n[Date \"2026.01.01\"]\n[Round \"1\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"*\"]\n\n1. e4 *\n",
    );

    let white = crate::print::board::board_svg(&game.nodes[0].pos);
    let black = crate::print::board::board_svg(&game.nodes[1].pos);
    assert!(white.contains("data-side-to-move=\"white\""));
    assert!(black.contains("data-side-to-move=\"black\""));
}

#[test]
fn build_document_embeds_the_board_in_every_diagram() {
    let game = opera();
    let doc = build_document(&game, meta(), &BTreeSet::new(), true);

    assert_eq!(doc.diagrams.len(), 1);
    assert!(doc.diagrams[0].svg.starts_with("<svg"));
    assert!(doc.diagrams[0].svg.contains("class=\"square\""));
    assert!(doc.diagrams[0].svg.contains("class=\"piece\""));
}

#[test]
fn typst_source_uses_a4_two_columns_and_a_running_header() {
    let doc = build_document(&opera(), meta(), &BTreeSet::new(), true);
    let source = crate::print::typst::source(&doc);

    assert!(source.contains("paper: \"a4\""));
    assert!(source.contains("columns: 2"));
    assert!(source.contains("header: context {"));
    assert!(source.contains("counter(page).display()"));
    assert!(source.contains("Test Database"));
    assert!(source.contains("Tester"));
    assert!(source.contains("2026-10-31"));
}

#[test]
fn typst_source_has_a_compact_heading_bold_moves_and_a_result() {
    let doc = build_document(&opera(), meta(), &BTreeSet::new(), false);
    let source = crate::print::typst::source(&doc);

    assert!(source.contains("Paul Morphy"));
    assert!(source.contains("Duke Karl and Count Isouard"));
    assert!(source.contains("Casual Game"));
    assert!(source.contains("Paris FRA"));
    assert!(source.contains("\"C41\""));
    assert!(!source.contains("ECO C41"));
    assert!(!source.contains("Board pieces"));
    assert!(source.contains("column-gutter: 1em"));
    assert!(source.contains("fill: white, stroke: 0.6pt + luma(60)"));
    assert!(source.contains("fill: black"));
    assert!(source.contains("#text(weight: \"bold\", \"1.e4\")"));
    assert!(source.contains("#text(weight: \"bold\", \"1-0\")"));
}

#[test]
fn typst_heading_stacks_players_and_keeps_their_elos_next_to_the_eco() {
    let game = parse(
        "[Event \"Elo test\"]\n[Site \"?\" ]\n[Date \"2026.01.01\"]\n[Round \"1\"]\n[White \"White Player\"]\n[WhiteElo \"2789\"]\n[Black \"Black Player\"]\n[BlackElo \"2698\"]\n[ECO \"C83\"]\n[Result \"*\"]\n\n1. e4 e5 *\n",
    );
    let doc = build_document(&game, meta(), &BTreeSet::new(), false);
    let source = crate::print::typst::source(&doc);

    let white = source.find("\"White Player\"").unwrap();
    let white_elo = source.find("\"2789\"").unwrap();
    let black = source.find("\"Black Player\"").unwrap();
    let black_elo = source.find("\"2698\"").unwrap();
    let eco = source.find("\"C83\"").unwrap();
    assert!(white < white_elo);
    assert!(white_elo < black);
    assert!(black < black_elo);
    assert!(black_elo < eco);
    assert!(!source.contains("ECO C83"));
}

#[test]
fn typst_source_keeps_each_diagram_between_the_moves_it_follows() {
    let game = opera();
    let mainline = game.mainline();
    let doc = build_document(
        &game,
        meta(),
        &BTreeSet::from([mainline[2], mainline[10]]),
        true,
    );
    let source = crate::print::typst::source(&doc);

    let after_nf3 = source.find("\"2.Nf3\"").unwrap();
    let d6 = source.find("\"d6\"").unwrap();
    let after_bc4 = source.find("\"6.Bc4\"").unwrap();
    let nf6 = source.find("#text(weight: \"bold\", \"Nf6\")").unwrap();
    let last_move = source.find("\"17.Rd8#\"").unwrap();
    let first = source.find("diagram-01.svg").unwrap();
    let second = source.find("diagram-02.svg").unwrap();

    assert!(after_nf3 < first);
    assert!(first < d6);
    assert!(after_bc4 < second);
    assert!(second < nf6);
    assert!(last_move < source.find("diagram-03.svg").unwrap());
}

#[test]
fn typst_source_renders_comments_variations_and_diagrams() {
    let game = opera();
    let mainline = game.mainline();
    let doc = build_document(&game, meta(), &BTreeSet::from([mainline[2]]), true);
    let source = crate::print::typst::source(&doc);

    assert!(source.contains("#text(\"Philidor's Defense.\")"));
    assert!(source.contains("#text(\"[\")"));
    assert!(source.contains("#text(\"]\")"));
    assert!(source.contains("inset: (left: 1.2em)"));
    assert!(source.contains("#image(\"diagrams/diagram-01.svg\""));
    assert!(source.contains("#image(\"diagrams/diagram-02.svg\""));
    assert!(source.contains("final position"));
    assert!(!source.contains("(Diagram)"));
}

#[test]
fn typst_source_preserves_annotations_and_nested_indentation() {
    let annotated = parse(
        "[Event \"Annotated\"]\n[Site \"?\"]\n[Date \"2026.01.01\"]\n[Round \"1\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"*\"]\n\n1. e4! {Opening [%clk 0:03:00] [%eval 0.35]} e5? {[%clk 0:02:59] [%eval -0.20]} *\n",
    );
    let doc = build_document(&annotated, meta(), &BTreeSet::new(), false);
    let source = crate::print::typst::source(&doc);
    assert!(source.contains("#text(\"Opening\")"));
    assert!(source.contains("[clk 3:00; eval +0.35]"));
    assert!(source.contains("[clk 2:59; eval -0.20]"));

    let nested = parse(
        "[Event \"Nested\"]\n[Site \"?\"]\n[Date \"2026.01.01\"]\n[Round \"1\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"*\"]\n\n1. e4 e5 (1... c5 2. Nf3 Nc6 (2... d6 3. d4) 3. Bb5) 2. Nf3 Nc6 *\n",
    );
    let doc = build_document(&nested, meta(), &BTreeSet::new(), false);
    let source = crate::print::typst::source(&doc);
    assert!(source.contains("inset: (left: 1.2em)"));
    assert!(source.contains("inset: (left: 2.4em)"));
}

#[test]
fn typst_source_preserves_custom_starts_and_chess960() {
    let game = parse(
        "[Event \"960\"]\n[Site \"?\"]\n[Date \"2026.01.01\"]\n[Round \"1\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"*\"]\n[FEN \"qnbnrkrb/pppppppp/8/8/8/8/PPPPPPPP/QNBNRKRB w GEge - 0 1\"]\n[Variant \"Fischerandom\"]\n[SetUp \"1\"]\n\n1. d4 d5 2. Nbc3 b6 3. O-O Ne6 *\n",
    );
    let doc = build_document(&game, meta(), &BTreeSet::new(), false);
    let source = crate::print::typst::source(&doc);

    assert!(source.contains("Start position:"));
    assert!(source.contains("QNBNRKRB"));
    assert!(source.contains("Fischerandom"));
}

#[test]
fn typst_source_escapes_string_literals_and_is_deterministic() {
    let doc = PrintDocument {
        meta: PrintMeta {
            source: "A \"quote\" \\ source".to_string(),
            owner: "O#1 [x] $y".to_string(),
            date: "2026-10-31".to_string(),
        },
        game: PrintGame {
            headers: vec![
                ("White".to_string(), "A \"quote\" \\ name".to_string()),
                ("Black".to_string(), "Ünïcode".to_string()),
                ("Result".to_string(), "*".to_string()),
            ],
            start_fen: None,
            variant: None,
            items: vec![PrintItem::Comment("He said \"hi\" \\ # $ [x]".to_string())],
            result: "*".to_string(),
        },
        diagrams: Vec::new(),
    };
    let source = crate::print::typst::source(&doc);

    assert!(source.contains("A \\\"quote\\\" \\\\ source"));
    assert!(source.contains("A \\\"quote\\\" \\\\ name"));
    assert!(source.contains("He said \\\"hi\\\" \\\\ # $ [x]"));
    assert!(source.contains("Ünïcode"));
    assert_eq!(source, crate::print::typst::source(&doc));
}

#[test]
fn typst_source_renders_diagrams_for_an_empty_game() {
    let doc = build_document(&Game::default(), meta(), &BTreeSet::new(), true);
    let source = crate::print::typst::source(&doc);

    assert!(source.contains("#image(\"diagrams/diagram-01.svg\""));
    assert!(!source.contains("(Diagram)"));
}

#[test]
fn write_bundle_writes_source_and_diagram_files() {
    let dir = std::env::temp_dir().join(format!("brett-print-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("opera-print_diagrams")).unwrap();
    std::fs::write(
        dir.join("opera-print_diagrams/diagram-09.svg"),
        "<svg>stale</svg>",
    )
    .unwrap();

    let path = dir.join("opera-print.typ");
    let game = opera();
    let mainline = game.mainline();
    let doc = build_document(&game, meta(), &BTreeSet::from([mainline[2]]), true);
    let bundle = crate::print::typst::write_bundle(&path, &doc).unwrap();

    assert_eq!(bundle.source, path);
    assert_eq!(bundle.diagrams.len(), 2);
    assert!(bundle.diagrams.iter().all(|path| path.exists()));
    assert!(bundle.diagrams[0].ends_with("opera-print_diagrams/diagram-01.svg"));
    assert!(bundle.diagrams[1].ends_with("opera-print_diagrams/diagram-02.svg"));
    assert!(!dir.join("opera-print_diagrams/diagram-09.svg").exists());

    let source = std::fs::read_to_string(&path).unwrap();
    assert!(source.contains("opera-print_diagrams/diagram-01.svg"));
    assert!(source.contains("opera-print_diagrams/diagram-02.svg"));
    assert!(!source.contains("(Diagram)"));
    assert!(
        std::fs::read_to_string(&bundle.diagrams[0])
            .unwrap()
            .contains("class=\"square\"")
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
