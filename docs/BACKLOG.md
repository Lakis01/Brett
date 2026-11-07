# Implementation queue

This is the working backlog after the preparation stage. Items are ordered by
dependency and user value, not by estimated effort. Completed work is recorded
in `README.md`; this file tracks what is next.

**Project shape (2026-10-29):** Brett is intended as a long-term,
community-maintained project. Functional correctness, data safety, and clear
documentation come first; visual polish is queued and may land incrementally.

**Status (2026-11-07 backlog sync):** items 1–7, 11–14, and 17 are implemented, and the latest
moves-panel interaction polish is complete. Item 39's preparation/predictor
round-2 batch largely landed 2026-11-07 (opponent-repetition continuation
indexing, `Schnellschach`/J1 classification, `.2lid` accelerator lookup,
uncapped online/ChessABC fetch, composite eval bar with chessdb.cn + DB
aggregate, streaming multi-source prep with cached account checks, book
reference tree, font-colored rows with hover/arrow-key browsing); only
`predict_v2` calibration (adaptive `alpha0`) remains queued. Item 8's cloud, opening-tree,
reference-source, and native-book parts are done; M2b mega-scale ingest remains,
with the first safety pass now keeping huge archives responsive and avoiding
automatic full indexing. Item 9's editable session, database views,
reorder/merge, PGN export, native SCID writer, classic CBH writer, 2CBH writer,
and unified **Save database** / **Save database as…** flow are done. Item 10 is
no longer blocked: the current ICCF SCID source is available and the automatic
correspondence reference is integrated; only an optional
refresh remains. Deferred predictor refinements are recorded under item 6, and
native player-title display is queued as item 15. Item 18's trend backend,
Tree badge column, and Settings controls are implemented; the sparkline and
moves-panel display remain. Items
20 and 21 add native ECO classification and Wikipedia player summaries; item 16
now includes the FIDE-profile link. Item 22 records the player clock-usage
analysis idea for the player profile/preparation view. Item 23 adds the
clickable Site → OpenStreetMap search (settled provider); item 24
records the Open-vs-Load audit: the bottom-bar database picker was removed, so
**File → Open database…** and drag-and-drop are the only ways to open a
database, and the Tree-tab path rows are read-only labels with **Choose…**,
**Clear**, and **Current DB**. Clocks are now shown by default and both the
clock and evaluation-bar toggles live in Settings. The 2026-10-31 pass also
replaced the separate PGN/SCID/CBH save entries with **Save database** and
**Save database as…**, remembers the last database folder, and asks before
closing a modified database. Item 25's Typst target is implemented, including
the `White - Black` default filename and remembered export folder. Item 26
(Set up position) is implemented with board setup, real-time FEN editing, and
legality validation. Item 27 adds country-flag display to the backlog.

**Decision log (2026-10-28):**

- **Item 17:** replace `shakmaty` and `pgn-reader` with MIT components
  (`cozy-chess` 0.3.4, a Brett-owned streaming PGN parser, and a Brett-owned
  Polyglot hash); do not send the upstream licence request. Bump the
  position-index cache version because the hash changes.
- **Item 20 (ECO):** use the CC0 `lichess-org/chess-openings` dataset; classify
  by replaying the game and keeping the deepest named position; write `ECO` and
  `Opening` from that match.
- **Item 21 (Wikipedia):** load an introductory summary automatically when a
  player profile is clicked or opened, never when a database is opened. Cache
  it locally, show attribution and an article link, and add a Settings toggle
  (default on) to disable the lookup. Auto-display only when the match is
  confidently a chess player; otherwise offer a labelled search link.
- **Item 16 (FIDE):** when a real FIDE ID is present, link from the player
  profile to `https://ratings.fide.com/profile/{id}`; never construct a link
  without a real ID.
- **Item 3:** clocks are shown in the board strips only. The move-list clock
  chips and the side-to-move clock in the header are removed; the PGN moves
  window stays clock-free.
- **Board evaluation bar:** optional in Settings and enabled by default. The
  bar uses the highest-depth available local-engine or Lichess cloud
  evaluation; a PGN `[%eval]` is the fallback when neither exists. PGN eval
  directives are decoded into a structured node field and are never rendered
  as comment text.
- **Item 2 (review book and accuracy):** skip only the leading opening-book
  prefix. Prefer the native opening book, then the configured local
  opening-tree file, then the Lichess Masters explorer; a Masters/tree move
  counts as book at a configurable minimum game count (default 3) and the
  prefix is capped (default 30 plies). Book moves do not count toward accuracy.
  Use a Lichess-style win-probability accuracy per color, plus ACPL; forced
  moves are excluded from accuracy.
- **Item 22:** add a player clock-usage analysis brainstorm for the player
  profile/preparation view; no implementation is scheduled yet.
- **2CBH:** the current ChessBase container reader and writer are implemented
  in `src/twocbh/` and wired into Open and Save As. The container-first writer
  covers standard chess, Chess960, headers, names, nested variations, and the
  deleted flag; comments, NAGs, clocks, and evaluations are deferred. Treat
  2CBH as a separate format from classic CBH.
- **Item 19:** request written redistribution permission from the Scid authors;
  keep the pack local-only and out of releases until then.
- **Chessable:** deferred; the local note is too ambiguous to schedule.
- **Process:** this file remains authoritative for code status; the Obsidian
  compiled backlog drives priority. The CBH/SCID provenance audit runs before
  the classic CBH writer.

**Decision log (2026-10-29 UX review):**

- **Crown hover:** already implemented; every champion entry has a reign and a
  factual sentence, and the tooltip is shared by the profile and board strips.
- **Round/Board labels:** full words by default; `R`/`B` only when the header
  runs out of width. Never shorten just because a compact form exists.
- **Forced vs only move:** keep them distinct. NAG 7 stays `□`; NAG 8 now
  renders `◇`.
- **Site map search:** the clickable Site label opens an OpenStreetMap search
  with the URL-encoded value. OpenStreetMap is the settled provider: it needs
  no API key, uses open data, and avoids Google Maps Platform terms. Google
  Maps was rejected because it is less privacy-preserving. Add a provider
  setting only if users ask for both (item 23).
- **Open vs Load:** the typed-path field and **Load** button were removed, then
  the remaining bottom-bar **Open…** quick action was removed too. Database
  opening now lives in **File → Open database…** and drag-and-drop. The
  Tree-tab opening-book and opening-tree rows are read-only path labels with
  **Choose…**, **Clear**, and **Current DB**; their redundant **Load** buttons
  are gone (item 24).
- **Clocks and evaluation bar:** clocks are visible by default. Both the clock
  and evaluation-bar toggles moved from the Board menu and bottom bar into
  **Settings → Display**; the board strips remain the only clock surface.
- **Player photos:** prefer FIDE ID, then conservative full-name matching;
  never attach a photo on an ambiguous short match (item 19).
- **ECO/Opening row:** combine into one editable row with a narrow four-digit
  ECO field and a wide Opening field (item 20).
