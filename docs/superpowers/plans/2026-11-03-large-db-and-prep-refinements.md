# Large Database Management & Preparation Refinements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix player game visibility and indexing for multi-million game databases (Mega Database 2026), prompt to persist position indexes for databases >500k games, implement regex filtering to exclude online Chess.com tournaments in preparation, temporarily swap move predictor v2 UI display to plain observed statistics while preserving v2 code, update backlog anchors for Stockfish 18 & 19, and release v0.1.2.

**Architecture:** 
- In `src/players.rs`, add regex-based online Chess.com event detection (`is_chesscom_event`) taking `event` and `site` tags, integrated into `PredictionConfig::exclude_chesscom` and checked during `PlayerBook::add_game`. Keep all v2 Dirichlet/Bayesian mathematical prediction structures intact.
- In `src/app.rs`, replace UI display of Dirichlet posterior probabilities with a clean table of observed player move statistics (SAN, games count, win/draw/loss counts, score %, last played date).
- In `src/app.rs` and `src/library.rs`, decouple game list rendering from `library.get()` (which returns `None` for virtual libraries like Mega Database) by leveraging `library.game_headers()` and `library.game_players()`, restoring full game display in the Players tab and Preparation view.
- In `src/app.rs`, start player indexing immediately upon loading any database. Add index persistence notification/prompt for databases with >500,000 games once position indexing completes.
- In `docs/BACKLOG.md`, deduplicate trailing sections and add Stockfish 18 & 19 release date anchors.
- Bump version to `0.1.2`, verify with formatting, clippy, and full tests, then commit, tag `v0.1.2`, and push.

**Tech Stack:** Rust (1.80+ / 2021 edition), egui / eframe, regex, cozy-chess, GitHub Actions CI/CD release workflow.

**Spec:** User instructions from context summary and prompt:
1. Add Stockfish 18 & 19 release date anchors to `docs/BACKLOG.md`.
2. Regex logic to exclude Chess.com tournaments when preparing (preventing database pollution).
3. Disable move predictor v2 in UI (display plain statistics of what the player has played in that position), keeping v2 mathematical model code intact.
4. Large database management:
   - For databases >500k games, ask user whether to hardcode/persist the position index once completed for instantaneous subsequent reloads.
   - Start indexing players immediately upon database load.
   - Show games of player during preparation and in player profiles for virtual/mega databases.
5. Bump version to `0.1.2` and publish release.

## Global Constraints
- Target platform: CachyOS / Linux, KDE Plasma 6.6.4, Intel Iris Xe, Wayland.
- Keep `players::predict` and `Prediction` Dirichlet mathematical model intact in `src/players.rs`.
- Do not introduce breaking regressions in PGN, SCID, or classic CBH support.
- Comply with `cargo clippy --all-targets -- -D warnings` and `cargo fmt`.

## Review Focus
1. Mega Database virtual slot mode: `library.get(index)` returns `None` for virtual collections; all header lookups must use `library.game_headers(index)` or `library.game_players(index)`.
2. Chess.com tournament regex: online events (`Chess.com INT`, `Titled Tuesday`, `Arena Kings`, `Speed Chess Championship`) must be excluded from preparation, while physical OTB events sponsored by Chess.com (e.g. `Site: Douglas, Isle of Man`) must be preserved.
3. Player indexing startup: must run in background thread for large databases without blocking the UI thread or game list browsing.
4. Position index persistence prompt: must only appear for databases exceeding 500,000 games and only once indexing has completed.
5. Move predictor UI fallback: must cleanly display observed statistics (Games, Score %, W/D/L, Last Played) and remain interactive (clicking plays move).

---

### Task 1: Update Backlog Anchors & Clean Up Deduplication

**Files:**
- Modify: `docs/BACKLOG.md:1180-1308`

**Interfaces:**
- Consumes: None
- Produces: Cleaned `docs/BACKLOG.md` with Stockfish 18 & 19 anchors in Item 34

- [x] **Step 1: Inspect and edit `docs/BACKLOG.md`**
Remove duplicate text block appended at the end of `docs/BACKLOG.md` and update Item 34 to include:
- Stockfish 16: 2023-06-30
- Stockfish 16.1: 2024-02-24
- Stockfish 17: 2024-09-06
- Stockfish 18: ~2025-09 (projected anchor)
- Stockfish 19: ~2026-09 (projected anchor)
With explicit linear continuous interpolation between stable anchors.

