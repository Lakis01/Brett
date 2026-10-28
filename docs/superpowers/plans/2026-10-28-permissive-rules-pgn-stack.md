# Permissive Rules and PGN Stack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the GPL `shakmaty` and `pgn-reader` crates with MIT `cozy-chess` behind a Brett-owned rules facade and a Brett-owned streaming PGN parser, preserving Chess960, byte-range indexing, SAN/FEN/UCI behavior, and every existing test.

**Architecture:** A new `src/chess` module wraps `cozy-chess` in Brett-owned scalar, board, move, SAN, FEN, and position types so call sites keep their current vocabulary. A new `src/pgn/reader.rs` implements the streaming tokenizer and visitor contract currently provided by `pgn-reader`. Once all call sites use the facade, both GPL crates are removed from `Cargo.toml` and `Cargo.lock`.

**Tech Stack:** Rust 2024 (MSRV 1.97), `cozy-chess` 0.3.4 (MIT), `cozy-chess-types` 0.2.2 (MIT), `anyhow`, `memmap2`, `eframe`/`egui`. No network access is required.

**Spec:** `docs/GPL_OPTIONS.md` (staged replacement sequence), `docs/BACKLOG.md` item 17.

## Global Constraints

- Brett's original code stays MIT. Do not add any GPL, AGPL, LGPL, or other copyleft dependency, and do not copy code from `shakmaty`, `pgn-reader`, python-chess, or any GPL/AGPL implementation.
- `cozy-chess` is the only new rules dependency: `cozy-chess = "0.3.4"` (MIT), which brings `cozy-chess-types` 0.2.2 (MIT). Keep its default feature set unless a compile error requires `std`.
- Chess960/DFRC is mandatory. Standard and Chess960 castling, X-FEN/Shredder FEN, and king-captures-rook internal castling representation must all keep working.
- Keep native Rust, read-only source databases, Mega Database as the default reference corpus, and the existing large-database streaming behavior. Never write to a user's PGN, SCID, or CBH source.
- Preserve the bundled ICCF correspondence reference and its exact notice. Do not touch the photo pack or redistribute any photo assets.
- The position-index on-disk `VERSION` becomes `2`; old caches are ignored, not migrated.
- `src/position.rs` keys use the new Brett position hash (`0x33ab_452b_8705_6e0f` for the start position). Polyglot `.bin` books use a separate Brett-owned Polyglot hash; they are not the same function.
- The PGN parser must be streaming, must expose an infallible logical `stream_position() -> u64` that excludes buffered lookahead, and must tolerate malformed/truncated input without aborting the file.
- After Task 13, `shakmaty` and `pgn-reader` must not appear in `Cargo.toml`, `Cargo.lock`, `src/`, `tests/`, or `examples/`.
- Run `cargo fmt --all` before each commit. The final gate is `cargo test --all-targets`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check`.

## Review Focus

These are the failure modes most likely to bite a user even though the happy-path tests pass. Each one has a named test in the owning task.

1. **Chess960 castling notation and rights.** A user opens a DFRC game whose king is not on E and whose rooks are not on A/H. X-FEN `KQkq` must resolve to the outermost rooks, `O-O`/`O-O-O` must parse and print correctly, and standard UCI `e1g1` must still work for orthodox chess. Tested by `chess960_castling_round_trip`, `fen_resolves_xfen_kq_to_outermost_rooks`, and `uci_standard_castling_round_trip` in Tasks 2–4.
2. **En passant hash semantics.** A pinned en-passant pawn is not a legal capture. The position index must not change key for that position, while a Polyglot `.bin` lookup must still use the standard Polyglot rule (adjacent pawn, legality irrelevant). Tested by `position_hash_uses_legal_ep_only` and `polyglot_pinned_ep_matches_standard_key` in Tasks 4 and 8.
3. **PGN comments and tags containing delimiters.** A comment may contain `[Event "..."]`, braces, semicolons, escaped quotes, and invalid UTF-8. None of that may start a new game or corrupt the byte ranges. Tested by `bracket_inside_comment_is_not_a_new_game` and `tags_support_escaped_quotes_and_lossy_utf8` in Task 5.
4. **Illegal or truncated movetext.** A PGN with an illegal SAN, an unclosed variation, or EOF mid-game must return the legal prefix and report the error, not abort the whole file. Tested by `illegal_san_is_reported_without_aborting`, `unclosed_variation_is_closed_at_eof`, and `truncated_game_returns_at_eof` in Tasks 5–6.
5. **Byte-range indexing on huge files.** `stream_position()` must point at the next game's `[`, never into the parser's read-ahead buffer, or `PgnLibrary::load` reads the wrong bytes. A header-only visitor must still advance to the next game boundary. Tested by `stream_positions_cover_each_game` and `header_only_visitor_advances_to_next_game` in Tasks 5 and 7.

---

## File Structure

**Create**

- `src/chess/mod.rs` — facade root and re-exports.
- `src/chess/types.rs` — `Color`, `Role`, `Piece`, `File`, `Rank`, `Square`, `Bitboard`.
- `src/chess/board.rs` — `Board`, `BoardRef`, `Setup`, `CastlingMode`, `CastlingSide`, `Castles`.
- `src/chess/move.rs` — `Move`, `UciMove`, parse/display helpers.
- `src/chess/san.rs` — `San`, `SanPlus`.
- `src/chess/fen.rs` — `Fen`, X-FEN/Shredder parsing and writing.
- `src/chess/chess.rs` — `Chess`, `Position` trait, `EnPassantMode`, hash and play.
- `src/pgn/reader.rs` — streaming PGN reader and visitor trait.
- `src/book/polyglot.rs` — Brett-owned Polyglot Zobrist hash.
- `src/book/polyglot_keys.rs` — 781-entry Polyglot random table transcribed from MIT `gigachess` 0.1.9.
- `tests/chess_facade.rs` — facade unit/integration tests.
- `tests/pgn_reader.rs` — parser tests.
- `tests/polyglot.rs` — Polyglot hash vectors.

**Modify**

- `Cargo.toml`, `Cargo.lock` — swap dependencies.
- `src/lib.rs` — `pub mod chess;`.
- `src/domain.rs`, `src/library.rs`, `src/position.rs` — remove `pgn-reader`, migrate rules types.
- `src/book.rs`, `src/book/bkt.rs` — Polyglot hash and facade types.
- `src/cbh/moves.rs`, `src/cbh/tests.rs`, `src/scid.rs`, `src/scid/writer.rs` — facade types.
- `src/pgn.rs`, `src/cloud.rs`, `src/recent.rs`, `src/players.rs`, `src/review.rs`, `src/settings.rs` — facade types.
- `src/app.rs`, `src/ui/board.rs`, `src/ui/pieces.rs` — facade types and About text.
- `tests/pgn_tree.rs`, `tests/position_index.rs`, `tests/scid.rs` — facade imports.
- `examples/position_report.rs`, `examples/predictor_eval.rs` — facade imports.
- `THIRD_PARTY_NOTICES.md`, `README.md`, `CONTRIBUTING.md`, `docs/POSITION_INDEX.md`, `docs/GPL_OPTIONS.md`, `docs/BACKLOG.md` — licence/status documentation.

---

### Task 1: Add `cozy-chess` and the facade scalar types

**Files:**
- Modify: `Cargo.toml`
- Create: `src/chess/mod.rs`, `src/chess/types.rs`
- Modify: `src/lib.rs`
- Test: `tests/chess_facade.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `Color`, `Role`, `Piece`, `File`, `Rank`, `Square`, `Bitboard` with the exact methods used by later tasks.

- [ ] **Step 1: Add the dependency**

Add `cozy-chess = "0.3.4"` to `[dependencies]` in `Cargo.toml`. Leave `shakmaty` and `pgn-reader` in place for now; they are removed in Task 13.

- [ ] **Step 2: Write the failing scalar test**

Create `tests/chess_facade.rs` with:

```rust
use std::str::FromStr;

use brett::chess::{Bitboard, Color, File, Piece, Rank, Role, Square};

#[test]
fn scalar_types_round_trip() {
    assert_eq!(Square::new(12), Square::E2);
    assert_eq!(Square::from_str("e4").unwrap(), Square::E4);
    assert_eq!(Square::E4.to_string(), "e4");
    assert_eq!(Square::from_coords(File::E, Rank::Fourth), Square::E4);
    assert_eq!(Square::E4.file(), File::E);
    assert_eq!(Square::E4.rank(), Rank::Fourth);
    assert_eq!(File::new(4), File::E);
    assert_eq!(Rank::new(0), Rank::First);
    assert_eq!(File::A.flip_horizontal(), File::H);
    assert_eq!(Rank::First.flip_vertical(), Rank::Eighth);
    assert_eq!(Color::White.other(), Color::Black);
    assert_eq!(Piece::new(Color::White, Role::Knight).char(), 'N');
    assert_eq!(Piece::new(Color::Black, Role::Queen).char(), 'q');
    assert!(Bitboard::from_square(Square::E4).contains(Square::E4));
    assert!(!Bitboard::EMPTY.contains(Square::E4));
    assert_eq!((Bitboard::from_square(Square::A1) | Bitboard::from_square(Square::H8)).to_u64(), 0x8000_0000_0000_0001);
}

#[test]
fn square_parsing_rejects_bad_input() {
    assert!(Square::from_str("").is_err());
    assert!(Square::from_str("i4").is_err());
    assert!(Square::from_str("e9").is_err());
    assert!(Square::from_str("e44").is_err());
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --test chess_facade`
Expected: FAIL because `brett::chess` does not exist.

- [ ] **Step 4: Implement the scalar types**

Create `src/chess/mod.rs` with `mod types; pub use types::*;` and add `pub mod chess;` to `src/lib.rs`.

Implement `src/chess/types.rs`:

- `Color` is a `Copy` enum `White | Black` with `ALL`, `other()`, `fold_wb()`, and `From`/`Into` conversions for `cozy_chess::Color`.
- `Role` is a `Copy` enum `Pawn | Knight | Bishop | Rook | Queen | King` with `ALL`, `char()` (uppercase), `from_char()`, and conversions for `cozy_chess::Piece`.
- `Piece` is a public-field struct `{ pub color: Color, pub role: Role }` with `new()` and `char()` (uppercase for White, lowercase for Black).
- `File` wraps a `u8` in `0..=7`; `Rank` wraps a `u8` in `0..=7`; both expose associated constants (`File::A..H`, `Rank::First..Eighth`), `ALL`, `new(u32)`, `to_u32()`, `From<File/Rank> for u32/usize`, `flip_horizontal()`/`flip_vertical()`, and `Display`.
- `Square` wraps a `u8` in `0..=63`; expose all 64 associated constants (generate them with a macro), `ALL`, `new(u32)`, `from_coords(File, Rank)`, `file()`, `rank()`, `to_u32()`, `From<Square> for u32/usize`, `FromStr`, `Display`, and conversions for `cozy_chess::Square`.
- `Bitboard` wraps a `u64`; expose `EMPTY`, `from_square(Square)`, `contains(Square)`, `is_empty()`, `to_u64()`, `BitOr`, `BitOrAssign`, and `From<cozy_chess::BitBoard>`/`Into<cozy_chess::BitBoard>`.
- Derive `Clone`, `Copy`, `Debug`, `PartialEq`, `Eq`, `Hash`, `PartialOrd`, `Ord` where the type supports it.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --test chess_facade`
Expected: PASS for both tests.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/chess/mod.rs src/chess/types.rs tests/chess_facade.rs
git commit -m "feat: add cozy-chess and Brett chess scalar types"
```

---

### Task 2: Facade board, setup, castling, and `Chess`

**Files:**
- Create: `src/chess/board.rs`, `src/chess/chess.rs`
- Modify: `src/chess/mod.rs`
- Test: `tests/chess_facade.rs`

**Interfaces:**
- Consumes: Task 1 scalar types.
- Produces:
  - `Board::{empty, startpos, piece_at, set_piece_at, remove_piece_at, king_of, role_at}`
  - `BoardRef::{piece_at, king_of, role_at}`
  - `Setup { board, turn, castling_rights, ep_square, halfmoves, fullmoves }`, `Setup::{empty, default, swap_turn}`
  - `CastlingMode::{Standard, Chess960}` with `is_chess960()`
  - `CastlingSide::{KingSide, QueenSide}`
  - `Castles::{castling_rights, rook, has, mode}`
  - `Chess::{default, from_setup, board, turn, castles, maybe_ep_square, halfmoves, fullmoves, to_setup, swap_turn, us}`
  - `PositionError` with `Display`.

- [ ] **Step 1: Write the failing board/setup test**

Append to `tests/chess_facade.rs`:

```rust
use brett::chess::{Board, CastlingMode, CastlingSide, Chess, Piece, Setup};
use std::num::NonZeroU32;

#[test]
fn board_and_custom_start_round_trip() {
    let mut board = Board::empty();
    assert!(board.piece_at(Square::E4).is_none());
    board.set_piece_at(Square::E4, Piece::new(Color::White, Role::Knight));
    assert_eq!(board.piece_at(Square::E4), Some(Piece::new(Color::White, Role::Knight)));
    assert_eq!(board.role_at(Square::E4), Some(Role::Knight));
    assert!(board.king_of(Color::White).is_none());

    let mut board = Board::startpos();
    board.remove_piece_at(Square::E2);
    board.set_piece_at(Square::E1, Piece::new(Color::White, Role::King));
    assert_eq!(board.king_of(Color::White), Some(Square::E1));
}

#[test]
fn setup_default_and_swap_turn() {
    let setup = Setup::default();
    assert_eq!(setup.turn, Color::White);
    assert_eq!(setup.fullmoves, NonZeroU32::new(1).unwrap());
    assert_eq!(setup.board.piece_at(Square::E1), Some(Piece::new(Color::White, Role::King)));
    assert_eq!(setup.board.piece_at(Square::D1), Some(Piece::new(Color::White, Role::Queen)));

    let mut setup = Setup::empty();
    setup.turn = Color::Black;
    setup.ep_square = Some(Square::E3);
    setup.swap_turn();
    assert_eq!(setup.turn, Color::White);
    assert_eq!(setup.ep_square, None);
}

/// DFRC start: king on G1, rooks on F1 and H1.
fn dfrc_setup() -> Setup {
    let mut board = Board::empty();
    for (square, role) in [
        (Square::A1, Role::Bishop),
        (Square::B1, Role::Bishop),
        (Square::C1, Role::Queen),
        (Square::D1, Role::Knight),
        (Square::E1, Role::Knight),
        (Square::F1, Role::Rook),
        (Square::G1, Role::King),
        (Square::H1, Role::Rook),
    ] {
        board.set_piece_at(square, Piece::new(Color::White, role));
    }
    for file in File::ALL {
        board.set_piece_at(
            Square::from_coords(file, Rank::Second),
            Piece::new(Color::White, Role::Pawn),
        );
    }
    // A black king is required for the position to validate; this test only
    // inspects White's rights.
    board.set_piece_at(Square::E8, Piece::new(Color::Black, Role::King));
    let mut rights = Bitboard::EMPTY;
    rights |= Bitboard::from_square(Square::F1);
    rights |= Bitboard::from_square(Square::H1);
    Setup {
        board,
        turn: Color::White,
        castling_rights: rights,
        ep_square: None,
        halfmoves: 0,
        fullmoves: NonZeroU32::new(1).unwrap(),
    }
}

#[test]
fn chess_from_setup_standard_and_chess960() {
    let chess = Chess::default();
    assert_eq!(chess.turn(), Color::White);
    assert_eq!(chess.board().piece_at(Square::E1), Some(Piece::new(Color::White, Role::King)));
    assert_eq!(chess.castles().rook(Color::White, CastlingSide::KingSide), Some(Square::H1));
    assert_eq!(chess.castles().rook(Color::White, CastlingSide::QueenSide), Some(Square::A1));
    assert_eq!(chess.fullmoves(), NonZeroU32::new(1).unwrap());

    let chess960 = Chess::from_setup(dfrc_setup(), CastlingMode::Chess960).unwrap();
    assert_eq!(chess960.castles().rook(Color::White, CastlingSide::KingSide), Some(Square::H1));
    assert_eq!(chess960.castles().rook(Color::White, CastlingSide::QueenSide), Some(Square::F1));
}

#[test]
fn standard_mode_drops_nonstandard_castling_rights() {
    let standard = Chess::from_setup(dfrc_setup(), CastlingMode::Standard).unwrap();
    assert!(!standard.castles().has(Color::White, CastlingSide::KingSide));
    assert!(!standard.castles().has(Color::White, CastlingSide::QueenSide));
    assert!(Chess::from_setup(Setup::empty(), CastlingMode::Standard).is_err());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test chess_facade`
Expected: FAIL because `Board`, `Setup`, and `Chess` do not exist.

- [ ] **Step 3: Implement board/setup/castling/Chess**

Create `src/chess/board.rs`:

- `Board { squares: [Option<Piece>; 64] }` with `empty()`, `startpos()`, `piece_at`, `set_piece_at`, `remove_piece_at`, `king_of`, `role_at`.
- `BoardRef<'a>(&'a cozy_chess::Board)` with `piece_at`, `king_of`, `role_at`; read `piece_on`/`color_on`, and find the king with `colored_pieces(color, cozy_chess::Piece::King).iter().next()`.
- `Setup` with the public fields listed above; `Default` is the orthodox start position; `empty()` is an empty board with White to move, no rights, no EP, halfmoves 0, fullmoves 1; `swap_turn()` flips the turn and clears `ep_square`.
- `CastlingMode`, `CastlingSide`.
- `Castles { mode, short: [Option<Square>; 2], long: [Option<Square>; 2] }` with `from_board(&cozy_chess::Board, CastlingMode)` and `from_setup(&Setup, CastlingMode)`. `from_setup` finds the king on its back rank, considers only `castling_rights` squares on that rank, picks the outermost rook left of the king for `long` and right of the king for `short`, and in `CastlingMode::Standard` keeps a side only when the king is on E and the rook is on A (long) or H (short). `castling_rights()` returns a `Bitboard` of both rook squares; `rook()` and `has()` read the arrays.

Create `src/chess/chess.rs`:

- `Chess { board: cozy_chess::Board, mode: CastlingMode }` with `Default` = `from_setup(Setup::default(), Standard).unwrap()`.
- `Chess::from_setup(setup, mode)`: build a `cozy_chess::BoardBuilder::empty()`, copy every facade piece into `builder.board[square.to_u32() as usize]`, set `side_to_move`, derive valid `CastleRights` from `Castles::from_setup`, set `en_passant`, clamp `halfmove_clock` to `100` and `fullmove_number` to `u16::MAX`, call `build()`, and map any error to `PositionError`.
- `board() -> BoardRef<'_>`, `turn()`, `castles()`, `maybe_ep_square()` (derive the rank from the side to move: rank 6 for White, rank 3 for Black), `halfmoves() -> u32`, `fullmoves() -> NonZeroU32`, `to_setup(EnPassantMode)`, `swap_turn()`, `us() -> Bitboard` (all pieces of the side to move).
- `PositionError(String)` with `Display` and `std::error::Error`.

`EnPassantMode` is introduced in Task 4. For Task 2, implement `to_setup` as `to_setup_always()` internally, and add the public `EnPassantMode` parameter in Task 4.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test chess_facade`
Expected: PASS for all Task 1 and Task 2 tests.

- [ ] **Step 5: Commit**

```bash
git add src/chess/mod.rs src/chess/board.rs src/chess/chess.rs tests/chess_facade.rs
git commit -m "feat: add Brett board, setup, castling, and Chess facade"
```

---

### Task 3: Facade moves, SAN, and UCI

**Files:**
- Create: `src/chess/move.rs`, `src/chess/san.rs`
- Modify: `src/chess/chess.rs`, `src/chess/mod.rs`
- Test: `tests/chess_facade.rs`

**Interfaces:**
- Consumes: Task 2 `Chess`, `Board`, `Castles`, `CastlingMode`, `CastlingSide`.
- Produces:
  - `Move::{Normal, EnPassant, Castle, Put}` with `role()`, `from()`, `to()`, `promotion()`, `to_uci(CastlingMode)`.
  - `UciMove::{Normal, Put, Null}` with `from_ascii(&[u8])`, `from_move(Move, CastlingMode)`, `to_move(&Chess)`, `from()`, `to()`, `FromStr`, `Display`.
  - `San::{Move(Move), Raw(String), Null}` with `to_move(&Chess)`.
  - `SanPlus { pub san: San, pub text: String }` with `from_move(Chess, Move)`, `from_ascii(&[u8])`, `to_move(&Chess)`, `Display`.
  - `Chess::{legal_moves, play, play_unchecked, is_checkmate}`.
  - `PlayError`, `SanError`, `UciMoveError`.

- [ ] **Step 1: Write the failing move/SAN/UCI test**

Append to `tests/chess_facade.rs`:

```rust
use brett::chess::{Move, San, SanPlus, UciMove};

fn play_san(pos: Chess, san: &str) -> Chess {
    let san = SanPlus::from_ascii(san.as_bytes()).unwrap();
    let mv = san.san.to_move(&pos).unwrap();
    pos.play(mv).unwrap()
}

#[test]
fn legal_moves_and_play_round_trip() {
    let start = Chess::default();
    assert_eq!(start.legal_moves().len(), 20);
    let e4 = SanPlus::from_ascii(b"e4").unwrap().san.to_move(&start).unwrap();
    assert_eq!(e4, Move::Normal {
        role: Role::Pawn,
        from: Square::E2,
        capture: None,
        to: Square::E4,
        promotion: None,
    });
    let after = start.clone().play(e4).unwrap();
    assert_eq!(after.turn(), Color::Black);
    assert_eq!(after.board().piece_at(Square::E4), Some(Piece::new(Color::White, Role::Pawn)));
}

#[test]
fn san_round_trip_and_checkmate_suffix() {
    let start = Chess::default();
    let e4 = SanPlus::from_ascii(b"e4").unwrap().san.to_move(&start).unwrap();
    assert_eq!(SanPlus::from_move(start.clone(), e4).to_string(), "e4");

    let mut pos = start;
    for san in ["e4", "e5", "Bc4", "Nc6", "Qh5", "Nf6"] {
        pos = play_san(pos, san);
    }
    let mate = SanPlus::from_ascii(b"Qxf7#").unwrap().san.to_move(&pos).unwrap();
    assert_eq!(SanPlus::from_move(pos.clone(), mate).to_string(), "Qxf7#");
    assert!(pos.clone().play(mate).unwrap().is_checkmate());
}

#[test]
fn uci_standard_castling_round_trip() {
    let mut pos = Chess::default();
    for san in ["e4", "e5", "Nf3", "Nc6", "Bc4", "Bc5"] {
        pos = play_san(pos, san);
    }
    let uci = UciMove::from_ascii(b"e1g1").unwrap();
    let mv = uci.to_move(&pos).unwrap();
    assert_eq!(mv, Move::Castle { king: Square::E1, rook: Square::H1 });
    assert_eq!(UciMove::from_move(mv, CastlingMode::Standard).to_string(), "e1g1");
    assert_eq!(SanPlus::from_move(pos, mv).to_string(), "O-O");
}

#[test]
fn en_passant_move_is_recognized() {
    let mut pos = Chess::default();
    for san in ["e4", "a6", "e5", "d5"] {
        pos = play_san(pos, san);
    }
    let mv = UciMove::from_ascii(b"e5d6").unwrap().to_move(&pos).unwrap();
    assert_eq!(mv, Move::EnPassant { from: Square::E5, to: Square::D6 });
    let after = pos.play(mv).unwrap();
    assert!(after.board().piece_at(Square::D5).is_none());
}

#[test]
fn illegal_san_is_an_error() {
    let start = Chess::default();
    let san = SanPlus::from_ascii(b"Kf3").unwrap();
    assert!(san.san.to_move(&start).is_err());
    assert!(SanPlus::from_ascii(b"").is_err());
    assert!(matches!(SanPlus::from_ascii(b"--").unwrap().san, San::Null));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test chess_facade`
Expected: FAIL because `Move`, `SanPlus`, and `UciMove` do not exist.

- [ ] **Step 3: Implement moves, SAN, and UCI**

Create `src/chess/move.rs`:

- `Move` variants mirroring the current call sites: `Normal { role, from, capture, to, promotion }`, `EnPassant { from, to }`, `Castle { king, rook }`, `Put { role, to }`.
- `role()`, `from()`, `to()` (for `Castle`, return the rook square), `promotion()`, `to_uci(mode)`.
- `UciMove` variants `Normal { from, to, promotion }`, `Put { role, to }`, `Null`.
- `UciMove::from_ascii`: `0000` -> `Null`; `Q@f7` -> `Put`; four/five characters -> `Normal`; otherwise `Err(UciMoveError)`.
- `UciMove::from_move`: for `Castle` with `CastlingMode::Standard`, emit the king's destination (G-file for kingside, C-file for queenside); with `Chess960`, emit the rook square.
- `UciMove::to_move(&Chess)`: build the UCI string and call `cozy_chess::util::parse_uci_move(&pos.board, &text)`, map the result to a facade `Move`, and verify it is in `pos.legal_moves()`. `Null` is an error; `Put` maps directly.
- `FromStr`, `Display`, `from()`, `to()` (both return `Option<Square>` for `UciMove`).

Create `src/chess/san.rs`:

- `San` and `SanPlus` as described in the Interfaces block.
- `San::to_move`: `Move(mv)` verifies `pos.legal_moves().contains(&mv)`; `Null` is an error; `Raw(text)` normalizes `0-0`/`0-0-0` to `O-O`/`O-O-O`, strips a trailing `e.p.`/`ep`, calls `cozy_chess::util::parse_san_move(&pos.board, &text)`, maps the result to a facade `Move`, and verifies legality.
- `SanPlus::from_move(pos, mv)` calls `cozy_chess::util::display_san_move(&pos.board, cozy_move)` and stores the resulting text.
- `SanPlus::from_ascii`: trim; empty -> `Err`; `--`/`Z0`/`z0` -> `San::Null` with text `"--"`; otherwise `San::Raw(normalized_text)`.
- `SanPlus::to_move` delegates to `san.to_move`.
- `Display for SanPlus` writes `text`.

Modify `src/chess/chess.rs`:

- `legal_moves()`: call `self.board.generate_moves(|moves| { for mv in moves { out.push(self.from_cozy(mv)); } false })` and collect.
- `from_cozy(cozy Move) -> Move`: castling when a king move lands on a rook square in `self.castles()`; en passant when a pawn moves diagonally to `maybe_ep_square()`; otherwise `Normal` with the moving piece's role and the captured piece's role.
- `play(self, mv)`: convert to a cozy `Move` (`Castle` becomes king-to-rook), call `try_play`, and return a new `Chess` with the same mode.
- `play_unchecked(self, mv)`: same conversion, call `play_unchecked`.
- `is_checkmate()`: `self.board.status() == cozy_chess::GameStatus::Won`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test chess_facade`
Expected: PASS for all tests through Task 3.

- [ ] **Step 5: Commit**

```bash
git add src/chess/mod.rs src/chess/chess.rs src/chess/move.rs src/chess/san.rs tests/chess_facade.rs
git commit -m "feat: add Brett move, SAN, and UCI facade"
```

---

### Task 4: Facade FEN, position hash, and `Position` trait

**Files:**
- Create: `src/chess/fen.rs`
- Modify: `src/chess/chess.rs`, `src/chess/mod.rs`
- Test: `tests/chess_facade.rs`

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces:
  - `EnPassantMode::{Legal, Always}`
  - `Position` trait with `board`, `turn`, `castles`, `maybe_ep_square`, `ep_square(EnPassantMode)`, `halfmoves`, `fullmoves`, `legal_moves`, `us`, `play`, `play_unchecked`, `is_checkmate`.
  - `Chess::{ep_square, position_hash}`.
  - `Fen { pub setup: Setup }` with `from_ascii(&[u8])`, `into_setup()`, `into_position(CastlingMode)`, `from_position(&Chess, EnPassantMode)`, `FromStr`, `Display`.
  - `FenError`.

- [ ] **Step 1: Write the failing FEN/hash test**

Append to `tests/chess_facade.rs`:

```rust
use brett::chess::{EnPassantMode, Fen, Position};

#[test]
fn fen_round_trips_start_position() {
    let start = Chess::default();
    let fen = Fen::from_position(&start, EnPassantMode::Legal).to_string();
    assert_eq!(fen, "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
    let parsed = Fen::from_ascii(fen.as_bytes()).unwrap();
    let pos = parsed.into_position(CastlingMode::Standard).unwrap();
    assert_eq!(pos.board().piece_at(Square::E1), Some(Piece::new(Color::White, Role::King)));
}

#[test]
fn fen_resolves_xfen_kq_to_outermost_rooks() {
    let fen = "bbqnnrkr/pppppppp/8/8/8/8/PPPPPPPP/BBQNNRKR w KQkq - 0 1";
    let setup = Fen::from_ascii(fen.as_bytes()).unwrap().into_setup();
    assert!(setup.castling_rights.contains(Square::F1));
    assert!(setup.castling_rights.contains(Square::H1));
    let pos = Fen::from_ascii(fen.as_bytes()).unwrap().into_position(CastlingMode::Chess960).unwrap();
    assert_eq!(Fen::from_position(&pos, EnPassantMode::Legal).to_string(), fen);
}

#[test]
fn fen_writes_file_letters_for_inner_rooks() {
    let mut board = Board::empty();
    board.set_piece_at(Square::G1, Piece::new(Color::White, Role::King));
    board.set_piece_at(Square::A1, Piece::new(Color::White, Role::Rook));
    board.set_piece_at(Square::D1, Piece::new(Color::White, Role::Rook));
    board.set_piece_at(Square::E8, Piece::new(Color::Black, Role::King));
    let mut rights = Bitboard::EMPTY;
    rights |= Bitboard::from_square(Square::D1);
    let setup = Setup { board, turn: Color::White, castling_rights: rights, ep_square: None, halfmoves: 0, fullmoves: NonZeroU32::new(1).unwrap() };
    let pos = Chess::from_setup(setup, CastlingMode::Chess960).unwrap();
    let fen = Fen::from_position(&pos, EnPassantMode::Legal).to_string();
    assert!(fen.contains(" w D "), "expected inner-rook file letter, got {fen}");
}

#[test]
fn position_hash_is_stable_and_uses_legal_ep_only() {
    assert_eq!(Chess::default().position_hash(), 0x33ab_452b_8705_6e0f);

    let legal = "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3";
    let no_ep = "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq - 0 3";
    let legal = Fen::from_ascii(legal.as_bytes()).unwrap().into_position(CastlingMode::Standard).unwrap();
    let no_ep = Fen::from_ascii(no_ep.as_bytes()).unwrap().into_position(CastlingMode::Standard).unwrap();
    assert_ne!(legal.position_hash(), no_ep.position_hash());

    let pinned = "4r1k1/8/8/3pP3/8/8/8/4K3 w - d6 0 1";
    let pinned_no_ep = "4r1k1/8/8/3pP3/8/8/8/4K3 w - - 0 1";
    let pinned = Fen::from_ascii(pinned.as_bytes()).unwrap().into_position(CastlingMode::Standard).unwrap();
    let pinned_no_ep = Fen::from_ascii(pinned_no_ep.as_bytes()).unwrap().into_position(CastlingMode::Standard).unwrap();
    assert_eq!(pinned.ep_square(EnPassantMode::Legal), None);
    assert_eq!(pinned.position_hash(), pinned_no_ep.position_hash());
}

#[test]
fn swap_turn_clears_ep_and_keeps_clocks() {
    let fen = "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3";
    let pos = Fen::from_ascii(fen.as_bytes()).unwrap().into_position(CastlingMode::Standard).unwrap();
    let swapped = pos.swap_turn().unwrap();
    assert_eq!(swapped.turn(), Color::Black);
    assert_eq!(swapped.maybe_ep_square(), None);
    assert_eq!(swapped.halfmoves(), 0);
    assert_eq!(swapped.fullmoves(), NonZeroU32::new(3).unwrap());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test chess_facade`
Expected: FAIL because `Fen`, `EnPassantMode`, and `position_hash` do not exist.

- [ ] **Step 3: Implement FEN, hash, and the Position trait**

Create `src/chess/fen.rs`:

- `Fen { pub setup: Setup }`.
- `Fen::from_ascii`: split into six fields. Parse the board rank by rank from Eighth down to First, accepting `pnbrqkPNBRQK` and digits 1–8. Parse `w`/`b`. Parse castling as `-` or a sequence of `KQkq` and `A-Ha-h`; `K`/`Q` resolve to the outermost rook on the king's side of that color's king on its back rank, where kingside means a higher file number and queenside means a lower file number for both colors. File letters resolve to the rook on that file and back rank; ignore rights without a matching rook. Parse EP as `-` or a square. Parse halfmoves as `u32`. Parse fullmoves as `NonZeroU32` with `0` mapped to `1`.
- `Fen::into_setup` returns the stored `Setup`.
- `Fen::into_position(mode)` calls `Chess::from_setup(self.setup, mode)`.
- `Fen::from_position(pos, ep_mode)` calls `pos.to_setup(ep_mode)` and wraps it.
- `Display`: board, turn, X-FEN castling, EP, halfmoves, fullmoves. For castling, consider all rooks of each color on that color's back rank; emit `K`/`Q` only when the right rook is the outermost rook on that side of the king, otherwise emit its file letter (uppercase for White, lowercase for Black). Emit `-` when there are no rights.
- `FromStr` delegates to `from_ascii`; `FenError` implements `Display` and `std::error::Error`.

Modify `src/chess/chess.rs`:

- Add `EnPassantMode { Legal, Always }`.
- `Chess::to_setup(ep_mode)`: copy the board, turn, castling rights from `self.castles()`, `ep_square(ep_mode)`, halfmoves, and fullmoves.
- `Chess::maybe_ep_square()`: map `board.en_passant()` to the correct rank for the side to move.
- `Chess::ep_square(mode)`: for `Always`, return `maybe_ep_square()`; for `Legal`, return it only when `legal_moves()` contains an `EnPassant` move.
- `Chess::position_hash()`: use `board.hash()` when `ep_square(Legal).is_some()`, otherwise `board.hash_without_ep()`.
- `Chess::swap_turn()`: `to_setup(Always)`, `setup.swap_turn()`, `from_setup(setup, mode)`.
- Define the `Position` trait with the methods listed in the Interfaces block and implement it for `Chess`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test chess_facade`
Expected: PASS for all facade tests.

- [ ] **Step 5: Commit**

```bash
git add src/chess/mod.rs src/chess/chess.rs src/chess/fen.rs tests/chess_facade.rs
git commit -m "feat: add Brett FEN, position hash, and Position trait"
```

---

### Task 5: Brett-owned streaming PGN reader

**Files:**
- Create: `src/pgn/reader.rs`
- Modify: `src/pgn.rs`
- Test: `tests/pgn_reader.rs`

**Interfaces:**
- Consumes: `San`, `SanPlus`, `San::Raw`, `San::Null` from Task 3.
- Produces:
  - `pgn::reader::{Reader, Visitor}`
  - `Reader::new(R)`, `Reader::stream_position() -> u64`, `Reader::read_game(&mut V) -> io::Result<Option<V::Output>>`
  - `Visitor` with `Tags`, `Movetext`, `Output`, `begin_tags`, `tag(&mut self, tags, name: &str, value: &str)`, `begin_movetext`, `wants_movetext() -> bool` (default `true`), `san(&mut self, mt, SanPlus)`, `nag(&mut self, mt, u8)`, `comment(&mut self, mt, &str)`, `begin_variation`, `end_variation`, `end_game`.

- [ ] **Step 1: Write the failing parser test**

Create `tests/pgn_reader.rs` with a recording visitor and these tests:

```rust
use std::io::Cursor;

use brett::chess::{San, SanPlus};
use brett::pgn::reader::{Reader, Visitor};
use std::ops::ControlFlow;

#[derive(Default)]
struct Recorder {
    events: Vec<Event>,
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Tag(String, String),
    San(String),
    Null,
    Nag(u8),
    Comment(String),
    Open,
    Close,
}

impl Visitor for Recorder {
    type Tags = Vec<(String, String)>;
    type Movetext = Vec<Event>;
    type Output = (Vec<(String, String)>, Vec<Event>);

    fn begin_tags(&mut self) -> ControlFlow<Self::Output, Self::Tags> {
        ControlFlow::Continue(Vec::new())
    }

    fn tag(&mut self, tags: &mut Self::Tags, name: &str, value: &str) -> ControlFlow<Self::Output> {
        tags.push((name.to_string(), value.to_string()));
        self.events.push(Event::Tag(name.to_string(), value.to_string()));
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, _tags: Self::Tags) -> ControlFlow<Self::Output, Self::Movetext> {
        ControlFlow::Continue(Vec::new())
    }

    fn san(&mut self, mt: &mut Self::Movetext, san: SanPlus) -> ControlFlow<Self::Output> {
        match san.san {
            San::Null => mt.push(Event::Null),
            _ => mt.push(Event::San(san.text)),
        }
        ControlFlow::Continue(())
    }

    fn nag(&mut self, mt: &mut Self::Movetext, nag: u8) -> ControlFlow<Self::Output> {
        mt.push(Event::Nag(nag));
        ControlFlow::Continue(())
    }

    fn comment(&mut self, mt: &mut Self::Movetext, comment: &str) -> ControlFlow<Self::Output> {
        mt.push(Event::Comment(comment.to_string()));
        ControlFlow::Continue(())
    }

    fn begin_variation(&mut self, mt: &mut Self::Movetext) -> ControlFlow<Self::Output> {
        mt.push(Event::Open);
        ControlFlow::Continue(())
    }

    fn end_variation(&mut self, mt: &mut Self::Movetext) -> ControlFlow<Self::Output> {
        mt.push(Event::Close);
        ControlFlow::Continue(())
    }

    fn end_game(&mut self, mt: Self::Movetext) -> Self::Output {
        (Vec::new(), mt)
    }
}

fn read_all(pgn: &[u8]) -> Vec<(Vec<(String, String)>, Vec<Event>)> {
    let mut reader = Reader::new(Cursor::new(pgn));
    let mut visitor = Recorder::default();
    let mut games = Vec::new();
    while let Some(game) = reader.read_game(&mut visitor).unwrap() {
        games.push(game);
    }
    games
}

#[test]
fn reads_tags_moves_nags_comments_and_variations() {
    let pgn = br#"[Event "Test"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 e5 2. Nf3 (2. Bc4 Nc6) Nc6 3. Bb5 a6 $1 {main line} 1-0
"#;
    let games = read_all(pgn);
    assert_eq!(games.len(), 1);
    let (_, events) = &games[0];
    assert!(events.contains(&Event::San("e4".into())));
    assert!(events.contains(&Event::San("Nf3".into())));
    assert!(events.contains(&Event::Open));
    assert!(events.contains(&Event::Close));
    assert!(events.contains(&Event::Nag(1)));
    assert!(events.contains(&Event::Comment("main line".into())));
}

#[test]
fn tags_support_escaped_quotes_and_lossy_utf8() {
    let pgn = b"[Event \"A \\\"quote\\\" and \\\\ slash\"]\n[White \"Jos\xe9\"]\n\n1. e4 *\n";
    let games = read_all(pgn);
    assert_eq!(games[0].0[0], ("Event".to_string(), "A \"quote\" and \\ slash".to_string()));
    assert_eq!(games[0].0[1].1, "Jos\u{fffd}");
}

#[test]
fn bracket_inside_comment_is_not_a_new_game() {
    let pgn = br#"[Event "One"]
[Result "*"]

{ Comment with [Event "Not a game"] and a blank line.

Still the same comment. } 1. e4 *

[Event "Two"]
[Result "*"]

1. d4 *
"#;
    let games = read_all(pgn);
    assert_eq!(games.len(), 2);
}

#[test]
fn stream_positions_cover_each_game() {
    let pgn = b"[Event \"One\"]\n\n1. e4 *\n\n[Event \"Two\"]\n\n1. d4 *\n";
    let mut reader = Reader::new(Cursor::new(pgn));
    let mut visitor = Recorder::default();
    let first_start = reader.stream_position();
    assert!(reader.read_game(&mut visitor).unwrap().is_some());
    let first_end = reader.stream_position();
    assert!(reader.read_game(&mut visitor).unwrap().is_some());
    let second_end = reader.stream_position();
    assert!(reader.read_game(&mut visitor).unwrap().is_none());
    assert_eq!(first_start, 0);
    assert!(first_end > first_start);
    assert!(second_end > first_end);
    let first = &pgn[first_start as usize..first_end as usize];
    let second = &pgn[first_end as usize..second_end as usize];
    assert!(first.windows(5).any(|w| w == b"One\""));
    assert!(second.windows(5).any(|w| w == b"Two\""));
}

#[test]
fn header_only_visitor_advances_to_next_game() {
    #[derive(Default)]
    struct Headers;
    impl Visitor for Headers {
        type Tags = Vec<(String, String)>;
        type Movetext = Vec<(String, String)>;
        type Output = Vec<(String, String)>;
        fn begin_tags(&mut self) -> ControlFlow<Self::Output, Self::Tags> { ControlFlow::Continue(Vec::new()) }
        fn tag(&mut self, tags: &mut Self::Tags, name: &str, value: &str) -> ControlFlow<Self::Output> {
            tags.push((name.to_string(), value.to_string()));
            ControlFlow::Continue(())
        }
        fn begin_movetext(&mut self, tags: Self::Tags) -> ControlFlow<Self::Output, Self::Movetext> { ControlFlow::Continue(tags) }
        fn wants_movetext(&self) -> bool { false }
        fn end_game(&mut self, tags: Self::Movetext) -> Self::Output { tags }
    }

    let pgn = b"[Event \"One\"]\n\n1. e4 {a} (1. d4) e5 *\n\n[Event \"Two\"]\n\n1. d4 *\n";
    let mut reader = Reader::new(Cursor::new(pgn));
    let mut visitor = Headers;
    let first = reader.read_game(&mut visitor).unwrap().unwrap();
    assert_eq!(first[0].1, "One");
    let second = reader.read_game(&mut visitor).unwrap().unwrap();
    assert_eq!(second[0].1, "Two");
    assert!(reader.read_game(&mut visitor).unwrap().is_none());
}

#[test]
fn null_moves_results_and_glued_move_numbers() {
    let pgn = b"[Event \"Null\"]\n\n1.e4 e5 2. Nf3 -- 3. Nxe5 1-0\n";
    let games = read_all(pgn);
    let (_, events) = &games[0];
    assert_eq!(events.iter().filter(|e| **e == Event::Null).count(), 1);
    assert!(events.contains(&Event::San("e4".into())));
    assert!(events.contains(&Event::San("Nxe5".into())));
}

#[test]
fn unclosed_variation_is_closed_at_eof() {
    let pgn = b"[Event \"Trunc\"]\n\n1. e4 (1. d4 d5\n";
    let games = read_all(pgn);
    let (_, events) = &games[0];
    assert!(events.contains(&Event::Open));
    assert!(events.contains(&Event::Close));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test pgn_reader`
Expected: FAIL because `brett::pgn::reader` does not exist.

- [ ] **Step 3: Implement the streaming reader**

Create `src/pgn/reader.rs` with:

- `Reader<R: Read>` holding a 64 KiB `Vec<u8>` buffer, `pos`, `stream_start: u64`, `eof: bool`, `line_start: bool`.
- `ensure(n)` compacts consumed bytes (`stream_start += pos; pos = 0`) and reads more until at least `n` bytes are available or EOF. `stream_position()` returns `stream_start + pos as u64`; never include read-ahead.
- `peek_byte`, `peek_n`, `next_byte`, `skip_whitespace`, `skip_line`, `skip_comment`.
- BOM skip only at position 0.
- `read_game`: skip leading whitespace, `%` escape lines, and leading comments; stop at `[`; call `begin_tags`; parse consecutive tag lines until a blank line or a non-`[` token; call `begin_movetext`; parse movetext; call `end_game`. `ControlFlow::Break` from any callback returns that output after skipping to the next game boundary.
- `read_tag`: `[Name "Value"]` with `\"` and `\\` escapes, UTF-8 lossy values, malformed tags tolerated.
- `parse_movetext`: skip whitespace; `{...}` and `;...` comments; `$n` NAGs; `!`, `?`, `!!`, `??`, `!?`, `?!` NAGs (1–6); nested `(`/`)` with `begin_variation`/`end_variation`; results `1-0`, `0-1`, `1/2-1/2`, `*`; move numbers (`1.`, `1...`, `1.e4`); null moves `--`, `Z0`, `z0`; `0-0`/`0-0-0` normalized to `O-O`/`O-O-O`; SAN tokens emitted as `SanPlus { san: San::Raw(text), text }` with trailing NAG glyphs split off. Tokens `e.p.`/`ep` are ignored.
- Stop at `[` only when `line_start` is true; do not consume it. Close any open variations before `end_game`. If `wants_movetext()` is false, tokenize and skip without calling the movetext callbacks.
- `Visitor` trait exactly as in the Interfaces block, using `std::ops::ControlFlow` and `std::io::Result`.

Modify `src/pgn.rs` to add `pub mod reader;` and `pub use reader::{Reader, Visitor};` while keeping the existing PGN export functions.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test pgn_reader`
Expected: PASS for all parser tests.

- [ ] **Step 5: Commit**

```bash
git add src/pgn.rs src/pgn/reader.rs tests/pgn_reader.rs
git commit -m "feat: add Brett streaming PGN reader"
```

---

### Task 6: Migrate `src/domain.rs` to the facade and new reader

**Files:**
- Modify: `src/domain.rs`, `tests/pgn_tree.rs`
- Test: `tests/pgn_tree.rs`, `src/domain.rs` unit tests

**Interfaces:**
- Consumes: `crate::chess::{CastlingMode, Chess, Color, EnPassantMode, Fen, Move, Position, Role, San, SanPlus, Square, UciMove}` and `crate::pgn::reader::{Reader, Visitor}`.
- Produces: unchanged public `domain` API (`Game`, `Node`, `Ply`, `parse_pgn`, `parse_pgn_file`, `last_move_squares`, `legal_targets`, `pick_move`, etc.).

- [ ] **Step 1: Migrate the test imports and write the failing assertions**

In `tests/pgn_tree.rs`, replace `shakmaty::Color` with `brett::chess::Color` and add `use brett::chess::{Move, Position, Role, Square};` where needed. Keep the existing behavior assertions. Add:

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test pgn_tree`
Expected: FAIL to compile because `src/domain.rs` still imports `pgn_reader` and `shakmaty`.

- [ ] **Step 3: Migrate `src/domain.rs`**

- Replace the imports with `crate::chess::{CastlingMode, Chess, Color, EnPassantMode, Fen, Move, Position, Role, San, SanPlus, Square, UciMove}` and `crate::pgn::reader::{Reader, Visitor}`.
- `parse_pgn` uses `Reader::new(std::io::Cursor::new(bytes))` and loops on `read_game`.
- `TreeVisitor` implements the new `Visitor`:
  - `type Tags = Vec<(String, String)>`, `Movetext = Builder`, `Output = Game`.
  - `tag(&mut self, tags, name: &str, value: &str)`.
  - `nag(&mut self, mt, nag: u8)`.
  - `comment(&mut self, mt, comment: &str)`.
  - `begin_variation`/`end_variation` return `ControlFlow::Continue(())`.
- `Builder::new` uses `Fen::from_ascii(...).into_position(CastlingMode::Standard)`.
- `Builder::push` keeps the null-move branch, using `Fen::from_position(&pos, EnPassantMode::Legal).into_setup()`, `setup.swap_turn()`, `setup.halfmoves = 0`, and the Black fullmove increment. `Chess::from_setup(setup, CastlingMode::Standard)`.
- `add_move` takes `crate::chess::Move`.
- `last_move_squares`, `castle_king_destination`, `legal_targets`, `pick_move` use facade types; `Move::Castle { king, rook }` still returns the rook square from `to()`.
- Replace `shakmaty::File::G/C` with `crate::chess::File::G/C`.
- Update the domain unit test FEN import to `crate::chess::Fen`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test pgn_tree`
Run: `cargo test --lib domain`
Expected: PASS for both, including the existing opera/variation/null-move tests and the new illegal/truncated tests.

- [ ] **Step 5: Commit**

```bash
git add src/domain.rs tests/pgn_tree.rs
git commit -m "refactor: migrate domain game tree to Brett chess and PGN reader"
```

---

### Task 7: Migrate `src/library.rs` and remove `pgn-reader` from indexing

**Files:**
- Modify: `src/library.rs`
- Test: `tests/pgn_index.rs`

**Interfaces:**
- Consumes: `crate::pgn::reader::{Reader, Visitor}`.
- Produces: unchanged `PgnLibrary` API and byte-range semantics.

- [ ] **Step 1: Add the failing byte-range assertion**

In `tests/pgn_index.rs`, add a test that indexes a file whose first game has a comment containing a fake tag and asserts each game loads correctly:

```rust
#[test]
fn comment_with_fake_tag_does_not_split_games() {
    let path = write_temp_pgn("fake-tag", THREE_GAMES);
    let library = PgnLibrary::build(&path).unwrap();
    assert_eq!(library.len(), 3);
    assert_eq!(library.load(0).unwrap().white(), "Alice");
    assert_eq!(library.load(1).unwrap().white(), "Carol");
    assert_eq!(library.load(2).unwrap().white(), "Eve");
    std::fs::remove_file(path).unwrap();
}
```

`THREE_GAMES` already contains the fake `[Event "Not a game"]` inside a comment.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test pgn_index`
Expected: FAIL to compile because `src/library.rs` still imports `pgn_reader`.

- [ ] **Step 3: Migrate `src/library.rs`**

- Replace `pgn_reader::{RawTag, Reader, Visitor}` with `crate::pgn::reader::{Reader, Visitor}`.
- Update the module doc: byte ranges come from the Brett parser's logical `stream_position()`, not `pgn-reader`'s `Seek`.
- `PgnLibrary::build`: `Reader::new(BufReader::new(file))`; `stream_position()` is infallible, so remove the `.context("querying PGN stream position")` calls.
- `HeaderVisitor`: `tag(&mut self, tags, name: &str, value: &str)`; keep `wants_movetext() -> false`; `begin_movetext` returns the tags; `end_game` returns them.
- Keep the `end <= start` progress guard.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test pgn_index`
Run: `cargo test --test local_fixtures`
Expected: PASS. The local-fixture test is a no-op when the git-ignored files are absent.

- [ ] **Step 5: Commit**

```bash
git add src/library.rs tests/pgn_index.rs
git commit -m "refactor: migrate PGN library indexing to Brett reader"
```

---

### Task 8: Polyglot Zobrist hash and `.bin` book migration

**Files:**
- Create: `src/book/polyglot.rs`, `src/book/polyglot_keys.rs`
- Modify: `src/book.rs`, `src/book/bkt.rs`
- Test: `tests/polyglot.rs`

**Interfaces:**
- Consumes: `crate::chess::{Chess, Color, Piece, Position, Role, Square}`.
- Produces: `crate::book::polyglot::position_key(&Chess) -> u64`, using the standard 781-entry Polyglot table.

- [ ] **Step 1: Write the failing Polyglot vector test**

Create `tests/polyglot.rs`:

```rust
use brett::book::polyglot::position_key;
use brett::chess::{CastlingMode, Chess, EnPassantMode, Fen, Position, SanPlus};

fn position(fen: &str) -> Chess {
    Fen::from_ascii(fen.as_bytes())
        .unwrap()
        .into_position(CastlingMode::Standard)
        .unwrap()
}

fn after(mut pos: Chess, sans: &[&str]) -> Chess {
    for san in sans {
        let san = SanPlus::from_ascii(san.as_bytes()).unwrap();
        let mv = san.san.to_move(&pos).unwrap();
        pos = pos.play(mv).unwrap();
    }
    pos
}

#[test]
fn polyglot_start_position_key() {
    assert_eq!(position_key(&Chess::default()), 0x463b_9618_1691_fc9c);
    assert_eq!(position_key(&after(Chess::default(), &["e4"])), 0x823c_9b50_fd11_4196);
    assert_eq!(position_key(&after(Chess::default(), &["e4", "c5"])), 0x644d_4afe_0256_4aeb);
}

#[test]
fn polyglot_kiwipete_and_castling_keys() {
    let kiwipete = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
    assert_eq!(position_key(&position(kiwipete)), 0xc3ce_103f_01d1_5e1d);

    let castles = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
    assert_eq!(position_key(&position(castles)), 0xfda2_39cc_692a_6053);

    let bare = "8/2P5/8/8/8/8/8/K6k w - - 0 1";
    assert_eq!(position_key(&position(bare)), 0xce58_c3b3_356f_fdac);
}

#[test]
fn polyglot_ep_includes_adjacent_pawn_regardless_of_legality() {
    let legal = "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3";
    assert_eq!(position_key(&position(legal)), 0x8260_57c0_f644_3c7c);

    let pinned = "4r1k1/8/8/3pP3/8/8/8/4K3 w - d6 0 1";
    let no_ep = "4r1k1/8/8/3pP3/8/8/8/4K3 w - - 0 1";
    assert_eq!(position_key(&position(pinned)), 0x7ddd_0eef_bfc8_435a);
    assert_eq!(position_key(&position(no_ep)), 0x6144_d03c_8370_d3fb);
    assert_eq!(position(pinned).ep_square(EnPassantMode::Legal), None);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test polyglot`
Expected: FAIL because `brett::book::polyglot` does not exist.

- [ ] **Step 3: Implement the Polyglot hash**

Create `src/book/polyglot_keys.rs` by copying the 781-entry `POLYGLOT_RANDOM_ARRAY` from the MIT-licensed `gigachess` 0.1.9 crate (local registry path pattern `~/.cargo/registry/src/*/gigachess-0.1.9/src/polyglot_keys.rs`). Keep the crate's MIT notice in `THIRD_PARTY_NOTICES.md` in Task 13. Do not copy the table from python-chess or any GPL implementation.

Create `src/book/polyglot.rs`:

- XOR `table[64 * piece_index + square]` for every occupied square, where `piece_index = 2 * role_index + color_index` with role order Pawn, Knight, Bishop, Rook, Queen, King and color order White, Black.
- XOR `table[768]` for White kingside, `table[769]` White queenside, `table[770]` Black kingside, `table[771]` Black queenside, using `Castles::has(color, side)`.
- XOR `table[772 + ep_file]` when `maybe_ep_square()` is set and the side to move has a pawn on the rank directly behind the EP square (rank 5 for White, rank 4 for Black) on an adjacent file. Legality of the capture is irrelevant.
- XOR `table[780]` when Black is to move.

Modify `src/book.rs`:

- Add `pub mod polyglot;` and `mod polyglot_keys;` (or keep `polyglot_keys` private inside `polyglot`). The integration test imports `brett::book::polyglot::position_key`, so `polyglot` itself must be public.
- Replace `pos.zobrist_hash::<Zobrist64>(EnPassantMode::Legal).0` with `polyglot::position_key(pos)`.
- Replace `shakmaty::zobrist::Zobrist64` and `shakmaty::{...}` imports with `crate::chess::{...}`.
- Keep `pos.ep_square(EnPassantMode::Legal)` for CTG.
- Update `has_kingside`/`has_queenside` to take `crate::chess::Bitboard`.
- Update `decode_polyglot_move`, `move_matches`, and `castle_destination` to facade types.
- Update the module doc: Polyglot lookup uses Brett's own table, not `shakmaty`.

Modify `src/book/bkt.rs` imports to `crate::chess::{Chess, Color, Position, Role, Square, UciMove}`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test polyglot`
Run: `cargo test --lib book`
Expected: PASS. The book sample tests are no-ops when `BRETT_BOOK_SAMPLE`, `BRETT_CTG_SAMPLE`, `BRETT_ABK_SAMPLE`, or `BRETT_BKT_SAMPLE` are unset.

- [ ] **Step 5: Commit**

```bash
git add src/book.rs src/book/bkt.rs src/book/polyglot.rs src/book/polyglot_keys.rs tests/polyglot.rs
git commit -m "feat: add Brett Polyglot hash and migrate book readers"
```

---

### Task 9: Migrate the position index and bump its version

**Files:**
- Modify: `src/position.rs`, `tests/position_index.rs`
- Test: `tests/position_index.rs`, `src/position.rs` unit tests

**Interfaces:**
- Consumes: `crate::chess::{CastlingMode, Chess, EnPassantMode, Fen, Move, Position, Role, SanPlus, Square, UciMove}` and `crate::pgn::reader::{Reader, Visitor}`.
- Produces: `position_key(&Chess) -> u64` based on `Chess::position_hash()`; index `VERSION = 2`.

- [ ] **Step 1: Update the failing key assertion**

In `src/position.rs`, change the unit test expected key to `0x33ab_452b_8705_6e0f`. In `tests/position_index.rs`, replace `use pgn_reader::SanPlus;` with `use brett::chess::SanPlus;` and `use shakmaty::{Chess, Position};` with `use brett::chess::{Chess, Position};`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test position_index`
Expected: FAIL to compile because `src/position.rs` still imports `pgn_reader` and `shakmaty`, and the key assertion still expects the old Polyglot key.

- [ ] **Step 3: Migrate `src/position.rs`**

- Set `const VERSION: u32 = 2;`.
- Replace `pgn_reader::{RawTag, Reader, SanPlus, Visitor}` with `crate::pgn::reader::{Reader, SanPlus, Visitor}`.
- Replace `shakmaty::{...}` with `crate::chess::{CastlingMode, Chess, EnPassantMode, Fen, Move, Position, Role, Square, UciMove}`; remove `Zobrist64`.
- `position_key(pos)` returns `pos.position_hash()`.
- `encode_move`/`decode_move`: adapt to facade `Move`; `castle_destination` uses `crate::chess::File::G/C`.
- `GameState::new`: `Fen::from_ascii(...).into_position(CastlingMode::Standard)`.
- `IndexVisitor`: new `Visitor` signatures; `nag` not needed; `tag(name: &str, value: &str)`.
- Keep the `seen` set and `max_plies` behavior.
- Update the module doc: the key is Brett's stable cozy-based hash, and old version-1 caches are ignored.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test position_index`
Run: `cargo test --lib position`
Expected: PASS. `PositionIndex::open` on an old version-1 cache returns the existing "stale/invalid" error path; no migration code is added.

- [ ] **Step 5: Commit**

```bash
git add src/position.rs tests/position_index.rs
git commit -m "refactor: migrate position index to Brett chess and bump version"
```

---

### Task 10: Migrate classic CBH and native SCID

**Files:**
- Modify: `src/cbh/moves.rs`, `src/cbh/tests.rs`, `src/scid.rs`, `src/scid/writer.rs`, `tests/scid.rs`
- Test: `cargo test --lib cbh`, `cargo test --test scid`

**Interfaces:**
- Consumes: facade board/setup/move/FEN types.
- Produces: unchanged CBH/SCID public behavior and byte formats.

- [ ] **Step 1: Update the test imports and add a Chess960 castling assertion**

In `src/cbh/tests.rs`, replace `use shakmaty::{Chess, Position, Role};` with `use crate::chess::{Chess, Position, Role};` and replace `shakmaty::Color::White`/`shakmaty::CastlingSide::KingSide` with `crate::chess::...`. In `tests/scid.rs`, replace any `shakmaty` imports with `brett::chess` equivalents.

Add to `src/cbh/tests.rs`:

```rust
#[test]
fn chess960_start_keeps_outer_rook_castling_rights() {
    let mut board = crate::chess::Board::empty();
    for (square, role) in [
        (crate::chess::Square::A1, Role::Rook),
        (crate::chess::Square::B1, Role::Knight),
        (crate::chess::Square::C1, Role::Bishop),
        (crate::chess::Square::D1, Role::Queen),
        (crate::chess::Square::E1, Role::King),
        (crate::chess::Square::F1, Role::Bishop),
        (crate::chess::Square::G1, Role::Knight),
        (crate::chess::Square::H1, Role::Rook),
        (crate::chess::Square::E8, Role::King),
    ] {
        let color = if square.rank() == crate::chess::Rank::Eighth { crate::chess::Color::Black } else { crate::chess::Color::White };
        board.set_piece_at(square, crate::chess::Piece::new(color, role));
    }
    let mut rights = crate::chess::Bitboard::EMPTY;
    rights |= crate::chess::Bitboard::from_square(crate::chess::Square::A1);
    rights |= crate::chess::Bitboard::from_square(crate::chess::Square::H1);
    let setup = crate::chess::Setup {
        board,
        turn: crate::chess::Color::White,
        castling_rights: rights,
        ep_square: None,
        halfmoves: 0,
        fullmoves: std::num::NonZeroU32::new(1).unwrap(),
    };
    let pos = Chess::from_setup(setup, crate::chess::CastlingMode::Chess960).unwrap();
    assert!(pos.castles().has(crate::chess::Color::White, crate::chess::CastlingSide::KingSide));
    assert!(pos.castles().has(crate::chess::Color::White, crate::chess::CastlingSide::QueenSide));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib cbh`
Run: `cargo test --test scid`
Expected: FAIL to compile because these modules still import `shakmaty`.

- [ ] **Step 3: Migrate CBH and SCID**

- `src/cbh/moves.rs`: replace imports with `crate::chess::{Bitboard, Board, CastlingMode, CastlingSide, Chess, Color, EnPassantMode, Fen, Move, Piece, Position, Role, Setup, Square, UciMove}`. Remove `FromSetup`; call `Chess::from_setup` directly. `Board::empty`, `set_piece_at`, `Setup`, `Bitboard`, `Castles::rook`, `pos.us()`, `pos.maybe_ep_square()`, `Move` construction, and `UciMove::from_move` all use the facade.
- `src/cbh/tests.rs`: facade imports as above.
- `src/scid.rs`: replace `shakmaty::fen::Fen` and `shakmaty::{...}` with facade imports. `Fen::from_position`, `Fen::from_ascii`, `into_position`, `Move` matching, `Square::from_coords`, `pos.us()`, `pos.castles()`, `pos.board().piece_at` all use the facade.
- `src/scid/writer.rs`: replace `shakmaty::fen::Fen`, `shakmaty::uci::UciMove`, and `shakmaty::{...}` with facade imports. `UciMove::from_ascii`, `to_move`, `Move::{role, from, to, promotion}`, `Fen::from_position` all use the facade.
- `tests/scid.rs`: facade imports.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib cbh`
Run: `cargo test --test scid`
Expected: PASS, including the synthetic classic-CBH fixtures and the SCID round-trip tests.

- [ ] **Step 5: Commit**

```bash
git add src/cbh/moves.rs src/cbh/tests.rs src/scid.rs src/scid/writer.rs tests/scid.rs
git commit -m "refactor: migrate CBH and SCID readers/writers to Brett chess"
```

---

### Task 11: Migrate the remaining library modules

**Files:**
- Modify: `src/pgn.rs`, `src/cloud.rs`, `src/recent.rs`, `src/players.rs`, `src/review.rs`, `src/settings.rs`
- Test: `cargo test --lib`, `cargo test --test board_drag`

**Interfaces:**
- Consumes: facade `Chess`, `Color`, `Fen`, `Position`, `SanPlus`, `UciMove`, `EnPassantMode`.
- Produces: unchanged module APIs.

- [ ] **Step 1: Update the imports and the review FEN call**

Replace `shakmaty::fen::Fen`, `shakmaty::san::SanPlus`, `shakmaty::uci::UciMove`, and `shakmaty::{...}` with `crate::chess::{...}` in each file:

- `src/pgn.rs`: `Fen`, `Chess`, `Color`, `EnPassantMode`.
- `src/cloud.rs`: `Chess`, `Color`, `SanPlus`, `UciMove`; change `white_score(&self, turn: crate::chess::Color)`.
- `src/recent.rs`: `UciMove`.
- `src/players.rs`: `Color`, `UciMove`, `Position`, and test-only `Chess`, `SanPlus`.
- `src/review.rs`: `Color`, `Fen`, `Position`, `EnPassantMode`; replace `shakmaty::fen::Fen::from_position` with `Fen::from_position`.
- `src/settings.rs`: `crate::chess::Color`.

Update the `src/players.rs` tests that use `shakmaty::Chess::default()` and `shakmaty::san::SanPlus::from_ascii` to `crate::chess::Chess`/`SanPlus`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib`
Expected: FAIL to compile because these modules still import `shakmaty`.

- [ ] **Step 3: Fix any API mismatches**

The facade deliberately mirrors the used `shakmaty` surface. If a call site needs a method not yet on the facade, add it to the facade rather than reaching into `cozy-chess` from the call site. Likely candidates: `Fen::from_position`, `UciMove::from_ascii`, `SanPlus::from_move`, `Chess::default`, `Position::ep_square`, and `Position::board`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib`
Run: `cargo test --test board_drag`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/pgn.rs src/cloud.rs src/recent.rs src/players.rs src/review.rs src/settings.rs
git commit -m "refactor: migrate library modules to Brett chess"
```

---

### Task 12: Migrate the application and UI

**Files:**
- Modify: `src/app.rs`, `src/ui/board.rs`, `src/ui/pieces.rs`
- Test: `cargo test --all-targets`, `cargo clippy --all-targets -- -D warnings`

**Interfaces:**
- Consumes: facade types.
- Produces: unchanged UI behavior, including clocks, board drag, review, and engine tabs.

- [ ] **Step 1: Update imports and the About text**

- `src/app.rs`: replace the four `shakmaty` imports with `crate::chess::{CastlingMode, Chess, Color, EnPassantMode, Fen, Move, Position, SanPlus, Square, UciMove}`. Change `prep_tree` and `fen_string` signatures to `&crate::chess::Chess`. Change the About text from "Rules and SAN: shakmaty. PGN: pgn-reader." to "Rules and SAN: cozy-chess. PGN: Brett's own streaming parser."
- `src/ui/board.rs`: replace `shakmaty::{Chess, Color, File, Position, Rank, Square}` with `crate::chess::{Chess, Color, File, Position, Rank, Square}`.
- `src/ui/pieces.rs`: replace `shakmaty::{Color, Piece, Role}` with `crate::chess::{Color, Piece, Role}`.

- [ ] **Step 2: Run the build to verify it fails**

Run: `cargo check --all-targets`
Expected: FAIL to compile because `src/app.rs` still imports `shakmaty`.

- [ ] **Step 3: Fix any UI API mismatches**

Keep the facade as the only rules boundary. Do not add `cozy-chess` imports to `src/app.rs` or `src/ui/`. If a call site needs `Move::to_uci`, `UciMove::from_move`, `Fen::from_position`, or `Position::board`, use the facade signatures from Tasks 2–4.

- [ ] **Step 4: Run the tests and lints to verify they pass**

Run: `cargo test --all-targets`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: PASS. The `#[ignore]`-marked large-corpus tests remain skipped.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/ui/board.rs src/ui/pieces.rs
git commit -m "refactor: migrate application and UI to Brett chess"
```

---

### Task 13: Remove the GPL crates, update notices/docs, and verify

**Files:**
- Modify: `Cargo.toml`, `examples/position_report.rs`, `examples/predictor_eval.rs`, `THIRD_PARTY_NOTICES.md`, `README.md`, `CONTRIBUTING.md`, `docs/POSITION_INDEX.md`, `docs/GPL_OPTIONS.md`, `docs/BACKLOG.md`
- Test: full workspace

**Interfaces:**
- Consumes: everything.
- Produces: a repository with no `shakmaty` or `pgn-reader` dependency and documentation that says so.

- [ ] **Step 1: Migrate the examples**

- `examples/position_report.rs`: `use shakmaty::Chess;` -> `use brett::chess::Chess;`.
- `examples/predictor_eval.rs`: `use shakmaty::Position;` -> `use brett::chess::Position;`, `use shakmaty::uci::UciMove;` -> `use brett::chess::UciMove;`, and `shakmaty::Color::White/Black` -> `brett::chess::Color::White/Black`.

- [ ] **Step 2: Remove the dependencies**

Delete `shakmaty = "0.30"` and `pgn-reader = "0.29"` from `Cargo.toml`. Run `cargo check --all-targets` so `Cargo.lock` drops the unused packages.

- [ ] **Step 3: Update the licence notices**

In `THIRD_PARTY_NOTICES.md`:

- Remove the "GPL-licensed chess and PGN crates" section.
- Add `cozy-chess` 0.3.4 and `cozy-chess-types` 0.2.2 as MIT, with their upstream links (`https://github.com/analog-hors/cozy-chess`).
- Add a short note that the 781-entry Polyglot random array in `src/book/polyglot_keys.rs` is transcribed from the MIT-licensed `gigachess` 0.1.9 crate, and reproduce the MIT notice.
- Keep the existing ICCF, SCID, CBH, photo, and `cbvault-format` notices unchanged.

In `README.md`:

- Update the "Permissive dependency path" bullet to say the replacement is complete.
- Replace the "Important: Brett currently links shakmaty and pgn-reader" section with a statement that Brett links only MIT/permissive dependencies, with `cozy-chess` for rules and a Brett-owned PGN parser.

In `CONTRIBUTING.md`:

- Change the stack sentence from `shakmaty` to `cozy-chess` plus the Brett parser.
- Change the MSRV note: Rust 1.97 or newer (edition 2024; `eframe`/`egui` set the floor).
- Replace the "current build links shakmaty and pgn-reader under GPL" paragraph with the MIT-only statement.

- [ ] **Step 4: Update the design/status docs**

- `docs/POSITION_INDEX.md`: replace "Zobrist key from `shakmaty`" with "Brett position hash from `cozy-chess`"; replace "streaming `pgn-reader` visitor" with "Brett streaming PGN visitor"; note `VERSION = 2`.
- `docs/GPL_OPTIONS.md`: add a "Status: implemented" note at the top of the staged sequence, and mark the unsent upstream licence request as obsolete. Do not send it.
- `docs/BACKLOG.md`: mark item 17 as done with the completion date and the new stack (`cozy-chess` 0.3.4 + Brett PGN reader). Keep the item 22 clock-usage entry unchanged.

- [ ] **Step 5: Run the full verification**

Run:

```bash
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo tree -i shakmaty
cargo tree -i pgn-reader
```

Expected:

- `cargo test --all-targets` PASS.
- `cargo clippy --all-targets -- -D warnings` PASS with no warnings.
- `cargo fmt --all -- --check` PASS.
- Both `cargo tree -i` commands fail with "package ID specification ... did not match any packages" or print nothing; either way, no GPL crate is in the graph.
- `grep -RIn "shakmaty\|pgn-reader\|pgn_reader" --exclude-dir=target --exclude=Cargo.lock .` returns only historical mentions in `docs/GPL_OPTIONS.md` and `docs/BACKLOG.md` that describe the replacement. If any `src/`, `tests/`, or `examples/` hit remains, fix it before committing.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock examples/position_report.rs examples/predictor_eval.rs THIRD_PARTY_NOTICES.md README.md CONTRIBUTING.md docs/POSITION_INDEX.md docs/GPL_OPTIONS.md docs/BACKLOG.md
git commit -m "chore: remove GPL chess and PGN crates and update notices"
```

---

## Self-Review

**Spec coverage**

- `docs/GPL_OPTIONS.md` stage 1 (facade over `cozy-chess`): Tasks 1–4.
- Stage 2 (Brett PGN parser replacing `pgn-reader`): Tasks 5–7.
- Stage 3 (remove `shakmaty`): Tasks 8–13.
- Chess960/DFRC: Tasks 2–4, 8, 10.
- X-FEN/Shredder FEN: Task 4.
- Polyglot `.bin` hash: Task 8.
- Position index `VERSION = 2` and new key: Task 9.
- Byte-range indexing and large-file streaming: Tasks 5, 7.
- Exact ICCF notice and photo-pack constraints: untouched; restated in Global Constraints and Task 13.
- No GPL/AGPL: Global Constraints, Tasks 1, 8, 13.

**Step scan**

Every task has a failing test, a run-to-fail step, an implementation step with exact signatures, a run-to-pass step, and a commit. The only deliberate cross-task test dependency is Task 2's DFRC setup, which avoids `Fen` by constructing `Setup` directly. Task 4's tests use `Fen` after it exists.

**Type consistency**

- `Chess::from_setup(setup, mode)` is used consistently in Tasks 2–4, 6, 9, 10.
- `Fen::from_ascii`/`into_position`/`from_position` signatures match Tasks 4, 6, 9, 10, 11, 12.
- `SanPlus::from_ascii`/`from_move`/`san.to_move` match Tasks 3, 6, 9, 11.
- `UciMove::from_ascii`/`from_move`/`to_move` match Tasks 3, 6, 8, 10, 11, 12.
- `Move` variants match all existing call sites (`Normal`, `EnPassant`, `Castle`, `Put`).
- `Board` is the 64-square facade array; `Chess` wraps `cozy_chess::Board`. This is intentional and is why `Board::empty()` + `set_piece_at` works for custom starts.

**Review Focus coverage**

Each of the five Review Focus lines has a named test in the owning task: Chess960 (Tasks 2–4, 10), EP hash semantics (Tasks 4, 8), PGN delimiters (Task 5), illegal/truncated movetext (Tasks 5–6), and byte-range indexing (Tasks 5, 7).

**Proportion**

The plan pins interfaces, tests, and values; it does not transcribe the facade or parser bodies. The only large copied artifact is the standard 781-entry Polyglot table, which is format data and is sourced from MIT `gigachess` rather than retyped.
