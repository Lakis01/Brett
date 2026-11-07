# Prep ChessABC Selection, Fullscreen, Cloud Folding, Live Reference Trees & Trends Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix preparation against `"bye"`, filter ChessABC profiles to `>= 1` game with default most-probable collapsed selection and `Confirm` collapse, add `F11` fullscreen, unify the Recent Games count selector, fold all `chessdb.cn` moves without Lichess eval hits into 1–2 compact rows, and support live sources (`Lichess Masters`, `Lichess All`, `ChessABC`) for General Reference Tree, Default Opening Book, and Move Trends (with an outdated-database warning for stale Mega/local databases).

**Architecture:**
- `src/players.rs` & `src/online.rs`: Reject non-player placeholders (`"bye"`, `"forfeit"`, etc.) and strip honorifics (`"Dr."`, `"Prof."`, etc.) in `player_key`. Filter ChessABC search candidates by name relevance and `totalElements >= 1`, and select the single most probable candidate via `best_chessabc_candidate`.
- `src/position.rs`, `src/cloud.rs`, `src/recent.rs`, `src/settings.rs`: Expose `PositionIndex::latest_date()`, `cloud::fetch_explorer_since()`, `recent::TrendSource`, `recent::is_database_outdated()`, and `recent::trends_from_window_counts()`, persisted in `DisplayPrefs`.
- `src/app.rs`: Fix opponent resolution order, add collapsible/confirmable ChessABC profile picker inside a bounded `ScrollArea`, handle `Key::F11` fullscreen, unify Recent Games limit `ComboBox`, fold all `chessdb.cn`-only moves without Lichess hits into one tight inline row block, and wire `LiveReferenceSource` + `TrendSource` across Tree, General Reference Tree, Opening Book, and Settings.

---

### Task 1: Exclude `"bye"` Placeholders, Strip Honorifics, Filter 0-Game ChessABC Candidates, and Pick Most-Probable Profile (`src/players.rs`, `src/online.rs`)

**Files:**
- Modify: `src/players.rs`
- Modify: `src/online.rs`

- [ ] **Step 1: Write failing tests in `src/players.rs` and `src/online.rs`**
  - In `src/players.rs::tests`:
    - `player_key_rejects_bye_and_placeholders`: assert `player_key("bye") == None`, `player_key("BYE") == None`, `player_key("forfeit") == None`, `player_key("unknown") == None`, `player_key("unpaired") == None`, `player_key("vacant") == None`.
    - `player_key_strips_honorifics`: assert `player_key("Kern, Johannes, Dr.") == player_key("Johannes Kern")`.
  - In `src/online.rs::tests`:
    - `best_chessabc_candidate_prefers_exact_and_closest_elo`: verify that given `Kern, Johann` (rating 0) and `Kern, Johannes, Dr.` (rating 2156), `best_chessabc_candidate(&candidates, "Johannes Kern", Some(2150))` selects `Kern, Johannes, Dr.`.
    - `filter_matching_chessabc_candidates_drops_substring_noise`: verify that `"Brustkern, Juergen"`, `"Bikernieks, Anrijs"`, and `"Kern, Benjamin"` are rejected for query `"Johannes Kern"`, while `"Kern, Johannes, Dr."`, `"Kern, J"`, and `"Kern, J1"` are kept.

- [ ] **Step 2: Implement in `src/players.rs` and `src/online.rs`**
  - In `src/players.rs`:
    - Update `player_key(name: &str) -> Option<String>` to filter out honorific tokens (`"dr"`, `"prof"`, `"ing"`, `"mag"`, `"dipl"`, `"jr"`, `"sr"`) when other name tokens exist, and return `None` when the resulting key is empty or matches a non-player placeholder (`"bye"`, `"forfeit"`, `"unpaired"`, `"vacant"`, `"unknown"`, `"none"`, `"n/a"`, `"na"`, `"tbd"`, `"anonymous"`).
  - In `src/online.rs`:
    - Add `pub games: Option<usize>` to `ChessAbcPlayer` (update struct literals in `src/online.rs` and `src/app.rs`).
    - Add `pub fn candidate_matches_query(query: &str, candidate_name: &str) -> bool` returning `true` when `name_match_score(query, candidate_name).is_some()` or `crate::players::matches_truncated_or_enumerated_name(query, candidate_name)` or `crate::players::player_key(query) == crate::players::player_key(candidate_name)`.
    - In `collect_chessabc_candidates`, retain only candidates where `candidate_matches_query(query, &player.name)` is `true`.
    - Add `fn chessabc_player_game_count(agent: &ureq::Agent, fide_id: &str) -> usize` querying `https://api.chessabc.com/games/player/paginated?fideId={fide_id}&page=0&size=1` and reading `totalElements` (returning `0` on missing/zero).
    - In `search_chessabc_candidates` and `resolve_chessabc_player`, populate `player.games = Some(count)` via `chessabc_player_game_count` and retain **only** candidates with `count >= 1`.
    - Add `pub fn best_chessabc_candidate(candidates: &[ChessAbcPlayer], query: &str, player_elo: Option<u32>) -> Option<ChessAbcPlayer>` that picks the single most probable candidate (using `auto_select_chessabc_candidate` first, then exact `player_key` match closest to `player_elo`, then `matches_truncated_or_enumerated_name` closest to `player_elo`, then `choose_chessabc_player`).

