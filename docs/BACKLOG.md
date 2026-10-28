# Implementation queue

This is the working backlog after the preparation stage. Items are ordered by
dependency and user value, not by estimated effort. Completed work is recorded
in `README.md`; this file tracks what is next.

**Status (2026-10-28):** items 1–7 and 11–14 are implemented, and the latest
moves-panel interaction polish is complete. Item 8's cloud, opening-tree,
reference-source, and native-book parts are done; M2b mega-scale ingest remains,
with the first safety pass now keeping huge archives responsive and avoiding
automatic full indexing. Item 9's editable session, database views,
reorder/merge, PGN export, and native SCID writer are done; the classic CBH
writer remains. Item 10 is no longer blocked: the current ICCF SCID source is available
and the automatic correspondence reference is integrated; only an optional
refresh remains. Deferred predictor refinements are recorded under item 6, and
native player-title display is queued as item 15. Item 18 records the
recently-trending move-popularity idea, including its graphical display.

**Decision log (2026-10-28):**

- **Item 17:** ask the upstream author for a permissive/dual licence first;
  make no dependency or licence change until a written answer is recorded.
- **Item 3:** complete the original scope — clocks after each move in the move
  list and the side-to-move clock in the header, in addition to the board
  strips.
- **2CBH:** intended, but blocked on a sample file. Treat it as a new container
  format, separate from the validated classic multi-file CBH reader/writer.
- **Item 19:** request written redistribution permission from the Scid authors;
  keep the pack local-only and out of releases until then.
- **Chessable:** deferred; the local note is too ambiguous to schedule.
- **Process:** this file remains authoritative for code status; the Obsidian
  compiled backlog drives priority. The CBH/SCID provenance audit runs before
  the classic CBH writer.

Game review follow-ups that are intentionally deferred: position-index book
marking and review caching. Clock display is complete: board strips, the move
list, and the game header all follow the same **Clocks** toggle.

The latest interaction pass hides absent or empty PGN fields by default, adds
**Show empty** and a virtual **Board** field written as `round.board`, opens a
mainline-first branch chooser at every multi-move position, numbers engine and
cloud analysis lines, and makes open-game move hover an opt-in setting.

## 1. UCI engine foundation

**Status:** implemented. The **Engine** tab accepts any UCI executable, exposes
Threads and Hash/RAM, persists the selection under `$XDG_CONFIG_HOME/brett/`,
and reuses the same engine settings for Game Review.

**Goal:** use the locally installed Stockfish for analysis without blocking the
UI.

- Detect `/usr/bin/stockfish` and allow an explicit engine path in settings
  (`BRETT_ENGINE` or a file picker).
- Spawn the engine with piped stdin/stdout (never through a shell), perform the
  UCI handshake, wait for `readyok`, and apply `Threads`, `Hash`, and `MultiPV`
  when advertised by the engine.
- Analysis panel for the current position: start/stop, depth/time limit,
  centipawn and mate scores, principal variation, MultiPV lines.
- Clicking a PV move plays it. Results for an old position must be discarded.
- Kill the engine cleanly on app exit and show protocol errors in the status
  line.
- Tests use a fake UCI process so CI does not depend on Stockfish.

**Depends on:** nothing. **Unblocks:** game review.

## 2. Game Review with the local engine

**Status:** implemented in `src/review.rs` and the **Review** tab. The optional
NAG write-back is included; book marking and caching are deferred.

**Goal:** annotate and grade a complete game.

- Analyze the mainline with a selectable depth or time per move, with progress
  and cancel.
- Record the evaluation before and after every move, the engine's best move,
  and centipawn loss.
- Classify moves (for example: best, good, inaccuracy, mistake, blunder) using
  documented thresholds and special handling for mates and forced moves.
- Show an evaluation graph, accuracy summary, and a clickable move-quality list.
  At minimum, review output is display-only; optionally write `?!`/`?`/`??`
  NAGs back into the in-memory game.
- Use the position index to mark book/opening moves where meaningful.
- Cache reviews by game content and engine settings if the analysis is slow.

**Depends on:** UCI engine.

## 3. Clock display

**Status:** implemented. The hidden-by-default **Clocks** toggle shows the
remaining time after each move, the side-to-move time in the game header, and
the latest time for each side in the board strips.

**Goal:** show clock information already present in PGN comments.

