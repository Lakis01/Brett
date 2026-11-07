# Preparation UX, Multi-Source Streaming, Recent Games Navigation, Eval Bar & Mega Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the 256s–398s Mega Database preparation bottlenecks, fix the 1-time move-repetition forever loop, add multi-stage progress streaming & in-place source editing, uncap online/recent game fetches with cached account checks and ChessABC initial+digit matching, color game rows by font color, enable hover + arrow-key game browsing and click-at-position loading, allow opening books as General Reference Trees, and upgrade the Eval Bar with `chessdb.cn` + DB aggregate sources while holding the last reliable evaluation.

**Architecture:** We extend `PlayerBook`/`PositionIndex`/`RecentIndex` to deduplicate `(position_key, move_code)` edges rather than aborting a game on a repeated position, add a targeted `GameLibrary::accelerated_games_for_player` trait method so `TwoCbhLibrary` filters `.2lid` player names before decoding `.2lgd` postings, replace the monolithic prep loader with incremental `PrepLoadMessage` updates, add `HoveredGameBrowse` for keyboard-driven hover previewing, and expand `eval::BoardEval` with `ChessDb` and `DbAggregate` sources plus ancestor fallback.

**Tech Stack:** Rust 2024 edition, `eframe`/`egui`, `cozy-chess`, `ureq`, `memmap2`, `rayon`.

**Spec:** `docs/superpowers/specs/2026-11-07-prep-ux-eval-and-perf-design.md`

## Global Constraints

- Do not add external crates; use existing workspace dependencies only.
- Preserve all unrelated comments, docstrings, and existing tests.
- All unit tests must run offline without network access (`cargo test`).
- Game-type colors must remain: Classical OTB `Color32::from_rgb(0x7E, 0xD3, 0x8A)`, Rapid/Blitz OTB `Color32::from_rgb(0x6A, 0xB0, 0xE8)`, Online `Color32::from_rgb(0xC6, 0x8A, 0xE8)`.
- ChessABC source checkbox label in Preparation setup must be `"ChessABC games"`.

## Review Focus

1. **Game with 1-time move repetition (`1. Nf3 Nf6 2. Ng1 Ng8 3. e4 e5`):** Must index `3. e4 e5` in `PlayerBook`, `PositionIndex`, and `RecentIndex` without double-counting if the exact same `(position_key, move_code)` edge is repeated a second time, and must not push duplicate `PlayerGameRef` entries for the same game at the start position.
2. **ChessABC enumerated initials without space (`"Kern, J1"`, `"Hinterreiter, M2"`):** `matches_truncated_or_enumerated_name("Kern, Johannes", "Kern, J1")` must return `true`, and `chessabc_search_queries("Kern, Johannes")` must include `"Kern J"`.
3. **Schnellschach with unknown site (`Event "Vereins-Schnellschach"`, `Site "?"`):** `classify_game_metadata` must return `(TimeControlClass::Rapid, false, true)` (Rapid and OTB, not Online).
4. **Targeted 2CBH player lookup (`accelerated_games_for_player`):** Must only decode `.2lgd` postings for player entities whose `.2lid` name matches `player_key` or `matches_truncated_or_enumerated_name`, returning the exact game indices in `<50ms`.
5. **Eval Bar ancestor hold & reset:** Stepping to a child ply with no immediate evaluation holds the parent/ancestor's `BoardEval` (instead of jumping to `0.00`), whereas loading a different game resets the held evaluation.

---

### Task 1: Repetition Continuation Indexing, `Schnellschach` Classification & Initial+Digit Name Tokenization

**Files:**
- Modify: `src/players.rs:605-670`, `src/players.rs:860-940`, `src/players.rs:1770-1900`
- Modify: `src/position.rs:590-608`, `src/position.rs:1645-1672`
- Modify: `src/recent.rs:740-775`

**Interfaces:**
- Produces:
  - `players::matches_truncated_or_enumerated_name(full: &str, cand: &str) -> bool` (handles attached digits like `"J1"`, `"M2"`)
  - `players::classify_game_metadata(game: &Game) -> (TimeControlClass, bool, bool)` (recognizes `(?i)schnellschach` as `Rapid` and OTB)
  - Post-repetition continuation indexing in `PlayerBook::add_game`, `PositionIndex::index_game_into`, `PgnVisitor::san`, and `RecentIndex::add_game`

