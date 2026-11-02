# Local Analysis Cache & Multi-Source Engine Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provide a high-performance, persistent local evaluation cache that stores analyzed positions, throttles writes during fast early plies, separates engine families (preserving Leela alongside Stockfish), compares Stockfish versions via a calibrated quality heuristic, and seamlessly integrates with ChessDB.cn and Lichess Cloud Eval in the position view.

**Architecture:** An embedded SQLite database (`analysis.db` with WAL mode) stores position analysis keyed by position hash, engine family, and multipv index. An asynchronous debouncer buffers streaming UCI events and persists them to disk (every 10s during fast shallow search, immediately per ply for deep search). When visiting positions, the UI queries the cache concurrently with ChessDB and Lichess, gating local lines by quality, dynamically showing available source columns, and stacking two variations (deeper on top) with single-line collapse.

**Tech Stack:** Rust 2024, `rusqlite` (bundled), `cozy-chess` position hashing, `egui` 0.37, existing UCI `engine.rs` and `cloud.rs`.

**Spec:** `docs/superpowers/specs/2026-11-02-local-analysis-cache-design.md`

## Global Constraints

- Pure Rust and open source: SQLite embedded via `rusqlite` with `bundled` feature flag (no external host C library dependencies).
- Safe crash recovery: WAL mode (`PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;`) ensures zero database corruption on power loss or SIGKILL.
- Zero UI frame drops: Database writes occur in a background worker thread via `std::sync::mpsc`.
- Engine family separation: Leela Chess Zero (`lc0`) and Stockfish never overwrite each other.
- Stockfish quality heuristic: Piecewise depth scaling (+50 Elo/ply d<=18, +35 Elo/ply d 19..30, +20 Elo/ply d 31+) + version baseline Elo + mate/seldepth bonuses.
- Write throttling: Depth < 20 throttles to at most once per 10 seconds; depth >= 20 commits immediately per ply increment.
- Quality gate: Local eval suppresses weak noise when deep Lichess data exists, but surfaces when deep or introducing candidate novelties.
- Variation stacking: Deeper variation on top; collapses to single line when only one source is available.

## Review Focus

- Correctness of Zobrist position hashing for standard chess and Chess960.
- Thread safety and non-blocking background commits when analyzing rapidly.
- Clean handling of corrupted or missing SQLite files.
- Accurate Stockfish version string parsing across release tags and date-stamped dev builds.
- Seamless collapsing from 2-line stacked variations to 1-line when either Lichess or Local is absent.

---

### Task 1: SQLite Storage Engine and Data Model (`src/analysis_cache.rs`)

