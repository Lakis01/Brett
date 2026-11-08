# Functional Rework, Compilation Trim & Modern Minimalist UI Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix Lichess export timeouts, ChessABC live trend baselines, and compilation layout overflow; default date cutoffs to all-time; drop the unused Database `Views` subsystem and bloated crates (`regex`, `arboard`, `webbrowser`); add 9-category customizable game colors without text category labels; add per-game score badges in Recent Games; overlay hovered games into the right-hand Moves section and board player strips across all game lists; unify the Tree/Reference/Preparation tables with horizontal stacked W/D/B bars; and execute the modern minimalist UI overhaul defined in `DESIGN.md`.

**Architecture:** We first harden data/network/classification primitives (`src/online.rs`, `src/players.rs`, `src/settings.rs`, `Cargo.toml`) in Tasks 1–2, then implement the functional UI reworks (hover-to-moves overlay, unified Tree/Reference table, horizontal W/D/B bar, clean color-coded game rows) in Task 3, and finally apply the holistic `DESIGN.md` visual overhaul (carbon-slate workspace, warm wood/stone board, player list with flags & latest OTB Elo, and top Player Profile Card with direct White/Black square `Prepare` triggers) in Task 4.

**Tech Stack:** Rust (`edition = "2024"`), `eframe` / `egui` 0.37, `ureq` 2.12, `serde_json` 1.

**Spec:** `docs/superpowers/specs/2026-11-08-functional-rework-and-ui-overhaul-design.md` and `DESIGN.md`.

## Global Constraints

- **No network calls in unit tests:** Test URL builders, incremental PGN streaming readers, category classifiers, and UI state transitions with in-memory fixtures (`std::io::Cursor`, synthetic `Game` records).
- **Zero compiler/clippy warnings:** `cargo clippy --lib --tests` and `cargo test` must pass cleanly at the end of every task.
- **No text category labels on game rows:** Game rows in `Games`, `Players`, and `Recent Games` must never display `"Classical OTB"`, `"Rapid/Blitz OTB"`, `"Online"`, etc. in their subtitle; font color from `GameCategoryColors` is the sole category indicator.
- **Horizontal W/D/B bars everywhere:** All W/D/B distributions render via `draw_wdl_bar` using White (`#E8ECF2`), Grey (`#6C7380`), and Black (`#20242C` with `#383E4C` stroke).

## Review Focus

1. **Lichess stream interrupted mid-download after reading valid games:** `read_pgn_stream_incremental` must return all complete PGN games parsed before an `io::ErrorKind::TimedOut` / `UnexpectedEof` rather than discarding the entire download.
2. **Live reference source (`"live:chessabc"`, `"live:lichess-masters"`, `"live:lichess-all"`) during `start_smart_prep_load`:** Must never pass `"live:..."` to `open_library(Path::new(...))`; must record `"Live reference prior"` and keep `player_book_status` compact.
3. **Hovering a game in `Recent Games` (inside the right panel) vs hovering a game in `Games` / `Players` (inside the left panel):** Hovering inside `Recent Games` must not unmount `prep_tree` under the pointer (which would cause hover-loss flicker), while hovering in the left panel must overlay `moves_content` in the right panel even if `RightTab::Tree` is selected.
4. **ChessABC live trends when all 500 fetched games are from the last 14 days:** `current_trend_override` for `TrendSource::ChessAbc` must compare `chessabc_live_tree` counts against the active reference/tree baseline via `recent::trends_from_window_counts` when `position_trends` returns empty.
5. **Latest OTB Elo calculation on `Player`:** Online blitz/bullet ratings must not overwrite a player's latest OTB Elo (`latest_otb_elo`); only non-online games update `latest_otb_elo` (falling back to overall `elo_max` / `elo_avg` only if a player has exclusively online games).

---

### Task 1: Resilient Lichess Streaming, Default `AllTime` Cutoffs, `GameCategory` & `latest_otb_elo`, and `Cargo.toml` Trim

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/players.rs`
- Modify: `src/online.rs`
- Modify: `src/app.rs` (only `open_browser_url` and `copy_to_clipboard` at lines `20304-20317`)

**Interfaces:**
- Produces in `src/players.rs`:
  - `PrepFilterConfig::default().online_date_cutoff == DateCutoff::AllTime`
  - `AccountFilter::default().date_cutoff == DateCutoff::AllTime`
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum GameCategory { OtbClassical, OtbRapid, OtbBlitz, OnlineClassical, OnlineRapid, OnlineBlitz, OnlineBullet, Correspondence, Unknown }`
  - `pub fn classify_game_category(game: &Game) -> GameCategory`
  - `pub fn classify_headers_category(event: Option<&str>, site: Option<&str>, time_control: Option<&str>) -> GameCategory`
  - `Player::latest_otb_elo: Option<u16>` and `pub fn display_elo(&self) -> Option<u16>` (returns `self.latest_otb_elo.or_else(|| (self.elo_max > 0).then_some(self.elo_max as u16))`)