- [ ] **Step 1: Write the failing unit tests in `src/players.rs`**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/players.rs`:
```rust
    #[test]
    fn player_book_indexes_moves_after_single_position_repetition() {
        let pgn = b"[White \"Kern, Johannes\"]\n[Black \"Opponent, Test\"]\n[Result \"1-0\"]\n\n1. Nf3 Nf6 2. Ng1 Ng8 3. e4 e5 4. Bc4 Nc6 1-0\n";
        let games = crate::domain::parse_pgn(pgn).expect("valid pgn");
        let book = PlayerBook::from_games(
            "johannes kern",
            "Kern, Johannes",
            &games,
            PredictionConfig::default(),
            20261107,
        );
        let start = crate::chess::Chess::default();
        let key = crate::position::position_key(&start);
        let filter = PrepFilterConfig::default();
        let moves = book.filtered_moves(key, Color::White, &filter);
        let sans: Vec<String> = moves
            .iter()
            .filter_map(|m| crate::position::decode_move(&start, m.mv))
            .map(|mv| crate::chess::SanPlus::from_move(start.clone(), mv).to_string())
            .collect();
        assert!(sans.contains(&"Nf3".to_string()), "expected Nf3 in {sans:?}");
        assert!(sans.contains(&"e4".to_string()), "expected post-repetition e4 in {sans:?}");
        let refs = book.recent_games_for_filtered(key, Color::White, &filter, 10);
        assert!(refs.len() <= 1, "same game must not be duplicated in game_refs");
    }

    #[test]
    fn truncated_name_matches_attached_enumeration_digits_and_schnellschach_is_otb_rapid() {
        assert!(matches_truncated_or_enumerated_name("Kern, Johannes", "Kern, J1"));
        assert!(matches_truncated_or_enumerated_name("Kern, Johannes", "Kern, J2"));
        assert!(matches_truncated_or_enumerated_name("Hinterreiter, Martin", "Hinterreiter,M1"));

        let mut g = Game::default();
        g.set_header("Event", "Bezirks-Schnellschachmeisterschaft".to_string());
        g.set_header("Site", "?".to_string());
        let (tc, is_online, _) = classify_game_metadata(&g);
        assert_eq!(tc, TimeControlClass::Rapid);
        assert!(!is_online);
    }
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test --lib player_book_indexes_moves_after_single_position_repetition truncated_name_matches_attached_enumeration_digits_and_schnellschach_is_otb_rapid`
Expected: FAIL (`e4` missing after repetition; `"Kern, J1"` returns `false`; `Schnellschach` classified as `Classical`/`Online`).

- [ ] **Step 3: Implement edge-deduplicated indexing, `name_tokens` alphabetic splitting, and `Schnellschach` matching**

1. In `src/players.rs:607`, update `name_tokens(name: &str) -> Vec<String>` to split on `!c.is_alphabetic()` instead of `!c.is_alphanumeric()`, so `"J1"` splits into `"j"`.
2. In `src/players.rs:860` (`tc_from_event_keywords`), match `lower.contains("rapid") || lower.contains("schnellschach")` → `Some(TimeControlClass::Rapid)`.
3. In `src/players.rs:900` (`looks_online`), when `site_t == "?"`, only return `true` if `event.trim().is_empty() || event.trim() == "?"` (already guarded, verify `schnellschach` does not trigger online).
4. In `src/players.rs:1773` (`PlayerBook::add_game`), replace `let mut seen = HashSet::new();` with `let mut seen_positions = HashSet::new(); let mut seen_edges: HashSet<(u64, u32)> = HashSet::new();`:
   - Guard `self.depths.entry(key).or_insert(depth)` and `self.game_refs.entry(key)` with `if seen_positions.insert(key)`.
   - After decoding `code`, guard the observation recording block with `if seen_edges.insert((key, code))` and continue advancing `node = child; depth = depth.saturating_add(1);` (stopping if `depth >= 400`) instead of `break`ing!
5. Apply the same `seen_edges: HashSet<(u64, u32)>` (continue on duplicate edge, stop only at `max_plies`) in `src/position.rs:597` (`index_game_into`), `src/position.rs:1654` (`PgnVisitor`), and `src/recent.rs:746` (`RecentIndex::add_game`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib player_book_indexes_moves_after_single_position_repetition truncated_name_matches_attached_enumeration_digits_and_schnellschach_is_otb_rapid`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/players.rs src/position.rs src/recent.rs
git commit -m "fix(players): index post-repetition continuations, match J1 initials and Schnellschach"
```

---

### Task 2: Fast Per-Player Accelerator Lookup (`accelerated_games_for_player`)

**Files:**
- Modify: `src/library.rs:464-470`
- Modify: `src/twocbh/mod.rs:777-833`, `src/twocbh/mod.rs:980-986`
- Modify: `src/scid.rs:375-382`
- Modify: `src/app.rs:21163-21209`

**Interfaces:**
- Consumes: `players::player_key`, `players::matches_truncated_or_enumerated_name`
- Produces:
  - `GameLibrary::accelerated_games_for_player(&self, player_key: &str, canonical_name: &str) -> Option<Vec<usize>>`
  - `TwoCbhLibrary::accelerated_games_for_player(&self, player_key: &str, canonical_name: &str) -> Option<Vec<usize>>`

- [ ] **Step 1: Write the failing test in `src/twocbh/tests.rs` (or `src/app.rs`)**

Add a test in `src/app.rs` `mod tests` verifying `player_games_from_library` prefers `accelerated_games_for_player` without calling `accelerated_player_games`:
```rust
    #[test]
    fn player_games_from_library_uses_targeted_player_accelerator() {
        struct TargetedLib {
            game: Game,
        }
        impl GameLibrary for TargetedLib {
            fn path(&self) -> &Path { Path::new("targeted.2cbh") }
            fn file_len(&self) -> u64 { 1 }
            fn len(&self) -> usize { 1 }
            fn indexed_in(&self) -> Duration { Duration::ZERO }
            fn get(&self, _: usize) -> Option<&IndexedGame> { None }
            fn load(&self, index: usize) -> anyhow::Result<Game> {
                assert_eq!(index, 0);
                Ok(self.game.clone())
            }
            fn filter(&self, _: &str) -> Vec<usize> { vec![0] }
            fn accelerated_player_games(&self) -> Option<Vec<crate::library::AcceleratedPlayer>> {
                panic!("must not scan all players when targeted lookup is available");
            }
            fn accelerated_games_for_player(
                &self,
                player_key: &str,
                _canonical_name: &str,
            ) -> Option<Vec<usize>> {
                if player_key == "johannes kern" {
                    Some(vec![0])
                } else {
                    Some(Vec::new())
                }
            }
        }
        let mut game = Game::default();
        game.set_header("White", "Kern, J1".to_string());
        game.set_header("Black", "Other, Player".to_string());
        let lib: Arc<dyn GameLibrary> = Arc::new(TargetedLib { game });
        let loaded = player_games_from_library(&lib, "johannes kern", "Kern, Johannes");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].white(), "Kern, Johannes");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib player_games_from_library_uses_targeted_player_accelerator`
Expected: FAIL (`accelerated_games_for_player` is not a member of trait `GameLibrary`).

- [ ] **Step 3: Implement `accelerated_games_for_player` on `GameLibrary`, `TwoCbhLibrary`, `ScidLibrary`, and wire `player_games_from_library`**

1. In `src/library.rs:468`, add default method on `GameLibrary`:
```rust
    fn accelerated_games_for_player(
        &self,
        _player_key: &str,
        _canonical_name: &str,
    ) -> Option<Vec<usize>> {
        None
    }