- Parse `[%clk H:MM:SS]` (and, if useful, `[%emt ...]`) from move comments.
- Add a **Clocks** toggle; it is hidden by default.
- When enabled, show the clock after each move in the move list and the side to
  move's remaining time in the header. Missing or malformed clock tags are
  simply absent, never guessed.
- Keep the human comment text separate from the machine clock token.
- Add PGN fixtures for increments, missing clock tags, and partial clocks.

**Depends on:** nothing.

## 4. Castling by dragging the king

**Goal:** both common drag gestures castle.

- Dropping the king on its castling destination (`g1`/`c1`, `g8`/`c8`) must
  castle, in addition to dropping it on the rook.
- Update target highlighting and drop handling for both colors and both sides.
- Add tests for king-to-destination, king-to-rook, illegal destinations, and
  drag cancellation.

**Depends on:** nothing.

## 5. Preparation view and recent games

**Status:** implemented. The preparation panel shows combined likelihood/tree
rows on the opponent's turn, the prepared player's line history on the user's
turn, the normal opening tree at all times, and a configurable recent-games list
whose rows open a read-only preview without losing preparation state.

**Goal:** never show an empty preparation panel when it is the user's move.

- At the opponent's turn, keep the likelihood table, but add the normal tree
  columns (games, W/D/L, score, Elo, years) so prediction and observed results
  are visible together.
- At the user's turn, show the prepared player's moves so far in the current
  line: move, date, tree score, and the likelihood that move had when it was
  predicted.
- Always show the normal opening-tree table for the current position, even in
  preparation mode.
- Add a collapsible **Recent games** window listing the most recent N games
  (default 10, configurable) that reached the current position. Sort newest
  first and make rows clickable.
- Clicking a recent game opens a read-only game view without destroying the
  preparation state; closing it returns to the same preparation position.

**Depends on:** player book and prediction model.

## 6. Predictor v2 and a static reference distribution

**Status:** implemented. Predictions now report posterior means and approximate
90% credible intervals, reserve probability for unobserved legal moves, scale
background strength with sample size, and use an optional separately cached
rating-filtered reference index with a visible fallback. `predictor_eval`
reports held-out log loss, Brier score, top-k accuracy, and top-1 calibration
against frequency and Elo-filtered frequency baselines. Color and
transpositions are handled through the player book and position keys, and move
popularity through the background counts. Time-control, event/era,
opponent-rating, local-engine, and fully incremental reference-refresh features
remain deferred; a changed reference source rebuilds the cached window.

**Goal:** make preparation probabilities honest, calibrated, and less dependent
on the loaded player database.

### Probability model

- Replace the single point estimate with a posterior mean and an uncertainty
  interval. A move seen in 2/2 games must not display as 100%.
- Start from a hierarchical/conjugate model: player repertoire + static
  background + recent form, with explicit sample-size shrinkage.
- Add observable features where they help: color, time control, event/era,
  opponent rating, move popularity, transpositions, and a local-engine signal.
- Report calibrated confidence, not just a percentage. Add held-out evaluation
  using log loss, Brier score, top-k accuracy, and calibration curves against
  frequency and Elo-only baselines.
- Keep the fallback to the database prior visible, and never imply certainty
  from a tiny sample.

### Static background distribution

- Load a separate **reference database** independently of the player's
  database. Prefer Mega Database, the Lichess masters database, or a
  rating-filtered Lichess corpus.
- For rating-filtered data, select a band around the player's rating (roughly
  ±200 Elo, with an explicit inflation mapping) rather than using all games.
- Cache the reference position index separately and key it by the reference
  source; never confuse it with the player's index.
- If no reference database is configured, fall back to the current database
  with a clear warning that the prior may be skewed.
- Keep reference indexing incremental and optional so opening a normal
  database does not require a second multi-gigabyte build.

**Depends on:** current player book. **Unblocks:** reliable preparation.

## 7. Classic CBH database support

**Status:** implemented in `src/cbh/` and wired into the normal database
opening path. The native reader is read-only and works behind `GameLibrary`,
so the Games, Players, Tree, and position-index paths need no format-specific
UI. Format details and validation results are in [CBH.md](CBH.md).

**Goal:** read the older multi-file ChessBase CBH format (not CBH2/2CBH).