- [ ] **Step 3: Run `cargo test --lib players` and `cargo test --lib online` and commit**
  - Commit message: `fix(players,online): exclude bye placeholders, strip honorifics, and filter 0-game ChessABC profiles`

---

### Task 2: Live Trend Plumbing, Lichess `since` Explorer, and Outdated Database Detection (`src/position.rs`, `src/cloud.rs`, `src/recent.rs`, `src/settings.rs`)

**Files:**
- Modify: `src/position.rs`
- Modify: `src/cloud.rs`
- Modify: `src/recent.rs`
- Modify: `src/settings.rs`

- [ ] **Step 1: Write failing tests in `src/position.rs`, `src/recent.rs`, and `src/settings.rs`**
  - In `src/position.rs`: test `latest_date()` returns the latest game date (`YYYYMMDD`) indexed.
  - In `src/recent.rs::tests`:
    - Test `is_database_outdated(20250101, 20261107, 180)` is `true` and `is_database_outdated(20260901, 20261107, 180)` is `false`.
    - Test `trends_from_window_counts` computes `TrendBadge::New`, `TrendBadge::Rising`, and `TrendBadge::Falling` from `(mv, recent_games, total_games)` tuples using empirical-Bayes shrinkage.
  - In `src/settings.rs::tests`: test `DisplayPrefs` round-trips `trend_source`.

- [ ] **Step 2: Implement in `src/position.rs`, `src/cloud.rs`, `src/recent.rs`, and `src/settings.rs`**
  - In `src/position.rs`: add `pub fn latest_date(&self) -> u32` on `PositionIndex`.
  - In `src/cloud.rs`: add `pub fn fetch_explorer_since(fen: &str, lichess_token: Option<&str>, explorer_source: ExplorerSource, since_year: u32, since_month: u32) -> Result<Explorer, String>`.
  - In `src/recent.rs`:
    - Add `TrendSource` enum (`ActiveTree`, `ChessAbc`, `LichessMasters`, `LichessAll`) with `ALL`, `label`, `short_label`, `config_value`, `from_config`.
    - Add `pub fn is_database_outdated(latest_date: u32, as_of: u32, max_age_days: i64) -> bool`.
    - Add `pub fn trends_from_window_counts(counts: &[(u32, u32, u32)], config: TrendConfig) -> HashMap<u32, MoveTrend>`.
  - In `src/settings.rs`: add `pub trend_source: crate::recent::TrendSource` to `DisplayPrefs` with JSON serialization/deserialization.

- [ ] **Step 3: Run `cargo test --lib position recent settings cloud` and commit**
  - Commit message: `feat(trends): add TrendSource, Lichess since explorer, window-count trends, and outdated DB check`

---

### Task 3: UI Integration — Opponent Resolution, Collapsible ChessABC Picker, `F11` Fullscreen, Unified Recent Selector, Folded `chessdb` Chips, and Live Reference/Book/Trends (`src/app.rs`)

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Write unit tests in `src/app.rs::tests`**
  - `resolve_prep_player_prefers_typed_search_over_selected_player_when_changing_opponent`: verify that when `selected_player` is `Some(0)` and the user types `"Hinterreiter, Markus"` in `prep_setup_search` (or `prep_setup_change_opponent` is true), `resolve_prep_player_index` resolves `"Hinterreiter, Markus"` instead of player `0`.
  - `cloud_engine_rows_leave_chessdb_only_moves_without_fake_lichess_pv`: verify that `chessdb`-only moves in `merge_multi_source_engine_rows` have `variation_source == None` and `lichess == None`.
  - `live_reference_source_round_trips_config_strings`: verify `LiveReferenceSource::from_config_str` parses `"live:lichess-masters"`, `"live:lichess-all"`, and `"live:chessabc"`.

