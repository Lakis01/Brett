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
- **Classic ChessBase CBH** databases through a native, read-only reader
  (`src/cbh/`). Brett opens the `.cbh` header file and its `.cbg` moves,
  `.cba` annotations, and `.cbp`/`.cbt`/`.cbc`/`.cbs` names directly. This is
  the older multi-file format, not CBH2/2CBH.
- Lazy game loading for all three formats, a searchable game list, and a
  **Players** tab with career W/D/L, score, Elo, year range, and per-player
  game lists.
- Full game-tree import for all formats: recursive variations, comments, NAGs,
  `SetUp`/`FEN` positions, and analysis null moves.
- Drag and drop a `.pgn`, `.si4`, `.si3`, or `.cbh` onto the window, or use
  **File → Open database…**.

### Opening tree and position index

- A stable Zobrist key is computed for every mainline position. `(position,
  move)` statistics are aggregated into a sorted, memory-mapped file with
  games, W/D/L, Elo, and date ranges.
- The **Tree** tab lets you click any indexed move. The index is built in the
  background on first open and cached under `$XDG_CACHE_HOME/brett/index/`.
- The index is built through a shared cache and edge format, with a streaming
  fast path for PGN and the generic library loader for SCID and classic CBH.
  See [docs/POSITION_INDEX.md](docs/POSITION_INDEX.md).

### Preparation

- Select a player on the **Players** tab and choose **Prepare as White** or
  **Prepare as Black**. Brett creates a scratch preparation game and opens the
  Tree tab.
- For each opponent turn, the prediction panel shows likely replies, their
  posterior mean and approximate 90% credible interval, the player's
  games/recent games/last-seen date/trend, and the matching database tree
  columns (games, W/D/L, score, Elo, and years). Click a row to play that reply.
- Probability mass is reserved for legal moves that do not appear in the data,
  so a move seen in 2/2 games is not shown as certain. The panel also shows a
  sample-size confidence label.
- At your turn the panel switches to the prepared player's moves so far in the
  current line, with each move's date, database score, and the likelihood and
  interval it had before it was played.
- The normal opening tree for the current position is always visible below the
  preparation section.
- **Recent games** lists the newest games in the player book that reached the
  current position. The limit is configurable (default 10). Clicking a game
  opens a read-only view; closing it returns to the same preparation position.
- The model is a time-decayed Dirichlet estimate with a background prior, a
  recent-window mixture, and an explicit residual for unobserved legal moves.
  Defaults: 18-month half-life, 24-month recent window, a sample-size-scaled
  background prior, and a 2-pseudo-game recent mixture.
- A separate reference PGN, SCID, or classic CBH database can supply the
  background prior.
  Brett indexes only the chosen rating window (default ±200 Elo, with a
  configurable rating offset) and caches it separately from the player index.
  If no reference is saved yet, `BRETT_REFERENCE` can supply a path. Without a
  usable reference, the panel visibly falls back to the current database prior
  and warns that it may be skewed.
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

### External references

- The **Refs** tab queries three sources for the board position:
  **chessdb.cn** known moves with score, rank, note, and win rate; the
  **Lichess opening explorer** with W/D/L statistics and top games; and the
  **Lichess cloud evaluation** with depth, nodes, and up to three PVs.
- Clicking a reference move plays it on the board. Requests run off the UI
  thread, are debounced while navigating, and are cached in memory per FEN.
- chessdb.cn and the Lichess cloud evaluation are public. The Lichess opening
  explorer now requires an OAuth bearer token (no additional scopes are
  needed). Add `{"lichess_token": "lip_..."}` to
  `$XDG_CONFIG_HOME/brett/references.json` (usually
  `~/.config/brett/references.json`) or set `BRETT_LICHESS_TOKEN`. The env
  variable takes precedence. Without a token, the explorer explains how to
  configure one and the other two references still work.

### Engine analysis

- The **Engine** tab works with any UCI engine: type its executable path or use
  **Browse…** to pick a downloaded engine. **Default Stockfish** resets the path
  to `$BRETT_ENGINE` or `/usr/bin/stockfish`.
