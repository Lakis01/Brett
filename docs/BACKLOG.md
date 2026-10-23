# Implementation queue

This is the working backlog after the preparation stage. Items are ordered by
dependency and user value, not by estimated effort. Completed work is recorded
in `README.md`; this file tracks what is next.

**Status (2026-10-23):** items 1 (UCI panel and engine settings), 2 (game
review), 3 (PGN clocks beside the board), 4 (castling drag), 5 (preparation
view v2 and recent games), 6 (predictor v2 and the static reference
distribution), and 7 (classic CBH support) are implemented. Deferred predictor
refinements are recorded under item 6.

Game review follow-ups that are intentionally deferred: position-index book
marking and review caching. Clock display currently lives in the board player
strips rather than move-list/header chips.

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

A sample is available at:

```text
/home/gratis01/Downloads/CBH/Pimped Correspondence Database.*
```

It is a classic multi-file database (`.cba`, `.cbb`, `.cbc`, `.cbe`, `.cbg`,
`.cbgi`, `.cbh`, `.cbj`, `.cbm`, `.cbp`, `.cbs`, `.cbt`, `.cbtt`, `.flags`,
`.ini`), not a 2CBH container.

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
  position-index updates.
- **Cloud references (done):** chessdb.cn `queryall`, the Lichess opening
  explorer (Masters by default; full Lichess with an OAuth token), and Lichess
  cloud evaluation, shown in the **Refs** tab with asynchronous requests and
  per-FEN caching. See `src/cloud.rs`.
- **Reference merging:** combine several databases into one opening tree
  without rebuilding unrelated caches.

## 9. Database editing, copy/merge, and export

**Status:** the editable ordered session (`src/session.rs`) provides stable row
IDs, lazy primary/external/local slots, reordering, and duplicate matching.
Games can be reordered by dragging rows in the **Games** list; dropping on the
upper or lower half of a row inserts before or after it. Cross-database copy
and merge still need a multi-database view, and there is no database writer or
persistent mutation layer yet.

- Save the current database (including in-memory edits) as PGN. Warn before a
  PGN export that would be very large.
- Add native SCID and classic CBH writers. The readers are independent and
  validated; the writers must round-trip through the readers and, where
  available, an independent implementation.
- Decide and document drag-and-drop semantics:
  - games dragged between database views are copied or moved to the drop
    target;
  - a database dropped onto an open database is merged;
  - external file drops ask whether to open, copy, or merge.
- Never write over a source database without an explicit Save As path. Keep
  the `/home/gratis01/Chess Databases/` and `/home/gratis01/Downloads/CBH/`
  sources read-only.
- Rebuild player and position indexes after a persistent mutation.

## 10. Correspondence database rebuild

**Status:** blocked on source access and confirmation. See
[CORRESPONDENCE_DATABASE.md](CORRESPONDENCE_DATABASE.md).

- The 2016 sample is 24,865 games, dated 2009-02-15 through 2016-02-08.
- Both sides have an inclusive `Elo >= 2400` filter; the minimum is exactly
  2400 and there is no evidence of an upper cutoff.
- The ICCF portion matches the official ICCF Games Archive. Older LSS, FICGS,
  and Remote games are also present and need a source decision.
- The current archive download requires a registered ICCF member.
