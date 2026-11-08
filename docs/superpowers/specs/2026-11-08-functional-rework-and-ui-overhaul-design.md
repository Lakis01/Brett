# Brett — Functional Rework, Compilation Trim & Modern Minimalist UI Overhaul Design Spec

**Date:** 2026-11-08  
**Project:** `/home/gratis01/Programs/Brett`  
**Skills Applied:** `brainstorming`, `writing-plans`, `impeccable` (`Operate` mode + `craft-floor.md`), `design-taste-frontend`

---

## 1. Context & Problem Statement

From live usage and log analysis (`2026-11-07T19:16..19:43`), the following functional, performance, and UX issues were identified, alongside a planned holistic UI overhaul:

1. **Lichess Game Downloads Timing Out at 180s (`src/online.rs`, `src/app.rs`):**
   - `ureq::AgentBuilder` configured `.timeout(Duration::from_secs(180))`, which sets a hard wall-clock deadline for the entire HTTP response stream rather than a per-read socket timeout. Because Lichess streams unauthenticated PGN exports at ~15–20 games/sec, any account with >2,500 games (`Makefrench`, `JohannesPhilidor`, `Ninja_Warrior`) hit the 180s cutoff and discarded all downloaded games.
   - Additionally, pressing **Start** in the preparation dialog while `refresh_prep_account_checks` was still fetching opened a second concurrent HTTP stream to Lichess for the same username.
2. **Live Reference Source Treated as Local File in `start_smart_prep_load` (`src/app.rs`):**
   - When `reference_path` was set to `"live:chessabc"`, `"live:lichess-masters"`, or `"live:lichess-all"`, `start_smart_prep_load` passed `Path::new("live:chessabc")` to `open_library`, logging `Reference file unreadable: opening live:chessabc: No such file or directory (os error 2)`.
3. **Right Panel Overflow During Playerbase Compilation (`src/app.rs`):**
   - `poll_prep_load` concatenated all source notes and multi-hundred-character error URLs into `self.player_book_status` and rendered it in a non-wrapping horizontal row, forcing the right panel's minimum width wider than the screen and locking the board/panel splitter until compilation completed.
4. **Slow / Empty ChessABC Live Trends (`src/online.rs`, `src/app.rs`):**
   - `fetch_recent_chessabc_games(1500)` fetched 15 pages sequentially (~15s). Because all 1,500 newest ChessABC games are from the last two weeks, `RecentTree` had `older_dated == 0` (no historical baseline), so `position_trends` returned no trend arrows.
5. **Default Date Cutoffs (`src/players.rs`):**
   - `PrepFilterConfig::default().online_date_cutoff` and `AccountFilter::default().date_cutoff` defaulted to `DateCutoff::Months(24)` instead of `DateCutoff::AllTime`.
6. **Redundant `Views` Feature & Dependency Bloat (`src/app.rs`, `Cargo.toml`):**
   - The `Database views` (`views: Vec<DatabaseView>`) window and `"Views"` button in the Database panel are unused and add ~500 lines of UI/state complexity.
   - `Cargo.toml` pulls in `webbrowser` (46 transitive crates), `arboard` (86 transitive crates, duplicating `eframe`'s built-in clipboard), and `regex` (6 heavy crates for 3 simple case-insensitive keyword checks).
7. **Granular Game-Category Color Coding Without Text Category Labels (`src/settings.rs`, `src/app.rs`):**
   - Game rows in the left Games list and Recent Games should never print text labels like `Classical OTB`, `Rapid/Blitz OTB`, or `Online`; color-coding the font is sufficient.
   - Recent Games rows must include the individual game's **result/score** (`1-0`, `½-½`, `0-1` colored by the prepared player's outcome).
   - Users must be able to customize colors in **Settings** for 9 granular categories: **OTB Classical**, **OTB Rapid**, **OTB Blitz**, **Online Classical**, **Online Rapid**, **Online Blitz**, **Online Bullet**, **Correspondence**, and **Other/Unknown**.
8. **Hover-to-Preview Overlay in Right-Hand Moves Section & Player View (`src/app.rs`):**
   - Hovering over any game row in **Recent Games**, the **Database Games list**, or the **Database Player view** (without clicking) must allow stepping through moves with `←`/`→`/`Home`/`End` AND overlay that hovered game's moves, player strips, and header info into the **Moves section to the right of the board**.