- Produces in `src/online.rs`:
  - `pub fn lichess_export_url_filtered(username: &str, max_games: usize, filter: Option<&crate::players::AccountFilter>, as_of: u32) -> String`
  - `pub fn read_pgn_stream_incremental(reader: impl std::io::BufRead, max_games: usize, mut progress: impl FnMut(usize)) -> Result<(String, usize)>`
  - `pub fn fetch_player_games_filtered(source: OnlineSource, username: &str, max_games: usize, filter: Option<&crate::players::AccountFilter>, as_of: u32) -> Result<Vec<domain::Game>>`

- [ ] **Step 1: Write failing unit tests in `src/players.rs` and `src/online.rs`**

Add tests in `src/players.rs`:
- `default_prep_and_account_filters_use_all_time_cutoff`: asserts `PrepFilterConfig::default().online_date_cutoff == DateCutoff::AllTime`, `PrepFilterConfig::default().otb_date_cutoff == DateCutoff::AllTime`, and `AccountFilter::default().date_cutoff == DateCutoff::AllTime`.
- `classify_game_category_distinguishes_otb_online_and_correspondence`: verifies `OtbClassical`, `OtbRapid` (`"Schnellschach"`), `OtbBlitz`, `OnlineClassical`, `OnlineRapid`, `OnlineBlitz`, `OnlineBullet`, and `Correspondence` (`Event "ICCF World Cup"` or `Site "ICCF"`).
- `player_latest_otb_elo_prefers_most_recent_otb_game_over_online`: indexes an older OTB game (`2024.05.01`, Elo `2110`), a newer OTB game (`2026.03.10`, Elo `2185`), and a newer online blitz game (`2026.10.01`, Site `"https://lichess.org/abcd1234"`, Elo `2450`), asserting `player.latest_otb_elo == Some(2185)` and `player.display_elo() == Some(2185)`.

Add tests in `src/online.rs`:
- `lichess_export_url_includes_perf_type_rated_and_since`: verifies `lichess_export_url_filtered("Makefrench", usize::MAX, Some(&filter), 20261108)` includes `perfType=classical,rapid,blitz` and omits `max=`.
- `read_pgn_stream_incremental_keeps_complete_games_on_partial_timeout`: uses a custom `Read` implementation that yields two complete PGN games followed by `io::Error::new(io::ErrorKind::TimedOut, "timed out")`, asserting `read_pgn_stream_incremental` returns `Ok((body, 2))` containing both games.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib default_prep_and_account_filters_use_all_time_cutoff classify_game_category_distinguishes lichess_export_url_includes_perf_type read_pgn_stream_incremental_keeps`
Expected: FAIL (missing items / old defaults).

- [ ] **Step 3: Remove `regex`, `arboard`, and `webbrowser` from `Cargo.toml` and update `src/players.rs`, `src/online.rs`, and `src/app.rs`**

1. In `Cargo.toml`: remove `arboard`, `webbrowser`, and `regex`.
2. In `src/app.rs:20304-20317`:
   - `fn open_browser_url(ctx: &egui::Context, url: &str)` calls `ctx.open_url(egui::OpenUrl::new_tab(url));` (with `std::process::Command::new("xdg-open").arg(url).spawn()` as a best-effort non-blocking Linux fallback if needed).
   - `fn copy_to_clipboard(ctx: &egui::Context, text: &str)` calls `ctx.copy_text(text.to_string());`.
3. In `src/players.rs`:
   - Replace `CHESSCOM_TOURNAMENT_EVENT_RE`, `CHESSCOM_INT_SITE_RE`, and `TOURNAMENT_KEYWORD_RE` with zero-regex ASCII case-insensitive word/phrase matching functions preserving all existing `is_chesscom_tournament_game` unit tests.
   - Change `PrepFilterConfig::default().online_date_cutoff` and `AccountFilter::default().date_cutoff` to `DateCutoff::AllTime` (update any unit test in `src/players.rs` that asserted `Months(24)` as the default).
   - Implement `GameCategory`, `classify_game_category`, and `classify_headers_category`.
   - Track `latest_otb_elo: Option<u16>` and `latest_otb_date: u32` on `Player` during `PlayerIndex` construction; add `Player::display_elo(&self) -> Option<u16>`.
4. In `src/online.rs`:
   - Change `ureq::AgentBuilder` in `import`, `fetch_player_games`, `fetch_player_games_filtered`, `fetch_chessabc_player_games`, and `fetch_recent_chessabc_games` to use `.timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(30))` instead of `.timeout(Duration::from_secs(180))`.
   - Implement `lichess_export_url_filtered`, `read_pgn_stream_incremental`, and `fetch_player_games_filtered`.
   - Update `fetch_recent_chessabc_games` to fetch up to `5` pages (`500` games) in parallel using `std::thread::scope` (capped at `max_games`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib players:: online::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/players.rs src/online.rs src/app.rs
git commit -m "feat(online,players): stream Lichess exports, default AllTime cutoffs, add GameCategory and latest_otb_elo, trim crate bloat"
```