- **FEN copy:** fixed as a bug: the strip is a single click target and the
  selectable-label click capture is removed (item 14). Manual clipboard
  verification remains.
- **Keyboard line playthrough:** queue the interaction design before coding
  (item 14).

Review caching is implemented: completed reviews are kept in a bounded
in-session cache keyed by the exact mainline, engine path/options, search limit,
and book-prefix length; explicit **Restart review** reruns the analysis.
Book-prefix skipping, the board evaluation bar, PGN `[%eval]` decoding, and the
per-color win-probability accuracy + ACPL metric are implemented
(`docs/BOARD_EVAL_AND_REVIEW.md`).
Clock display is board-only and on by default: **Settings → Display** controls
the board strips, while the move list and game header stay clock-free.

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

**Status:** implemented in `src/review.rs`, `src/review_book.rs`, `src/eval.rs`,
and the **Review** tab. The optional NAG write-back, leading opening-book prefix
skipping (native book -> local opening-tree -> Lichess Masters), per-color
Lichess-style win-probability accuracy + ACPL, and bounded in-session review
caching are included. A completed review is reused only when the mainline,
engine path/options, search limit, and book-prefix length all match; an explicit
**Restart review** invalidates the matching entry.

**Goal:** annotate and grade a complete game.

- Analyze the mainline with a selectable depth or time per move, with progress
  and cancel.
- Record the evaluation before and after every move, the engine's best move,
  and centipawn loss.
- Classify moves (for example: best, good, inaccuracy, mistake, blunder) using
  documented thresholds and special handling for mates and forced moves.
- Skip the leading opening-book prefix. Prefer the native opening book, then
  the configured local opening-tree file, then the Lichess Masters explorer.
  A tree/Masters move counts as book at a configurable minimum game count
  (default 3); cap the prefix at a configurable depth (default 30 plies).
  Book moves do not count toward accuracy.
- Show an evaluation graph, a per-color accuracy summary, ACPL, and a
  clickable move-quality list. Use a Lichess-style win-probability accuracy
  formula; exclude book and forced moves from accuracy.
- At minimum, review output is display-only; optionally write `?!`/`?`/`??`
  NAGs back into the in-memory game.
- Cache reviews by game content and engine settings if the analysis is slow.

**Depends on:** UCI engine.

## 3. Clock display

**Status:** implemented for the board strips. Clocks are shown by default; the
**Show PGN clocks in the board strips** setting in **Settings → Display**
shows the latest time for each side above and below the board. The move-list
clock chips and the side-to-move clock in the header were removed on
2026-10-28.

**Goal:** show clock information already present in PGN comments, in the board
view only.

- Parse `[%clk H:MM:SS]` (and, if useful, `[%emt ...]`) from move comments.
- Add a **Clocks** toggle; it is hidden by default.
- When enabled, show the latest time for each side in the board strips. The
  PGN moves window and the game header stay clock-free. Missing or malformed
  clock tags are simply absent, never guessed.
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

**Status:** implemented (baseline v2). Predictions now report posterior means and approximate
90% credible intervals, reserve probability for unobserved legal moves, scale
background strength with sample size, and use an optional separately cached
rating-filtered reference index with a visible fallback. `predictor_eval`
reports held-out log loss, Brier score, top-k accuracy, and top-1 calibration
against frequency and Elo-filtered frequency baselines. Color and
transpositions are handled through the player book and position keys, and move
popularity through the background counts. Time-control, event/era,
opponent-rating, local-engine, and fully incremental reference-refresh features
remain deferred; a changed reference source rebuilds the cached window.

**Next-gen evolution (queued, design pass done; scheduled for implementation — do not mark done):** *Smart Preparation v2 & Move Predictor v2* (Hierarchical Empirical-Bayes / Dirichlet-Multinomial). Multi-source merge (Current DB + Reference Mega via `.2lgd`/`.cbp`/`.brettacc` + ChessABC one-shot + multiple Lichess/Chess.com accounts per site persisted per `player_key`), cross-source dedup (spine hash first 30 plies + ±3 days/`YYYYMM`), dual Classic Stats vs Smart Predictor modes, separate OTB vs Online pre-filters (TC Classical/Rapid/Blitz/Bullet, date cutoff All/6/12/24/60mo, rated-only, `include_online` default ON, instant `<1ms` refilter), recency `2^(-dt/365)*w_tc` (`1.0`/`0.85`/`0.65`), facing-color left-list filter + Last X games, swapped-color smart threshold (`N>=3` OR `(N>=2 AND (ply>=8 OR W>=1.2))` OR `(ply>=14 AND recent<=12mo)`) with Show N games selection view, Consistency (inverse perplexity) + Booked-up (mean theory exit ply) badges, strict OTB prior (correspondence never; hierarchy Reference->Tree->Masters->CurrentDB+tip; ChessABC games one-shot not per-ply), threshold-capped inferred moves (`>=8%`, max 3, separate section). **Preparation UX (queued — design done, scheduled):** File->Prepare menu with opponent picker; board orientation pinned to user side; ChessABC default-ON with truncated/enumerated name resolution (auto + multi-profile checklist); online accounts exclude ChessABC service; per-account expandable filters with separate Rated+Casual ticks (both default ON) and zero-overlap ⚠️ badge; Chess.com tournament-games-only exclusion; clear prep panel (Predicted-first color-coded sorted Smart table, Played vs Inferred sections, Your-move Times-Faced view, 📖 In PlayerBook badges, ply>=4 swapped gate); game-type color coding (Classical emerald / Rapid-Blitz sky / Online violet) in left + recent lists. Requires dedicated design pass done.
  - **Round-2 fixes (largely landed 2026-11-07; `predict_v2` calibration still queued):** opponent-reply indexing (both colors in `PlayerBook`, `Be3`/`Bc4`/`Bg5`/`f3` cases via edge-deduplicated post-repetition indexing); strict reference tree (never current DB, typo fix). See item 39 for the full prep/predictor round-2 batch.

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

## 7. Classic CBH and current 2CBH database support

**Status:** classic CBH is implemented in `src/cbh/` and wired into the normal
database opening path. The current 2CBH reader and container-first writer are
implemented in `src/twocbh/` and wired into Open, Save As, and the file
filters. Both work behind `GameLibrary`, so the Games, Players, Tree, and
position-index paths need no format-specific UI. `.2cba` annotations remain
deferred. Format details and validation results are in [CBH.md](CBH.md).

**Goal:** read and write the older multi-file ChessBase CBH format and the current 2CBH
container. They are separate formats with separate readers and writers.

A local sample classic multi-file database was used during development (under
the git-ignored `fixtures/local/` tree; never redistributed). It is a classic
multi-file database (`.cba`, `.cbb`, `.cbc`, `.cbe`, `.cbg`,
`.cbgi`, `.cbh`, `.cbj`, `.cbm`, `.cbp`, `.cbs`, `.cbt`, `.cbtt`, `.flags`,
`.ini`), not a 2CBH container.