- [ ] **Step 2: Implement all 6 UI improvements in `src/app.rs`**
  1. **Opponent resolution (`resolve_prep_player_index` & `prep_setup_dialog`):**
     - In `prep_setup_dialog`: `let chosen = if self.prep_setup_change_opponent { self.prep_setup_player } else { self.prep_setup_player.or(self.selected_player) }.filter(|&index| self.players.get(index).is_some());`.
     - In `resolve_prep_player_index`: check `self.prep_setup_player`, then `self.prep_setup_search` (matching existing player or calling `ensure_player`), then `(!self.prep_setup_change_opponent).then_some(self.selected_player).flatten()`, then `prep_setup_accounts`, then `prep_setup_chessabc_candidates`.
  2. **ChessABC default most-probable profile, lazy search on `"Change profile…"`, and `"Confirm"` collapse:**
     - Add `prep_setup_chessabc_expanded: bool` to `App` (default `false`).
     - In `poll_chessabc_candidates`: filter `candidates.retain(|c| c.games != Some(0))`. Use `online::best_chessabc_candidate(&candidates, &query, elo)` to select the single best profile by default and set `self.prep_setup_chessabc_expanded = false`.
     - In `prep_setup_dialog`: wrap the dialog body above the bottom buttons in `egui::ScrollArea::vertical().id_salt("prep-setup-scroll").max_height(560.0)`. When `!self.prep_setup_chessabc_expanded`, show the compact selected profile summary + **`Change profile…`** button (which sets `prep_setup_chessabc_expanded = true` and triggers `refresh_chessabc_candidates` if needed). When `self.prep_setup_chessabc_expanded`, show a **`Confirm`** button (collapsing `prep_setup_chessabc_expanded = false`) and render the candidate checkboxes inside `egui::ScrollArea::vertical().id_salt("prep-chessabc-scroll").max_height(140.0)`.
  3. **Application Fullscreen (`F11` / `Fn+F11`):**
     - At the start of `handle_keys`, check `ctx.input(|i| i.key_pressed(Key::F11))` and toggle `egui::ViewportCommand::Fullscreen(!is_fullscreen)`.
  4. **Unified Recent Games Selector:**
     - Replace the separate `DragValue` + `Show all` checkbox in `Recent games` with a single `egui::ComboBox::from_id_salt("prep-recent-limit-combo")` offering `"All"` (`recent_all = true`) and `[5, 10, 15, 25, 50, 100]` (`recent_all = false`, `recent_limit = n`).
  5. **Cloud Engine (`chessdb.cn`) Folding & Compact Chip Layout:**
     - In `merge_multi_source_engine_rows`: set `lichess_pv: None` in step 3.
     - In `cloud_engine_section`: show all rows without the 8-row `More moves` cap; render all rows with `row.lichess.is_some() || row.stockfish.is_some() || row.leela.is_some() || row.variation.len() > 1` via `cloud_row_ui`, and fold **all** remaining `chessdb`-only rows into a single `ui.horizontal_wrapped` block with `item_spacing = vec2(5.0, 2.0)` and inline `chessdb:` label.
  6. **Live Reference Tree / Default Book (`Lichess Masters`, `Lichess All`, `ChessABC`) + Live/Database Trend Feature & Outdated DB Warning:**
     - Add `LiveReferenceSource` (`LichessMasters`, `LichessAll`, `ChessAbc`) recognized in `book_path` and `reference_path`.
     - Add background live ChessABC recent tree (`chessabc_live_tree: Option<crate::recent::RecentTree>`, `chessabc_live_rx`, `chessabc_live_status`) and Lichess `since` explorer cache (`explorer_since_cache`, `explorer_since_rx`).
     - Render live sources in `General Reference Tree`, `Opening book`, and `prior_for_position`, with quick-select buttons in `reference_setup_dialog`, Tree tab, References tab, and Preparation `General Reference Tree`.
     - Wire `trend_source` in `App`, `Settings → Move trends`, and inline in the Tree / General Reference Tree headers, plus the outdated-database warning banner when `TrendSource::ActiveTree` is used on an index with `is_database_outdated(index.latest_date(), players::today(), 180)`.

- [ ] **Step 3: Run full `cargo test` suite and commit**
  - Commit message: `feat(ui,prep): collapsible ChessABC picker, F11 fullscreen, unified recent limit, folded chessdb, and live reference/trends`

