# Local Analysis Cache & Multi-Source Engine Integration

Date: 2026-11-02
Status: design for approval

## Intent

As a chess player analyzes positions using local engines (Stockfish, Leela Chess Zero, etc.), each analysis computes deep move evaluations and variations that are currently lost when navigating to another position or closing the application. This project builds a persistent, high-performance local analysis cache that automatically stores analyzed positions (engine identity, depth, score, and principal variations).

The cache intelligently manages disk writes to remain completely transparent to the user: fast, early plies are debounced to write periodically (every 10s), while deep plies commit on every ply. The cache distinguishes engine families (never letting Stockfish overwrite Leela evaluations), compares Stockfish runs via a calibrated quality heuristic (depth, version baseline Elo, seldepth, nodes), and integrates seamlessly into the Cloud Engine view alongside ChessDB.cn and Lichess Cloud Eval—presenting local lines when their quality is competitive with or superior to remote data, and formatting multiple variations cleanly in a stacked two-line layout.

## Goals

1. **Persistent Local Cache:**
   - Store locally analyzed positions in an embedded SQLite database (`analysis.db`) in `$XDG_CACHE_HOME/brett/` (or `$HOME/.cache/brett/`) using WAL mode.
   - Key records by position hash (canonical FEN / Zobrist hash), engine family, engine version, multipv index, depth, score (`cp` / `mate`), nodes, seldepth, time, and UCI PV line.

2. **Smart Write Throttling:**
   - During rapid initial search (depth < 20 plies or > 1 ply/sec), buffer in memory and flush to SQLite at most once every 10 seconds.
   - At deep plies (depth >= 20 plies) or upon search completion (`bestmove` / stop), commit immediately per ply increment.
   - Non-blocking writes via a dedicated worker thread or async batch channel so egui frame rates never drop.

3. **Multi-Engine Family Retention:**
   - Partition engine records by family: `Stockfish`, `Leela` (`lc0`), and `Generic`.
   - Higher Stockfish depth will never overwrite Leela entries; both coexist for the same position.

4. **Fine-Grained Stockfish Evaluation Comparison & Superseding:**
   - Implement a calibrated quality score $Q(v, d, \text{seldepth}, \text{nodes})$ incorporating version baseline strength and depth scaling to evaluate whether a newer build at shallower depth beats an older build at deeper depth (or vice-versa).
   - If the new analysis exceeds the cached entry by a stability margin, it supersedes the previous entry.
   - Provide a user setting in **Settings $\rightarrow$ Engine** to "Supersede older Stockfish versions using quality heuristic" (default: true) vs. "Keep all engine versions unless identical".

5. **Cloud Engine View Integration (ChessDB, Lichess, Local):**
   - Query the local cache concurrently with ChessDB.cn and Lichess cloud eval when visiting a position.
   - **Dynamic Columns:** Display `chessdb eval` | `Lichess eval` | `Local eval` columns dynamically based on availability.
   - **Quality Gate:** Suppress weak, shallow local evaluations when deep remote Lichess data already exists, ensuring local lines appear when they provide superior or off-book value.
   - **Variation Stacking:** For candidate moves with multiple available PVs (Lichess and Local), render them in a stacked 2-line layout (`\n`) with the deeper variation on top, collapsing into a single row when only one source provides a line.

## Non-goals

- Replacing the real-time UCI engine worker with a pure database replay; the active engine continues to run as requested.
- Storing entire multi-million game move trees in SQLite (game indexing remains in `.brettpos` memory-mapped files).
- Network syncing or cloud uploads of personal analysis.
- Third-party database imports (e.g. ChessBase `.cbh` engine analysis books) in this initial pass.

## Constraints

- Pure Rust and standard open-source dependencies. SQLite is embedded via `rusqlite` with the `bundled` feature flag to avoid requiring host system SQLite development headers.
- Safe crash recovery: WAL mode (`PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;`) ensures no database corruption on abrupt termination or SIGKILL.
- Memory safety and zero UI frame hitching: Database reads and writes are offloaded or executed in bounded micro-transactions.

## Approaches