```
2. In `src/twocbh/mod.rs`, add `pub fn accelerated_games_for_player(&self, player_key: &str, canonical_name: &str) -> Option<Vec<usize>>`:
   - Check `let acc = self.accelerator.as_ref()?;`.
   - First pass over `entity in 0..self.entity_bound() as u32`: read `let player = self.names.player(entity);`. Skip if `player.name == "?"`. Collect entity IDs where `crate::players::player_key(&player.name).as_deref() == Some(player_key)` (and if none match exactly across the bound, collect entities where `crate::players::matches_truncated_or_enumerated_name(canonical_name, &player.name)`).
   - Only for those matching `entity` IDs, call `acc.player_games(entity)` and map each `record_id` via `self.game_index_for_record(record_id)` without calling `self.file.record(record_id)`! Sort and dedup the resulting `Vec<usize>`, returning `Some(indices)`.
   - Wire `fn accelerated_games_for_player` in `impl GameLibrary for TwoCbhLibrary`.
3. In `src/app.rs:21163` (`player_games_from_library`), check `if let Some(indices) = library.accelerated_games_for_player(player_key, canonical_name)` first before falling back to `library.accelerated_player_games()` or `(0..library.len())`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib player_games_from_library_uses_targeted_player_accelerator`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/library.rs src/twocbh/mod.rs src/app.rs
git commit -m "perf(twocbh): filter .2lid names before decoding .2lgd player postings"
```

---

### Task 3: ChessABC `<Surname> <Initial>` Search & Uncapped Online Game Fetching

**Files:**
- Modify: `src/online.rs:70-185`, `src/online.rs:233-298`, `src/online.rs:526-548`, `src/online.rs:603-644`, `src/online.rs:865-920`

**Interfaces:**
- Produces:
  - `online::chessabc_search_queries(query: &str) -> Vec<String>` (includes `"<Surname> <FirstInitial>"`, e.g. `"Kern J"`)
  - Uncapped support when `max_games >= usize::MAX / 2` (or `max_games == 0` / `usize::MAX`) in `online::fetch_player_games`, `online::fetch_chessabc_games_by_fide_id`, `download_player_pgn`, and `download_chessabc_games` (no `5000` clamp when uncapped; Lichess URL omits `max=` when uncapped; Chess.com iterates all archives when uncapped; ChessABC pages all `totalPages` when uncapped).

- [ ] **Step 1: Write the failing unit tests in `src/online.rs`**

Add to `#[cfg(test)] mod tests` in `src/online.rs`:
```rust
    #[test]
    fn chessabc_search_queries_include_surname_and_first_initial() {
        let queries = chessabc_search_queries("Kern, Johannes");
        assert!(
            queries.iter().any(|q| q.eq_ignore_ascii_case("Kern J")),
            "expected 'Kern J' in {queries:?}"
        );
    }

    #[test]
    fn lichess_export_url_omits_max_when_uncapped() {
        assert_eq!(
            lichess_export_url("DrNykterstein", usize::MAX),
            "https://lichess.org/api/games/user/DrNykterstein?clocks=false&evals=false&opening=false&tags=true&sort=dateDesc"
        );
        assert_eq!(
            lichess_export_url("DrNykterstein", 300),
            "https://lichess.org/api/games/user/DrNykterstein?max=300&clocks=false&evals=false&opening=false&tags=true&sort=dateDesc"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib chessabc_search_queries_include_surname_and_first_initial lichess_export_url_omits_max_when_uncapped`
