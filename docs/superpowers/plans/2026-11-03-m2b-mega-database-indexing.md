# M2b: High-Performance Multi-Million-Game Indexing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement zero-allocation mainline move streaming, parallel Rayon chunked ingestion with in-memory edge aggregation, a tiered LSM-style multi-segment position index reader, asynchronous background compaction, and remove artificial 1M-game UI guards so archives like Mega Database 2025 (11.15M+ games) index at 50,000–100,000+ games/sec with bounded memory (< 512 MiB RSS).

**Architecture & Specifications:** Described in [docs/superpowers/specs/2026-11-03-m2b-mega-database-indexing-design.md](file:///home/gratis01/Programs/Brett/docs/superpowers/specs/2026-11-03-m2b-mega-database-indexing-design.md).

---

## File Structure & Responsibilities

| File | Responsibility |
|---|---|
| `src/cbh/fast_index.rs` | Zero-allocation mainline move streamer for classic CBH (`.cbg`/`.cbj`) using stack-allocated board and `PieceLists`. |
| `src/twocbh/fast_index.rs` | Zero-allocation mainline move streamer for 2CBH (`.2cbg`) using 16-bit token stream and stack-allocated board. |
| `src/position/segment.rs` | Immutable sorted segment file writer (`.bpx`) and memory-mapped reader (`IndexSegment`). |
| `src/position/segmented_index.rs` | Multi-segment `PositionIndex` reader: binary searches across all active segments, live progressive availability, and delta segment appends. |
| `src/position/chunked_builder.rs` | Parallel Rayon chunked builder (250k games per chunk) with in-memory pre-aggregation, flushing segments to disk. |
| `src/position/compactor.rs` | Asynchronous $K$-way streaming min-heap compactor that consolidates active segments into a single `.brettpos` file. |
| `src/position.rs` | Re-exports segmented index, builder, and compactor; maintains compatibility with existing `PositionIndex` API. |
| `src/app.rs` | Lifts `FULL_INDEX_MAX_GAMES = 1_000_000` cap, binds live progress reporting (games/sec, segment counts, ETA), and connects async indexing. |

---

## Tasks

### Task 1: Zero-Allocation CBH Mainline Move Streamer

**Context:** Currently, `PositionIndex::build_library_to` loads full `domain::Game` objects via `library.load(i)`. For classic CBH, we can read `.cbg` move records directly with a stack-allocated `cozy_chess::Board` and `PieceLists`, decoding only the mainline moves up to `max_plies` with zero heap allocation per game.

- [ ] **Step 1.1: Write failing test for CBH fast mainline streamer**
  In `src/cbh/tests.rs` (or `src/cbh/fast_index.rs`), test that `stream_cbh_mainline` decodes identical move plies and Zobrist keys to `library.load(i)` for standard starts, promotions, castling, and Chess960.
- [ ] **Step 1.2: Implement `stream_cbh_mainline` in `src/cbh/fast_index.rs`**
  Walk `.cbg` bytes using `PieceLists::scan` and `tables::MODE_*`, playing moves directly on `Chess` / `cozy_chess::Board` and invoking a callback `consumer(key: u64, move_code: u32)`. Stop when the mainline line terminates or `plies >= max_plies`.
- [ ] **Step 1.3: Run tests and verify**
  Run `cargo test --lib cbh::fast_index` and ensure all tests pass.
- [ ] **Step 1.4: Commit**
  `git commit -m "feat(cbh): zero-allocation mainline move streamer for position indexing"`

---

### Task 2: Zero-Allocation 2CBH Mainline Move Streamer

**Context:** 2CBH stores moves in `.2cbg` as a stream of 16-bit tokens. The mainline is the first line ending with `END_LINE` (`0xFFFF`). We can step the board directly without allocating variations, AST nodes, or comments.

- [ ] **Step 2.1: Write failing test for 2CBH fast mainline streamer**
  In `src/twocbh/tests.rs`, test that `stream_2cbh_mainline` matches `library.load(i)` mainline moves across standard games and Chess960 starts.
- [ ] **Step 2.2: Implement `stream_2cbh_mainline` in `src/twocbh/fast_index.rs`**
  Iterate `tokens: &[u16]`, convert each move token to `Move`, play unchecked on `cozy_chess::Board`, and invoke `consumer(key, move_code)`. Break on `END_LINE` or when `plies >= max_plies`.
- [ ] **Step 2.3: Run tests and verify**
  Run `cargo test --lib twocbh::fast_index` and ensure tests pass.
- [ ] **Step 2.4: Commit**
  `git commit -m "feat(twocbh): zero-allocation mainline move streamer for 2cbh indexing"`

---

### Task 3: Immutable Segment Format & Multi-Segment Querying

**Context:** Instead of a single monolithic `.brettpos` file, an index consists of one or more sorted segment files (`IndexSegment`). Each segment is an immutable memory-mapped file with an 80-byte header and 48-byte sorted `EdgeRecord`s.

- [ ] **Step 3.1: Write failing test for multi-segment binary search and merging**
  Create two small synthetic segment files with overlapping and distinct positions. Assert that querying `moves(key)` across both segments sums `games`, `white`, `draw`, `black`, and Elo counts accurately, matching a pre-merged index.
- [ ] **Step 3.2: Implement `IndexSegment` in `src/position/segment.rs`**
  Implement `IndexSegment::open(path)`, `IndexSegment::write(path, header, records)`, and `IndexSegment::moves(key) -> &[EdgeRecord]`.
- [ ] **Step 3.3: Implement multi-segment query in `src/position/segmented_index.rs`**
  Implement `SegmentedIndex` holding `Vec<IndexSegment>`. Querying a key executes binary search across each segment and aggregates matching records in a small heap or hash map into `Vec<MoveStat>`.
- [ ] **Step 3.4: Run tests and verify**
  Run `cargo test --lib position::segment` and ensure all tests pass.
- [ ] **Step 3.5: Commit**
  `git commit -m "feat(position): immutable sorted segments and multi-segment querying"`

---

### Task 4: Parallel Rayon Chunked Builder with In-Memory Pre-Aggregation

**Context:** The builder partitions the database into 250,000-game chunks. Rayon processes chunks in parallel. Each worker maintains an in-memory `HashMap<(u64, u32), EdgeRecord>` where repeated moves aggregate *before* writing to disk.

- [ ] **Step 4.1: Write failing test for chunked builder with edge pre-aggregation**
  Build a test database in chunks of 500 games with max edges per chunk. Verify that the emitted run segments are individually sorted and deduplicated, and that the combined query results match a single-pass build.
- [ ] **Step 4.2: Implement `build_chunked_parallel` in `src/position/chunked_builder.rs`**
  Use Rayon `into_par_iter()` over chunks of game indices. Each worker streams mainline moves using the fast streamers (or streaming PGN visitor), accumulates edges in memory, sorts by `(key, move_code)`, flushes a `.tmp_segment_XXX.bpx` file, and reports progress.
- [ ] **Step 4.3: Connect progressive availability to `SegmentedIndex`**
  As each segment flushes, register it with the active `SegmentedIndex` so the opening tree can query it immediately while subsequent chunks are still processing.
- [ ] **Step 4.4: Run tests and verify**
  Run `cargo test --lib position::chunked_builder` and verify parallel correctness.
- [ ] **Step 4.5: Commit**
  `git commit -m "feat(position): parallel chunked builder with in-memory pre-aggregation"`

---

### Task 5: Asynchronous Background Compactor ($K$-Way Merge) & Incremental Delta Appends

**Context:** When all chunks complete, an asynchronous compactor merges the segments into a single consolidated `base.brettpos` file and unlinks the temporary segment files. If 5,000 new games are imported later, they are written to a small delta segment without rewriting the base index.

- [ ] **Step 5.1: Write failing test for background compactor and delta append**
  Verify that compacting 5 segments produces a bit-for-bit valid consolidated `.brettpos` index, and that appending a new delta segment updates position counts correctly.
- [ ] **Step 5.2: Implement `compact_segments` in `src/position/compactor.rs`**
  Stream active segments through the existing `write_merged_sources` min-heap algorithm into `base.brettpos.tmp`, atomically rename to `base.brettpos`, update the reader's segments to `[base.brettpos]`, and clean up intermediate segment files.
- [ ] **Step 5.3: Implement `append_delta_segment` in `src/position/segmented_index.rs`**
  Allow building and registering a new delta segment when new games are appended to an existing database.
- [ ] **Step 5.4: Run tests and verify**
  Run `cargo test --lib position::compactor` and ensure all tests pass.
- [ ] **Step 5.5: Commit**
  `git commit -m "feat(position): background k-way compactor and incremental delta segments"`

---

### Task 6: UI Integration & Removing the 1M-Game Guard

**Context:** In `src/app.rs`, lift `FULL_INDEX_MAX_GAMES = 1_000_000`. Connect `build_chunked_parallel` to background tasks with live progress (throughput in games/sec, active segments, compaction state).

- [x] **Step 6.1: Update limits and background index triggers in `src/app.rs`**
  Remove the `FULL_INDEX_MAX_GAMES` cap in `start_index`, `start_tree_index`, `start_correspondence_index`, and `start_reference_index`.
- [x] **Step 6.2: Connect real-time progress and throughput metrics**
  Display current throughput (e.g., `"Indexing: 3.5M / 11.1M games (45k games/sec · 14 segments live)"`) in `tree_status` and index progress bars.
- [x] **Step 6.3: Verify end-to-end with unit and integration tests**
  Run `cargo test --all-targets` to verify that all 325+ tests pass with zero regressions.
- [x] **Step 6.4: Format and lint**
  Run `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings`.
- [x] **Step 6.5: Commit and push**
  `git commit -m "feat(app): lift 1M-game cap and bind live multi-million-game indexing progress"`