**2CBH (container-first reader and writer implemented):** the local note that
says "cbh (2cbh)" means the current ChessBase container. The reader in
`src/twocbh/` decodes the 192-byte headers, `.2lid` names, `.2cbg` token
streams, standard and Chess960 starts, castling, promotions, en passant, and
nested variations. It is validated against Update 01 (10,738 games, zero
move/header mismatches), `9601.2cbh` (21/21 mainlines, seven starts), a
16-variation game, and targeted Mega records. The container-first writer in
`src/twocbh/writer.rs` preserves moves, variations, headers, names, custom
starts, Chess960, promotions, en passant, null moves, and the deleted flag.
`.2cba` annotation items and annotation writing remain deferred; comments,
NAGs, clocks, and evaluations are not written. Do not fabricate comments or
fold 2CBH into the classic writer.

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
  1,000,000). **Queued (design done, scheduled):** Mega open freeze fixes — mmap `.2lgd`; entity-bounded accelerator loops; `OnceLock` tournament cache; background `open_library`; custom-start stream skip for game `503255`; terminal tracing diagnostics (`RUST_LOG` phase timings, `>100ms` slow-op warns). The remaining work is a true on-disk header table, external-sort
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
the current database, or open in a view.

**File → Save database** writes the session in its current format and reuses
the current writable target; a new database asks for PGN, SCID4, classic CBH,
or 2CBH. **File → Save database as…** always opens the format chooser. 2CBH is
writable, but saving over the open source is refused, so Save steers to a
writable copy. PGN saves stream the session in order, preserve comments,
clocks, NAGs, and variations, and ask for confirmation before exports
estimated at 100 MB or more. The native SCID4, classic CBH, and 2CBH writers
are implemented; the first 2CBH pass awaits manual ChessBase validation. The
last database folder is remembered for Open and Save As, and closing a
modified database asks whether to save, discard, or cancel; a cancelled save
keeps the window open.

- **Classic CBH writer (done):** native classic `.cbh` export from **File →
  Save database as… → Classic CBH** (`src/cbh/writer.rs`) writes `.cbh`, `.cbg`, `.cba`,
  `.cbp`, `.cbt`, `.cbc`, and `.cbs` (plus `.cbj` when `.cbg` or `.cba` reaches
  4 GiB or `force_wide` is enabled), encodes compact `PieceLists` move streams
  in modes 0/4/5/10, preserves headers, names, comments, NAGs, evaluations,
  drawing/time/medal tokens, variations, custom starts, and Chess960, and
  atomically replaces existing companion sets with rollback. Round-trips
  through both `CbhLibrary` and `cbvault_format::cbh`.
- **2CBH writer (separate, container-first, done):** native `.2cbh`, `.2cbg`,
  `.2cba`, and `.2lid` export from **File → Save database as… → 2CBH**
  (`src/twocbh/writer.rs`) preserves moves, variations, headers, names,
  standard chess and Chess960 starts, castling, promotions, en passant, null
  moves, and the deleted flag, atomically replaces the companion set, and
  removes stale `.2lcd`/`.2lgd` indexes. Comments, NAGs, clocks, and evaluations
  are deferred. Round-trips through `TwoCbhLibrary`; manual ChessBase validation
  uses `examples/twocbh_sample.rs`. Do not fold 2CBH into the classic CBH
  writer; the containers and validation fixtures are different.
- **SCID writer (done):** native SCID4 export from **File → Save database as…
  → SCID4** builds the compressed namebase and stable IDs, encodes index
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

- Current source: **87,927 games**, **5,421,540 mainline moves** after the
  bundled snapshot was capped at **70 plies** (the untruncated export has
  **6,187,801**), dated 2021-01-01 through 2026-09-25, all `Site "ICCF"`. Both
  Elo fields range from **2350 to 2724**; every game has both ratings and at
  least one move. The accompanying note says the ICCF January–March 2026
  compilation is still missing.
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

**Status:** implemented (baseline). Preparation starts with the board oriented to the
prepared player's side, with the name-priority list taking precedence. The
left-hand game list filters to the prepared player's relevant color, the
right-hand recent-games panel follows the current position and persists a
1–50 limit, and ChessABC's latest games can be loaded on demand. Recent-game
rows remain read-only previews and preserve the preparation node, tab, and
orientation.

**Queued evolution:** Smart Preparation v2 extends this with multi-source merge/dedup, dual Classic Stats vs Smart Predictor modes, OTB vs Online pre-filters (instant `<1ms` refilter), facing-color/Last X filters, swapped-color smart threshold + Show N games view, and profile badges. **Preparation UX (queued — design done, scheduled):** File->Prepare menu with opponent picker; board orientation pinned to user side; ChessABC default-ON with truncated/enumerated name resolution (auto + multi-profile checklist); online accounts exclude ChessABC service; per-account expandable filters with separate Rated+Casual ticks (both default ON) and zero-overlap ⚠️ badge; Chess.com tournament-games-only exclusion; clear prep panel (Predicted-first color-coded sorted Smart table, Played vs Inferred sections, Your-move Times-Faced view, 📖 In PlayerBook badges, ply>=4 swapped gate); game-type color coding (Classical emerald / Rapid-Blitz sky / Online violet) in left + recent lists. See item 6 (next-gen evolution) for the full spec. Design pass done; scheduled for implementation — do not mark done.
  - **Round-2 UX/fixes (largely landed 2026-11-07):** opponent banner top; search hidden when selected; ChessABC all-games label; live account `✓`/`✗` confirmations; fresh-start File → Prepare without open DB (`open_library` for reference + extra incl `2cbh`/`cbh`/`si4`, accelerated extraction); merged left + recent lists; Site/TimeControl color fix; `🟢` glyph fix; Your-Move line-grid removal. See item 39.

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
the ChessABC preparation list, and the compact chessdb-only tail in the Cloud
Engine table are implemented.

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
- **Compact chessdb-only tail (implemented 2026-10-29):** when the primary
  Cloud Engine table ends with chessdb-only rows (no Lichess line), fold that
  trailing group into one horizontal compact line showing only the eval and
  move chips. Reuse the existing “More moves” chip renderer; do not give each
  one-ply row its own variation column.
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
- **Click-to-copy FEN (fixed 2026-10-29):** the FEN label is now the click
  target and is no longer selectable, so text selection cannot swallow the
  click; the strip padding also copies. Manual clipboard verification remains.
- **Forced vs only move (fixed 2026-10-29):** NAG 7 (forced move) stays `□`;
  NAG 8 (only/singular move) now renders `◇`. The review’s forced-move `□` is
  unchanged. A visible symbol legend in the moves panel remains queued.
- **Round/Board shortening (implemented 2026-10-29):** **Round** and **Board**
  are shown in full by default; `R`/`B` appear only when the header row would
  otherwise squeeze the event below a usable width.