A local sample classic multi-file database was used during development (under
the git-ignored `fixtures/local/` tree; never redistributed). It is a classic
multi-file database (`.cba`, `.cbb`, `.cbc`, `.cbe`, `.cbg`,
`.cbgi`, `.cbh`, `.cbj`, `.cbm`, `.cbp`, `.cbs`, `.cbt`, `.cbtt`, `.flags`,
`.ini`), not a 2CBH container.

**2CBH:** the local note that says "cbh (2cbh)" is confirmed to mean a real
2CBH container is wanted, but the work is blocked until a sample file is
available. Do not extend this validated classic reader speculatively; 2CBH is
a separate format reader/writer with its own fixtures and provenance.

Implemented:

- Open the `.cbh` index and its `.cbg` moves, `.cba` annotations, and
  `.cbp`/`.cbt`/`.cbc`/`.cbs` name files directly. A `.cbj` wide index is
  consulted when the move or annotation file exceeds 4 GiB.
- Decode move-encoding modes 0, 4, 5, and 10 natively, including explicit
  start positions, Chess960 castling, promotions, en passant, null moves,
  recursive variations, and the stored node order used by annotations.
- Decode text comments, NAGs, coloured squares and arrows, time spent, engine
  evaluations, and medals into the normal game tree. Drawing tokens are kept
  as PGN comment text, the same form ChessBase's export uses.
- Skip mode-11 games with a visible error and show the header only.
- Reject malformed headers, records, and entity files without writing to the
  source database.

Validation:

- The full personal sample opens as **24,865 games** and **2,003,054 nodes**
  with **zero decode errors**.
- A full structural export comparison against the independent MIT `cbvault`
  reader found no mismatches in tree paths, SAN, or NAGs across all games.
- All **168 annotated games** match `cbvault` for comments and NAGs after the
  drawing/evaluation tokens are included.
- Header values match except 90 names where `cbvault`'s current name-buffer
  path drops a final non-ASCII character; Brett preserves the byte-decoded
  name. Brett also keeps an `Annotator` tag for 134 games that `cbvault` does
  not emit.
- A synthetic byte-level writer in `src/cbh/tests.rs` covers headers, names,
  modes 0/4/5/10, explicit starts and Chess960, variations, comments, NAGs,
  drawing/evaluation tokens, empty move records, wide `.cbj` offsets,
  deleted/mode-11/truncated games, bad headers, and position indexing. No
  personal database is committed.

**Depends on:** format research (complete). **Unblocks:** the classic-database
part of the roadmap.

## 8. Larger infrastructure

- **M2b mega-scale ingest:** external sorting, on-disk header table, incremental
  position-index updates. The Mega Database 2025 test (11,151,119 games) exposed
  the current limits: eager header/session/player work used roughly 23 GiB,
  the automatic position index ran at about 2,000 games/sec, and the desktop
  became unresponsive. Immediate safety work now interns repeated CBH header
  strings, keeps primary/external session slots lazy, moves the player
  directory off the UI thread above 500,000 games, and refuses automatic full
  indexing above 250,000 games (manual full indexing is disabled above
  1,000,000). The remaining work is a true on-disk header table, external-sort
  position index, and incremental updates so a multi-million-game archive can
  be indexed without a multi-gigabyte RAM spike.
- **Cloud references (done):** chessdb.cn `queryall`, the Lichess opening
  explorer (Masters by default; full Lichess with an OAuth token), and Lichess
  cloud evaluation, with asynchronous requests and per-FEN caching. The Tree
  tab shows the explorer table and Lichess top games; the Cloud Engine in the
  Engine tab shows cloud variations. The Refs tab configures sources rather
  than duplicating either table. See `src/cloud.rs`.
- **Cloud Engine (done):** the Engine tab switches between **Local**,
  **Cloud**, and **Both**. The Cloud Engine table combines chessdb.cn and the
  Lichess cloud evaluation, includes Lichess variations, and supports clickable
  SAN prefixes. The Tree tab shows a Lichess Masters/full reference table.
- **Opening-tree files (done):** the Tree tab can index a separate PGN, SCID,
  or classic CBH database and persist it as the default opening-tree source.
  Native Polyglot `.bin`, ChessBase `.ctg`, Arena `.abk`, and Shredder `.bkt`
  readers now live in `src/book.rs` and `src/book/`.
- **Reference sources (done):** the Refs tab exposes the current database and
  a Mega-style reference database path, plus Lichess/Chess.com/ChessABC
  opponent lookups (each service listed once). Importing is **File → Import**.
  The correspondence database is configured in **Settings**, not Refs.
