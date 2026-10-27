# Brett

A native Linux chess preparation workspace. It is a functional re-implementation
of the parts of ChessBase that matter for opening preparation — not a clone of
its UI, and not a Wine package.

## Quick start (Linux)

```sh
cargo build --release
./target/release/brett fixtures/opera.pgn
```

You need Rust 1.97 or newer (the chess and PGN crates require it), a C
toolchain, the X11/Wayland development packages required by `eframe`, and
`zenity` or `kdialog` for file dialogs.
Stockfish is optional: Brett looks for `$BRETT_ENGINE`, then
`/usr/bin/stockfish`, and any UCI engine can be selected in the Engine tab.

The first launch opens the bundled public-domain game. Use **File → Open
database…** or drop a `.pgn`, `.si4`, `.si3`, or `.cbh` onto the window. Personal
settings, caches, and secrets live outside the repository; see
[CONTRIBUTING.md](CONTRIBUTING.md) and [Data safety](#data-safety).

## Platform support

Linux is the supported and tested platform. Development and visual checks run
on KDE Plasma Wayland with Xwayland, and the binary is built with both the X11
and Wayland backends. **Windows and macOS are not currently supported or
tested.** The file dialogs shell out to `zenity`/`kdialog`, configuration uses
XDG directories, and the windowing/rendering stack has not been validated on
either platform. A port would need a native dialog backend, platform-specific
config/cache paths, and a fresh build-and-test pass; do not assume the current
source compiles or runs there.

## ICCF database notice

Brett can use an optional ICCF correspondence database as a local, read-only
reference. Brett does not bundle or redistribute that database.

> **The ICCF database is provided for personal use only and may not be sold or
> re-sold to any third party.**

Obtain the database from ICCF, or compile it from ICCF server games under
ICCF's terms, and comply with ICCF's terms. See
[ICCF_NOTICE.md](ICCF_NOTICE.md).

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
- Large archives such as the 11.1-million-game Mega Database are handled with
  interned headers, lazy session rows, and an O(1) row lookup. Above 500,000
  games the player directory is built in the background, so the game list stays
  usable while it runs. A full position index is built automatically only up to
  250,000 games; larger databases show a manual action, and full indexing is
  refused above 1,000,000 games until the external-sort indexer lands. Reference
  and opening-tree indexes have the same guard, and very large source files are
  rejected before they are opened just to be rejected.

### Opening tree and position index

- A stable Zobrist key is computed for every mainline position. `(position,
  move)` statistics are aggregated into a sorted, memory-mapped file with
  games, W/D/L, Elo, and date ranges.
- The **Tree** tab lets you click any indexed move. The index is built in the
  background on first open and cached under `$XDG_CACHE_HOME/brett/index/`.
- A separate **opening-tree file** can be indexed for the Tree tab. Choose a
  PGN, SCID, or classic CBH database with **Choose…**; the path is saved in
  `display.json` and loaded on the next launch. **Current DB** switches back to
  the open database. The Tree tab also shows a Lichess **Masters** or full
  **Lichess** reference table for the current position, plus **Lichess top
  games**. Top games live in the Tree tab (not Refs) and open a read-only
  preview when clicked.
- Native opening books: Polyglot `.bin`, ChessBase `.ctg` (with `.cto`/`.ctb`),
  Arena `.abk`, and Shredder `.bkt`. The Tree tab exposes each format through
  the same opening-book path and shows games, weight, score, W/D/B, and
  format-specific annotations.
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
- **Settings → My player names** stores your aliases, highest priority first.
  The first alias is labelled **You** in the Players tab and replaces the
  literal `You` in preparation tag fields (the board strip still uses the
  alias). When a game contains one of the aliases, opening it flips the board
  to that side; the first matching name in list order wins, and the previous
  orientation is kept when nothing matches. Recent-game previews and **Exit
  prep** restore the side you were on before.
- Leaving preparation restores the game that was open before it started.
- See [docs/PREPARATION.md](docs/PREPARATION.md).

### Online imports

- **File → Import from Lichess…**, **Import from Chess.com…**, or **Import
  from ChessABC…** opens the single import dialog. It downloads public games
  for a username using the unauthenticated APIs. The Refs tab's **Online
  accounts** buttons are for scouting an opponent, not for importing; each
  service appears once.
- Lichess exports up to the requested number of games in one request.
  Chess.com has no bulk PGN endpoint, so Brett combines up to 36 recent monthly
  archives. ChessABC searches by player name or FIDE id.
- Imports are capped at 5,000 games and cached under
  `$XDG_CACHE_HOME/brett/online/`. Nothing is written next to a source database.

### References, cloud evaluation, and online accounts

- The **Cloud Engine** in the Engine tab combines chessdb.cn and the Lichess
  cloud evaluation. Its columns are **chessdb eval**, **Lichess eval (depth)**,
  and **line**; the move is part of the line, so there is no separate Move
  column. Every move in a Lichess line is hover-previewable and clickable.
- Rows are sorted by the average of the available chessdb/Lichess scores, best
  first for the side to move: White descending, Black ascending. Rows without
  an evaluation stay last. The best eight rows are always visible; the rest
  are under **More moves (N)**. Chessdb-only rows are grouped compactly, say
  once that they have no Lichess evaluation, and truncate after one ply.
- Colour is the delta to the best move for the side to move, not absolute White
  advantage: best green, worst red, missing evaluation grey, and mates dominate
  centipawns. chessdb.cn scores are normalised to White's point of view.
  Castling is matched across UCI encodings, and SAN uses `O-O`/`O-O-O`.
- The **Refs** tab configures reference sources: the current database, a
  separate reference database (Mega Database by default when present), and
  online opponent lookups. Cloud variations are not duplicated there; the
  Cloud Engine owns them. Online accounts are listed once, and importing is
  **File → Import**.
- The **Tree** tab shows the local opening tree, the Lichess Masters/Lichess
  reference table, and **Lichess top games**. Clicking a top game opens a
  read-only preview.
- A configured local correspondence SCID database is used automatically as an
  Engine-tab reference. Its inclusion and path are set in **Settings →
  Correspondence database** (not in Refs). The default location is
  `~/Downloads/Brett Resources/Correspondence Database/Correspondence_2350.si4`;
  `BRETT_CORRESPONDENCE` overrides it. The first open builds a cached position
  index in the background, and later opens reuse the memory-mapped cache. See
  [docs/CORRESPONDENCE_DATABASE.md](docs/CORRESPONDENCE_DATABASE.md).
- Requests run off the UI thread, are debounced while navigating, and are
  cached in memory per FEN. Hovering a candidate move overlays the resulting
  position without changing the selected game node; removing the pointer
  restores the original position.
- chessdb.cn and the Lichess cloud evaluation are public. Lichess
  opening-explorer requests in practice need a personal OAuth bearer token (no
  scopes are required); if Lichess rejects the request, Brett points you to
  **Settings → Lichess API**. To create a token, sign in at
  <https://lichess.org>, open
  <https://lichess.org/account/oauth/token/create>, give it a name such as
  `Brett`, leave every scope unchecked, and press **Create**. Copy the
  `lip_...` value immediately; Lichess shows it only once. Paste it into
  **Settings → Lichess API**, or set `BRETT_LICHESS_TOKEN` to keep it out of
  `references.json`. The environment variable takes precedence.

### Engine analysis

- The **Engine** tab works with any UCI engine: type its executable path or use
  **Browse…** to pick a downloaded engine. **Default Stockfish** resets the path
  to `$BRETT_ENGINE` or `/usr/bin/stockfish`.
- Choose the engine's **Threads** and **Hash / RAM** size (up to 32,768 MB),
  then press **Apply & restart**. The settings are sent as UCI `setoption`
  commands when the engine advertises those options, saved in
  `$XDG_CONFIG_HOME/brett/engine.json` (or
  `~/.config/brett/engine.json`), and reused at the next launch. Display and
  preparation/reference settings are stored in `display.json` and
  `preparation.json` beside it.
- Choose depth or time, 1–5 MultiPV lines, and auto-analysis on position change.
  The **Auto analyze** checkbox is saved in `display.json` and restored at the
  next launch, so it keeps the last state until you change it.
  Hovering a PV move previews its position; clicking a move plays the line up
  to that point. Results from an older position are discarded. PVs are shown
  with real move numbers (`25... e5 26. Nf3 Nc6`) and labelled **Engine PV**,
  **Lichess PV**, or **Lichess line** so analysis-line numbering never looks
  like a chess move number.
- The **Show** selector switches the Engine tab between **Local**, **Cloud**,
  and **Both**. **Both** keeps local UCI analysis and the **Cloud Engine**
  table visible together. **Cloud** is a true Cloud-only mode: the local
  engine is stopped, and local engine/hardware settings, local analysis, and
  Game review are hidden, not merely replaced by an eval. **Enable local
  engine** returns to Both.
- **Game review** is a toggle to the right of **Engine and hardware**. It opens
  a panel beside local analysis; it is hidden in Cloud-only mode because game
  review runs on the local engine.
- The worker performs the UCI handshake off the UI thread, waits for `readyok`,
  kills the engine cleanly on exit, and reports protocol or process errors in
  the panel.

### Game review

- The **Game review** panel, opened from the **Engine** tab, analyzes every
  mainline position with a second local process using the same selected engine,
  thread count, and hash size, by depth or time per move, with a progress bar
  and cancel button. It is unavailable in Cloud-only mode.
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
- Player names, optional title badges (`WhiteTitle`/`BlackTitle`), and ratings
  are shown above and below the board on their playing side. **Settings →
  Player ratings** chooses the Elo/DWZ/USCF priority and can show every
  available rating instead of only the preferred one.
- A persistent FEN strip under the board always shows the current position;
  clicking the field copies the FEN. The move counter beside it counts full
  moves, not plies.
- The Moves header keeps one metadata line: compact `R`/`B` chips, the event
  truncated rather than wrapped or squeezed, and the result right-aligned.
  When the game has an `Opening` tag, its bare ECO code (for example `A07`)
  appears as a gold chip beside the opening name; otherwise the ECO code stays
  in the top bar as before.
- The right-side PGN panel has a styled game header, an editable tag table,
  result badge, move cards, selected-move highlighting, variation cards, and
  clickable comment previews. WhiteElo and BlackElo are separate editable
  fields to the right of the White/Black name boxes; Elo is not appended to the
  name and there is no derived rating chip. Fields that are absent or empty are
  hidden by default; **Show empty** reveals the usual metadata fields so a new
  game can be filled in without adding each tag by hand. Board is hidden when
  empty unless **Show empty** is on or it was added with **+ Add tag → Board**,
  and it is written as `round.board` inside the Round tag. The Date field can
  be typed directly or opened as a calendar anchored to the field.
- **More tags (N)** / **Fewer tags** replaces the old arrow expanders. The
  additional-tag list is capped in height and scrolls, so a game with many
  tags does not push the move list off the panel. Tag names are fixed; values
  can be edited, tags can be removed, and common missing tags can be added.
- PGN clock comments (`[%clk ...]`) are parsed separately from human comments
  and hidden by default. The **Clocks** toggle in the bottom bar displays the
  latest remaining time for each side at the right of its board strip.
- Hovering a move in the open game's move list previews the resulting
  position. This is controlled by **Settings → Preview moves on hover** and is
  off by default; tree, engine, reference, and read-only preview hovers are
  always available.
- Chess figurines are used for piece moves. The embedded DejaVu Sans fallback
  provides the chess and annotation glyphs.
- Clicking an annotation opens the inline annotation editor. Right-clicking a
  move or annotation opens the grouped NAG palette, including `!?`, `+=`, `=+`,
  `∞`, `□`, and the other standard symbols. Comments are editable in the same
  panel.
- Keyboard: `←`/`→` previous/next move, `↑`/`↓` previous/next game,
  `Home`/`End` start/end, `F` flip board, `T` tree, `M` moves,
  `E` engine, `R` review (under Engine), `C` references, `P` players,
  `A` annotate the selected move, `O` online opponent panel, `D` date
  calendar, `V` database views, `Escape` closes the calendar, annotation
  editor, or board selection.
- At a position with more than one continuation, `→` opens a branch popup
  instead of silently following the mainline. The mainline is selected first
  and variations follow by most recent use in this session. Use `↑`/`↓` to
  choose, `Enter` or `→` to play, `1`–`9` to pick directly, and `Esc`/`←` to
  cancel. The popup also appears inside variations.

### Settings

- **Player ratings** — choose Elo, DWZ, or USCF as the priority and optionally
  show all available ratings.
- **My player names** — one alias per line, highest priority first. The first
  alias is labelled **You** in the Players tab, replaces the literal `You` in
  preparation tag fields, and drives auto-orientation when a game contains one
  of the aliases.
- **Correspondence database** — include or exclude the local correspondence
  database in the Engine tab, choose its file, or restore the conventional
  location. This section is no longer duplicated in Refs.
- **Lichess API** — create-token link, token field, and the
  `BRETT_LICHESS_TOKEN` override. The token is stored in `references.json`
  only if you save it here.

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

New contributors should read [CONTRIBUTING.md](CONTRIBUTING.md) first. It
covers the Linux prerequisites, build/test commands, secret hygiene, read-only
source rules, and the platform-support caveat.

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
| `src/online.rs` | Public Lichess, Chess.com, and ChessABC imports |
| `src/cloud.rs` | chessdb.cn, Lichess opening explorer, and Lichess cloud eval |
| `src/domain.rs` | Game tree, PGN parsing, move lines, figurines, NAG palette, clocks |
| `src/engine.rs` | Asynchronous UCI process, handshake, options, parser, worker threads |
| `src/book.rs` | Native Polyglot, ChessBase, Arena, and Shredder opening-book readers |
| `src/book/` | Shredder `.bkt` reader implementation |
| `src/cbv.rs` | ChessBase `.cbv` container extraction into a classic CBH library |
| `src/recent.rs` | In-memory recent-game trees and move-trend statistics |
| `src/session.rs` | Editable ordered session, database views, reorder/merge, PGN export |
| `src/settings.rs` | Persistent engine, cloud, reference-database, display, and preparation settings |
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
- **Moves-panel interaction polish (done).** PGN fields hide when absent or
  empty, **Show empty** reveals the standard metadata, Board is written as
  `round.board`, branches open a keyboard chooser, engine/cloud lines use real
  move numbers, and open-game move hover is an opt-in setting.
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
- **Cloud references (done).** The **Refs** tab configures the current/reference
  database and online opponent lookups; the **Tree** tab shows the Lichess
  opening explorer (Masters by default, full Lichess optional) and Lichess top
  games. The **Cloud Engine** in the Engine tab combines chessdb.cn and Lichess
  cloud evaluations with clickable Lichess variations. All requests are
  asynchronous and cached per position.
- **UI and release polish (done).** Cloud Engine columns, average-eval sorting,
  top-eight/More moves, Cloud-only guards, multi-section right panel, My player
  names and auto-orientation, persistent FEN strip and full-move counter,
  inline WhiteElo/BlackElo fields, compact PGN tags, title badges, multiple
  ratings, ICCF notice, and secret-safe contribution rules.
- **Permissive dependency path.** Replace the GPL-licensed `shakmaty` and
  `pgn-reader` crates with permissively licensed chess-rules/PGN
  implementations, or deliberately relicense Brett as GPL. Until then, built
  binaries are GPL-3.0-or-later; see
  [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md#gpl-licensed-chess-and-pgn-crates).
- **M2b — Mega-scale ingest.** External sorting, an on-disk header table, and
  incremental updates for multi-million-game databases.
- **Database editing and export (in progress).** Drag rows to reorder the
  current list, drag games between internal database views, and drag a
  database onto another to merge it. **File → Save database as PGN…** writes
  the session in its current order, preserving headers, comments, clocks,
  NAGs, and variations; exports estimated at 100 MB or more ask for
  confirmation first. The SCID and classic CBH writers remain.
- **Opening-tree files and reference sources (done).** A separate PGN, SCID,
  or classic CBH opening-tree database can be indexed for the Tree tab and
  saved as the default. The Refs tab can point preparation at the current
  database, a Mega-style reference database, or a Lichess/Chess.com/ChessABC
  account. Native Polyglot `.bin`, ChessBase `.ctg`, Arena `.abk`, and
  Shredder `.bkt` books are supported.

The detailed queue and acceptance criteria are in
[docs/BACKLOG.md](docs/BACKLOG.md).

## Data safety

Source PGN, SCID, and classic CBH collections are **read-only**. Brett opens
them for reading only. Derived indexes and imported games go to
`$XDG_CACHE_HOME/brett/`, and personal preferences go to
`$XDG_CONFIG_HOME/brett/`; nothing is written next to a source database. PGN
exports default to the home directory, refuse to overwrite the open source
file, and refuse targets inside the protected source roots. The conventional
`$HOME/Chess Databases` and `$HOME/Downloads/CBH` directories are protected
automatically when they exist; add more with `BRETT_READ_ONLY_ROOTS`.

The Lichess reference token, if configured, lives in `references.json` in the
config directory, or in `BRETT_LICHESS_TOKEN`. Do not commit either. The ICCF
database is not redistributed by Brett; see [ICCF_NOTICE.md](ICCF_NOTICE.md).
See [docs/DATA_SAFETY.md](docs/DATA_SAFETY.md) and
[CONTRIBUTING.md](CONTRIBUTING.md#secrets-and-personal-settings).

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

Brett's original application code is **MIT** — see [LICENSE](LICENSE). The
piece artwork remains CC BY-SA 3.0, and the font remains under its own licence.

**Important:** Brett currently links `shakmaty` and `pgn-reader`, both
GPL-3.0-or-later. A built binary is therefore a combined work under
GPL-3.0-or-later, even though the original Brett code is MIT. Do not describe
a built binary as MIT-only. Replacing those crates is on the backlog; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md#gpl-licensed-chess-and-pgn-crates).