9. **Horizontal Stacked W/D/B Bar & Unified Tree / Reference Presentation (`src/app.rs`):**
   - Visualize all W/D/B statistics as a single horizontal stacked bar (**White** = white wins, **Grey** = draws, **Black** = black wins).
   - Rework the **Tree / Reference** and **Preparation** candidate-move views into a single unified, scannable table combining opponent stats, reference/book stats (with W/D/B bars), trend arrows, and evaluation.
10. **Modern Minimalist UI Overhaul (`impeccable` + `design-taste-frontend`):**
    - Overhaul the visual presentation into a clean, modern, minimalist analytical workspace (`DESIGN.md`):
      - **Board & Player Strips:** Warm wood/stone board palette, slim vertical eval bar, compact player strips above/below the board with portrait, gold title pill (`GM`), name, latest OTB Elo, country flag, and clock.
      - **Database Players List:** Country flag on the right, prominent gold title pill + **latest OTB Elo** (`Player::latest_otb_elo`), game count, score %, and year span.
      - **Selected Player Profile Card (top of Players view):** Left portrait/silhouette, right gold title badge + FIDE rating + horizontal W/D/B bar + win/draw/loss summary, and a single **`Prepare`** control with clickable **White (`⬜`)** and **Black (`⬛`)** squares that preselect the color and open/start preparation.

---

## 2. Architecture & Component Design

### 2.1 Phase 1: Functional Rework, Bug Fixes & Compilation Trim

#### A. Resilient Lichess Streaming, Server-Side Filtering & Live Reference Guard (`src/online.rs`, `src/app.rs`)
- Replace `.timeout(Duration::from_secs(180))` in `ureq::AgentBuilder` with `.timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(30))` so active streams never hit a 180s wall-clock cutoff.
- Add `fetch_player_games_filtered(source, username, max_games, filter: Option<&AccountFilter>, as_of: u32)` in `src/online.rs`:
  - For `OnlineSource::Lichess`, pass query parameters `&perfType=...` (mapped from `filter.time_controls`: `classical,rapid,blitz,bullet`), `&rated=true` (when `!filter.include_casual && filter.include_rated`), and `&since=<unix_ms>` (when `filter.date_cutoff` is `Months` or `SinceDate`).
  - Stream Lichess PGNs via `std::io::BufReader` + `BufRead::read_line`, parsing completed games incrementally (`split_pgn_chunks`), invoking the progress callback every 100 games, and returning all complete games parsed so far if a read error/timeout occurs after `>= 1` game has already been received.
- In `refresh_prep_account_checks` (`src/app.rs`), probe Lichess accounts with `max_games = 200` (fast verification in <2s) or reuse cached games; in `start_smart_prep_load`, only reuse `cached_checks` when the cached check was a full fetch (or fetch full games with `fetch_player_games_filtered`).
- In `start_smart_prep_load` (`src/app.rs:7358,7483`), treat `LiveReferenceSource::from_config_str(reference_path.trim()).is_some()` the same as `reference_is_book` (recording `"Live reference prior"` instead of calling `open_library`).

#### B. Non-Overflowing Compilation Progress & Fast ChessABC Live Trends (`src/app.rs`, `src/online.rs`)
- Truncate/sanitize individual source error strings in `start_smart_prep_load` to short human-readable labels (e.g., `"Lichess timed out"` instead of a 140-char URL) and render `self.player_book_status` inside a wrapping/truncated container with an explicit `ProgressBar` (`step as f32 / total as f32`) so the right panel never expands its minimum width.
- Speed up `fetch_recent_chessabc_games`: fetch 5 pages (`500` newest OTB games) concurrently across worker threads (`std::thread::scope`) in ~0.5s.
- In `current_trend_override` (`src/app.rs:5712`), when `self.trend_source == TrendSource::ChessAbc`:
  - Build `(mv, recent_games, total_games)` tuples by combining `chessabc_live_tree.moves(key)` (`recent_games`) with the active baseline tree (`tree_index` or `reference.index` or cached Lichess Masters explorer) so `recent::trends_from_window_counts` always has a historical baseline even when all ChessABC live games are from the current month.
  - Show a compact spinner next to the `Trends:` dropdown whenever `chessabc_live_rx.is_some()` or `explorer_since_rx.is_some()`.