- Choose the engine's **Threads** and **Hash / RAM** size (up to 32,768 MB),
  then press **Apply & restart**. The settings are sent as UCI `setoption`
  commands when the engine advertises those options, saved in
  `$XDG_CONFIG_HOME/brett/engine.json` (or
  `~/.config/brett/engine.json`), and reused at the next launch. Reference
  database settings are stored separately in `preparation.json` beside it.
- Choose depth or time, 1–5 MultiPV lines, and auto-analysis on position change.
  Clicking a PV line plays its first move. Results from an older position are
  discarded.
- The worker performs the UCI handshake off the UI thread, waits for `readyok`,
  kills the engine cleanly on exit, and reports protocol or process errors in
  the panel.

### Game review

- The **Review** tab analyzes every mainline position with a second process
  using the same selected engine, thread count, and hash size, by depth or time
  per move, with a progress bar and cancel button.
- Each move is graded from the centipawn change between the engine evaluation
  before and after the move: best move, excellent (≤20 cp lost), good
  (≤50), inaccuracy (≤100), mistake (≤250), or blunder (>250). Forced moves
  are marked without a centipawn penalty, and mate scores are compressed so
  they dominate normal evaluations without breaking the graph.
- The panel shows an estimated accuracy percentage, a white-perspective
  evaluation graph, and a clickable move list. Clicking a row jumps to that
  move. **Write NAGs to mainline** copies the grades into the in-memory tree as
  `!!`/`!`/`?!`/`?`/`??` annotations.
- Accuracy is a documented heuristic based on centipawn loss, not an official
  rating.

### Board and move list

- 2D board with the cburnett piece set, coordinates, legal-move hints, click or
  drag-and-drop to play moves, and branching into variations. Castling accepts
  a king dragged to its destination square (`g1`/`c1`/`g8`/`c8`) as well as to
  the rook.
- Player names and ratings are shown above and below the board on their playing
  side. Elo is preferred; `WhiteDWZ`/`BlackDWZ` is used when no Elo tag exists.
- PGN clock comments (`[%clk ...]`) are parsed separately from human comments
  and hidden by default. The **Clocks** toggle in the bottom bar displays the
  latest remaining time for each side at the right of its board strip.
- The right-side PGN panel has a styled game header, an editable tag table,
  result badge, move cards, selected-move highlighting, variation cards,
  clickable comment previews, and a collapsible FEN. Tag names are fixed; values
  can be edited, tags can be removed, and common missing tags can be added.
- Chess figurines are used for piece moves. The embedded DejaVu Sans fallback
  provides the chess and annotation glyphs.
- Clicking an annotation opens the inline annotation editor. Right-clicking a
  move or annotation opens the grouped NAG palette, including `!?`, `+=`, `=+`,
  `∞`, `□`, and the other standard symbols. Comments are editable in the same
  panel.
- Keyboard: `←`/`→` previous/next move, `↑`/`↓` previous/next game,
  `Home`/`End` start/end, `F` flip board, `T` tree, `M` moves,
  `E` engine, `R` review, `A` annotate the selected move, `Escape` closes the
  annotation editor or board selection.

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
cargo run --example predictor_eval -- file.pgn "Player Name" 20
                                                # chronological held-out metrics
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
| `src/cbh/` | Native classic ChessBase CBH reader and `CbhLibrary` |
| `src/position.rs` | Position index, opening-tree statistics, shared library builder |
| `src/players.rs` | Player directory, per-player position books, Bayesian prediction |
| `src/evaluation.rs` | Held-out log loss, Brier score, top-k accuracy, calibration |
| `src/online.rs` | Public Lichess and Chess.com imports |
| `src/cloud.rs` | chessdb.cn, Lichess opening explorer, and Lichess cloud eval |
| `src/domain.rs` | Game tree, PGN parsing, move lines, figurines, NAG palette, clocks |
| `src/engine.rs` | Asynchronous UCI process, handshake, options, parser, worker threads |
| `src/settings.rs` | Persistent engine and reference-database settings |
| `src/review.rs` | Sequential game review, move grading, accuracy, eval chart data |
| `src/app.rs` | egui application, tabs, preparation flow, annotation editor |
| `src/ui/fonts.rs` | Embedded DejaVu Sans fallback for chess symbols |
| `docs/` | Data safety, position-index format, preparation model, SCID/CBH readers, backlog |