---

### Task 2: Configurable `GameCategoryColors`, Drop `Views`, Fix Live Reference Prep Load & Compilation Overflow, and Fix ChessABC Trends Baseline

**Files:**
- Modify: `src/settings.rs`
- Modify: `src/app.rs`

**Interfaces:**
- Produces in `src/settings.rs`:
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct GameCategoryColors { pub otb_classical: [u8; 3], pub otb_rapid: [u8; 3], pub otb_blitz: [u8; 3], pub online_classical: [u8; 3], pub online_rapid: [u8; 3], pub online_blitz: [u8; 3], pub online_bullet: [u8; 3], pub correspondence: [u8; 3], pub unknown: [u8; 3] }`
  - `DisplayPrefs::category_colors: GameCategoryColors` (serialized to/from `display.json`)
- Produces in `src/app.rs`:
  - `App::category_colors: GameCategoryColors`
  - `App::game_category_color(&self, cat: players::GameCategory) -> Color32`

- [ ] **Step 1: Write failing unit tests in `src/settings.rs` and `src/app.rs`**

- In `src/settings.rs`: `display_prefs_round_trips_game_category_colors` testing custom RGB hex serialization/deserialization in `DisplayPrefs`.
- In `src/app.rs`: `chessabc_trend_override_uses_reference_baseline_when_live_tree_is_all_recent` verifying that when `self.trend_source = TrendSource::ChessAbc` and `chessabc_live_tree` has games all from today while `tree_index` has historical baseline games at the starting position, `current_trend_override` returns non-empty `MoveTrend` entries via `trends_from_window_counts`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib display_prefs_round_trips_game_category_colors chessabc_trend_override_uses_reference_baseline`
Expected: FAIL.

- [ ] **Step 3: Implement `GameCategoryColors`, drop `DatabaseView`, and fix compilation status & ChessABC trends**

1. In `src/settings.rs`:
   - Add `GameCategoryColors` with default palette matching `DESIGN.md` (`otb_classical: [0x7E, 0xD3, 0x8A]`, `otb_rapid: [0x56, 0xB6, 0xC2]`, `otb_blitz: [0x61, 0xAF, 0xEF]`, `online_classical: [0xB4, 0x8E, 0xAD]`, `online_rapid: [0xC6, 0x78, 0xDD]`, `online_blitz: [0xE5, 0xC0, 0x7B]`, `online_bullet: [0xE0, 0x6C, 0x75]`, `correspondence: [0xD1, 0x9A, 0x66]`, `unknown: [0xEC, 0xEF, 0xF4]`), plus `color_for(self, cat: crate::players::GameCategory) -> [u8; 3]`.
   - Persist `category_colors` in `DisplayPrefs`.