- [x] **Step 2: Verify diff in `docs/BACKLOG.md`**
Run: `git diff docs/BACKLOG.md`
Expected: Duplication removed, Stockfish 18 & 19 anchors present.

- [x] **Step 3: Commit**
```bash
git add docs/BACKLOG.md
git commit -m "docs: add Stockfish 18 & 19 anchors and deduplicate backlog"
```

---

### Task 2: Implement Chess.com Tournament Exclusion Regex in Preparation

**Files:**
- Modify: `src/domain.rs` (add `pub fn site(&self) -> &str`)
- Modify: `src/players.rs` (add `is_chesscom_event`, update `PredictionConfig` and `PlayerBook::add_game`)
- Test: `src/players.rs` (unit tests for `is_chesscom_event` and `PlayerBook` filtering)

**Interfaces:**
- Consumes: `game.event()`, `game.site()`
- Produces: `pub fn is_chesscom_event(event: &str, site: &str) -> bool`, `PredictionConfig.exclude_chesscom: bool`

- [x] **Step 1: Add failing unit tests for `is_chesscom_event` in `src/players.rs`**
Write tests testing:
- `Site: "Chess.com INT"` -> true
- `Site: "chess.com"` -> true
- `Event: "Titled Tuesday intern op 21st Feb Early"`, `Site: ""` -> true
- `Event: "Speed Chess Championship"`, `Site: ""` -> true
- `Event: "Arena Kings"`, `Site: ""` -> true
- `Event: "PRO Chess League"`, `Site: ""` -> true
- `Event: "Tata Steel Masters"`, `Site: "Wijk aan Zee"` -> false
- `Event: "Chess.com Isle of Man International"`, `Site: "Douglas, Isle of Man"` -> false

- [x] **Step 2: Run test to verify it fails**
Run: `cargo test --lib players::tests::chesscom_exclusion`
Expected: FAIL (function not found)

- [x] **Step 3: Implement `is_chesscom_event` and `PredictionConfig.exclude_chesscom`**
Implement `is_chesscom_event(event: &str, site: &str) -> bool` using compiled lazy regex:
- If `site` matches `(?i)\bchess\.?com\b`, return `true`.
- If `event` matches `(?i)(\btitled\s+tuesday\b|\bpro(?:\s+chess)?\s+league\b|\barena\s+kings\b|\b(?:speed|bullet)\s+chess\s+championship\b)`, return `true`.
- If `event` matches `(?i)\bchess\.?com\b` and `site` is either empty or not a known physical venue, return `true`.
Add `exclude_chesscom: bool` (default: `true`) to `PredictionConfig`.
In `PlayerBook::add_game`:
```rust
if self.config.exclude_chesscom && is_chesscom_event(game.event(), game.site()) {
    return;
}
```

- [x] **Step 4: Run test to verify it passes**
Run: `cargo test --lib players::tests::chesscom_exclusion`
Expected: PASS

- [x] **Step 5: Commit**
```bash
git add src/domain.rs src/players.rs
git commit -m "feat(prep): exclude online chess.com tournaments from player preparation"
```

---

### Task 3: Disable Move Predictor v2 in UI & Display Plain Statistics

**Files:**
- Modify: `src/app.rs:10696-10890`

**Interfaces:**
- Consumes: `book.moves(key) -> Vec<PlayerMoveStat>`, `PlayerPositionStats`
- Produces: Plain observed move statistics table in Preparation view, preserving all v2 prediction math in `src/players.rs`.

- [x] **Step 1: Check existing `prep_tree` rendering in `src/app.rs`**
Verify how `prediction_data` is used in `src/app.rs` around lines 10696–10890.

- [x] **Step 2: Update preparation moves display to render plain observed statistics**
In `prep_tree`:
- For opponent turn, fetch observed moves from `book.moves(key)`.
- If moves exist, display:
  - Header: summary of observed games in position (`N player games here · W X · D Y · L Z · score S%`).
  - Table columns: `Move`, `Games`, `Score`, `W/D/L`, `Last Played`.
  - Rows: figurine SAN (clickable to play, hoverable to preview), count of games, score %, `+W =D -L`, and last played date.
- If no moves exist: display `"No direct games by [Player] reached this position."`.
- Keep `self.prep_prediction_data(...)` and `players::predict` available in codebase for future re-enabling.

- [x] **Step 3: Run existing player tests to ensure no regressions**
Run: `cargo test --lib players`
Expected: PASS

