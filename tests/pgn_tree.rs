use brett::chess::{Color, Move, Position, Role, Square};
use brett::domain::{self, Game, Token};

fn opera() -> Game {
    let games = domain::parse_pgn(include_bytes!("../fixtures/opera.pgn")).unwrap();
    assert_eq!(games.len(), 1);
    games.into_iter().next().unwrap()
}

fn find(g: &Game, san: &str) -> usize {
    g.nodes
        .iter()
        .position(|n| n.san() == Some(san))
        .unwrap_or_else(|| panic!("no node with SAN {san}"))
}

#[test]
fn headers_round_trip() {
    let g = opera();
    assert_eq!(g.white(), "Paul Morphy");
    assert_eq!(g.black(), "Duke Karl and Count Isouard");
    assert_eq!(g.result(), "1-0");
    assert_eq!(g.header("ECO"), Some("C41"));
    assert!(
        g.errors.is_empty(),
        "unexpected import errors: {:?}",
        g.errors
    );
}

#[test]
fn mainline_is_complete() {
    let g = opera();
    assert_eq!(g.mainline_len(), 33);
    let last = g.mainline().last().copied().unwrap();
    assert_eq!(g.nodes[last].san(), Some("Rd8#"));
    // The final position is checkmate.
    assert!(g.final_position().is_checkmate());
}

#[test]
fn variation_hangs_off_the_alternative_move() {
    let g = opera();
    let d4 = find(&g, "d4");
    let exd4 = find(&g, "exd4");
    let bg4 = find(&g, "Bg4");

    assert_eq!(g.nodes[exd4].parent, Some(d4));
    assert_eq!(g.nodes[bg4].parent, Some(d4));
    assert_eq!(g.nodes[d4].children.len(), 2);
    assert_eq!(g.nodes[d4].children[0], bg4, "mainline must stay first");
    assert_eq!(g.nodes[d4].children[1], exd4);
    assert_eq!(g.nodes[exd4].mv.as_ref().unwrap().color, Color::Black);
    assert_eq!(g.nodes[exd4].mv.as_ref().unwrap().number, 3);
}

#[test]
fn figurine_san_uses_unicode_pieces_without_touching_pawns_or_castling() {
    assert_eq!(domain::figurine_san("Nf3"), "♘f3");
    assert_eq!(domain::figurine_san("Bxe6+"), "♗xe6+");
    assert_eq!(domain::figurine_san("exd8=Q+"), "exd8=♕+");
    assert_eq!(domain::figurine_san("O-O-O"), "O-O-O");
    assert_eq!(domain::figurine_san("b4"), "b4");
    assert_eq!(domain::nag_symbol(1), Some("!"));
    assert_eq!(domain::nag_symbol(3), Some("!!"));
    assert_eq!(domain::nag_symbol(13), Some("∞"));
    assert_eq!(domain::nag_symbol(14), Some("+="));
    assert_eq!(domain::nag_symbol(15), Some("=+"));
    assert_eq!(domain::nag_symbol(255), None);
}

#[test]
fn move_lines_place_variations_on_an_indented_line() {
    let g = opera();
    let lines = domain::move_lines(&g);

    let variation = lines
        .iter()
        .find(|line| {
            line.depth == 1
                && line
                    .tokens
                    .iter()
                    .any(|token| matches!(token, Token::Move { san, .. } if san == "exd4"))
        })
        .expect("exd4 variation line");
    assert!(matches!(variation.tokens.first(), Some(Token::OpenParen)));
    assert!(matches!(variation.tokens.last(), Some(Token::CloseParen)));

    let mainline_has_exd4 = lines
        .iter()
        .filter(|line| line.depth == 0)
        .flat_map(|line| &line.tokens)
        .any(|token| matches!(token, Token::Move { san, .. } if san == "exd4"));
    assert!(!mainline_has_exd4, "variation leaked into the mainline");

    let line_moves = lines
        .iter()
        .flat_map(|line| &line.tokens)
        .filter(|token| matches!(token, Token::Move { .. }))
        .count();
    let tree_moves = g.nodes.iter().filter(|node| node.mv.is_some()).count();
    assert_eq!(line_moves, tree_moves, "every move appears exactly once");
}

#[test]
fn comments_attach_to_the_preceding_move() {
    let g = opera();
    assert!(g.nodes[0].comment.contains("Opera Game"));
    let d6 = find(&g, "d6");
    assert!(g.nodes[d6].comment.contains("Philidor"));

    let nc3 = g
        .nodes
        .iter()
        .find(|n| n.comment.contains("usual continuation"))
        .expect("variation comment");
    assert_eq!(nc3.san(), Some("Nc3"));
}

#[test]
fn tokens_include_variation_parens() {
    let g = opera();
    let tokens = domain::tokens(&g);
    let opens = tokens
        .iter()
        .filter(|t| matches!(t, Token::OpenParen))
        .count();
    let closes = tokens
        .iter()
        .filter(|t| matches!(t, Token::CloseParen))
        .count();
    assert_eq!(opens, 1);
    assert_eq!(closes, 1);
    let moves = tokens
        .iter()
        .filter(|t| matches!(t, Token::Move { .. }))
        .count();
    assert_eq!(moves, 37, "33 mainline plies plus 4 variation plies");

    // PGN convention: the alternative is written after the move it replaces.
    let bg4 = tokens
        .iter()
        .position(|t| matches!(t, Token::Move { san, .. } if san == "Bg4"))
        .expect("Bg4 token");
    assert!(matches!(tokens[bg4 + 1], Token::OpenParen));
}