Expected: FAIL

- [ ] **Step 3: Implement `<Surname> <FirstInitial>` query generation and uncapped Lichess/Chess.com/ChessABC downloads**

1. In `src/online.rs` (`chessabc_search_queries`), extract the surname and first given-name initial from `query` (handling both `"Surname, First"` and `"First Surname"`), and `push_unique(&mut out, format!("{surname} {initial}"))` plus `push_unique(&mut out, deunicode::deunicode(&format!("{surname} {initial}")))`.
2. Extract helper `fn lichess_export_url(username: &str, max_games: usize) -> String`: when `max_games == 0 || max_games > 50_000`, omit `max={max_games}&` from the query string.
3. In `fetch_player_games`, `fetch_chessabc_player_games`, and `fetch_chessabc_games_by_fide_id`, treat `max_games == 0 || max_games > 50_000` as uncapped (`usize::MAX`) instead of `.clamp(1, 5000)`.
4. In `download_player_pgn` for `OnlineSource::ChessCom`, when `max_games > 50_000`, iterate all `urls.iter().rev()` without `.take(MAX_ARCHIVES)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib chessabc_search_queries_include_surname_and_first_initial lichess_export_url_omits_max_when_uncapped`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/online.rs
git commit -m "feat(online): add surname-initial ChessABC query and uncapped game downloads"
```

---

### Task 4: Composite Eval Bar (`chessdb.cn` + Lichess Cloud + Local DB Aggregate + Local Engine) & Hold Last Reliable Value

**Files:**
- Modify: `src/eval.rs:8-175`
- Modify: `src/ui/eval_bar.rs:16-65`
- Modify: `src/app.rs:8465-8562`, `src/app.rs:16685-16700`

**Interfaces:**
- Produces:
  - `EvalSource::{Local, Cloud, ChessDb, Pgn, DbAggregate}`
  - `BoardEval::from_chessdb(white_cp: i32) -> Self`
  - `BoardEval::from_db_aggregate(white_wins: u64, draws: u64, black_wins: u64) -> Option<Self>`
  - `eval::choose_composite(candidates: impl IntoIterator<Item = BoardEval>) -> Option<BoardEval>`
  - `App::current_board_eval_with_hold(&self) -> (Option<BoardEval>, bool)` (returns `(eval, is_held)`)
  - `eval_bar::draw_with_hold(ui: &mut Ui, eval: Option<BoardEval>, is_held: bool, side: f32) -> egui::Response`

- [ ] **Step 1: Write the failing unit tests in `src/eval.rs` and `src/app.rs`**

Add to `src/eval.rs` `mod tests`:
```rust
    #[test]
    fn composite_eval_orders_local_cloud_chessdb_pgn_and_db_aggregate() {
        let agg = BoardEval::from_db_aggregate(60, 20, 20).expect("enough games");
        assert_eq!(agg.source, EvalSource::DbAggregate);
        assert!(matches!(agg.score, WhiteScore::Cp(cp) if cp > 100));

        let cdb = BoardEval::from_chessdb(42);
        assert_eq!(cdb.source, EvalSource::ChessDb);
        assert_eq!(cdb.score, WhiteScore::Cp(42));

        assert_eq!(
            choose_composite([agg, cdb]),
            Some(cdb),
            "chessdb.cn should outrank raw DB winrate aggregate"
        );
    }