## Roadmap

- **M1 — database index (done).** Header-only first pass, lazy per-game trees,
  and byte-offset loading.
- **M2a — position index and opening tree (done).** Zobrist keys, on-disk
  sorted/mmap store, W/D/L, Elo and date aggregates, clickable tree.
- **Native SCID (done).** SCID4/SCID3 reader, namebase, tags, variations,
  comments, NAGs, custom FEN, null moves, and position indexing.
- **Preparation stage (done).** Player directory, player books, prediction,
  Prepare as White/Black, online imports, annotation editing, PGN panel.
- **UCI engine (done).** Any UCI executable, persistent Threads/Hash settings,
  MultiPV analysis, asynchronous worker, clickable principal variations,
  Stockfish default.
- **Clock display (done).** PGN `[%clk]` parsing, hidden by default, shown
  beside the players at the board.
- **Castling by dragging the king (done).** King-to-destination and
  king-to-rook gestures both work.
- **PGN metadata (done).** Editable common tags, fixed tag names, delete/add,
  Elo/DWZ ratings, and player strips above and below the board.
- **Game Review (done).** Per-position Stockfish pass with progress/cancel,
  centipawn-loss grades, forced-move handling, accuracy summary, evaluation
  graph, clickable move list, and optional NAG write-back.
- **Preparation view v2 (done).** Normal tree statistics at every turn,
  opponent move history, a configurable clickable recent-games window, and a
  read-only game preview that preserves preparation state.
- **Predictor v2 and reference distribution (done).** Posterior uncertainty,
  unobserved-move residual mass, a separately cached rating-matched reference
  index, visible fallback, held-out metrics, and calibration output.
- **Classic CBH support (done).** Native read-only support for the older
  multi-file format, including modes 0/4/5/10, variations, comments, NAGs,
  drawing tokens, custom starts, and Chess960. See
  [docs/CBH.md](docs/CBH.md).
- **Cloud references (done).** chessdb.cn, the Lichess opening explorer, and
  Lichess cloud evaluation in the **Refs** tab, with asynchronous requests and
  per-position caching.
- **M2b — Mega-scale ingest.** External sorting, an on-disk header table, and
  incremental updates for multi-million-game databases.
- **Database editing and export.** Copy/merge on drag and drop, plus PGN, SCID,
  and classic CBH writers with a size warning for PGN.

The detailed queue and acceptance criteria are in
[docs/BACKLOG.md](docs/BACKLOG.md).

## Data safety

**`/home/gratis01/Chess Databases/` is treated as read-only.** Brett opens PGN,
SCID, and classic CBH file sets for reading only. Derived indexes and imported
games go to `$XDG_CACHE_HOME/brett/`, and engine/reference preferences go to
`$XDG_CONFIG_HOME/brett/`; nothing is written next to a source database. The
Lichess reference token, if configured, lives in `references.json` in that
config directory (or in `BRETT_LICHESS_TOKEN`).
See [docs/DATA_SAFETY.md](docs/DATA_SAFETY.md).

## Third-party assets and references

The piece images in `assets/pieces/` are the **cburnett** set by
Colin M. L. Burnett, licensed **CC BY-SA 3.0**, converted from the SVG sources
in the [Lichess repository](https://github.com/lichess-org/lila/tree/master/public/piece/cburnett).
See `assets/pieces/README.md`.

The embedded fallback font is **DejaVu Sans**, under the Bitstream Vera /
DejaVu licence. See `assets/fonts/README.md`.

The SCID and classic CBH readers are independent implementations written from
published on-disk formats. The SCID source tree and the AGPL ChessBase bridge
were used as format references only; no code from either is linked or copied.
See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Licence

MIT (application code) — see [LICENSE](LICENSE). The piece artwork remains
CC BY-SA 3.0, and the font remains under its own licence; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
