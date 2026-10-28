# cozy-chess 0.3.4 API notes for the Brett facade

Verified against the crate sources in the local Cargo registry while planning
Task 17. These notes exist so a later task or a different implementer does
not have to rediscover the same details.

- Licences: `cozy-chess` 0.3.4 and `cozy-chess-types` 0.2.2 are MIT.
- Default features: none. `std` only adds `std::error::Error` impls.
- Start position hash: `0x33ab_452b_8705_6e0f`.
- Standard start perft 1–3: 20 / 400 / 8902.
- `chess960_startpos(0)` = `bbqnnrkr/pppppppp/8/8/8/8/PPPPPPPP/BBQNNRKR`,
  hash `0x6c0f795d8a1e7910`.

## Board and builder

- `BoardBuilder` fields are public: `board: [Option<(Piece, Color)>; 64]`,
  `side_to_move`, `castle_rights: [CastleRights; 2]`,
  `en_passant: Option<Square>`, `halfmove_clock: u8`,
  `fullmove_number: u16`.
- `BoardBuilder::empty()` and `BoardBuilder::startpos()` exist;
  `build(&self) -> Result<Board, BoardBuilderError>`.
- Builder validation: exactly one king per color, at most 16 pieces and 8
  pawns, no pawns on back ranks, castling king/rook on the back rank with
  the rook present, EP pawn present, halfmove <= 100, fullmove > 0, and the
  side not to move is not in check.
- `CastleRights { short: Option<File>, long: Option<File> }`; `short` is the
  kingside rook file, `long` the queenside rook file.
- `Board::castle_rights(color) -> CastleRights`.
- `Board::en_passant() -> Option<File>`; the EP square rank depends on the
  side to move (rank 6 when White is to move, rank 3 when Black).
- `Board::king()` panics on a kingless board. Use
  `colored_pieces(color, Piece::King).iter().next()`.
- `Board::piece_on(square) -> Option<Piece>` (role only);
  `Board::color_on(square) -> Option<Color>`.
- `Board::status() -> GameStatus`; `Won` means checkmate, `Drawn` means
  stalemate.

## Moves

- `cozy_chess::Move { from: Square, to: Square, promotion: Option<Piece> }`.
- Castling is king-from to rook-square; promotions carry the promotion role.
- `Board::generate_moves(|moves: PieceMoves| { ... })`; `PieceMoves`
  implements `IntoIterator<Item = Move>` and already expands promotions.
- `Board::try_play(mv) -> Result<(), IllegalMoveError>`;
  `Board::play_unchecked(mv)`.
- `Board::null_move()` exists but increments the halfmove clock. Brett's
  null-move code resets the clock to 0, so the facade must rebuild the
  position through `Setup` instead of calling it.

## SAN and UCI

- `util::parse_san_move(&Board, &str) -> Result<Move, MoveParseError>` ignores
  `+`/`#`, handles `O-O`/`O-O-O` from the board's castle rights, and returns
  king-to-rook for castling.
- `util::display_san_move(&Board, Move) -> impl Display` writes SAN with
  `+`/`#` and `O-O`/`O-O-O`.
- `util::parse_uci_move(&Board, &str)` converts standard castling `e1g1` to
  king-to-rook `e1h1`.
- `util::display_uci_move(&Board, Move)` converts king-to-rook back to
  standard castling when applicable.

## FEN and hashing

- `Board::from_fen(&str, shredder: bool)` and `Board::from_str`.
- The parser caps fullmove `0` to `1` and the halfmove clock to `100`.
- `Board::from_str` prints `KQkq` even for non-a/h rooks, so Brett writes
  X-FEN itself and resolves `K`/`Q` to the outermost rook on the king's side
  (kingside = higher file, queenside = lower file, both colors).
- `Board::hash()` includes en passant whenever the board has an EP file;
  `hash_without_ep()` excludes it. Brett uses `hash()` only when a legal EP
  capture exists, otherwise `hash_without_ep()`.
- cozy hashes are not Polyglot-compatible. Polyglot `.bin` books use Brett's
  own 781-entry table.

## Polyglot

- Table layout: 768 piece-square entries, 4 castling entries (768–771),
  8 EP-file entries (772–779), and the side-to-move entry (780).
- Piece order: White pawn, Black pawn, White knight, Black knight, …,
  White king, Black king; index `64 * piece_index + square`.
- Castling order: White kingside, White queenside, Black kingside,
  Black queenside.
- EP is included when the side to move has an adjacent pawn, regardless of
  whether the capture is legal. The position index uses legal EP only.
- Test vectors (python-chess 1.11.2):
  - startpos `0x463b_9618_1691_fc9c`
  - after 1.e4 `0x823c_9b50_fd11_4196`
  - after 1.e4 c5 `0x644d_4afe_0256_4aeb`
  - Kiwipete `0xc3ce_103f_01d1_5e1d`
  - `rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3`
    `0x8260_57c0_f644_3c7c`
  - `r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1` `0xfda2_39cc_692a_6053`
  - `8/2P5/8/8/8/8/8/K6k w - - 0 1` `0xce58_c3b3_356f_fdac`
  - `4r1k1/8/8/3pP3/8/8/8/4K3 w - d6 0 1` `0x7ddd_0eef_bfc8_435a`
  - same without EP `0x6144_d03c_8370_d3fb`