### Approach 1: Embedded SQLite (`rusqlite` bundled) with WAL and background write worker (Chosen)
- **Architecture:** SQLite database with indexed `(pos_hash, engine_family, multipv)` schema. Writes pass through an in-memory debouncer channel to a worker thread.
- **Why Chosen:** Provides immediate ACID compliance, robust indexing, simple handling of multi-PV lines, zero chance of file corruption during power loss, and clean schema migrations.

### Approach 2: Custom Append-Only Binary Log with In-Memory Index
- **Architecture:** Binary file similar to `src/position.rs` with in-memory hash map.
- **Trade-off:** Avoids the `rusqlite` dependency, but requires complex manual compaction, tombstone management when superseding records, and custom crash recovery. Rejected for long-term data integrity risks.

## Detailed Architecture & Implementation

### 1. Storage Schema (`src/analysis_cache.rs`)

```sql
CREATE TABLE IF NOT EXISTS position_analysis (
    pos_hash INTEGER NOT NULL,          -- 64-bit Zobrist hash of position
    fen TEXT NOT NULL,                  -- canonical EPD/FEN
    engine_family TEXT NOT NULL,        -- 'stockfish', 'leela', 'other'
    engine_name TEXT NOT NULL,          -- e.g. 'Stockfish 17', 'Lc0 v0.31.2'
    multipv INTEGER NOT NULL,           -- 1-based multipv index
    depth INTEGER NOT NULL,
    seldepth INTEGER,
    score_cp INTEGER,
    score_mate INTEGER,
    nodes INTEGER,
    time_ms INTEGER,
    nps INTEGER,
    pv TEXT NOT NULL,                   -- space-separated UCI move strings
    quality_score REAL NOT NULL,        -- computed heuristic quality rating
    analyzed_at INTEGER NOT NULL,       -- unix epoch seconds
    PRIMARY KEY (pos_hash, engine_family, multipv, engine_name)
);

CREATE INDEX IF NOT EXISTS idx_pos_family ON position_analysis(pos_hash, engine_family);
```

### 2. Write-Throttling & Debounce Mechanism

```
Engine Event Stream (src/engine.rs / self.engine / self.review_engine)
         │
         ▼
AnalysisDebouncer (in src/app.rs or src/analysis_cache.rs)
  ├─ If depth < 20 and time_since_last_flush < 10s:
  │    Buffer latest EngineInfo in memory
  ├─ If depth >= 20 and depth > last_persisted_depth:
  │    Flush immediately to SQLite worker
  └─ On BestMove or Search Stop:
       Flush buffered lines immediately
```

### 3. Fine-Grained Stockfish Evaluation Decision Heuristic

To decide whether an incoming analysis $A_{\text{new}}$ is more valuable than an existing cached analysis $A_{\text{old}}$, we compute a composite **Quality Rating** $Q$:

$$Q(A) = R(V) + \text{SearchStrength}(d) + B_{\text{seldepth}} + B_{\text{nodes}} + B_{\text{mate}}$$

1. **Version Baseline Elo $R(V)$:**
   Calibrated from Fishtest and CCRL rating lists across Stockfish versions:
   - Stockfish 11: 3450
   - Stockfish 12: 3520 (First NNUE)
   - Stockfish 13: 3570
   - Stockfish 14: 3620
   - Stockfish 15: 3690
   - Stockfish 16: 3760
   - Stockfish 16.1: 3790
   - Stockfish 17: 3830
   - Git development builds: $3830 + 0.25 \times (\text{days since SF 17 release})$.
   - Unknown/Generic: 3400.

2. **Depth Scaling Function $\text{SearchStrength}(d)$:**
   Computer chess search strength exhibits diminishing returns per nominal ply:
   - Plies 1 to 18: $+50 \text{ Elo / ply}$ (rapid tactical settling)
   - Plies 19 to 30: $+35 \text{ Elo / ply}$ (strategic consolidation)
   - Plies 31+: $+20 \text{ Elo / ply}$ (deep horizon resolution)

   $$\text{SearchStrength}(d) = \sum_{p=1}^d \text{EloPerPly}(p)$$