- **Reference merging:** combine several databases into one opening tree
  without rebuilding unrelated caches.

## 9. Database editing, copy/merge, and export

**Status:** the editable ordered session (`src/session.rs`) provides stable row
IDs, lazy primary/external/local slots, reordering, duplicate matching, and
merging. Games can be reordered by dragging rows in the **Games** list;
dropping on the upper or lower half of a row inserts before or after it. Extra
databases open in the floating **Database views** window. Games can be dragged
between the main list and a view, with an **Insert here** / **Append** prompt;
dragging a database handle onto another database merges it. Merge rejects
duplicates with order-insensitive, transliterated player-name matching plus
first-move verification. External file drops ask whether to open, merge into
the current database, or open in a view. **File → Save database as PGN…**
streams the session in order, preserves comments, clocks, NAGs, and
variations, and asks for confirmation before exports estimated at 100 MB or
more. The native SCID4 writer is implemented and validated against SCID 4.27;
the classic CBH writer remains.

- Add the native classic CBH writer. The readers are independent and validated;
  the writer must round-trip through the reader and, where available, an
  independent implementation.
- **2CBH writer (separate):** blocked on a sample file. Do not fold 2CBH into
  the classic CBH writer; the containers and validation fixtures are different.
- **SCID writer (done):** native SCID4 export from **File → Save database as
  SCID…** builds the compressed namebase and stable IDs, encodes index
  flags/offsets, inverts the compact move stream, preserves
  comments/NAGs/variations and custom starts, and atomically replaces the
  `.si4`/`.sn4`/`.sg4` triple with rollback. Round-trips through `ScidLibrary`
  and opens in SCID vs. PC 4.27 (`scidt`, `scidpgn`) with all moves and
  annotations intact.
- Keep the documented drag-and-drop semantics:
  - games dragged between database views are copied to the drop target;
  - a game dropped at a list position asks whether to insert there or append;
  - a database dropped onto an open database is merged;
  - external file drops ask whether to open, merge, or open in a view.
- Never write over a source database without an explicit Save As path. Keep
  the conventional `$HOME/Chess Databases/` and `$HOME/Downloads/CBH/` sources
  read-only (and any extra roots in `BRETT_READ_ONLY_ROOTS`).
- Rebuild player and position indexes after a persistent mutation.

## 10. Correspondence database refresh

**Status:** source available and integrated. The current ICCF SCID database is
redistributed in `databases/iccf/Correspondence_2350.si4` (with its `.sn4` and
`.sg4` companions); the automatic Engine-tab correspondence reference is
implemented, and the file decodes with zero errors. See
[CORRESPONDENCE_DATABASE.md](CORRESPONDENCE_DATABASE.md).

- Current source: **87,927 games**, **6,187,801 mainline moves**, dated
  2021-01-01 through 2026-09-25, all `Site "ICCF"`. Both Elo fields range from
  **2350 to 2724**; every game has both ratings and at least one move. The accompanying note says the
  ICCF January–March 2026 compilation is still missing.
- The snapshot is optional but encouraged, carries the ICCF non-sale notice,
  and is refreshed between releases. The three files must always be replaced
  together.
- The current filter is inclusive `WhiteElo >= 2350` and `BlackElo >= 2350`;
  there is no evidence of an upper cutoff.
- The older 2016 sample is 24,865 games (2009-02-15 through 2016-02-08) with an
  inclusive `Elo >= 2400` filter and a mix of ICCF, LSS, FICGS, and Remote
  games. It is retained as historical provenance, not as the active reference.
- A future refresh only needs a newer ICCF export, the same two-sided 2350
  filter, de-duplication by players/date/result/move sequence, and a new output
  triple. Never overwrite the source or the old 2016 sample.

## 11. Native opening-book formats and Tree integration

**Status:** Polyglot `.bin`, ChessBase `.ctg`, Arena `.abk`, and Shredder
`.bkt` readers are implemented and validated against the sample books. The Tree
tab exposes all four formats through the opening-book path, including BKT
move-quality letters in the Note column. Env-gated fixture tests cover every
format at the start position, including legal moves and W/D/L consistency.

Development used local `.ctg`, `.bin`, `.abk`, and `.bkt` samples from the
user's book collection. They are not part of the repository and must not be
committed; point the Tree/opening-book picker at your own copies.