- [x] **Step 4: Commit**
```bash
git add src/app.rs
git commit -m "feat(prep): display plain observed move statistics in preparation view"
```

---

### Task 4: Fix Player & Preparation Games Display for Virtual / Mega Database

**Files:**
- Modify: `src/library.rs` (ensure helper methods on `GameLibrary`)
- Modify: `src/app.rs` (fix `prep_recent_data`, `players_content`, and game list rows to use `game_headers`)
- Test: `tests/twocbh_local.rs` (verify player games and prep recent games on virtual database)

**Interfaces:**
- Consumes: `library.game_headers(index)`, `library.game_players(index)`
- Produces: Working game lists in Player Profile and Preparation view for virtual databases.

- [x] **Step 1: Write test in `tests/twocbh_local.rs` for virtual library header retrieval**
Verify `game_headers(0)` and `game_players(0)` succeed on virtual databases.

- [x] **Step 2: Run test to verify behavior**
Run: `cargo test --test twocbh_local`
Expected: PASS

- [x] **Step 3: Update `src/app.rs` to use `game_headers` instead of `library.get()`**
- In `prep_recent_data` (line ~10498):
  Replace `library.get(game.index)` with `library.game_headers(game.index)`.
- In `players_content` (lines ~7746–7754 and ~8022–8080):
  Replace `library.get(idx)` with `library.game_players(idx)` and `library.game_headers(idx)`.

- [x] **Step 4: Run tests**
Run: `cargo test --test twocbh_local`
Expected: PASS

- [x] **Step 5: Commit**
```bash
git add src/library.rs src/app.rs tests/twocbh_local.rs
git commit -m "fix(library): support game lists and prep recent games in virtual databases"
```

---

### Task 5: Background Player Indexing on Database Open & Large DB Index Persistence Prompt

**Files:**
- Modify: `src/app.rs` (trigger background player indexing on open; show persist index prompt for databases >500k games)

**Interfaces:**
- Consumes: `library.len() > 500_000`, `index_rx`
- Produces: Instant background player indexing on load, index persistence prompt on completion for large databases.

- [x] **Step 1: Ensure background player indexing starts immediately upon open**
In `install_library`:
- For all databases, trigger player indexing immediately (in background thread for large databases).
- Ensure `self.players_status` accurately reflects background progress.

- [x] **Step 2: Implement position index persistence prompt for databases > 500k games**
In `src/app.rs`:
- Add state `prompt_persist_index: Option<PathBuf>` to `App`.
- When position indexing completes (in `poll_index`, `poll_tree_index`, `poll_reference`), if database game count > 500,000 and index was newly built:
  Set `prompt_persist_index = Some(db_path)`.
- Render a notification/prompt in the status bar or Tree tab:
  "Position index complete (X games). Persist index to disk for instant reloads next time? [Save Index] [Dismiss]"
- When [Save Index] is clicked: write cached index to disk and clear prompt.

- [x] **Step 3: Verify build and run tests**
Run: `cargo test --lib app`
Expected: PASS

- [x] **Step 4: Commit**
```bash
git add src/app.rs
git commit -m "feat(index): start player indexing on open and prompt to persist index for large databases"
```

---

### Task 6: Bump Version to 0.1.2, Run Verification, and Push Release

**Files:**
- Modify: `Cargo.toml:3` (`version = "0.1.2"`)
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: All completed tasks
- Produces: Release tag `v0.1.2` pushed to remote repository.

- [x] **Step 1: Bump version in `Cargo.toml`**
Change `version = "0.1.1"` to `version = "0.1.2"`.
Run `cargo check` to update `Cargo.lock`.

- [x] **Step 2: Run formatting and clippy**
Run:
`cargo fmt --all -- --check`
`cargo clippy --all-targets -- -D warnings`

- [x] **Step 3: Run full test suite**
Run: `cargo test --all-targets`

- [ ] **Step 4: Commit and tag release**
```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.1.2"
git tag -a v0.1.2 -m "Release v0.1.2"
```

- [ ] **Step 5: Push commits and tags**
```bash
git push origin main
git push origin v0.1.2
```
Expected: Pushed to GitHub; GitHub Actions triggers build and release workflow.

- [ ] **Step 4: Commit and tag release**
```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.1.2"
git tag -a v0.1.2 -m "Release v0.1.2"
```

- [ ] **Step 5: Push commits and tags**
```bash
git push origin main
git push origin v0.1.2
```
Expected: Pushed to GitHub; GitHub Actions triggers build and release workflow.

