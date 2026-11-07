# Cycle 2 Design Spec: Prep Opponent & ChessABC Selection, Fullscreen, Recent Limit, Cloud Engine Folding, and Live Reference Trees & Trends

**Date:** 2026-11-07  
**Author:** Gemini 4 Argon High (`plan`)  
**Target Subagents:** Gemini 3.8 Flash (`Model: "flash"`) in `antigravity-ide`

---

## 1. Problem Statement & Root-Cause Analysis

### 1.1 Preparing Against `"bye"` (`src/players.rs`, `src/app.rs`)
- **Symptom:** The user searched for `Markus Hinterreiter` in the Preparation Setup dialog, saw the ChessABC profile resolved, and started preparation — but preparation actually loaded against `bye` (1,359 games in `Eigene Partien.si4` and 11,923 games in `Mega Database 2026.2cbh`).
- **Root Causes:**
  1. `players::player_key(name: &str)` in `src/players.rs` only rejected `""` and `"?"`. In tournament databases, unpaired/forfeit games use `[Black "bye"]` or `[White "bye"]`, making `"bye"` the #1 most frequent entry (index `0`) in `PlayerDirectory`.
  2. When a database is installed, `self.selected_player` defaults to `Some(0)` (`"bye"`).
  3. In `resolve_prep_player_index` (`src/app.rs:7471`) and `prep_setup_dialog` (`src/app.rs:7720`), `self.prep_setup_player.or(self.selected_player)` was evaluated *before* `self.prep_setup_search`, and even when `self.prep_setup_change_opponent` was `true`. Thus typing a name into `prep_setup_search` or clicking `"Change opponent"` still resolved to `self.selected_player` (`"bye"`).

### 1.2 ChessABC Candidate Overflow, 0-Game Profiles, and Default Selection (`src/online.rs`, `src/app.rs`)
- **Symptom:**
  - Preparing for `Johannes Kern` first auto-selected FIDE ID `1646885` (`Kern, Johann`), which has 0 games on ChessABC (`ChessABC returned no standard games for FIDE id 1646885`), while his actual profile `4604776` (`Kern, Johannes, Dr.`, Elo 2156, 11 games) was skipped because `"Dr."` prevented an exact `player_key` match.
  - Clicking `"Find profiles"` queried `"Kern J"`, which matched 26 substring results on ChessABC (`Bikernieks`, `Brustkern`, `Kern, Benjamin`, etc.) and rendered all 26 checkboxes uncollapsed inside the modal, pushing `"Start preparation"` below the bottom of the screen.
- **Design Fix:**
  1. **Honorific stripping & candidate relevance filtering (`src/players.rs`, `src/online.rs`):**
     - Strip academic/generational honorific tokens (`"dr"`, `"prof"`, `"ing"`, `"mag"`, `"dipl"`, `"jr"`, `"sr"`, `"ii"`, `"iii"`) in `player_key` so `"Kern, Johannes, Dr."` normalizes to `"johannes kern"`.
     - In `collect_chessabc_candidates`, retain only candidates that match the query via `name_match_score(query, &player.name).is_some()` or `players::matches_truncated_or_enumerated_name(query, &player.name)`.
     - Query `chessabc_player_game_count(agent, &player.fide_id)` (`GET /games/player/paginated?fideId={id}&page=0&size=1` -> `totalElements`) for candidate profiles and **retain only profiles with `games >= 1`** (recording `games: Option<usize>` on `ChessAbcPlayer`).
  2. **Default most-probable profile when player is already selected (`src/online.rs`, `src/app.rs`):**
     - Add `online::best_chessabc_candidate(candidates, query, player_elo) -> Option<ChessAbcPlayer>` which always picks the single most probable candidate (preferring exact `player_key` match closest in Elo, then truncated/enumerated name match closest in Elo, then highest `name_match_score` + rating proximity).
     - When opening `prep_setup_dialog` for an already-selected player, resolve and select the single most probable ChessABC profile by default and keep the candidate checklist **collapsed** (`prep_setup_chessabc_expanded = false`).
     - Show a compact one-line summary (`ChessABC: <Name> · Elo <X> · FIDE <ID> (N games)`) with a **`Change profile…`** button. Only clicking **`Change profile…`** expands the candidate checklist (and triggers a candidate search if not yet loaded).
     - When the candidate checklist is expanded, show a **`Confirm`** button that collapses the candidate list (`prep_setup_chessabc_expanded = false`), and wrap the candidate list in a bounded `egui::ScrollArea::vertical().max_height(150.0)` (plus a `ScrollArea` for the dialog body above the bottom action bar) so the window never overflows the screen.

