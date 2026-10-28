# Board clocks, engine bar, and book-aware review

**Status:** approved 2026-10-28. Implementation follows item 17 (the
`shakmaty`/`pgn-reader` replacement) so these features land on the new facade.
This document covers the board-view clock change, PGN `[%eval]` decoding, the
optional engine-evaluation bar, and the book-aware game review with a revised
accuracy metric.

## 1. Goals

- Decode PGN `[%clk ...]` and `[%eval ...]` directives into structured fields.
- Show clocks in the board view only; keep the PGN moves window and the game
  header clock-free.
- Never render `[%eval ...]` as comment text.
- Add a board-view evaluation bar, enabled by default in Settings.
- Use the best available evaluation: highest-depth local-engine or Lichess
  cloud evaluation; PGN eval is the fallback when neither exists.
- Skip the leading opening-book prefix in game review. Book moves do not count
  toward accuracy.
- Replace the current single-number heuristic accuracy with a per-color,
  win-probability-based metric plus ACPL.

## 2. Decisions

| Topic | Decision |
| --- | --- |
| Engine-bar placement | Vertical bar immediately left of the board, White advantage upward. |
| Evaluation priority | Highest depth wins between local engine and Lichess cloud; PGN eval is the fallback. Equal depth: local > cloud > PGN. |
| Review book source | Native opening book first, then the configured local opening-tree file, then the Lichess Masters explorer. |
| Book threshold | Minimum game count, default 3, configurable in Settings. |
| Book truncation | Only the leading mainline prefix is skipped; cap at 30 plies by default, configurable. |
| Accuracy | Lichess-style win-probability accuracy per color, plus ACPL. Book and forced moves are excluded. |

## 3. PGN clock and eval decoding

### 3.1 Data model

`src/domain.rs` gains a White-perspective PGN evaluation:

```rust
pub struct PgnEval {
    pub cp: Option<i32>,   // centipawns, White POV
    pub mate: Option<i32>, // mate in N, White POV
}
```

`Node` gains `pub eval: Option<PgnEval>`. `Clock` and the existing `Node.clock`
stay unchanged.

### 3.2 Parsing rules

`parse_comment_directives` becomes a small struct-returning parser:

```rust
struct CommentDirectives {
    text: String,
    clock: Option<Clock>,
    eval: Option<PgnEval>,
}
```

- `[%eval 0.15]` → `cp = 15`.
- `[%eval -0.32]` → `cp = -32`.
- `[%eval #3]` → `mate = 3`; `[%eval #-3]` → `mate = -3`.
- A trailing depth suffix such as `[%eval 0.15,20]` is ignored; the first
  comma-separated field is the evaluation.
- Malformed `%eval` directives are removed from the comment text rather than
  shown. The same applies to `%clk`.
- Other directives (`%emt`, etc.) remain text, as today.
- `add_comment` and `replace_comment` update the structured eval only when the
  new text contains an `%eval`; otherwise the existing eval is preserved, just
  like clocks.
- PGN export re-emits `[%eval ...]` and `[%clk ...]` from the structured fields
  so round-tripping does not lose data.

The Lichess broadcast fixture
`/home/gratis01/Downloads/Brett Resources/lichess_broadcast_round-3_2026.10.15.pgn`
uses `{ [%eval 0.15] [%clk 1:30:56] }`. It is the primary parser fixture.

### 3.3 Clock placement

- The **Clocks** toggle continues to control the board player strips.
- Remove `move_clock_chip` and the `show_clocks` plumbing from
  `render_move_tokens_flat`, `render_readonly_move_tokens`, and
  `render_readonly_move_line`.
- Remove the side-to-move clock row from `game_header_card`; the Moves tab is
  clock-free.
- Update the Board-menu and bottom-bar tooltips to say “board strips” rather
  than “move list and header.”

## 4. Engine evaluation bar

### 4.1 Data model

Add `src/eval.rs`:

```rust
pub enum EvalSource { Local, Cloud, Pgn }

pub struct BoardEval {
    pub score: WhiteScore,
    pub depth: u32,
    pub source: EvalSource,
}

pub enum WhiteScore {
    Cp(i32),   // White POV
    Mate(i32), // White POV
}
```

Conversions:

- Local engine `Score` is side-to-move POV; flip for Black.
- Lichess cloud `cp`/`mate` are already White POV.
- PGN `PgnEval` is White POV.
- `choose` picks the greatest depth; ties go local > cloud > PGN. PGN has
  depth 0, so it only supplies the bar when neither engine nor cloud has an
  evaluation.
- `WhiteScore::win_percent` uses the published Lichess win-probability curve:

  ```text
  Win% = 50 + 50 * (2 / (1 + exp(-0.00368208 * cp)) - 1)
  ```

  Centipawns are clamped to ±1000 for numerical stability; mates map to 0 or
  100. The formula is implemented independently in Brett; no Lichess code is
  copied.