#[test]
fn add_move_reuses_existing_child() {
    let mut g = Game::default();
    let e4 = Move::Normal {
        role: Role::Pawn,
        from: Square::E2,
        to: Square::E4,
        capture: None,
        promotion: None,
    };
    let first = g.add_move(0, e4);
    assert_eq!(g.nodes[first].san(), Some("e4"));
    let again = g.add_move(0, e4);
    assert_eq!(first, again);
    assert_eq!(g.nodes[0].children.len(), 1);

    let d4 = Move::Normal {
        role: Role::Pawn,
        from: Square::D2,
        to: Square::D4,
        capture: None,
        promotion: None,
    };
    let second = g.add_move(0, d4);
    assert_ne!(first, second);
    assert_eq!(g.nodes[0].children, vec![first, second]);
    assert_eq!(g.nodes[second].san(), Some("d4"));
}

#[test]
fn custom_start_position_is_honoured() {
    let pgn = br#"[Event "Endgame"]
[Site "?"]
[Date "2026.01.01"]
[Round "?"]
[White "A"]
[Black "B"]
[Result "*"]
[FEN "7k/8/8/8/8/8/8/6K1 w - - 0 1"]
[SetUp "1"]

1. Kf2 Kg8 2. Kf3 Kf7 *
"#;
    let games = domain::parse_pgn(pgn).unwrap();
    assert_eq!(games.len(), 1);
    let g = &games[0];
    assert!(g.errors.is_empty(), "{:?}", g.errors);
    assert_eq!(g.mainline_len(), 4);
    assert_eq!(g.nodes[find(g, "Kf2")].mv.as_ref().unwrap().number, 1);
}

#[test]
fn chess960_variant_imports_castling_with_960_rook() {
    let pgn = br#"[Event "960"]
[Site "?"]
[Date "2026.01.01"]
[Round "1"]
[White "A"]
[Black "B"]
[Result "*"]
[FEN "qnbnrkrb/pppppppp/8/8/8/8/PPPPPPPP/QNBNRKRB w GEge - 0 1"]
[Variant "Fischerandom"]
[SetUp "1"]

1. d4 d5 2. Nbc3 b6 3. O-O Ne6 *
"#;
    let games = domain::parse_pgn(pgn).unwrap();
    assert_eq!(games.len(), 1);
    let g = &games[0];
    assert!(g.errors.is_empty(), "{:?}", g.errors);
    assert_eq!(g.mainline_len(), 6);
    let castle = find(g, "O-O");
    assert_eq!(g.nodes[castle].mv.as_ref().unwrap().uci, "f1g1");
    assert_eq!(g.nodes[castle].pos.turn(), Color::Black);
}

#[test]
fn null_moves_are_accepted() {
    let pgn = br#"[Event "Analysis"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 e5 2. Nf3 -- 3. Nxe5 *
"#;
    let games = domain::parse_pgn(pgn).unwrap();
    let g = &games[0];
    assert!(g.errors.is_empty(), "{:?}", g.errors);
    assert_eq!(g.mainline_len(), 5);

    let null = g.nodes.iter().position(|n| n.san() == Some("--")).unwrap();
    let ply = g.nodes[null].mv.as_ref().unwrap();
    assert_eq!(ply.uci, "0000");
    assert_eq!(ply.number, 2);
    assert_eq!(ply.color, Color::Black);
    assert_eq!(g.nodes[null].pos.turn(), Color::White);

    let nxe5 = g
        .nodes
        .iter()
        .position(|n| n.san() == Some("Nxe5"))
        .unwrap();
    let ply = g.nodes[nxe5].mv.as_ref().unwrap();
    assert_eq!(ply.number, 3);
    assert_eq!(ply.color, Color::White);
}

#[test]
fn illegal_moves_are_reported_not_fatal() {
    let pgn = br#"[Event "Broken"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 e5 2. Kf3 *
"#;
    let games = domain::parse_pgn(pgn).unwrap();
    let g = &games[0];
    assert_eq!(g.errors.len(), 1);
    assert!(g.errors[0].contains("Kf3"));
    // The legal prefix is still imported.
    assert_eq!(g.mainline_len(), 2);
}

#[test]
fn illegal_san_is_reported_without_aborting() {
    let pgn = br#"[Event "Broken"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 e5 2. Kf3 Nc6 3. Bb5 *
"#;
    let games = domain::parse_pgn(pgn).unwrap();
    let g = &games[0];
    assert_eq!(g.errors.len(), 1);
    assert!(g.errors[0].contains("Kf3"));
    assert_eq!(g.mainline_len(), 2);
}

#[test]
fn truncated_game_returns_at_eof() {
    let pgn = br#"[Event "Trunc"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 e5 2. Nf3"#;
    let games = domain::parse_pgn(pgn).unwrap();
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].mainline_len(), 3);
}
