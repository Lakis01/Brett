# Implementation queue

This is the working backlog after the preparation stage. Items are ordered by
dependency and user value, not by estimated effort. Completed work is recorded
in `README.md`; this file tracks what is next.

**Status (2026-10-22):** items 1 (UCI panel and engine settings), 2 (game
review), 3 (PGN clocks beside the board), and 4 (castling drag) are implemented.
Item 5, preparation view v2, is next.

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

**Goal:** read the older multi-file ChessBase CBH format (not CBH2/2CBH).

A sample is available at:

```text
/home/gratis01/Downloads/CBH/Pimped Correspondence Database.*
```

It is a classic multi-file database (`.cba`, `.cbb`, `.cbc`, `.cbe`, `.cbg`,
`.cbgi`, `.cbh`, `.cbj`, `.cbm`, `.cbp`, `.cbs`, `.cbt`, `.cbtt`, `.flags`,
`.ini`), not a 2CBH container.

Research and implementation:

- Copy the sample to `/tmp/opencode` for analysis; the Downloads copy and the
  chess database tree remain read-only.
- Identify the roles of the index, game, name, annotation, and metadata files
  from independently published format descriptions; do not copy GPL/runtime
  converter code.
- Implement the reader behind `GameLibrary` so players, games, filters, and
  position indexing work without format-specific UI.
- Add a small lawfully redistributable fixture or a synthetic writer test; do
  not commit the personal/commercial sample database.
- Validate decoded headers, mainline, variations, comments, and NAGs against a
  trusted export of the same sample.

**Depends on:** format research. **Blocks:** the CBH part of the database
roadmap.

## 8. Larger infrastructure

- **M2b mega-scale ingest:** external sorting, on-disk header table, incremental
  position-index updates.
- **Cloud evaluation:** Lichess cloud eval and ChessDB.cn with local Stockfish
  fallback.
- **Reference merging:** combine several databases into one opening tree
  without rebuilding unrelated caches.