- Read Arena `.abk` entries (28-byte little-endian records, root index 900,
  `nextMove`/`nextSibling` tree links) and build a position-indexed move map.
  Preserve `ngames`, `nwon`, `nlost`, priority, and promotion.
- Read Shredder `.bkt` as a 256-bucket Zobrist hash table plus appended
  overflow buckets. Each 1,552-byte bucket has 32 slots of 48 bytes and a
  16-byte footer. Decode the two hash words, packed move (from/to/piece/
  capture/promotion), quality nibble, rating/year sums, and White-perspective
  W/D/L totals. Use the format's fixed a1-first Zobrist tables.
- Open `.bin`, `.ctg`, `.abk`, and `.bkt` through the Tree tab’s book path.
  Show move, SAN, games/weight, score, and book-specific annotations. Keep the
  local database tree and Lichess references available beside the book.
- Add a native “Book” section to the Tree tab with Load/Choose/Current DB
  controls, persisted path, and a clear empty-book state. Do not block the UI
  while a large book is mapped or indexed.
- Add fixture tests for every format and a legal-move/score sanity check at the
  start position and at least one transposition.

**Depends on:** `src/book.rs`. **Unblocks:** book-based preparation and
position-index book marking.

## 12. Preparation mode refinements

**Status:** implemented. Preparation starts with the board oriented to the
prepared player's side, with the name-priority list taking precedence. The
left-hand game list filters to the prepared player's relevant color, the
right-hand recent-games panel follows the current position and persists a
1–50 limit, and ChessABC's latest games can be loaded on demand. Recent-game
rows remain read-only previews and preserve the preparation node, tab, and
orientation.

- **Board orientation:** when preparation starts, flip the board to the side the
  prepared player will play (or the user’s side, per the explicit setting). Do
  not inherit the previous game’s orientation.
- **Player-name priority list:** add a SCID-like ordered list of player names in
  settings. When a prepared player matches an entry, always orient the board to
  that player’s side; earlier entries win ties. Persist the list with
  preparation settings and show the resolved orientation in the prep header.
- **Left-hand game list:** while preparing as Black, show only the prepared
  player’s White games in the player’s game list. While preparing as White,
  show only their Black games. Outside preparation mode, keep both colors.
- **Current-position recent games:** make the right-hand “Recent games” list
  reliably show games that reached the current position. Record the position
  after every move as well as before it, handle transpositions/repetitions, and
  add a regression test using a real `PlayerBook`.
- **Configurable maximum:** persist the maximum number of recent games (default
  10, range 1–50) in display settings and apply it to both the list and the
  “showing x of y” label. The list must be newest-first and filter to the
  prepared player’s color.
- Keep recent-game rows read-only and preserve the preparation node, tab, and
  board orientation when the preview closes.

**Depends on:** `PlayerBook` and preparation settings.

## 13. Cloud analysis depth and ChessABC

**Status:** Lichess depth/full-line display, ChessABC import/reference search,
and the ChessABC preparation list are implemented.

- **Lichess depth:** show the Lichess cloud-evaluation depth as part of the
  analysis itself, not only as a separate caption. Include it in the Cloud
  Engine header/rows and in the Lichess variation block so a line is never
  presented as an unqualified evaluation. Keep chessdb and Lichess scores
  visibly distinct.
- **Full Lichess line:** display the complete principal variation in Engine →
  Cloud Analysis → Lichess Line, with hover previews and click-to-play for each
  prefix. Do not truncate to six plies.
- **ChessABC API (investigated):** ChessABC exposes a public JSON API at
  `https://api.chessabc.com`. Relevant endpoints found in the web client:
  - `GET /players/search?name=...` — player search, returns FIDE id, name,
    country, title, rating, and slug.
  - `GET /games/player/paginated?fideId=...&page=N&size=M` — a player’s games.
  - `GET /games/paginated?page=N&size=M` — recent games across the database.
  - `GET /games/{id}` — one game, including PGN-like `moves`.
  Responses include event, site, round, players, titles, FIDE ids, Elo, result,
  ECO, date, and a SAN move string. The corpus is very current (sample response
  dated October 2026). There is no authentication requirement for these reads.
- **ChessABC import/reference (done):** `OnlineSource::ChessAbc` searches by
  player name (comma-normalized for the API) or FIDE id, picks an exact name
  match or the highest-rated candidate, pages through the latest games, and
  converts them to PGN with Elo, title, FIDE-id, ECO, date, and FEN tags.