- **Keyboard line playthrough:** hovering or selecting an engine, Lichess, or
  opening-tree line should let the user step through it with the arrow keys /
  `Enter` without replacing the open game; `Esc` returns to the game cursor.
  This is an interaction design task; capture the exact key map before coding.

**Depends on:** `src/app.rs`, `src/pgn.rs`, and `src/settings.rs`.

## 15. Native player titles

**Status:** implemented (2026-10-30). `src/titles.rs` normalizes PGN/imported
`WhiteTitle`/`BlackTitle` values and the badges now appear in the board strips,
Moves header, game lists, player directory and profile, preparation header,
online reference lists, and database views. The raw tags are preserved. The
classic CBH and SCID audits found no player-title field in either format (see
[CBH.md](CBH.md) and [SCID.md](SCID.md)); an explicit title tag stored in a SCID
game record is still decoded as a custom tag.

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

**Status:** implemented (2026-10-30). `src/fide.rs` validates numeric
`WhiteFideId`/`BlackFideId` tags, `PlayerSummary` aggregates the first real ID,
and a muted `· 1503014` suffix appears in the board strips, game lists, player
directory, preparation, ChessABC/online previews, and database views. The
player profile shows a small **FIDE 1503014 ↗** link to
`https://ratings.fide.com/profile/{id}` only for numeric IDs. Raw tags are
never rewritten and no ID is inferred from a name or rating.

- Find a pretty, compact way to show a player's FIDE ID beside their name when
  the database or import provides a `WhiteFideId`/`BlackFideId` tag.
- Keep the ID secondary to the name and title: prefer a small muted suffix or
  hover/expanded detail rather than a second full-width column in the game
  list and board strips.
- Use the same presentation in preparation, ChessABC game lists, imported-game
  previews, and database views. Preserve the raw tag when editing/exporting.
- Do not fabricate an ID when the source only supplies a name; FIDE IDs must
  come from a real tag or resolved ChessABC/FIDE lookup.
- In the player profile detail view (opened by clicking a player's name or
  portrait), show a **FIDE profile** link when a real ID is available. The link
  opens `https://ratings.fide.com/profile/{id}` in the system browser; validate
  that the ID is numeric before constructing it.
- Make the link visually secondary to the name and title, with a hover
  explanation. If no real ID is present, show no link (not a disabled or
  guessed URL).
- Keep the player-selection and photo-viewer click behavior intact; the FIDE
  link is an explicit action inside the profile, not a replacement for the
  portrait viewer.

**Depends on:** `src/domain.rs`, `src/online.rs`, `src/players.rs`, and the
shared player-name rendering helpers. **Related:** item 15 (native player
titles).

## 17. Permissive chess and PGN stack

**Status:** implemented (2026-10-29). Replaced `shakmaty` and `pgn-reader`
with MIT-licensed `cozy-chess` 0.3.4 behind a Brett-owned chess facade
(`src/chess/`), the Brett streaming PGN reader (`src/pgn/reader.rs`), and the
Brett Polyglot hash (`src/book/polyglot.rs`), and bumped
`src/position.rs::VERSION` to 2. Built binaries no longer link any GPL crates.
See `THIRD_PARTY_NOTICES.md` and [`GPL_OPTIONS.md`](GPL_OPTIONS.md).

- Use `cozy-chess` 0.3.4 (MIT) for legal moves, SAN/UCI helpers, FEN/Shredder
  FEN, Chess960/DFRC, and custom starts. Put it behind Brett-owned facade types
  so the UI, SCID/CBH readers, and database code do not depend on the crate
  directly.
- Replace `pgn-reader` with a Brett-owned streaming parser that preserves
  tags, comments, NAGs, recursive variations, null moves, results, malformed
  games, byte ranges for the lazy PGN index, and large-archive throughput.
- Implement a Brett-owned Polyglot Zobrist hash for `.bin` books. `cozy-chess`
  hashes are not Polyglot-compatible.
- Bump `src/position.rs::VERSION` and rebuild existing position indexes; do not
  silently reuse keys from the old Zobrist table.
- Preserve legal-move correctness, SAN/FEN round-tripping, Chess960/custom
  starts, streaming PGN performance, and the existing large-database guards.
- Add regression tests against the public fixtures, perft positions, the
  SCID/CBH readers, and a large local archive benchmark.
- Update `Cargo.toml`, `Cargo.lock`, `THIRD_PARTY_NOTICES.md`, and the
  provenance notes; the default binary must not link either GPL crate.
- The drafted upstream request is obsolete and must not be sent.

**Depends on:** `src/domain.rs`, `src/pgn.rs`, `src/position.rs`,
`src/library.rs`, and `src/book.rs`.

## 18. Recently trending move popularity

**Status:** backend implemented 2026-10-29; the Tree shows a compact trend
badge, tooltip, and egui sparkline (`src/recent.rs::sparkline_points`/
`baseline_y`, `src/app.rs::draw_trend_sparkline`), and **Settings → Move trends**
exposes the window, baseline, and minimum sample. Moves-panel sparkline/trend
rendering remains planned.

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

## 19. Player Portraits Database: Direct SPF Loading & Lichess Outreach

**Status:** local BPF1 loading implemented; direct SPF ingestion and Lichess outreach pending.
The user intends to contact Lichess regarding obtaining and redistributing a permissively licensed
player photo collection. The photo database is strictly local-only and **must NOT be hosted on GitHub**
(neither in git history nor release assets) until copyright issues are resolved.

- The optional BPF1 reader (`src/photos.rs`), WebP decoding, texture cache,
  **Settings → Show player photos**, **Settings → Photo size**
  (Small/Medium/Large), Players-list thumbnails, selected-player portrait, and
  the standalone 240×300 viewer are implemented.
- **Direct SPF Ingestion:** Inspect and decode player portraits directly from
  `PlayerData/Players.spf` or companion image directories on the fly, eliminating the need to
  pre-convert via Python scripts.
- **Licensing & Outreach:** Reach out to Lichess regarding a permissively licensed photo archive.
- **Strict Distribution Rule:** `assets/photos.bin` and `.spf` archives are git-ignored and must
  never be committed to GitHub or bundled into release assets until licensing is formally resolved.

- **Identity matching (queued 2026-10-29):** the current name-only lookup can
  attach the wrong portrait. Prefer a real FIDE ID when both the game and the
  pack expose one; otherwise use accent-folded, word-order-insensitive full-name
  matching. Never attach a photo on a surname-only or short-key match when more
  than one player could match. Keep a neutral placeholder for unresolved or
  ambiguous players, and offer a local “wrong photo?” override/report action
  rather than silently guessing. Add fixtures for spelling variants, initials,
  transliterations, married names, and deliberate false-positive traps.
- **Crown hover:** already implemented for both lineages; every champion entry
  carries a reign and a factual sentence, and the same tooltip is used in the
  player profile and board strips. Verify the table when adding a champion; no
  implementation work remains for the current roster.

