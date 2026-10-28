# SDD ledger — plan: docs/superpowers/plans/2026-10-28-permissive-rules-pgn-stack.md

## Workspace

- Worktree: `/home/gratis01/.local/share/opencode/worktree/a178f3/permissive-rules-pgn`
- Branch: `permissive-rules-pgn` (created from `main` at `c696941`)
- Plan committed on `main` as `c696941` before branching.
- Spec: `docs/GPL_OPTIONS.md` (staged replacement sequence) and `docs/BACKLOG.md` item 17.

## Handover state

- Branch: `permissive-rules-pgn`; HEAD `dc86ad1`.
- Completed: Tasks 1–2, both reviews clean.
- In flight: Task 3 implemented at `dc86ad1`; task review not yet dispatched. Task 3 BASE is `dd129ab`.
- Next action: generate the review package for `dd129ab..dc86ad1`, dispatch the task reviewer, record the verdict here.
- Live artifacts in this directory: `progress.md`, `task-N-brief.md`, `task-N-report.md`, `review-<base>..<head>.diff`.
- Recovery: `git log --oneline c696941..HEAD`; the plan is committed; briefs are regenerable with the skill's `task-brief` script.
- Caveat: this directory is git-ignored (`.superpowers/sdd/.gitignore`). Copy it or commit a snapshot before moving the work to another machine.

## Baseline

- `cargo test --all-targets` started before Task 1 dispatch (fresh worktree, no `target/`).
- Baseline result: PASS — all suites green (lib, integration, examples; `position_index` 9/9, `scid` 3/3).

## Preflight scan

### Per-task self-consistency

| Task | Tests vs implementation | Files created vs later touched | Finding |
|---|---|---|---|
| 1 | Tests use `Square::new`, `from_coords`, `file`, `rank`, `File::new`, `Rank::new`, flips, `Piece::char`, `Bitboard::{from_square,contains,to_u64}`, `BitOr`; implementation lists all. | `src/chess/{mod,types}.rs`, `tests/chess_facade.rs`; later tasks append to both. | F1: expected `A1 \| H8` value was wrong (`0x8100…`); H8 is bit 63, so the OR is `0x8000…`. Fixed in plan. |
| 2 | Tests use `Board::{empty,set_piece_at,piece_at,role_at,king_of,remove_piece_at}`, `Setup::{default,empty,swap_turn}`, `Chess::{default,from_setup,turn,board,castles,fullmoves}`, `Castles::{rook,has}`; implementation lists all. | `src/chess/{board,chess}.rs`; Task 3 modifies `chess.rs`, Task 4 modifies it again. | F2: original test used `Fen` before Task 4. Fixed: `dfrc_setup()` builds `Setup` directly. |
| 3 | Tests use `Move::Normal{capture}`, `San::Null`, `SanPlus::{from_ascii,from_move,san.to_move}`, `UciMove::{from_ascii,from_move,to_move}`, `Chess::{legal_moves,play,is_checkmate}`; implementation lists all. | `src/chess/{move,san}.rs`, modifies `chess.rs`. | None. |
| 4 | Tests use `Fen::{from_ascii,from_position,into_position,into_setup}`, `EnPassantMode`, `Position`, `position_hash`, `swap_turn`; implementation lists all. | `src/chess/fen.rs`, modifies `chess.rs`. | F3: X-FEN `K`/`Q` said “right/left of the king”, which is ambiguous for Black. Fixed to “kingside = higher file, queenside = lower file” for both colors. |
| 5 | Tests use `Reader::{new,stream_position,read_game}` and every `Visitor` callback named in the interface; implementation lists them. | `src/pgn/reader.rs`, `src/pgn.rs`; Tasks 6, 7, 9 consume it. | None. `SanPlus` is imported from `brett::chess`, matching Task 3. |
| 6 | Tests rely on `Game::errors`, `Game::mainline_len`, `parse_pgn`; confirmed present in `src/domain.rs`. Illegal SAN returns early in `Builder::push`, so `errors.len()==1` and `mainline_len()==2` are correct. | `src/domain.rs`, `tests/pgn_tree.rs`. | None. |
| 7 | Test uses `PgnLibrary::{build,len,load}` and `Game::white`; confirmed `load` returns `domain::Game` and `Game::white()` exists. | `src/library.rs`, `tests/pgn_index.rs`. | None. |
| 8 | Tests import `brett::book::polyglot::position_key`; plan said `mod polyglot;` (private). | `src/book/{polyglot,polyglot_keys}.rs`, `src/book.rs`, `src/book/bkt.rs`, `tests/polyglot.rs`. | F4: module must be `pub mod polyglot;`. Fixed in plan. |
| 9 | Tests use `position_key`, `SanPlus`, `Chess`, `Position`; implementation lists them. | `src/position.rs`, `tests/position_index.rs`. | F5: `SanPlus` import path corrected to `brett::chess::SanPlus`. Fixed in plan. |
| 10 | Tests use facade `Board`, `Setup`, `Chess`, `Castles`, `Bitboard`, `Position`; implementation lists them. | `src/cbh/{moves,tests}.rs`, `src/scid.rs`, `src/scid/writer.rs`, `tests/scid.rs`. | None. |
| 11 | Import-only migration; each file's needed names are listed. | `src/{pgn,cloud,recent,players,review,settings}.rs`. | None. |
| 12 | About string “Rules and SAN: shakmaty. PGN: pgn-reader.” confirmed at `src/app.rs:8968`; `prep_tree`/`fen_string` signatures confirmed. | `src/app.rs`, `src/ui/{board,pieces}.rs`. | None. |
| 13 | Commands and expected outcomes are concrete. | `Cargo.toml`, examples, notices, docs. | None. |