- **ChessABC preparation (done):** Prep Mode can load the prepared player’s
  latest ChessABC games on demand, filters them to the side being prepared,
  respects the recent-game limit, and opens them as read-only previews.
- Keep Lichess and Chess.com behavior unchanged. Do not scrape the web client
  when the JSON API is available.

**Depends on:** `src/online.rs` and `src/cloud.rs`.

## 14. Moves-panel interaction polish

**Status:** implemented and covered by tests.

- **PGN tag visibility:** absent and empty fields are hidden by default. The
  **Show empty** toggle reveals Event, Site, Date, Round, Board, White,
  WhiteElo, WhiteTeam, Black, BlackElo, BlackTeam, Result, ECO, and Opening.
  Empty placeholders (`""`, `"?"`, and `"????.??.??"`) count as absent.
- **Board field:** Board is hidden when the Round tag has no board part. It
  appears after **+ Add tag → Board**, stays visible while empty, and is
  written into the Round tag as `round.board`; a separate Board header is
  folded back into Round on save.
- **Branch chooser:** `→` at a position with more than one continuation opens
  a popup instead of silently following the mainline. The mainline is selected
  first and variations follow by most recent use in the session. `↑`/`↓`
  choose, `Enter`/`→` play, `1`–`9` pick directly, and `Esc`/`←` cancel. The
  chooser also works inside variations.
- **Numbered analysis lines:** engine, Lichess, and cloud variations show real
  move numbers such as `25... e5 26. Nf3 Nc6` and are labelled **Engine PV**,
  **Lichess PV**, or **Lichess line**, so line numbering cannot be mistaken for
  a chess move number.
- **Move-hover setting:** **Settings → Preview moves on hover** controls only
  the open game's move list and is off by default. Tree, engine, reference, and
  read-only preview hovers remain enabled.

**Depends on:** `src/app.rs`, `src/pgn.rs`, and `src/settings.rs`.

## 15. Native player titles

**Status:** PGN `WhiteTitle`/`BlackTitle` badges are implemented in the board
strips and the Moves header. CBH/SCID title-field decoding and the wider
surfaces below remain.

- Decode the PGN `WhiteTitle`/`BlackTitle` tags and classic CBH/SCID title
  fields natively, including FIDE titles (GM, IM, FM, CM, WGM, WIM, WFM, WCM)
  and common historical or correspondence abbreviations.
- Show the title as a compact badge immediately before the player name in the
  board strips, game list, player directory, preparation header, and exported
  display contexts, using the same visual hierarchy as chess.com.
- Keep the raw title tag intact when editing and exporting; display decoding
  must not rewrite the stored value.
- Fall back to no badge when the title is absent, empty, or unknown, and do not
  infer a title from an Elo rating.
- Cover PGN, SCID, and classic CBH sources with unit tests, including women's
  titles and abbreviated/unusual spellings.

**Depends on:** `src/domain.rs`, `src/cbh/`, `src/scid.rs`, and the shared
player-name rendering helpers. **Unblocks:** consistent titled-player display
across preparation and game browsing.

## 16. FIDE ID presentation

**Status:** planned.

- Find a pretty, compact way to show a player's FIDE ID beside their name when
  the database or import provides a `WhiteFideId`/`BlackFideId` tag.
- Keep the ID secondary to the name and title: prefer a small muted suffix or
  hover/expanded detail rather than a second full-width column in the game
  list and board strips.
- Use the same presentation in preparation, ChessABC game lists, imported-game
  previews, and database views. Preserve the raw tag when editing/exporting.
- Do not fabricate an ID when the source only supplies a name; FIDE IDs must
  come from a real tag or resolved ChessABC/FIDE lookup.

**Depends on:** `src/domain.rs`, `src/online.rs`, and the shared player-name
rendering helpers. **Related:** item 15 (native player titles).

## 17. Permissive chess and PGN stack

**Status:** decision pending. Brett currently links `shakmaty` (chess rules,
SAN, FEN) and `pgn-reader` (streaming PGN) under GPL-3.0-or-later. The original
Brett code is MIT, but a built binary is a combined GPL work. The 2026-10-28
decision is to ask the upstream author for a permissive/dual licence first and
to make no dependency or licence change until a written answer is recorded.
See `THIRD_PARTY_NOTICES.md` and the option-by-option analysis in
[`GPL_OPTIONS.md`](GPL_OPTIONS.md).