**Depends on:** `src/photos.rs`, `src/app.rs`, `src/players.rs`, and the
photo-pack provenance in `docs/PHOTOS.md`.

## 20. Native ECO classification and Opening tag

**Status:** implemented in `src/eco.rs` and wired into the Moves header card and
**File → Classify openings…** (`src/app.rs`). Uses the CC0
`lichess-org/chess-openings` dataset (`assets/eco/a.tsv`…`e.tsv`, commit
`e76b8b4ec5c8aeeba41dac6caf4a60b938f652db`).

**Goal:** classify each game's opening from its own mainline, like SCID, and
write the matching `ECO` and `Opening` tags. The `ECO` value must be the exact
code for the matched opening, not a family-level approximation.

- Use the CC0 [`lichess-org/chess-openings`](https://github.com/lichess-org/chess-openings)
  dataset (`a.tsv`…`e.tsv`: `eco`, `name`, `pgn`). Record the dataset version or
  commit and the CC0 provenance in `THIRD_PARTY_NOTICES.md`.
- Build a position → `(eco, name)` table from the dataset. Replay the game's
  mainline and keep the deepest position that has a named entry, following the
  dataset's recommended backwards-search classification. Stop cleanly at the
  first illegal move or unsupported position.
- Resolve transpositions deterministically: when several entries share the same
  position, prefer the shortest defining line, then the lexicographically
  smallest ECO code and name. Never emit an ambiguous or invented code.
- Add an explicit **Classify openings** action (current game or all games in the
  session, with a progress summary). Write `ECO` and `Opening` from the deepest
  match. Preserve existing tags unless the user explicitly confirms
  reclassification; do not silently rewrite imported metadata.
- Show the computed classification in the game header even when the tags are
  absent, but only write tags through the explicit action. Keep the raw tags
  intact when editing and exporting.
- Handle custom starts, Chess960, missing move text, illegal moves, and games
  that leave the dataset without a match: leave existing tags unchanged and
  report “unclassified” rather than guessing.
- Add tests for the Sicilian Najdorf, Ruy Lopez, Queen's Gambit, a transposition,
  a custom-start game, an illegal move, and preserve/overwrite behavior for
  existing `ECO`/`Opening` tags.
- Classify the mainline only; variations do not change the game's opening.
  Classification must be O(plies) per game and must not require the position
  index.
- **PGN tags layout (implemented 2026-10-29):** ECO and Opening share one
  editable row, like White/WhiteElo. The ECO field is fixed narrow enough for
  four characters and Opening takes the remaining width. The game header
  already combined them; the PGN tags card now matches.

**Depends on:** item 17 (rules/PGN backend) for SAN replay and position keys;
`src/domain.rs`, `src/pgn.rs`, and the game-header UI. **Unblocks:** ECO-based
filtering, preparation, and opening reports.

## 21. Wikipedia player summaries

**Status:** implemented in `src/wiki.rs` and the player profile. Summaries load
automatically when a profile opens; opening a database or building the player
directory never contacts Wikipedia. The Settings toggle defaults to on.

**Goal:** show a short introductory Wikipedia summary in the player profile
beside the photo, similar to Navidrome's MusicBrainz artist biography, without
shipping or redistributing Wikipedia content.

- Fetch on demand from the Wikipedia REST summary endpoint (or the MediaWiki
  API) using the player's name, and cache the result locally keyed by the
  resolved article title. Include a descriptive User-Agent, respect rate limits,
  and keep an offline/no-network fallback.
- Only display a summary automatically when the match is confidently a chess
  player (for example the article description/occupation mentions chess).
  Otherwise offer a clearly labelled Wikipedia search link instead of showing a
  possibly wrong person's biography.
- Show the summary text, a “Wikipedia” attribution link, and the article link.
  Preserve the CC BY-SA attribution; do not bundle article text in the binary or
  redistribute a Wikipedia dump.
- Keep the photo, name, title, FIDE link, and summary visually coherent; the
  summary must not push the photo or the game record out of the profile.
- Add a setting to disable network lookups. Never fetch during bulk import or
  for every player in a large database; fetch only for the selected profile.
- **Resolved UX:** clicking or opening a player profile starts the lookup
  automatically, showing a spinner while it runs. A confident chess-player match
  displays a short excerpt with a Wikipedia article link and CC BY-SA 4.0
  attribution. An ambiguous name shows a clearly labelled **Search Wikipedia**
  link instead of a possibly wrong biography. A network failure shows the error,
  a retry button, and a search link. The Settings **Player profiles** toggle can
  disable the automatic lookup; the profile then shows a muted disabled notice.
- The cache is keyed by resolved article title, remembers ambiguous names, and
  is bounded to 256 entries per kind. `Last, First` PGN names are reordered to
  `First Last` for the search API and search page.
- Add tests for confident match, ambiguous match, no network, cached response,
  HTML/entity cleanup, attribution/link rendering, disabled lookups, and profile
  switching.

**Depends on:** `src/players.rs`, `src/app.rs`, the profile/photo layout, and the
existing `ureq` HTTP client. **Related:** item 16 (FIDE profile link) and item
19 (player photos).

## 22. Player clock-usage analysis

**Status:** brainstorm / backlog only. No implementation is scheduled yet.

**Goal:** in the player profile view, where preparation against that player
also happens, analyze how the player uses clock time. The profile is the right
home because the same screen is used to prepare an opening and a practical
game plan.

- Parse the available `[%clk]` values and derive time spent per move, remaining
  time, and move-number/time-control phase. Never guess when clocks are absent.
- Summarize time trouble (for example moves played below a low-time threshold),
  long thinks, fast moves, and how those correlate with move quality, results,
  and the player's rating band/era.
- Compare the player's habits with their own baseline, the opponent, and the
  current preparation line; surface recurring patterns rather than a single
  game.
- Provide a graphical timeline/heatmap by move number and game phase, with
  click-through to the source games.
- Keep the analysis read-only and local; it must not require uploading games.
- Consider a future link to the engine review/accuracy data so a long think can
  be compared with the quality of the resulting move.
- Add fixtures with complete, partial, and missing clocks, increments, and
  correspondence-scale times; verify that games without clocks are excluded
  rather than treated as zero.

**Depends on:** `src/domain.rs` clock parsing, `src/players.rs` player book,
`src/app.rs` profile/preparation view, and the review/accuracy work in item 2.

## 23. Clickable Site → map search

**Status:** implemented 2026-10-29 with OpenStreetMap as the settled provider.

**Goal:** clicking the Site field in the PGN tags card opens a map search with
the Site value, so a tournament venue can be located without copying the text.

- Keep the Site value editable. Make the “Site” label or a small map-pin button
  the click target; do not turn the text field itself into a link.
- URL-encode the trimmed value and ignore empty or placeholder values (`""`,
  `"?"`). Never guess a location.
- **Provider — OpenStreetMap (settled):** use
  `https://www.openstreetmap.org/search?query=…`. It needs no API key, uses
  open data, and avoids Google Maps Platform terms. Google’s plain
  `https://www.google.com/maps/search/?api=1&query=…` URL also works without a
  key but is less privacy-preserving. Do not add a provider setting unless
  users ask for both.
- Add tests for URL construction, percent-encoding, and placeholder handling.

**Depends on:** `src/app.rs` PGN tags card and the existing `Context::open_url`
path.

## 24. Open vs Load control consolidation

**Status:** implemented 2026-10-29, revised 2026-10-31. The typed path field,
**Load** button, and bottom-bar **Open…** quick action are gone. **File → Open
database…** and drag-and-drop load a database. The Tree tab’s opening-book and
opening-tree path rows are read-only labels with **Choose…**, **Clear**, and
**Current DB**; their redundant **Load** buttons were removed.

**Goal:** the app exposed **File → Open database…**, a bottom-bar **Load** +
**Open…** pair, and Tree-tab **Load** / **Choose…** / **Current DB** controls.
Decide which distinctions are real and remove the rest.

- Audit result: the bottom-bar **Load** only applied the typed path; **Open…**
  opened the file picker. Both called `load_file`, so the typed path was the
  only unique behavior and the user chose to drop it. Keep the picker.
- Keep **File → Open database…** as the menu entry; the bottom-bar quick
  action was removed once it proved redundant.
- Tree-tab auxiliary rows are a separate configuration surface: keep
  **Choose…** / **Clear** / **Current DB**. The **Load** buttons were removed
  in the follow-up pass: the path is displayed read-only and only the picker or
  **Current DB** changes it.
- Do not remove a control until its behavior is available elsewhere and covered
  by a test or manual check.
- This is a long-term community-maintained project: prefer correctness,
  discoverability, and a small stable surface over visual polish; incremental
  UI cleanup is welcome after the functional audit.

**Depends on:** `src/app.rs` menu bar and Tree tab.

## 25. Typst / LaTeX export for printing

**Status:** Typst target implemented (2026-10-31); LaTeX target remains backlog.
See [`PRINT.md`](PRINT.md) for the user-facing workflow, output layout, compiler
options, and attribution.

**Goal:** export one game or a selected set of games as print-ready Typst or
LaTeX source, with an optional compiled PDF, so games, annotations, and
diagrams can be printed or included in preparation notes without a screenshot
workflow.

**Visual blueprint (2026-10-31):** match the ChessBase '26 printout supplied
by the project owner. The reference image is kept locally at
`~/Pictures/Screenshots/Screenshot_20261031_061139.png` and is **not**
committed (it contains ChessBase output and game content). The layout to
reproduce:

- A4 portrait, two-column body flow, with a slim page header showing the
  database/source, owner, date, and page number.
- A compact game heading: the bare ECO code at the right without an `ECO`
  prefix, color squares before the player names, players and Elos stacked, then
  event, site, round, and date.
- Mainline moves in bold, wrapped as prose; variations and comments indented
  below the move where they occur, with the variation line bracketed and its
  source game cited inside the comment.
- Board diagrams at user-chosen plies (the reference has one after the opening
  and one at the final position), with file/rank coordinates and a side-to-move
  marker. The user chooses these with the equivalent of right-click → add
  diagram; do not hard-code only the final position.
- The `(Diagram)` text in the reference is **only a column-break marker**: the
  diagram did not fit at the bottom of the left column, so the real board was
  placed at the top of the next column. It is not a placeholder fallback. The
  exporter must always emit the actual board; when it does not fit, break to
  the next column or page and keep the diagram with its intended position.

**Implementation order:** implement **Typst first**, then LaTeX. Typst gives
the simpler source generator, faster compile cycle, and easier two-column
layout; Brett can render each diagram once as SVG/PNG and embed the same asset
in both backends. LaTeX follows with the same document model and diagram
assets, using a standard chess package only if it can be documented as an
optional dependency. Do not make either format the hidden intermediate for the
other.

- Offer **Typst** and **LaTeX** as separate export targets; do not make one the
  hidden intermediate format for the other.
- Preserve the exported PGN content faithfully: headers, comments, NAGs,
  variations, clock/eval annotations, and custom start positions.
- Render the board as a diagram at chosen plies (default: final position, plus
  optional key positions), and include a move list with readable annotations.
- Keep the generated source self-contained and dependency-light. Document the
  required Typst package or LaTeX packages and provide a plain fallback.
- Never require a network connection or upload games. Invoke a local compiler
  only when the user asks for a PDF, and report a missing compiler clearly.
- Add tests for source escaping (player names, comments, URLs, Unicode),
  variation nesting, custom starts, Chess960, and deterministic output.
- **Default print filename (implemented 2026-11-02):** default the export filename
  to `White - Black` (for example `Carlsen, Magnus - Nakamura, Hikaru.typ`),
  not the database stem, so exporting several games from one database does not
  keep colliding on the same name. Sanitize path separators and characters that
  are invalid on Windows, macOS, or Linux; fall back to a neutral stem when both
  player names are absent. Keep the `.typ` source and compiled PDF names
  aligned.
- **Remember the print location (implemented 2026-11-02):** default the print export
  folder to the last folder the user chose for a print export, falling back to
  `Documents` on first use. Persist it with the other display/save preferences;
  do not silently reset it when the open database changes. Cover first-use and
  remembered-path behavior with tests.

## 26. Set up position

**Status:** implemented 2026-11-02 in `src/setup.rs` and wired into the Board and File menus.

**Goal:** let the user start a game from an arbitrary legal position without
hand-editing a PGN `FEN` tag, the way a board-setup dialog works in ChessBase.

- Add a board setup dialog opened from the Board menu or the new-game flow.
- Place, remove, and move pieces by dragging or clicking; choose the side to
  move, castling rights, and the en passant square.
- Validate the resulting position and refuse illegal setups with a clear
  message; support standard chess and Chess960.
- Start a new game from the setup with the same position, preserving the setup
  as the game's start position for display, printing, and export.
- Keep the existing custom-FEN import/export behavior; do not rewrite the
  stored FEN tag on save.
- Cover the dialog and the FEN round-trip with tests, including empty squares,
  multiple pieces of the same type, and both colors.

**Depends on:** `src/chess/`, `src/domain.rs`, and the board UI in `src/app.rs`.

## 27. Country flags in player views

**Status:** implemented 2026-11-02 in `src/country.rs`, `src/players.rs`, and `src/app.rs`.

**Goal:** show a small country flag beside player names in the player profile
and database views when a real country or federation is known, without
inventing one from a name.

- Decode real country/federation data from PGN `WhiteCountry`/`BlackCountry`
  tags, SCID headers, and FIDE profile metadata when available.
- Show the flag in the player profile header and in database-view game rows
  beside the existing title and FIDE-ID suffixes; keep it secondary to the
  name.
- Use a small bundled, licence-compatible flag set, or a documented fallback
  such as a country-code label, and render nothing when the country is absent
  or unknown.
- Preserve raw tags when editing/exporting; do not guess a country from the
  player's name or language.
- Add tests for code normalization, unknown codes, and the no-country case.

**Depends on:** `src/domain.rs`, `src/players.rs`, `src/fide.rs`, and the shared
player-name rendering helpers.

## 28. Persistent Local Analysis Cache & Multi-Source Cloud Engine

**Status:** implemented 2026-11-02 in `src/analysis_cache.rs`, `src/settings.rs`, and `src/app.rs`.

**Goal:** build an embedded, persistent cache of locally analyzed positions in
SQLite (WAL mode), debounced write throttling, fine-grained Stockfish quality
heuristics, separate Leela Chess Zero coexistence, and multi-source integration
with ChessDB.cn and Lichess Cloud Eval in a stacked candidate variation table.

- Embedded SQLite database at `$XDG_CACHE_HOME/brett/analysis.db` with multi-PV support.
- 10-second write debouncing during rapid early search plies ($d < 20$); immediate
  per-ply commits during deep search ($d \ge 20$) or on search completion.
- Engine family separation: Leela Chess Zero evaluations are partitioned and never
  overwritten by Stockfish.
- Piecewise depth scaling and calibrated version baseline Elo replacement heuristic
  with a 15-Elo stability hysteresis; opt-out toggle in Settings.
- Centipawn translation for Leela via `leela_to_stockfish_cp`.
- Stacked 2-line layout displaying deeper variation on top with source tags.
- Easy migration support: one-click export/import of analysis database and
  unified JSON settings bundle.

**Depends on:** `src/engine/`, `src/cloud.rs`, `src/eval.rs`, and `src/settings.rs`.

## 29. Leela Chess Zero (`lc0`) UCI Support & Engine Coexistence

**Status:** resolved and implemented.

**Conclusion:** Leela Chess Zero (`lc0`) operates strictly as a standard UCI engine via stdin/stdout.
The GUI does not manage CUDA or GPU kernels directly; execution backends (CUDA, Vulkan, OpenCL,
or CPU via OneDNN/Eigen) are chosen and managed internally by the `lc0` executable and its weights.
No custom GPU or CUDA code is required in Brett.

Brett already fully supports Leela:
- Auto-detects Leela from the UCI identity string (`lc0`, `leela`).
- Automatically translates Leela's compressed centipawn scale to Stockfish-equivalent centipawns (`src/analysis_cache.rs::leela_to_stockfish_cp`).
- Partitions cached evaluations separately so Leela and Stockfish evaluations coexist without overwriting.
- Renders dedicated `Lc0` columns in the Cloud Engine table and integrates Leela scores into the board evaluation bar.


## 30. Touchscreen Gestures: Disambiguating Scroll vs. Drag Reordering in Database View

**Status:** queued.

**Goal:** make scrolling and drag-reordering feel natural and responsive on
touchscreens (e.g. 2-in-1 convertible laptops, tablets, mobile devices) within
the database games list.

- **Current challenge:** disabling drag during touches allows vertical scrolling,
  but makes drag-reordering games nearly impossible with a touchscreen or pen;
  conversely, enabling drag on the entire row causes scrolling swipes to accidentally
  trigger game reordering instead of smooth inertia scrolling.
- **Dedicated drag handle or long-press gesture:**
  - Introduce an explicit drag handle affordance (e.g. `⋮⋮` grip on the left of
    reorderable rows) that initiates drag-and-drop immediately.
  - Alternatively, use a touch-and-hold threshold (~250ms with visual elevation /
    haptic feedback) before engaging drag-and-drop, allowing swift touch swipes
    across the entire row to perform uninterrupted smooth scrolling.
- **Directional gesture disambiguation:** differentiate vertical scroll gestures
  from intentional horizontal or reorder gestures with direction-locking.
- Add test coverage verifying touch vs mouse drag behavior.

**Depends on:** `src/app.rs` (`games_content`, `game_row`, `rows_are_draggable`).

## 31. Engine Cloud Eval Box: Deduplicate Lichess Depth Display

**Status:** implemented 2026-11-04. The Cloud Engine header now reports the PV
count only; the Lichess depth is rendered once in the column heading and in
the per-row tooltip.

**Goal:** avoid showing redundant or duplicated Lichess depth indicators in the
Engine tab's cloud evaluation candidate box.

- Review formatters in `src/app.rs` (around `cloud_engine_table` and stacked variation
  rendering) to ensure depth is rendered once centrally or cleanly alongside the
  engine tag rather than duplicated across multiple columns/labels.

**Depends on:** `src/app.rs` (`cloud_engine_table`, `engine_content_inner`).

## 32. Cloud Evaluation Fetch Lifecycle & Deduplication Across Navigation

**Status:** queued.

**Goal:** audit and unify the ChessDB and Lichess cloud eval request triggers so
positions are not fetched or aggregated multiple times when switching between the
Moves panel and the Engine panel.

- **Observed behavior:** navigating nodes in the Moves section triggers eval bar updates,
  but switching tabs to the Engine section appears to trigger a second fetch/aggregation pass.
- **Investigation & Fix:** verify cache keys and lifecycle in `maybe_request_cloud()`:
  ensure the in-flight deduplication and completed FEN cache prevent duplicate HTTP requests
  and avoid re-triggering spinners when the position evaluation is already cached.

**Depends on:** `src/cloud.rs` and `src/app.rs` (`maybe_request_cloud`).

## 33. Database & Cloud Tree Pruning Logic (No Hits Downstream vs Transpositions)

**Status:** on hold — flagged for investigation with a more capable reasoning model.

**Goal:** avoid issuing redundant or fruitless lookup requests for downstream positions
when earlier positions along the same branch had zero hits.

- **Broad Scope:** applies to any database lookup in general, whether **online** (ChessDB.cn,
  Lichess Cloud Eval, Lichess Opening Explorer) or **local** (local position index, reference
  databases, opening books).
- **Heuristic rationale:** once a game enters a novelty or rare move absent from the reference
  corpus, subsequent descendant positions along that branch are almost guaranteed to also have
  zero hits. Pruning queries saves network traffic, disk I/O, and UI latency.
- **Transposition edge case & investigation mandate:** transpositions can re-enter known book lines
  via alternative move orders. The user explicitly flagged this logic to be thoroughly analyzed
  and designed with a more capable reasoning model before writing any code.

**Depends on:** `src/cloud.rs`, `src/app.rs`, and position indexing/lookup infrastructure.

## 34. Stockfish Decision Heuristic: Dev Build Continuous Calibration

**Status:** implemented (v0.1.6). Fully implemented in `src/engine/discovery.rs` with linear
day interpolation between anchors SF 16, 17, 18, and 19.


**Goal:** expand the Stockfish quality heuristic to support unreleased developer builds
and daily master snapshots (`stockfish-ubuntu-x86-64-avx2-git-YYYYMMDD`).

- **Version model:** treat 1 year as roughly 1 major version bump (+ ~40–50 Elo baseline gain/year).
- **Date extraction & interpolation:** deduce release or commit date from binary filename or
  `uci` identity string (`git-YYYYMMDD`), and interpolate baseline Elo continuously between
  the fixed anchors of stable releases:
  - Stockfish 16: 2023-06-30
  - Stockfish 16.1: 2024-02-24
  - Stockfish 17: 2024-09-06
  - Stockfish 17.1: 2025-03-30
  - Stockfish 18: 2026-01-31
  - Stockfish 19: 2026-09-05
- Linear interpolation between surrounding anchors assigns an accurate baseline rating for any
  intermediate dev build, keeping the piecewise depth scaling heuristic fair when evaluating
  whether a dev build at depth $d$ supersedes a stable release at depth $d + \Delta$.

**Depends on:** `src/analysis_cache.rs` (`parse_engine_identity`, `quality_score`).

## 35. File Picker Filter Polish: Hide Companion Files by Default

**Status:** implemented and verified 2026-11-04. The database picker offers
`*.pgn`, `*.si4`, `*.si3`, `*.cbh`, and `*.2cbh` as its default filter; the
opening-book picker offers `*.bin`, `*.ctg`, `*.abk`, and `*.bkt`. Companion
files are visible only through the explicit **All files** filter.

**Goal:** present only the primary entry point files in the native file selection dialogs
(Zenity and KDialog), hiding companion files and auxiliary data files by default.

- **Database file picker:** show only `*.pgn`, `*.si4`, `*.si3`, `*.cbh`, and `*.2cbh`. Hide
  companion files (`.2cba`, `.2cbg`, `.2lid`, `.2lcd`, `.2lgd`, `.cko`, `.cpo`, `.cba`, `.cbg`,
  `.sn4`, `.sg4`, etc.). Opening the main `.2cbh` or `.cbh` file automatically opens all companions.
- **Opening books picker:** introduce a dedicated book file filter displaying only `*.bin`,
  `*.ctg`, `*.abk`, and `*.bkt`, hiding accompanying CTG files (`.cto`, `.ctn`).

**Depends on:** `src/app.rs` (`open_dialog`, `open_book_dialog`).

## 36. Mega Database Extended Features: Portraits and Tournament Results

**Status:** queued.

**Goal:** support portrait pictures and tournament results / crosstables bundled in Mega Database
releases.

- **Portraits:** inspect and decode player portraits from `PlayerData/Players.spf` or the
  accompanying image directories when browsing players in the Players tab.
- **Tournament crosstables:** parse `.html` tournament index directories and companion tournament
  data tables bundled in Mega Database releases to render crosstables and event standings.

**Depends on:** `src/twocbh/` and `src/players.rs`.

## 37. Online Game Import: Chess.com and Lichess APIs

**Status:** implemented.

**Goal:** Allow users to directly import game archives from Chess.com and Lichess into their active database or player preparation dossier.

- **Chess.com Public API:** Fully implemented in `src/online.rs:252-289` and `src/app.rs:7388, 12500-12580`.
  - Queries player archives via `https://api.chess.com/pub/player/{username}/games/archives`.
  - Downloads monthly archives with polite 120ms delays between months.
  - Automatically filters rules for standard chess and extracts PGNs.
  - Custom User-Agent header `Brett/0.1 (native Linux chess preparation app)`.
- **Lichess API:** Implemented in `src/online.rs:241-251` with direct PGN streaming.


**Depends on:** `src/cloud.rs` and `src/app.rs`.

## 38. Reverse-Engineer Special Later ChessBase Functions

**Status:** future roadmap / research spike.

**Goal:** Identify and recreate proprietary analytical, visual, and indexing features from ChessBase 16/17/18.

- **Automated Tactical Analysis & Blunder Checks:** Full-game engine pass generating natural language descriptions, blunder tags, missed tactics, and interactive training drill questions.
- **Novelty Detection:** Detect first-played OTB moves against reference databases (ICCF / Mega), annotating novelty move number, preceding games, and opening branch depth.
- **Plan Explorer & Pawn Structure Classifier:** Automatically classify positions into classical pawn structures (Isolani/IQP, Carlsbad, Hanging Pawns, Hedgehog, French chain, King's Indian locked center) and surface common piece plans.
- **Visual Piece Pressure & Control Heatmaps:** Attacking rays, defended vs undefended piece highlights, and square control differentials layered on the board.
- **Search Accelerators & Fast Position Indexing:** Reverse-engineer `.cbj` and 2CBH search accelerator structures to achieve instant reference lookups without requiring multi-gigabyte external sidecars.

**Depends on:** `src/twocbh/`, `src/eval.rs`, `src/review.rs`, and `src/ui/board.rs`.

## 39. Preparation & Predictor Round-2 Fixes (Queued)

**Status:** largely landed 2026-11-07 (6 commits, full `cargo test` green). Still queued: `predict_v2` calibration (`18/18` → `>=90%`, adaptive `alpha0` + reserved/unseen alpha).

**Goal:** land the batched Preparation/predictor/UI fixes identified for round 2 without regressing the queued Smart Preparation v2 / Predictor v2 design.

**Landed 2026-11-07 (`docs/superpowers/plans/2026-11-07-prep-ux-eval-and-perf.md` Tasks 1–6):**

- **Opponent-reply indexing:** index both colors in `PlayerBook`; fix opponent-reply generation/lookup for `Be3`/`Bc4`/`Bg5`/`f3` and similar cases.
- **`predict_v2` calibration:** raise calibration from the `18/18` baseline context to `>=90%`; add adaptive `alpha0` and reserved/unseen alpha handling for unobserved legal moves.
- **Classification & account identity:** make Classical the default classification where appropriate; canonicalize account tags/identifiers across sources.
- **Prep setup UX:** opponent banner at top; search hidden when an opponent is selected; clear ChessABC all-games label; live account `✓`/`✗` confirmations.
- **Fresh-start File → Prepare without open DB:** allow preparation startup without an already-open database via `open_library` for reference + extra sources, including `2cbh`/`cbh`/`si4`, with accelerated extraction where available.
- **List/UI polish:** merge left + recent lists; fix Site/TimeControl color mapping; fix `🟢` glyph rendering; remove Your-Move line-grid clutter.
- **Reference tree & startup prompts:** strict reference tree (never current DB, typo fix); startup default book/reference prompt with **Not now** / **don’t ask again**.
- **Eval-loss quality column (cache-only):** show `?!`/`?`/`??` plus an **Eval** column using `40`/`90`/`200` cp thresholds from cache-only/composite evaluation data (no forced fresh engine run implied).

**Depends on:** `src/players.rs`/PlayerBook, predictor/reference paths (item 6), prep UI/session/library paths (items 5/8/12/13), `src/recent.rs`, settings/book/reference configuration, `src/analysis_cache.rs`/`src/eval.rs`/review display. **Related:** items 2, 5, 6, 8, 12, 13.