#### C. Default `DateCutoff::AllTime` (`src/players.rs`)
- Change `PrepFilterConfig::default().online_date_cutoff` (`src/players.rs:1072`) and `AccountFilter::default().date_cutoff` (`src/players.rs:1145`) from `DateCutoff::Months(24)` to `DateCutoff::AllTime`.

#### D. Granular Game-Category Colors & Clean Game Subtitles (`src/players.rs`, `src/settings.rs`, `src/app.rs`)
- Define `GameCategory` in `src/players.rs`:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  pub enum GameCategory {
      OtbClassical,
      OtbRapid,
      OtbBlitz,
      OnlineClassical,
      OnlineRapid,
      OnlineBlitz,
      OnlineBullet,
      Correspondence,
      Unknown,
  }
  ```
  with `pub fn classify_game_category(game: &Game) -> GameCategory` (checking correspondence keywords/site first, then `classify_game_metadata(game)`).
- Define `GameCategoryColors` in `src/settings.rs` (persisted in `DisplayPrefs` / `display.json` as hex strings `#RRGGBB`), with color pickers (`ui.color_edit_button_srgba`) and a `"Reset category colors"` button in **Settings → Appearance**.
- Remove `type_label` from game row subtitles in `games_content`, `players_content`, and `prep_recent_rows` / `prep_unified_recent_rows`.
- In `PrepRecentRow` (`src/app.rs`), store `result_str: String` (`"1-0"`, `"½-½"`, `"0-1"`) and `player_outcome: Option<f32>` (`1.0` win, `0.5` draw, `0.0` loss for the prepared player) and render a colored result/score badge on the right of each Recent Game row.

#### E. Hover-to-Preview Moves Overlay (`src/app.rs`)
- Wire hover detection in `games_content` (library/session rows) and `players_content` (library rows) as well as `prep_tree` (`Recent games`) via `activate_hovered_game_browse_with_loader(id, loader_fn)` so `library.load` is only invoked once when `browse.id != id`.
- When `self.hovered_game_browse` is `Some(browse)`:
  - `board_meta` (`src/app.rs:18547`) reads `&browse.game` and `browse.cursor_node` for the board position, last-move highlight, top/bottom player strips, clocks, and move counter.
  - `moves_content` (`src/app.rs:14094`) renders `&browse.game` at `browse.cursor_node` (read-only, with a subtle `"Previewing hovered game · ←/→ to step · click row to load"` banner at the top).
  - If the user is hovering a game in the **left panel** (`browse.id.starts_with("game:") || browse.id.starts_with("player:") || browse.id.starts_with("merged:")`) while `RightTab::Moves` is not currently visible, `moves_panel` temporarily displays `moves_content` in the right panel so the hovered game's moves and headers are immediately visible to the right of the board. When hovering a game in `Recent games` (`browse.id.starts_with("recent:")`, inside the right panel), `prep_tree` renders a compact live move-sheet card for `browse.game` above the Recent Games list (avoiding un-mounting the hovered row under the pointer).

#### F. Horizontal Stacked W/D/B Bar & Unified Tree / Reference Table (`src/app.rs`)
- Unify `wdl_bar` / `record_bar` into a clean horizontal stacked bar widget `draw_wdl_bar(ui, white: u32, draw: u32, black: u32, width: f32, height: f32)`:
  - Draws a single rounded bar (`height = 14.0`, `corner_radius = 3.0`) with three proportional segments:
    - **White wins:** `#E8ECF2` (off-white) with dark percentage label when segment width >= 26px.
    - **Draws:** `#6C7380` (neutral grey) with light percentage label when segment width >= 26px.
    - **Black wins:** `#20242C` (charcoal black, `1px` `#383E4C` inner border) with light percentage label when segment width >= 26px.
  - Hover tooltip shows exact counts and percentages: `White: W (X%) · Draw: D (Y%) · Black: B (Z%)`.