### 1.3 Application Fullscreen Toggle (`F11` / `Fn+F11`) (`src/app.rs`)
- Handle `Key::F11` at the top of `App::handle_keys` (before `ctx.egui_wants_keyboard_input()`) so `Fn+F11` / `F11` always toggles `egui::ViewportCommand::Fullscreen(!is_fullscreen)`.

### 1.4 Unified Recent Games Count Selector (`src/app.rs`)
- Replace the separate `DragValue` + `Show all` checkbox in `Recent games` with a single `egui::ComboBox` selector offering `"All"` (`prep_recent_all = true`) alongside discrete increments `[5, 10, 15, 25, 50, 100]` (`prep_recent_all = false`, `prep_recent_limit = n`).

### 1.5 Cloud Engine (`chessdb.cn`) Move Folding & Compact Chip Layout (`src/app.rs`)
- In `merge_multi_source_engine_rows`:
  - Fix step 3 so `chessdb`-only candidate moves set `lichess_pv: None` instead of `Some(vec![mv.uci.clone()])` (which previously fabricated `[Lichess d0]`).
- In `cloud_engine_section`:
  - Show **all** candidate moves (remove the 8-row `More moves (...)` truncation).
  - Partition `rows` into:
    - `line_rows`: moves with a Lichess cloud-eval hit, local engine evaluation, or multi-ply variation (`row.lichess.is_some() || row.stockfish.is_some() || row.leela.is_some() || row.variation.len() > 1`), rendered with `cloud_row_ui`.
    - `folded_chessdb_rows`: all remaining `chessdb.cn` moves without a Lichess eval hit or engine line, folded together into a single compact `horizontal_wrapped` block with tight spacing (`vec2(5.0, 2.0)`) and inline `chessdb:` prefix so all `chessdb`-only moves fit in 1–2 rows combined.

### 1.6 Live Sources (`Lichess Masters`, `Lichess All`, `ChessABC`) for General Reference Tree, Default Book, and Move Trends (with Outdated DB Warning) (`src/cloud.rs`, `src/recent.rs`, `src/position.rs`, `src/settings.rs`, `src/app.rs`)
- **Live Source Identifiers (`LiveReferenceSource`):**
  - Support `"live:lichess-masters"`, `"live:lichess-all"`, and `"live:chessabc"` in both `book_path` (Default Opening Book) and `reference_path` (General Reference Tree & Preparation Prior).
  - Add quick-select buttons (`Lichess Masters`, `Lichess All`, `ChessABC`) in:
    - Startup `reference_setup_dialog` (for both Opening Book and Reference Database)
    - Tree Tab (`Opening book` row)
    - References Tab (`Reference sources` card)
    - Preparation Tab (`General Reference Tree` section)
- **Live Trend Sources (`TrendSource`) & Outdated Database Warning:**
  - Add `TrendSource` (`ActiveTree` / Mega DB, `ChessAbc`, `LichessMasters`, `LichessAll`), configurable in **Settings → Move trends** and via an inline selector bar in the Tree / General Reference Tree panels.
  - Add `PositionIndex::latest_date(&self) -> u32` in `src/position.rs`. When `TrendSource::ActiveTree` is used on a local/Mega database whose latest game date is more than 180 days older than `players::today()` (or undated), display a clear warning banner:
    `⚠ Database may be outdated for trends (latest game: YYYY.MM.DD). Consider switching Trends to ChessABC or Lichess Masters.`
  - For `TrendSource::ChessAbc`: build/cache a background `RecentTree` from `online::fetch_recent_chessabc_games(1500)` and compute `MoveTrend`s (comparing recent ChessABC window against the active reference/tree baseline when available, or within the ChessABC dated buckets).
  - For `TrendSource::LichessMasters` and `TrendSource::LichessAll`: add `cloud::fetch_explorer_since` (`&since=YYYY` for Masters, `&since=YYYY-MM` for Lichess) and `recent::trends_from_recent_and_total` to compute empirical-Bayes shrunk `MoveTrend` badges (`new`, `rising`, `falling`, `steady`, `±Xpp`, confidence, and tooltip) from the recent window vs prior baseline (`total - recent`).