- `WhiteScore::label` renders `+0.35`, `-1.20`, `#3`, or `#-3`.

### 4.2 Evaluation sources

**Local engine**

- Use MultiPV line 1 for the current FEN, guarded by `engine_fen`.
- If the engine bar is enabled, `engine_auto` is on, and Brett is not in
  cloud-only mode, the main engine analyzes the current position even when the
  Engine tab is not visible.
- If `engine_auto` is off, the bar uses any existing local result plus
  cloud/PGN; it does not start a new search.
- During a running game review, do not spawn a second local engine. Reuse the
  review engine's current info when it matches the board position; otherwise
  use cloud/PGN.

**Lichess cloud**

- Add `cloud::fetch_eval_only(fen)`, a lightweight request to
  `https://lichess.org/api/cloud-eval?fen=…&multiPv=1`.
- A 404 means “no cloud evaluation”; cache that as `None`.
- Cache results by FEN in the app (bounded, like `cloud_cache`). Network errors
  are not cached, so they can retry.
- Debounce requests by 350 ms, matching the existing reference fetch.
- Do not fetch chessdb or the opening explorer just for the bar.

**PGN eval**

- Use the current node's structured `eval`.
- Hover previews do not change the bar; it follows the selected node.

### 4.3 Rendering and layout

- New `src/ui/eval_bar.rs` draws a 16 px vertical bar.
- White fills from the bottom; the dark background is Black. White advantage
  therefore grows upward.
- A subtle center line marks equality. A border and rounded corners match the
  board strips.
- When no evaluation exists, draw a neutral 50% bar.
- The hover tooltip shows the score, source, and depth, for example
  `White +0.35 · Lichess cloud d24` or `Black +1.20 · local engine d18`.
  For mate: `White mates in 3`.
- Layout: a horizontal group containing the bar and the board column. The bar
  is exactly the board's height and aligned with the board, not with the player
  strips. The board-size calculation subtracts the bar width and gap so the
  board still fits.
- If the engine bar is disabled, the existing centered board layout is
  unchanged.

### 4.4 Settings

- `DisplayPrefs.engine_bar: bool`, default `true`.
- Settings → Display: “Show engine evaluation bar.”
- Persist through the existing `display.json` round-trip.
- Add a test for the default and JSON round-trip.

## 5. Book-aware game review

### 5.1 Book sources and priority

Add `src/review_book.rs` with a `BookSource` trait and a
`detect_prefix(game, sources, max_plies)` function.

Sources are checked in this order:

1. **Native opening book** (`opening_book_path`): if the position has a book
   entry and the played move is listed, it is a book move.
2. **Local opening-tree file** (`opening_tree_path`): if the played move appears
   in the position index with at least the configured minimum games, it is a
   book move.
3. **Lichess Masters explorer**: if the played move's total game count in the
   Masters response is at least the minimum, it is a book move.

Only the leading mainline prefix is considered. Detection stops at the first
non-book move or at the configured maximum number of plies. This prevents a
large tree or the Masters database from swallowing an entire game.

If a source has no entry for the position, the next source is tried. The first
source that confirms the move wins. If no source confirms it, the book prefix
ends.

Network behavior:

- Masters requests are sequential and only made while the prefix is still
  book.
- Use a small total budget (about 5 seconds); on timeout or error, stop the
  Masters source and review the remaining moves.
- Network failure never blocks review.
- If no source is loaded or available, the prefix is 0 and every mainline move
  is reviewed.

If the whole mainline is book, do not start an engine review; show “All N
mainline moves are opening book; nothing to review.”

### 5.2 Review settings

`DisplayPrefs` gains:

- `review_skip_book: bool`, default `true`.
- `review_book_min_games: u32`, default `3`, range 1–100.
- `review_book_max_plies: u32`, default `30`, range 4–80.

Settings → Game review exposes these with a short explanation of the source
priority.

### 5.3 Review flow

`start_review` captures the current game and review config. If book skipping is
enabled, it starts a background `review_book` worker and shows “Checking
opening book…”. `poll_review_book` applies the result only if the same game is
still current, then calls `begin_review(game, config, book_plies)`.

`ReviewState::new(game, config, book_plies)` builds its position list from the
last book position plus every reviewed move:

- `book_plies == 0`: root position, then the whole mainline.
- `book_plies > 0`: the node after the last book move becomes the review root,
  followed by the first non-book move and the rest of the mainline.
- `book_plies == mainline.len()`: no moves to review.

`clear_review` cancels both the book worker and the review engine.

The review panel shows “Opening book: N moves skipped” and the review table
starts at the first non-book move. Book moves are not listed as graded moves.

## 6. Accuracy metric

### 6.1 Per-move accuracy

For each reviewed, non-book move with both evaluations:

1. Convert the before/after scores to White win probability.
2. Convert to the mover's win probability.
3. Compute the per-move accuracy with the published Lichess curve:

   ```text
   Accuracy% = 103.1668 * exp(-0.04354 * (winPercentBefore - winPercentAfter)) - 3.1669
   ```

   Add the published +1 uncertainty bonus and clamp to 0–100. If the move does
   not reduce the mover's win probability, accuracy is 100.

`MoveReview` gains `win_before`, `win_after`, and `accuracy`. Forced moves keep
`Quality::Forced` and have no accuracy contribution.

### 6.2 Game accuracy

For each color, build the sequence of White-POV win percentages for the
reviewed positions. Then:

1. Window size = `(positions.len() / 10).clamp(2, 8)`.
2. Build the leading windows and all sliding windows, as Lichess does.
3. Weight each window by its standard deviation, clamped to 0.5–12.
4. Compute each move's accuracy for the mover and attach the window weight.
5. Take the weighted mean and the harmonic mean per color.
6. The final color accuracy is the average of those two means.

Also compute ACPL per color: the average centipawn loss over reviewed,
non-book, non-forced moves, with each loss capped at 1000.

Return:

```rust
pub struct AccuracySummary {
    pub white: Option<f64>,
    pub black: Option<f64>,
    pub white_acpl: Option<f64>,
    pub black_acpl: Option<f64>,
    pub white_moves: usize,
    pub black_moves: usize,
}
```

The UI shows `White 87.3% · Black 82.1%` and `ACPL 18 · 24`, with a tooltip
explaining that book and forced moves are excluded and that this is an
estimate, not an official rating. While a review is running, show the partial
values and label them as partial.

### 6.3 Why this metric

Win probability is less position-dependent than raw centipawns: losing 300 cp
in an equal position matters more than losing 300 cp in an already decided
game. The volatility weighting gives more influence to critical positions, and
the harmonic mean prevents a single terrible move from being averaged away by
many easy moves. ACPL remains visible because players and coaches are familiar
with it, but it is not the primary accuracy number.

## 7. File-by-file changes

| File | Change |
| --- | --- |
| `src/domain.rs` | `PgnEval`, `Node.eval`, comment directive parser, tests. |
| `src/pgn.rs` | Re-emit structured `%eval` and `%clk` on export; round-trip tests. |
| `src/eval.rs` | `WhiteScore`, `BoardEval`, source selection, win% and accuracy formulas. |
| `src/ui/eval_bar.rs` | Vertical bar rendering and tooltip. |
| `src/cloud.rs` | `fetch_eval_only` with optional 404 handling. |
| `src/review_book.rs` | Book-source trait, native/tree/Masters sources, prefix detection. |
| `src/review.rs` | Book-prefix start, win/accuracy fields, per-color `AccuracySummary`, tests. |
| `src/settings.rs` | `engine_bar`, `review_skip_book`, `review_book_min_games`, `review_book_max_plies`; JSON and tests. |
| `src/app.rs` | Board-eval state and polling, layout, settings UI, review start/poll, clock cleanup. |
| `docs/BACKLOG.md` | Item 22 and the 2026-10-28 decisions. |
| Obsidian `Brett Backlog.md` | Item 22 and the revised clock/review decisions. |

No new dependencies are required.

## 8. Testing and verification

- Parser: Lichess broadcast comments, human comment plus eval, mate eval,
  malformed eval, clock/eval coexistence, comment-edit preservation.
- Export: PGN round-trip preserves clocks and evals without leaking them into
  comment text.
- Eval selection: local vs cloud depth, tie-breaking, PGN fallback, mate
  conversion, side-to-move flipping.
- Accuracy: known win-probability pairs, symmetry for both colors, one blunder
  versus many quiet moves, harmonic-mean behavior, book/forced exclusion, no
  evals.
- Book detection: fake source prefix, first non-book move stops detection, max
  plies cap, native book, local tree threshold, Masters parser, all-book game.
- Settings: defaults and JSON round-trip.
- Manual/visual: board strips show clocks; Moves tab has no clock chips or
  header clock; engine bar sits left of the board and tracks the selected node;
  review table starts after the book prefix and shows per-color accuracy.
- Run `cargo fmt --all -- --check`,
  `cargo clippy --all-targets -- -D warnings`, and the full test suite.

## 9. Assumptions and open choices

- The engine bar auto-analyzes when `engine_auto` is on. Users who want no local
  search can turn auto-analysis off or use cloud-only mode.
- The default book cap is 30 plies; users can raise or lower it.
- Accuracy is labelled as an estimate; it is not presented as an official
  Lichess or FIDE number.
- PGN eval is always treated as a fallback, even when it reports mate, because
  it carries no search depth.
- Book detection is mainline-only; variations are not reviewed.

## 10. Approval

Please confirm this design, especially:

1. The engine bar may start the local engine when the bar is enabled and
   auto-analysis is on, even if the Engine tab is not visible.
2. The review book prefix defaults to 30 plies and is configurable.
3. The accuracy summary is per color, with ACPL alongside it.