**Files:**
- Modify: `Cargo.toml`
- Create: `src/analysis_cache.rs`
- Modify: `src/lib.rs`
- Test: `src/analysis_cache/tests.rs` (or inline `mod tests` in `src/analysis_cache.rs`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
  pub enum EngineFamily {
      Stockfish,
      Leela,
      Generic,
  }

  #[derive(Clone, Debug, PartialEq)]
  pub struct CachedAnalysis {
      pub pos_hash: u64,
      pub fen: String,
      pub engine_family: EngineFamily,
      pub engine_name: String,
      pub multipv: usize,
      pub depth: u32,
      pub seldepth: Option<u32>,
      pub score_cp: Option<i32>,
      pub score_mate: Option<i32>,
      pub nodes: Option<u64>,
      pub time_ms: Option<u64>,
      pub nps: Option<u64>,
      pub pv: Vec<String>,
      pub quality_score: f64,
      pub analyzed_at: u64,
  }

  pub struct AnalysisDb {
      conn: rusqlite::Connection,
  }

  impl AnalysisDb {
      pub fn open_at(path: &Path) -> Result<Self>;
      pub fn open_default() -> Result<Self>;
      pub fn get_position(&self, pos_hash: u64) -> Result<Vec<CachedAnalysis>>;
      pub fn store(&mut self, analysis: &CachedAnalysis, supersede_older: bool) -> Result<bool>;
      pub fn count_positions(&self) -> Result<usize>;
      pub fn clear(&mut self) -> Result<()>;
  }
  ```

- [ ] **Step 1: Write failing unit tests**
  - Test SQLite schema creation, WAL mode configuration, storing and querying entries by position hash.
  - Test that Leela entries and Stockfish entries for the same position coexist under their respective `engine_family` keys.
  - Test multi-PV line storage and retrieval.

- [ ] **Step 2: Run test to verify failure**
  - Run: `cargo test --lib analysis_cache`
  - Expected: Failure because `rusqlite` and `analysis_cache` do not exist yet.

- [ ] **Step 3: Implement `src/analysis_cache.rs` and update `Cargo.toml`**
  - Add `rusqlite = { version = "0.33", features = ["bundled"] }` to `Cargo.toml`.
  - Implement SQLite initialization with table `position_analysis` and index `idx_pos_family`.
  - Register `pub mod analysis_cache;` in `src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**
  - Run: `cargo test --lib analysis_cache`
  - Expected: PASS.

- [ ] **Step 5: Commit**
  - `git add Cargo.toml Cargo.lock src/analysis_cache.rs src/lib.rs`
  - `git commit -m "feat(cache): add persistent SQLite analysis database engine"`

---

### Task 2: Fine-Grained Stockfish Quality Heuristic & Version Parser

**Files:**
- Modify: `src/analysis_cache.rs`
- Test: `src/analysis_cache.rs` (unit tests)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Debug, PartialEq)]
  pub struct ParsedEngineVersion {
      pub family: EngineFamily,
      pub major: Option<u32>,
      pub minor: Option<u32>,
      pub dev_date: Option<u32>, // YYYYMMDD
      pub baseline_elo: f64,
  }

  pub fn parse_engine_identity(name: &str) -> ParsedEngineVersion;
  pub fn compute_search_strength(depth: u32) -> f64;
  pub fn compute_quality_rating(
      family: EngineFamily,
      baseline_elo: f64,
      depth: u32,
      seldepth: Option<u32>,
      nodes: Option<u64>,
      score_mate: Option<i32>,
  ) -> f64;

  pub fn should_supersede(
      incoming: &CachedAnalysis,
      existing: &CachedAnalysis,
      heuristic_supersede: bool,
  ) -> bool;
  ```

- [ ] **Step 1: Write failing unit tests**
  - Test parsing `"Stockfish 17"`, `"Stockfish 16.1"`, `"Stockfish dev-20241018"`, `"Lc0 v0.31.2"`.
  - Test piecewise depth scaling function (+50 for plies 1..18, +35 for 19..30, +20 for 31+).
  - Test quality comparison:
    - Same engine name: supersedes whenever $d_{\text{new}} \ge d_{\text{old}}$.
    - SF 16 deep ($d=26$) beats SF 17 shallow ($d=20$).
    - SF 17 deep ($d=24$) supersedes SF 16 ($d=26$).
    - Proven mate bonus dominance.
    - Opt-out toggle behavior (`heuristic_supersede == false` only supersedes on identical engine name).

- [ ] **Step 2: Run test to verify failure**
  - Run: `cargo test --lib analysis_cache::tests::test_quality_rating`
  - Expected: FAIL (functions not yet implemented).

- [ ] **Step 3: Implement quality rating and superseding logic**
  - Implement parsing, piecewise integration, bonuses, and replacement rules in `src/analysis_cache.rs`.

- [ ] **Step 4: Run test to verify it passes**
  - Run: `cargo test --lib analysis_cache`
  - Expected: PASS.

- [ ] **Step 5: Commit**
  - `git add src/analysis_cache.rs`
  - `git commit -m "feat(cache): implement fine-grained Stockfish quality heuristic and version parser"`

---

### Task 3: Background Worker & Analysis Write Debouncer

**Files:**
- Modify: `src/analysis_cache.rs`
- Test: `src/analysis_cache.rs` (unit tests)

**Interfaces:**
- Produces:
  ```rust
  pub struct AnalysisDebouncer {
      last_flush: Instant,
      last_persisted_depth: u32,
      buffered: BTreeMap<usize, CachedAnalysis>,
  }

  pub enum CacheCommand {
      Store(CachedAnalysis),
      StoreBatch(Vec<CachedAnalysis>),
      Flush,
  }

  pub struct AnalysisCacheHandle {
      sender: std::sync::mpsc::Sender<CacheCommand>,
  }

  impl AnalysisCacheHandle {
      pub fn spawn(db_path: Option<PathBuf>, heuristic_supersede: bool) -> Self;
      pub fn store(&self, analysis: CachedAnalysis);
      pub fn store_batch(&self, analyses: Vec<CachedAnalysis>);
  }
  ```

- [ ] **Step 1: Write failing unit tests**
  - Test debouncer throttling: mock rapid incoming info events at $d < 20$ ensuring flushes occur only after 10s interval.
  - Test deep plies: incoming events at $d \ge 20$ trigger immediate commit on ply increments.
  - Test search completion (`bestmove` / stop) immediately flushing any buffered lines.

- [ ] **Step 2: Run test to verify failure**
  - Run: `cargo test --lib analysis_cache::tests::test_debouncer`
  - Expected: FAIL.

- [ ] **Step 3: Implement `AnalysisDebouncer` and `AnalysisCacheHandle` background worker**
  - Implement background thread processing `CacheCommand` channel, wrapping transactions in `AnalysisDb`.

- [ ] **Step 4: Run test to verify it passes**
  - Run: `cargo test --lib analysis_cache`
  - Expected: PASS.

- [ ] **Step 5: Commit**
  - `git add src/analysis_cache.rs`
  - `git commit -m "feat(cache): add background worker and 10s/per-ply write debouncer"`

---

### Task 4: Multi-Source Engine Rows & Cloud Engine Integration

**Files:**
- Modify: `src/app.rs`
- Test: `src/app.rs` (unit tests)

**Interfaces:**
- Updates `CloudEngineRow` in `src/app.rs`:
  ```rust
  struct CloudEngineRow {
      uci: String,
      san: String,
      chessdb: Option<i32>,
      lichess: Option<(Option<i32>, Option<i32>)>, // (cp, mate)
      lichess_depth: Option<u32>,
      local: Option<(Option<i32>, Option<i32>)>,   // (cp, mate)
      local_depth: Option<u32>,
      local_engine: Option<String>,
      variation: Vec<String>,       // primary (deeper) line
      alt_variation: Option<Vec<String>>, // secondary line if both present
      primary_is_local: bool,
  }
  ```
- Functions:
  - `merge_multi_source_engine_rows(snapshot, local_cached, pos, active_family) -> Vec<CloudEngineRow>`
  - Quality gate check: suppresses local eval if shallow and deep Lichess data exists; accepts if deep or novelty.
  - Stacking UI renderer: 2-line layout with deeper line on top, collapsing to single line when only 1 line exists.

- [ ] **Step 1: Write failing unit tests**
  - Test merging: when ChessDB, Lichess (d35), and Local (d40) exist:
    - Primary line is Local (d40), secondary line is Lichess (d35).
    - All three columns show scores.
  - Test quality gate:
    - Lichess has d40; Local has d14 -> Local is suppressed.
    - No Lichess eval exists -> Local (d16) is displayed.
    - Local has d24 novelty move -> Local novelty row is displayed.
  - Test single-line collapsing: only Lichess or only Local collapses cleanly to 1 variation line.

- [ ] **Step 2: Run test to verify failure**
  - Run: `cargo test --lib app::tests::multi_source_engine_rows`
  - Expected: FAIL.

- [ ] **Step 3: Implement multi-source row merging and UI rendering in `src/app.rs`**
  - Update `cloud_engine_section` and `cloud_row_ui` to render dynamic columns (`chessdb eval`, `Lichess eval`, `Local eval`).
  - Render stacked 2-line layout with subtle source labels (`(local dXX)` / `(lichess dYY)`), clickable moves, and hover previews on both lines.

- [ ] **Step 4: Run test to verify it passes**
  - Run: `cargo test --lib app::tests::multi_source_engine_rows`
  - Expected: PASS.

- [ ] **Step 5: Commit**
  - `git add src/app.rs`
  - `git commit -m "feat(ui): integrate local cached analysis into Cloud Engine with stacked variations"`

---

### Task 5: Settings, Wiring & Persistence Lifecycle

**Files:**
- Modify: `src/settings.rs`
- Modify: `src/app.rs`
- Test: `src/settings.rs`, `src/app.rs`

**Interfaces:**
- Add to `DisplayPrefs` / `EnginePrefs`:
  - `cache_engine_analysis: bool` (default: true)
  - `heuristic_supersede: bool` (default: true)
- Wire `App`:
  - Initialize `AnalysisCacheHandle` and `AnalysisDebouncer`.
  - On `EngineEvent::Info`, feed lines to `AnalysisDebouncer`.
  - On `EngineEvent::BestMove` or engine exit, flush debouncer.
  - In `Settings -> Engine`, add toggle checkboxes and display cache stats (positions cached, clear button).

- [ ] **Step 1: Write failing unit tests**
  - Test settings JSON round-trip for `cache_engine_analysis` and `heuristic_supersede`.
  - Test that disabling `cache_engine_analysis` bypasses writes.

- [ ] **Step 2: Run test to verify failure**
  - Run: `cargo test --lib settings::tests::engine_cache_prefs`
  - Expected: FAIL.

- [ ] **Step 3: Implement settings and app lifecycle wiring**
  - Update `src/settings.rs` and `src/app.rs`.

- [ ] **Step 4: Run test to verify it passes**
  - Run: `cargo test --lib settings`
  - Expected: PASS.

- [ ] **Step 5: Commit**
  - `git add src/settings.rs src/app.rs`
  - `git commit -m "feat(settings): add analysis cache preferences and app lifecycle integration"`

---

### Task 6: End-to-End Verification and Linting

**Files:**
- All modified and new files.

- [ ] **Step 1: Run format check**
  - Run: `cargo fmt --all -- --check`
  - If diffs exist, run `cargo fmt --all`.

- [ ] **Step 2: Run clippy**
  - Run: `cargo clippy --all-targets -- -D warnings`
  - Ensure 0 warnings across all crates and targets.

- [ ] **Step 3: Run full test suite**
  - Run: `cargo test --all-targets`
  - Ensure all 300+ library tests and integration suites pass.

- [ ] **Step 4: Commit**
  - `git add .`
  - `git commit -m "chore: verify tests, clippy, and formatting for local analysis cache"`