2. In `src/app.rs`:
   - Add `category_colors: settings::GameCategoryColors` to `App`, wired to `DisplayPrefs` load/save and editable in **Settings → Appearance** (`settings_dialog`) with 9 color pickers (`ui.color_edit_button_srgba`) and a `"Reset to default colors"` button.
   - Remove `DatabaseView`, `views: Vec<DatabaseView>`, `views_open: bool`, `database_views_window`, `"Views"` button in `database_panel`, `"Open in database views"` in `pending_open_dialog`, and `ViewGameDrag` / `ViewDatabaseDrag`. Update `App::new` / `App::with_file` accordingly.
   - In `refresh_prep_account_checks`: probe online accounts with `online::fetch_player_games_filtered(source, &username, 200, None, players::today())` or share results; in `start_smart_prep_load`, call `online::fetch_player_games_filtered(account.source, &account.username, max_games, Some(&account.filter), as_of_account)` when not cached (or when the cached probe hit the 200-game cap).
   - In `start_smart_prep_load` (`src/app.rs:7358,7483`): check `let reference_is_live = LiveReferenceSource::from_config_str(reference_path.trim()).is_some();` and when `reference_is_live`, push `"Live reference prior"` instead of calling `open_library`. Shorten any error note to at most 48 characters (`truncate_status_error`) so URLs never bloat `notes`.
   - In `prep_tree`: render compilation progress in a bounded vertical/wrapped block with `egui::ProgressBar::new(progress_fraction).desired_width(ui.available_width().min(320.0))` and a truncated/wrapped label so the right panel never overflows horizontally.
   - In `current_trend_override` (`src/app.rs:5712`): for `TrendSource::ChessAbc`, if `recent::position_trends(&moves, self.trend_config())` is empty (because all live ChessABC games fall in the recent window), look up baseline move counts from `self.tree_index.as_ref().or_else(|| self.reference.as_ref().map(|r| &r.index))` (or `self.cached_masters_prior(key)`) and compute trends via `recent::trends_from_window_counts(&counts, self.trend_config())`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/settings.rs src/app.rs
git commit -m "feat(app,settings): add customizable GameCategoryColors, drop Views, fix live ref prep load and ChessABC trend baseline"
```

---

### Task 3: Hover-to-Preview Moves Overlay, Score Badge in Recent Games, Horizontal W/D/B Bar & Unified Tree / Reference Table

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Produces in `src/app.rs`:
  - `fn draw_wdl_bar(ui: &mut Ui, white: u32, draw: u32, black: u32, width: f32, height: f32) -> egui::Response`
  - `fn activate_hovered_game_browse_lazy(&mut self, id: String, load: impl FnOnce() -> Option<Game>)`
  - `PrepRecentRow` fields: `result_badge: String`, `player_outcome: Option<f32>` (`1.0` win, `0.5` draw, `0.0` loss)
  - Unified candidate/opening table in `prep_tree` and `tree_content`.

- [x] **Step 1: Write failing unit tests in `src/app.rs`**
- [x] **Step 2: Run tests to verify they fail**
- [x] **Step 3: Implement Clean Game Subtitles, Hover Moves Overlay, `draw_wdl_bar`, and Unified Tree/Refs Table**
- [x] **Step 4: Run tests to verify they pass**
- [x] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(ui,prep): add hover moves overlay, recent game score badges, horizontal W/D/B bars, and unified tree/ref table"
```

---

### Task 4: Modern Minimalist UI Overhaul (`DESIGN.md` — Theme, Board Strips, Database Players List & Top Player Profile Card)

**Files:**
- Modify: `src/ui/board.rs`
- Modify: `src/ui/eval_bar.rs`
- Modify: `src/app.rs`

**Interfaces:**
- Implements the visual system specified in `DESIGN.md`:
  - Carbon-slate dark surface tokens (`BG #111317`, `PANEL #171A20`, `SURFACE #1E222A`, `BORDER #262B35`) and warm natural wood/stone board palette (`#DFCFB8` / `#9E7B56`).
  - Refined `player_strip` above/below the board (`Photo 28x28`, `Name`, `(Elo)`, gold `Title` pill, `Flag`, right-aligned `Clock`).
  - Refined `player_list` rows in Database → `Players` (`Title pill` + `Name` + prominent `latest_otb_elo` (`display_elo()`), right-aligned `Flag`, subtitle with games, score %, and years).
  - Reworked **Top Player Profile Card** in `players_content` with left `64x64` portrait/silhouette, right gold `Title` pill + `Flag` + `FIDE Rating` + horizontal `W/D/B` bar, and a single **`Prepare`** action bar with clickable **White (`⬜`)** and **Black (`⬛`)** color squares (`paint_prepare_color_square`).

- [x] **Step 1: Write failing unit tests in `src/app.rs`**
- [x] **Step 2: Run tests to verify they fail**
- [x] **Step 3: Implement the `DESIGN.md` UI Overhaul across `src/ui/board.rs`, `src/ui/eval_bar.rs`, and `src/app.rs`**
- [x] **Step 4: Run full test suite and clippy**
- [x] **Step 5: Commit**

```bash
git add src/ui/board.rs src/ui/eval_bar.rs src/app.rs PRODUCT.md DESIGN.md docs/superpowers/specs/2026-11-08-functional-rework-and-ui-overhaul-design.md docs/superpowers/plans/2026-11-08-functional-rework-and-ui-overhaul.md
git commit -m "feat(ui): overhaul workspace theme, board strips, player list with OTB Elo, and player profile card"
```