- Send the upstream request; record the answer and date in
  `GPL_OPTIONS.md`.
- If granted, update the dependency/licence notes and keep the permissive
  build.
- If refused, choose between adopting GPL-3.0-or-later for the combined binary
  and funding the `cozy-chess` + streaming-PGN replacement.
- Preserve legal-move correctness, SAN/FEN round-tripping, Chess960/custom
  starts, streaming PGN performance, and the existing large-database guards.
- Keep any replacement behind the current domain types so the UI and database
  readers do not depend on a particular chess crate.
- Add regression tests against the public fixtures and the SCID/CBH readers.
- While the decision is pending, do not describe a distributed binary as
  MIT-only if it still links the GPL crates.

**Depends on:** `src/domain.rs`, `src/pgn.rs`, `src/position.rs`, and
`src/library.rs`.

## 18. Recently trending move popularity

**Status:** planned.

**Goal:** surface opening ideas whose popularity has recently picked up, rather
than only showing all-time frequency, and make the trend visible at a glance.

- Compute move popularity in rolling time windows from game dates (for example
  the last 6, 12, and 24 months) and compare each move's recent share against
  its longer-term baseline at the same position.
- Use a statistically honest trend signal: require a minimum recent sample,
  shrink small samples toward the baseline, and show the sample counts beside
  the trend so a 2-game spike is never presented as a trend. Prefer a
  score/percentage-point delta plus a confidence indication over a bare
  "up/down" arrow.
- Distinguish a genuinely new idea (recent share high, historical share near
  zero) from a revival (historical share meaningful, recent share rising).
- Show the trend graphically in the Tree and moves panels: a compact sparkline
  of share by time bucket per move, plus a trend column/badge (rising, steady,
  falling, new). Hovering or expanding a row should reveal the bucket values and
  counts. Keep the existing all-time games/W-D-L/score columns intact.
- Make the time window, minimum sample, and baseline period configurable, with
  sensible defaults; persist them with the other tree/display settings.
- Use the position index (including game dates) and, when configured, the
  reference database, so trends are computed from a stable corpus rather than
  only the currently loaded player database.
- Handle sparse data explicitly: no trend badge when there is insufficient
  history, and a clear "recent" vs "baseline" explanation in the tooltip.
- Add unit tests with synthetic dated games covering a new idea, a revival, a
  declining move, a small-sample false positive, and transpositions.

**Depends on:** the position index and game-date plumbing; related to item 6
(predictor/reference distribution) and the Tree panel. **Unblocks:** trend-aware
preparation and repertoire review.

## 19. Player-photo pack redistribution

**Status:** local loading implemented; redistribution blocked on permission.
The 2026-10-28 decision is to request written permission from the Scid authors;
keep the pack local-only and out of releases until an answer is recorded.

- The optional BPF1 reader (`src/photos.rs`), WebP decoding, texture cache,
  **Settings → Show player photos**, **Settings → Photo size**
  (Small/Medium/Large), Players-list thumbnails, selected-player portrait, and
  the standalone 240×300 viewer are implemented. Name lookup also handles
  accent-folded and short pack keys. The gold world-champion crown, its
  classical/FIDE lineage setting, and the hover explanation of each reign are
  implemented separately in [`CHAMPIONS.md`](CHAMPIONS.md).
- The pack is derived from Scid's `Player_photos.zip`
  (`scid/PlayerData/Player_photos.zip`). The archive has no explicit
  redistribution licence; Scid's `COPYING` identifies database files as
  copyright Pascal Georges and asks users to contact the authors first.
- Send a permission request that names the derived BPF1/WebP pack, the
  intended distribution channel, and the required attribution; record the
  answer and date in `docs/PHOTOS.md` and `THIRD_PARTY_NOTICES.md`.
- `assets/photos.bin` is git-ignored and must not be committed or bundled in a
  release, installer, or AppImage without written permission.
- If permission is granted, record it and decide whether to ship a pack. Until
  then, keep the `BRETT_PHOTOS`/`assets/photos.bin` discovery path and the
  graceful no-pack fallback.
- A separate public-domain/CC-licensed source is an alternative only after its
  licence and attribution requirements are audited.

**Depends on:** `src/photos.rs`, `src/app.rs`, and the photo-pack provenance in
`docs/PHOTOS.md`.