3. **Bonus Signals:**
   - **Proven Mate ($B_{\text{mate}}$):** $+20,000$ points. If both have mate, shorter distance to win or longer distance in defense is preferred.
   - **Selective Depth ($B_{\text{seldepth}}$):** $+0.5 \times \max(0, \text{seldepth} - d)$ (rewards tactical branching verification).
   - **Nodes Searched ($B_{\text{nodes}}$):** $+3.0 \times \log_{10}(\max(1, \text{nodes}) / 10^5)$ (resolves exact depth ties).

4. **Replacement & MultiPV Granularity:**
   - If `engine_name` is identical: supersede whenever $d_{\text{new}} \ge d_{\text{old}}$.
   - MultiPV runs update the specific `multipv` slots searched (e.g. a 3-PV search updates slots 1, 2, 3; an in-depth 1-PV refinement updates slot 1 without wiping slots 2 and 3).
   - When comparing different Stockfish versions:
     - When `heuristic_supersede == true` (default): supersede if $Q(A_{\text{new}}) > Q(A_{\text{old}}) + 15.0$ (15 Elo stability hysteresis).
     - When `heuristic_supersede == false`: keep both distinct versions in the cache.

### 4. Integration with Cloud Engine (ChessDB & Lichess)

1. **Lookup & Live Streaming:**
   - When visiting a position, query local `analysis.db` concurrently with ChessDB and Lichess.
   - When the user runs the live engine on the position, live lines feed into the Cloud Engine view as soon as they meet the quality gate, transitioning seamlessly to the cached record when persisted.
2. **Quality Gate:**
   Local eval qualifies to be displayed if:
   - No Lichess cloud eval exists for this position (e.g. deeper in the game tree, off-book lines, or custom variants), OR
   - Local search is deep ($d_{\text{local}} \ge 22$) and $d_{\text{local}} \ge d_{\text{lichess}} - 4$, OR
   - Local search has a hard minimum depth floor ($d_{\text{local}} \ge 18$), is within 50 cp of the best move, and uncovered a candidate move not present in Lichess's top PVs.
3. **Table Sorting & Centipawn Averaging:**
   - Local scores (converted to White POV) factor into candidate move centipawn averaging.
   - If local analysis introduces a candidate move absent from remote sources, it creates an active candidate row with its local variation line.
4. **UI Layout:**
   - Columns adjust dynamically:
     - `chessdb eval` (visible if ChessDB has scores)
     - `Lichess eval (dXX)` (visible if Lichess has cloud eval)
     - `Local eval (dYY)` (visible if local cache has qualified eval)
   - Variation lines:
     - If both Lichess and Local PVs are available for a candidate move:
       - Line 1 (Top): Deeper variation (tagged with source & depth, e.g. `[Local d38]`).
       - Line 2 (Bottom): Secondary variation (e.g. `[Lichess d32]`).
     - If only one source has a variation: collapses into a single line.
     - Move interaction (hover preview and click to play line) works identically across both lines.

## User Configuration & Preferences

In **Settings $\rightarrow$ Engine / Analysis**:
- `Cache local engine analysis` (checkbox, default: `true`).
- `Supersede older Stockfish versions using quality heuristic` (checkbox, default: `true`).
- `Cache size / clear cache button` (shows total positions cached, option to purge).

## Verification Plan

1. **Unit Tests:**
   - `analysis_cache` round-trip tests (insert, query by position hash, multi-PV retrieval).
   - Write-throttling / debounce unit tests (10s threshold vs. per-ply deep commit).
   - Stockfish version parsing and quality calculation tests (verifying older deep runs supersede shallow newer runs and vice versa).
   - Family separation test (verifying Leela and Stockfish coexist without overwriting).
2. **Integration Tests:**
   - End-to-end engine analysis run: start engine on position, ensure entries stream into cache.
   - UI row merging: simulate ChessDB, Lichess, and Local cache snapshots; assert dynamic columns and stacked 2-line layout behavior.
3. **Compilation & Clippy:**
   - `cargo test --all-targets`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo fmt --all -- --check`