```
And in `src/app.rs` `mod tests`:
```rust
    #[test]
    fn eval_bar_holds_most_recent_reliable_eval_along_line_when_current_ply_unavailable() {
        let mut app = test_app();
        let pgn = b"[White \"A\"]\n[Black \"B\"]\n\n1. e4 { [%eval +0.35] } 1... e5 2. Nf3 *\n";
        let games = crate::domain::parse_pgn(pgn).unwrap();
        app.current = Some(games.into_iter().next().unwrap());
        app.goto(3); // after 2. Nf3, no eval at node 2 or 3, but node 1 has +0.35
        let (eval, held) = app.current_board_eval_with_hold();
        assert!(held, "expected held=true when inheriting ancestor eval");
        assert_eq!(eval.unwrap().score, crate::eval::WhiteScore::Cp(35));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib composite_eval_orders_local_cloud_chessdb_pgn_and_db_aggregate eval_bar_holds_most_recent_reliable_eval_along_line_when_current_ply_unavailable`
Expected: FAIL

- [ ] **Step 3: Implement `EvalSource::{ChessDb, DbAggregate}`, `choose_composite`, and `current_board_eval_with_hold`**

1. In `src/eval.rs`:
   - Add `ChessDb` and `DbAggregate` to `EvalSource` with `tie_rank`: `Local => 5, Cloud => 4, ChessDb => 3, Pgn => 2, DbAggregate => 1`.
   - Implement `BoardEval::from_chessdb(white_cp: i32) -> Self` (`depth: 0, source: EvalSource::ChessDb`).
   - Implement `BoardEval::from_db_aggregate(white: u64, draws: u64, black: u64) -> Option<Self>`: return `None` if `white + draws + black < 5`; compute `w = ((white as f64 + 0.5 * draws as f64) / total as f64).clamp(0.05, 0.95)`, `cp = ((-1.0 / 0.00368208) * ((1.0 / w) - 1.0).ln()).round() as i32`, returning `Some(Self { score: WhiteScore::Cp(cp), depth: 0, source: EvalSource::DbAggregate })`.
   - Update `BoardEval::tooltip` to format `EvalSource::ChessDb => "chessdb.cn"` and `EvalSource::DbAggregate => "DB aggregate"`.
   - Add `pub fn choose_composite(candidates: impl IntoIterator<Item = BoardEval>) -> Option<BoardEval>` (preserving `pub fn choose` for existing callers).
2. In `src/app.rs`:
   - Extract `fn eval_for_node(&self, node_idx: usize) -> Option<BoardEval>` that checks for that node's `pos`/`fen`:
     - `local`: `self.local_board_eval(&fen, turn)` (for `node_idx == self.node`) or `self.analysis_cache` lookup for ancestor nodes
     - `cloud`: Lichess `cloud_eval` from `self.cloud`, `self.cloud_cache`, or `self.bar_cloud_cache`
     - `chessdb`: top move (`moves.first()`) from `self.cloud` / `self.cloud_cache` `chessdb.as_ref().ok()`, converted via `BoardEval::from_chessdb(first.white_score(turn))`
     - `pgn`: `node.eval.and_then(BoardEval::from_pgn)`
     - `db_agg`: stats at `position::position_key(pos)` from `self.reference_tree_index().or(self.index.as_ref())` converted via `BoardEval::from_db_aggregate(u64::from(s.white), u64::from(s.draw), u64::from(s.black))`
     - Combine with `eval::choose_composite([local, cloud, chessdb, pgn, db_agg].into_iter().flatten())`.
   - Implement `fn current_board_eval_with_hold(&self) -> (Option<BoardEval>, bool)`:
     - If `let Some(direct) = self.eval_for_node(self.node)`, return `(Some(direct), false)`.
     - Otherwise walk `parent` pointers from `self.node` up `self.current`; on the first ancestor returning `Some(ancestor_eval)`, return `(Some(ancestor_eval), true)`.
     - If none found, return `(None, false)`.
3. In `src/ui/eval_bar.rs`, add `pub fn draw_with_hold(ui: &mut Ui, eval: Option<BoardEval>, is_held: bool, side: f32) -> egui::Response` (append `" · held from previous ply (computing…)"` to tooltip when `is_held`), and call it from `src/app.rs:16693`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib composite_eval_orders_local_cloud_chessdb_pgn_and_db_aggregate eval_bar_holds_most_recent_reliable_eval_along_line_when_current_ply_unavailable`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/eval.rs src/ui/eval_bar.rs src/app.rs
git commit -m "feat(eval): add chessdb.cn + DB aggregate to eval bar and hold last reliable score"
```

---

### Task 5: Incremental Multi-Source Prep Compilation, Non-Refetching Account Checks, ChessABC Confirmation, Live Source Editing & Opening Book Reference Tree

**Files:**
- Modify: `src/app.rs:425-490`, `src/app.rs:4926-5200`, `src/app.rs:6548-7790`, `src/app.rs:14090-14970`

**Interfaces:**
- Consumes: `online::fetch_player_games`, `online::fetch_chessabc_games_by_fide_id`, `player_games_from_library`, `Book::open`
- Produces:
  - `AccountCheckResult` caching `games: Arc<Vec<Game>>` so `refresh_prep_account_checks` only fetches newly added/unchecked keys and re-filters in memory when `AccountFilter` changes
  - `poll_chessabc_candidates` triggering `refresh_prep_account_checks()` and displaying `✓`/`⏳`/`✗` next to `"ChessABC games"`
  - Streaming `PrepLoadMessage` (`Progress` + `Done`) updating `self.player_book`, `self.prep_merged_games`, and `self.prep_load_steps` after each source completes
  - In-place `"Add / Edit sources…"` button in `prep_tree` that opens the setup dialog and updates the running `PrepSession` without losing `self.current` or `self.node`
  - Shared one-time Mega reference index (`library.len() > 500_000` uses `filter: None, max_plies: 36`) and Opening Book (`*.bin`, `*.ctg`, `*.abk`, `*.bkt`) support in `General Reference Tree` and `prior_for_position`

- [ ] **Step 1: Write the failing unit tests in `src/app.rs`**

Add to `src/app.rs` `mod tests`:
```rust
    #[test]
    fn refresh_prep_account_checks_preserves_already_verified_accounts() {
        let mut app = test_app();
        app.prep_setup_accounts.push(players::PrepOnlineAccount {
            source: OnlineSource::Lichess,
            username: "existing_user".to_string(),
            enabled: true,
            filter: players::AccountFilter::default(),
        });
        let key = prep_account_check_key(OnlineSource::Lichess, "existing_user");
        let mut g = Game::default();
        g.set_header("White", "existing_user".to_string());
        g.set_header("Black", "opp".to_string());
        app.prep_setup_account_checks.insert(
            key.clone(),
            AccountCheckResult {
                key: key.clone(),
                fetching: false,
                total: 1,
                kept: 1,
                error: None,
                games: Arc::new(vec![g]),
            },
        );
        app.refresh_prep_account_checks();
        let check = app.prep_setup_account_checks.get(&key).unwrap();
        assert!(!check.fetching, "already verified account must not be reset to fetching");
        assert_eq!(check.total, 1);
    }

    #[test]
    fn general_reference_tree_and_prior_accept_opening_book() {
        let mut app = test_app();
        let pos = Chess::default();
        let key = position::position_key(&pos);
        // Configure an in-memory/temp Polyglot book and verify prior_for_position uses it.
        let tmp = std::env::temp_dir().join("brett_test_ref_book.bin");
        // 16-byte polyglot entry for initial position e2e4 (polyglot key 0x463b96181691fc9c, move e2e4 = 0x031c, weight 10)
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x463b96181691fc9cu64.to_be_bytes());
        bytes.extend_from_slice(&0x031cu16.to_be_bytes());
        bytes.extend_from_slice(&10u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        std::fs::write(&tmp, &bytes).unwrap();
        app.set_reference_path_or_book(tmp.display().to_string());
        let (moves, source) = app.prior_for_position(key);
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(source, players::PriorSource::Reference);
        assert!(!moves.is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib refresh_prep_account_checks_preserves_already_verified_accounts general_reference_tree_and_prior_accept_opening_book`
Expected: FAIL

- [ ] **Step 3: Implement cached non-refetching account checks, ChessABC confirmation, streaming `PrepLoadMessage`, in-place source update, and book reference tree**

1. Add `games: Arc<Vec<Game>>` to `AccountCheckResult` (`src/app.rs:449`). In `refresh_prep_account_checks` (`src/app.rs:7060`):
   - If `self.prep_setup_account_checks.get(&request.key)` already exists with `!existing.fetching && existing.error.is_none()`, recompute `existing.kept` in memory against `request.filter` from `existing.games` and **do not** re-queue a network request for it!
   - When `request` is newly queued, fetch with uncapped `usize::MAX` (`PREP_ONLINE_MAX = usize::MAX`, `PREP_CHESSABC_MAX = usize::MAX`) and store the fetched `Arc<Vec<Game>>` in `AccountCheckResult`.
2. In `poll_chessabc_candidates` (`src/app.rs:6640`), call `self.refresh_prep_account_checks()` after updating `self.prep_setup_chessabc_selected`.
3. In `prep_setup_dialog` (`src/app.rs:7388`), change `"All ChessABC games for this player"` to `"ChessABC games"`, and render the aggregate ChessABC `account_check_text` (`✓ N games` / `⏳` / `✗`) right next to `"ChessABC games"` as well as per candidate.
4. In `start_smart_prep_load` (`src/app.rs:6720`):
   - Reuse any cached `Arc<Vec<Game>>` from `self.prep_setup_account_checks` so checked accounts/ChessABC profiles are not downloaded a second time!
   - Replace the single-shot channel with `PrepLoadMessage`:
     - Before each source starts, send `PrepLoadMessage::Status { step, total, current_label, completed_notes }`.
     - After each source finishes, rebuild the partial deduplicated `PlayerBook` and `Vec<Game>` from all sources collected so far and send `PrepLoadMessage::Progress { step, total, current_label, sources_label, book, games }`.
   - In `poll_prep_load`, apply `PrepLoadMessage::Progress` immediately to `self.player_book`, `self.prep_merged_games`, and `self.player_book_status` so the user sees live progress (`Compiling playerbase (2/5): Loading ChessABC… · Ready: Current DB 44 + Reference 253`) and can immediately prepare while remaining sources stream in.
5. In `prep_tree` (`src/app.rs:14093`), add an `"Add / Edit sources…"` button in the header and a live progress banner when `self.prep_load_rx.is_some()`. In `prep_setup_dialog`, when `self.prep.is_some()` for the same player, clicking `"Update preparation"` (or `"Start preparation"`) updates `prep.accounts`, `prep.chessabc_fide_ids`, and `prep.filter` and calls `start_smart_prep_load(index)` **without** resetting `self.current` or `self.node`.
6. Support Opening Books (`*.bin`, `*.ctg`, `*.abk`, `*.bkt`) in `set_reference_path_or_book` / `Choose reference database or book…` (`self.reference_book: Option<Book>`), converting book moves at `pos` into `position::MoveStat` in `prior_for_position` and rendering them in `📚 General Reference Tree`. For `ensure_reference_for_player`, when `library.len() > 500_000`, use `filter: None` and `max_plies: 36` so Mega uses a single shared opening cache path across all opponents.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib refresh_prep_account_checks_preserves_already_verified_accounts general_reference_tree_and_prior_accept_opening_book`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(prep): stream multi-source compilation, cache account checks, support book reference tree"
```

---

### Task 6: Font-Colored Game Rows, Merged Player List, Uncapped Recent Games with Continuation Moves, Hover + Arrow-Key Browsing & Click-at-Position Loading

**Files:**
- Modify: `src/app.rs:977-995`, `src/app.rs:9785-9806`, `src/app.rs:10700-11400`, `src/app.rs:14015-15075`, `src/app.rs:19695-19795`

**Interfaces:**
- Produces:
  - Font-colored `game_row` (`type_color` applied to primary text instead of `paint_game_type_dot`)
  - `players_content` rendering `self.prep_merged_games` when active prep matches `player.key`
  - Uncapped `"All"` toggle (`self.prep_recent_all: bool`) in Recent Games
  - `continuation_moves_snippet(game: &Game, key: u64, cursor_node: Option<usize>) -> String` shown on Recent Game rows
  - `HoveredGameBrowse` (`source_id`, `game: Game`, `anchor_node: usize`, `cursor_node: usize`) stepped via `Key::ArrowLeft` / `Key::ArrowRight` while hovering a game row
  - `open_game_preview_at(&mut self, game: Game, label: impl Into<String>, target_node: usize)` jumping to the current/browsed position instead of ply 0
  - Ancestor-repetition loop detection in `prep_tree` sorting non-repeating moves ahead of moves that repeat an ancestor in `self.current`

- [ ] **Step 1: Write the failing unit tests in `src/app.rs`**

Add to `src/app.rs` `mod tests`:
```rust
    #[test]
    fn open_game_preview_starts_at_current_preparation_position_not_ply_zero() {
        let mut app = test_app();
        let prep_pgn = b"[Event \"Preparation\"]\n[White \"You\"]\n[Black \"Kern, Johannes\"]\n\n1. e4 e5 2. Nf3 Nc6 *\n";
        app.current = Some(crate::domain::parse_pgn(prep_pgn).unwrap().remove(0));
        app.goto(3); // after 1. e4 e5 2. Nf3

        let recent_pgn = b"[White \"Player A\"]\n[Black \"Kern, Johannes\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 1-0\n";
        let recent_game = crate::domain::parse_pgn(recent_pgn).unwrap().remove(0);
        app.open_game_preview(recent_game, "Recent game 1");
        assert_eq!(
            app.node, 3,
            "clicking recent game must load at the position we were just at (ply 3), not ply 0"
        );
    }

    #[test]
    fn hovered_game_arrow_keys_step_hovered_game_and_update_preview() {
        let mut app = test_app();
        let prep_pgn = b"[Event \"Preparation\"]\n[White \"You\"]\n[Black \"Kern, Johannes\"]\n\n1. e4 e5 *\n";
        app.current = Some(crate::domain::parse_pgn(prep_pgn).unwrap().remove(0));
        app.goto(2); // after 1. e4 e5

        let hovered_pgn = b"[White \"X\"]\n[Black \"Kern, Johannes\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bc4 *\n";
        let hovered_game = crate::domain::parse_pgn(hovered_pgn).unwrap().remove(0);
        app.activate_hovered_game_browse("merged:0".to_string(), hovered_game);
        assert_eq!(app.hovered_game_browse.as_ref().unwrap().cursor_node, 2);

        app.step_hovered_game(1);
        assert_eq!(app.hovered_game_browse.as_ref().unwrap().cursor_node, 3);
        assert_eq!(app.node, 2, "preparation node must not move while stepping hovered game");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib open_game_preview_starts_at_current_preparation_position_not_ply_zero hovered_game_arrow_keys_step_hovered_game_and_update_preview`
Expected: FAIL

- [ ] **Step 3: Implement font-colored rows, merged player-profile games, uncapped recent games with continuation moves, hover-key browsing, click-at-position loading, and prep loop de-prioritization**

1. **Font color in `game_row` (`src/app.rs:19696`):**
   - Pass `type_color: Option<Color32>` into `game_row` (or apply via `name_format.color = type_color.unwrap_or_else(|| visuals.strong_text_color())`) and remove `paint_game_type_dot` calls in `game_list` and `prep_tree`.
2. **Player profile game list (`src/app.rs:11244`):**
   - In `players_content`, when `self.prep.as_ref().is_some_and(|p| p.player_key == player.key) && !self.prep_merged_games.is_empty()`, render rows from `self.prep_merged_games` (filtered by `prep_color` when applicable) instead of only `self.library` `game_indices`.
3. **Recent Games `"All"` toggle & continuation moves (`src/app.rs:14016-14945`):**
   - Add `prep_recent_all: bool` (default `true` so all games reaching the position are shown unless capped) with a `"Show all"` checkbox beside `Show last [N] games`.
   - Add `continuation: String` to `PrepRecentRow` computed by finding the node in `game` with `position_key == key` and formatting the next up to 6 mainline plies (e.g. `▶ 3. Bb5 a6 4. Ba4 Nf6`), appending it to the row subtitle.
4. **Hover + Arrow-Key browsing & Click-at-Position (`src/app.rs`):**
   - Define `struct HoveredGameBrowse { id: String, game: Game, anchor_node: usize, cursor_node: usize }` and `hovered_game_active_this_frame: bool` on `App`.
   - Implement `find_node_for_position_key(game: &Game, target_key: u64) -> Option<usize>`.
   - Implement `activate_hovered_game_browse(&mut self, id: String, game: Game)` and `step_hovered_game(&mut self, delta: isize)` which updates `cursor_node` along the mainline/parent and sets `self.hover_preview` to `HoverPreview` of `game.nodes[cursor_node]`.
   - In `handle_keys` (`src/app.rs:9789`), when `self.hovered_game_active_this_frame && self.hovered_game_browse.is_some()`, route `right` (`+1`), `left` (`-1`), `home`, and `end` to `step_hovered_game` instead of `self.next_move()` / `self.prev_move()`.
   - In `open_game_preview` (`src/app.rs:15025`), compute `target_node`: if `self.hovered_game_browse` matches the clicked game, use `cursor_node`; otherwise find the node in `game` matching `return_game`'s `return_node` position key (`find_node_for_position_key`), falling back to `0`. Call `self.goto(target_node)` instead of `self.goto(0)`.
5. **Preparation ancestor-loop de-prioritization (`src/app.rs:14572`):**
   - Collect `ancestor_keys: HashSet<u64>` along `self.node`'s parent chain in `self.current`. When sorting `smart_rows` / `player_moves`, check if playing `row.mv` from `pos` lands in `ancestor_keys`; sort non-repeating moves before ancestor-repeating moves and show `↺` on repeating moves.

- [ ] **Step 4: Run the tests and full test suite to verify everything passes**

Run: `cargo test --lib open_game_preview_starts_at_current_preparation_position_not_ply_zero hovered_game_arrow_keys_step_hovered_game_and_update_preview && cargo test`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(ui): font-colored game rows, hover arrow-key browsing, click-at-position, uncapped recent games"
```

