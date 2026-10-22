# Brett

A native Linux chess preparation workspace. It is a functional re-implementation
of the parts of ChessBase that matter for opening preparation — not a clone of
its UI, and not a Wine package.

## What works

### Databases

- **PGN** files with a streaming header index: opening a 50,000-game file records
  headers and byte ranges, then builds a full tree only for the selected game.
- **SCID4 and SCID3** databases through an independent native reader
  (`src/scid.rs`). Brett reads the `.si4`/`.sn4`/`.sg4` triple directly; no SCID
  installation, conversion step, or GPL code is involved. Legacy SCID3
  (`.si3`/`.sn3`/`.sg3`) uses the same reader.
- Lazy game loading for both formats, a searchable game list, and a **Players**
  tab with career W/D/L, score, Elo, year range, and per-player game lists.
- Full game-tree import for both formats: recursive variations, comments, NAGs,
  `SetUp`/`FEN` positions, and analysis null moves.
- Drag and drop a `.pgn`, `.si4`, or `.si3` onto the window, or use
  **File → Open database…**.

### Opening tree and position index

- A stable Zobrist key is computed for every mainline position. `(position,
  move)` statistics are aggregated into a sorted, memory-mapped file with
  games, W/D/L, Elo, and date ranges.
- The **Tree** tab lets you click any indexed move. The index is built in the
  background on first open and cached under `$XDG_CACHE_HOME/brett/index/`.
- The index is built through a shared cache and edge format, with a streaming
  fast path for PGN and the generic library loader for SCID.
  See [docs/POSITION_INDEX.md](docs/POSITION_INDEX.md).

### Preparation

- Select a player on the **Players** tab and choose **Prepare as White** or
  **Prepare as Black**. Brett creates a scratch preparation game and opens the
  Tree tab.
- For each opponent turn, the prediction panel shows likely replies, their
  probability, games seen, recent games, last-seen date, and trend. Click a row
  to play that reply.
- The model is a time-decayed Dirichlet estimate with a database prior and a
  recent-window mixture. Defaults: 18-month half-life, 24-month recent window,
  3 pseudo-games of database prior, and a 2-pseudo-game recent mixture.
- Player books are built from the player's mainline games in a background
  thread. Player identities ignore punctuation and word order, so
  “Fischer, Robert James” and “Robert James Fischer” share a book.
- Leaving preparation restores the game that was open before it started.
- See [docs/PREPARATION.md](docs/PREPARATION.md).

### Online imports

- **File → Import from Lichess…** or **Import from Chess.com…** downloads
  public games for a username using the unauthenticated APIs.
- Lichess exports up to the requested number of games in one request.
  Chess.com has no bulk PGN endpoint, so Brett combines up to 36 recent monthly
  archives.
- Imports are capped at 5,000 games and cached under
  `$XDG_CACHE_HOME/brett/online/`. Nothing is written next to a source database.

### Board and move list

- 2D board with the cburnett piece set, coordinates, legal-move hints, click or
  drag-and-drop to play moves, and branching into variations.
- The right-side PGN panel has a styled game header, result badge, move cards,
  selected-move highlighting, variation cards, clickable comment previews, and
  a collapsible FEN.
- Chess figurines are used for piece moves. The embedded DejaVu Sans fallback
  provides the chess and annotation glyphs.
- Clicking an annotation opens the inline annotation editor. Right-clicking a
  move or annotation opens the grouped NAG palette, including `!?`, `+=`, `=+`,
  `∞`, `□`, and the other standard symbols. Comments are editable in the same
  panel.
- Keyboard: `←`/`→` previous/next move, `↑`/`↓` previous/next game,
  `Home`/`End` start/end, `F` flip board, `T` tree, `M` moves, `A` annotate the
  selected move, `Escape` closes the annotation editor or board selection.

### Benchmarks

On the local 49,780-game `Frese Sammlung` SCID database (8.0 MiB triple, 3.8 M
mainline moves), in the development profile:

- header index: **0.28 s**, zero decode errors;
- position index: **16.5 s** for 2.94 M positions / 3.01 M move records, a
  **137.6 MiB** memory-mapped file;
- start position: 48,619 games with a move available.

The native reader was also exercised against every `.si4` database in the local
reference corpus with zero errors. `cargo run --example scid_report -- file.si4`
decodes every game and reports failures; use it after format changes.

