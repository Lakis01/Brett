//! Public-domain test data shared by unit and integration tests.
//!
//! Keeping the game inline means the test suite needs no checked-in PGN or
//! binary database fixtures.

/// The Opera Game (Morphy vs. Duke Karl and Count Isouard, Paris 1858),
/// including the annotations and variation used by parser and SCID tests.
pub const OPERA_PGN: &str = r#"[Event "Casual Game"]
[Site "Paris FRA"]
[Date "1858.??.??"]
[Round "?"]
[White "Paul Morphy"]
[Black "Duke Karl and Count Isouard"]
[Result "1-0"]
[ECO "C41"]
[Opening "Philidor Defense"]

{The Opera Game, played in 1858 in the Italian Opera House in Paris.}
1. e4 e5 2. Nf3 d6 {Philidor's Defense.} 3. d4 Bg4 (3... exd4 4. Nxd4 Nf6 5. Nc3 {The usual continuation.}) 4. dxe5 Bxf3 5. Qxf3 dxe5 6. Bc4 Nf6 7. Qb3 Qe7 8. Nc3 c6 9. Bg5 b5 10. Nxb5! cxb5 11. Bxb5+ Nbd7 12. O-O-O Rd8 13. Rxd7 Rxd7 14. Rd1 Qe6 15. Bxd7+ Nxd7 16. Qb8+ Nxb8 17. Rd8# 1-0
"#;