- **Unified Tree / Reference Presentation:**
  - In **Preparation (`prep_tree`)**: merge the **Reference / Book** statistics directly into the main **Candidate Moves** grid (`Move` | `Prob` | `Opp Games` | `Opp W/D/B` | `Ref Games` | `Ref W/D/B` | `Trend` | `Eval`) with a compact source-selector pill bar in the card header (`Ref: Mega / Local | Lichess Masters | Lichess All | ChessABC` + `Trends: ...`), eliminating the separate redundant General Reference Tree table below it.
  - In **Tree Tab (`tree_content`)**: merge the active database tree, opening book / live reference, horizontal W/D/B bar, trend arrow, and evaluation into one unified opening table with compact source selector pills in the header.

#### G. Drop `Views` & Trim Dependency Bloat (`src/app.rs`, `src/players.rs`, `Cargo.toml`)
- Remove `DatabaseView`, `views: Vec<DatabaseView>`, `views_open`, `database_views_window`, `"Views"` button in `database_panel`, and `ViewGameDrag` / `ViewDatabaseDrag` from `src/app.rs`.
- Remove `webbrowser`, `arboard`, and `regex` from `Cargo.toml`:
  - Replace `webbrowser::open(url)` in `open_browser_url` with `ctx.open_url(egui::OpenUrl::new_tab(url))` (backed by `eframe`'s `"links"` feature).
  - Replace `arboard::Clipboard` fallback in `copy_to_clipboard` with `ctx.copy_text(text.to_string())` (backed by `eframe`'s `"clipboard"` feature).
  - Replace the 3 `regex::Regex` statics in `src/players.rs` (`CHESSCOM_TOURNAMENT_EVENT_RE`, `CHESSCOM_INT_SITE_RE`, `TOURNAMENT_KEYWORD_RE`) with zero-allocation ASCII case-insensitive word-boundary helpers.

---

### 2.2 Phase 2: Modern Minimalist UI Overhaul (`DESIGN.md`)

1. **Refined Visual Theme & Spacing (`src/app.rs`, `src/ui/board.rs`, `src/ui/eval_bar.rs`):**
   - Apply the carbon-slate palette (`BG_CANVAS #111317`, `SURFACE_PANEL #171A20`, `SURFACE_ELEVATED #1E222A`, `BORDER_SUBTLE #262B35`) and warm natural wood/stone chessboard squares (`#DFCFB8` light, `#9E7B56` dark) as the default theme.
   - Flatten nested card-in-card frames in the left and right panels into clean, single-level sections separated by `1px` hairlines.
2. **Board Player Strips (`src/app.rs`):**
   - Render sleek player strips directly above and below the board (`height: 36px`), aligned to the board width:
     - Left: rounded portrait (`28x28`, or subtle silhouette badge), bold player name, `(Elo)` in secondary tabular type, gold title pill (`GM`, `IM`, etc.), and country flag.
     - Right: large crisp monospace clock (`14:32`) or compact move counter pill.
3. **Database Players List & Latest OTB Elo (`src/players.rs`, `src/app.rs`):**
   - Track `latest_otb_elo: Option<u16>` (and `latest_otb_date: u32`) on `Player` in `src/players.rs` during player indexing (updated from non-online games with a valid Elo tag, falling back to `elo_max` or `elo_avg` when undated).
   - Render each row in `player_list` with:
     - Line 1: `[Gold Title Pill]` `[Player Name (strong)]` `[Elo NNNN (prominent accent/tabular)]` ... right-aligned `[Country Flag]`.
     - Line 2: `N games · score X% · YYYY–YYYY` in secondary text.
4. **Top Player Profile Card with Direct Color-Square `Prepare` Action (`src/app.rs`):**
   - When a player is clicked in the Database `Players` tab, render a cohesive **Player Profile Card** at the top of the panel:
     - Top split: `64x64` rounded portrait (with silhouette fallback when no photo exists) on the left; on the right: gold title badge (`GM`) + country flag + player name, `FIDE Rating` (latest OTB Elo) row, `Games / Score` row, and a full-width horizontal stacked `W/D/B` bar (`White` / `Grey` / `Black`).
     - Action bar: a clean **`Prepare`** bar featuring two tactile color-square buttons on the right — a **White square (`⬜`)** and a **Black square (`⬛`)** (representing the color you or the opponent plays, clearly labeled on hover: *"Prepare with White (opponent plays Black)"* / *"Prepare with Black (opponent plays White)"*) — clicking either square immediately opens/starts preparation with that color preselected.