## Development

```sh
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all

cargo run --release -- path/to/file.pgn
cargo run --release -- path/to/database.si4

cargo run --example report -- file.pgn           # PGN import statistics
cargo run --example index_report -- file.pgn     # header index + lazy-load timing
cargo run --example position_report -- file.pgn  # opening-tree index benchmark
cargo run --example scid_report -- file.si4      # decode every SCID game
scripts/scid-to-pgn.sh "/path/to/db" /tmp/opencode/out.pgn
```

`scripts/scid-to-pgn.sh` copies the SCID triple to `/tmp/opencode` first; it
never writes to the source database.

## Code map

| Path | Purpose |
| --- | --- |
| `src/library.rs` | `GameLibrary` interface, streaming PGN index, shared source metadata |
| `src/scid.rs` | Native SCID3/SCID4 reader and `ScidLibrary` |
| `src/position.rs` | Position index, opening-tree statistics, SCID/PGN generic builder |
| `src/players.rs` | Player directory, per-player position books, Bayesian prediction |
| `src/online.rs` | Public Lichess and Chess.com imports |
| `src/domain.rs` | Game tree, PGN parsing, move lines, figurines, NAG palette |
| `src/app.rs` | egui application, tabs, preparation flow, annotation editor |
| `src/ui/fonts.rs` | Embedded DejaVu Sans fallback for chess symbols |
| `docs/` | Data safety, position-index format, preparation model, SCID reader, backlog |

## Roadmap

- **M1 — database index (done).** Header-only first pass, lazy per-game trees,
  and byte-offset loading.
- **M2a — position index and opening tree (done).** Zobrist keys, on-disk
  sorted/mmap store, W/D/L, Elo and date aggregates, clickable tree.
- **Native SCID (done).** SCID4/SCID3 reader, namebase, tags, variations,
  comments, NAGs, custom FEN, null moves, and position indexing.
- **Preparation stage (done).** Player directory, player books, prediction,
  Prepare as White/Black, online imports, annotation editing, PGN panel.
- **UCI engine** using the installed Stockfish at `/usr/bin/stockfish`, with
  MultiPV analysis and clickable lines.
- **Game Review** using the local engine: per-move evaluations, centipawn loss,
  accuracy, blunder classification, and an evaluation graph.
- **Clock display** parsed from PGN `[%clk]` comments, hidden by default.
- **Castling by dragging the king** onto `g1`/`c1`/`g8`/`c8`, in addition to
  dragging it onto the rook.
- **Preparation view v2:** normal tree statistics at every turn, opponent move
  history, calibrated likelihoods with uncertainty, a static and rating-matched
  reference distribution, and a clickable recent-games window.
- **Classic CBH support** for the older multi-file format (sample located;
  research pending).
- **M2b — Mega-scale ingest.** External sorting, an on-disk header table, and
  incremental updates for multi-million-game databases.
- **Cloud evaluation** (Lichess cloud eval, ChessDB.cn) with local Stockfish
  fallback.

The detailed queue and acceptance criteria are in
[docs/BACKLOG.md](docs/BACKLOG.md).

## Data safety

**`/home/gratis01/Chess Databases/` is treated as read-only.** Brett opens PGN
and SCID files for reading only. Derived indexes and imported games go to
`$XDG_CACHE_HOME/brett/`; nothing is written next to a source database.
See [docs/DATA_SAFETY.md](docs/DATA_SAFETY.md).

## Third-party assets and references

The piece images in `assets/pieces/` are the **cburnett** set by
Colin M. L. Burnett, licensed **CC BY-SA 3.0**, converted from the SVG sources
in the [Lichess repository](https://github.com/lichess-org/lila/tree/master/public/piece/cburnett).
See `assets/pieces/README.md`.

The embedded fallback font is **DejaVu Sans**, under the Bitstream Vera /
DejaVu licence. See `assets/fonts/README.md`.

The SCID reader is an independent implementation written from the published
on-disk format. The GPL SCID source tree was used as a format reference only;
no SCID code is linked or copied. See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Licence

MIT (application code) — see [LICENSE](LICENSE). The piece artwork remains
CC BY-SA 3.0, and the font remains under its own licence; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