### Cross-task interface pairs

| Pair | Shared file/interface | Produced → consumed | Finding |
|---|---|---|---|
| 1↔2 | `src/chess/mod.rs`, `tests/chess_facade.rs`, scalar types | `Color/Role/Piece/File/Rank/Square/Bitboard` → board/setup/Chess | OK |
| 1↔3 | scalar types | scalars → move/SAN/UCI | OK |
| 1↔4 | scalar types | scalars → FEN/hash | OK |
| 2↔3 | `src/chess/chess.rs`, `Board`, `Castles`, `CastlingMode` | board/Chess → legal moves/play/SAN | OK |
| 2↔4 | `src/chess/chess.rs`, `Setup`, `Chess` | setup/Chess → FEN/hash/`Position` | OK |
| 2↔10 | `Board::empty`/`set_piece_at`, `Setup`, `Castles::rook`, `us`, `maybe_ep_square`, `swap_turn` | facade → CBH custom starts | OK |
| 3↔4 | `Move` | move → `Position::play`/hash | OK |
| 3↔5 | `San`, `SanPlus` | SAN → PGN parser | OK |
| 3↔6 | `Move`, `SanPlus`, `UciMove` | facade → game tree | OK |
| 3↔8 | `Move::to_uci` | facade → book tests | OK |
| 3↔9 | `Move`, `UciMove` | facade → move codec/index | OK |
| 3↔10 | `Move`, `UciMove` | facade → CBH/SCID | OK |
| 3↔11 | `UciMove`, `SanPlus` | facade → library modules | OK |
| 3↔12 | `UciMove` | facade → app previews | OK |
| 4↔6 | `Fen`, `Position`, `EnPassantMode` | FEN/hash → domain | OK |
| 4↔8 | `Fen` in polyglot tests | FEN parser → test positions | OK |
| 4↔9 | `Fen`, `Position`, `position_hash` | FEN/hash → index | OK |
| 4↔10 | `Fen`, `Position` | FEN/hash → CBH/SCID | OK |
| 4↔11 | `Fen`, `Position` | FEN/hash → review/pgn | OK |
| 4↔12 | `Fen`, `Position` | FEN/hash → app | OK |
| 5↔6 | `Reader`, `Visitor` | parser → `domain::parse_pgn` | OK |
| 5↔7 | `Reader`, `Visitor` | parser → library byte ranges | OK |
| 5↔9 | `Reader`, `Visitor` | parser → position index | OK |
| 6↔7 | `domain::Game` | `library.load` returns a `Game` | OK |
| 7↔9 | PGN byte-range indexing | independent consumers of `Reader` | OK |
| 8↔12 | `Book::moves` | book reader → app/review | OK |
| 9↔11 | position stats | index → `players` prep | OK |
| 9↔12 | position index | index → app | OK |
| 10↔12 | CBH/SCID loaders | formats → app | OK |
| 11↔12 | module APIs | library → app | OK |
| 13↔1–12 | `Cargo.toml`, notices, docs | all tasks → dependency removal | OK; removal is intentionally last |

## Rulings

- R1 (F1): Task 1's expected bitboard value is `0x8000_0000_0000_0001`; the plan text is corrected. Cost if wrong: none — the bit layout is unambiguous.
- R2 (F2): Task 2 builds the DFRC start with `Setup` directly, not `Fen`, so Task 2 stays independently testable. Cost if wrong: a redundant helper if `Fen` were available earlier.
- R3 (F3): X-FEN `K`/`Q` resolve by file order (kingside = higher file) for both colors, never by visual left/right. Cost if wrong: mirrored castling rights in some Black Chess960 FENs.
- R4 (F4): `book::polyglot` is public because the integration test imports it; `polyglot_keys` stays private. Cost if wrong: an unnecessarily public module.
- R5 (F5): `SanPlus` lives in `brett::chess`; the parser re-exports `Reader`/`Visitor` only. Cost if wrong: a duplicate type.
- R6: The Polyglot random table is transcribed from MIT `gigachess` 0.1.9; its notice is added in Task 13. Cost if wrong: a missing attribution.
- R7: `cozy-chess` keeps default features; add `std` only if a compile error requires it. Cost if wrong: one extra feature line.

## Task log

- Task 1: complete (commits c696941..e6bc92b, review clean). Minor accepted: `pub mod types` widens the facade path; committed tests do not exercise the cozy conversion bridge; `Square::ALL` is hand-written beside the macro. None block; conversion correctness was verified by the reviewer against cozy 0.2.2's documented rank-major layout.
- Task 2: complete (commits 21210ae..dd129ab, review clean). DONE_WITH_CONCERNS accepted: test `use` lines merged into the header (bodies verbatim) and `Chess::default` is an `impl Default` (clippy). Minor accepted: `to_setup_always`/`BoardRef::new` public surface, no direct EP-rank test, unreachable `NonZeroU32` fallback, `us_of` indirection. Reviewer verified Standard/Chess960 rights and EP mapping against cozy 0.3.4.

## Tasks

- [x] Task 1: add `cozy-chess` and facade scalars
- [x] Task 2: board, setup, castling, `Chess`
- [ ] Task 3: moves, SAN, UCI
- [ ] Task 4: FEN, position hash, `Position`
- [ ] Task 5: Brett streaming PGN reader
- [ ] Task 6: migrate `domain.rs`
- [ ] Task 7: migrate `library.rs`
- [ ] Task 8: Polyglot hash and `.bin` book
- [ ] Task 9: position index + `VERSION = 2`
- [ ] Task 10: CBH and SCID
- [ ] Task 11: remaining library modules
- [ ] Task 12: app and UI
- [ ] Task 13: remove GPL crates, notices, docs, full gate
