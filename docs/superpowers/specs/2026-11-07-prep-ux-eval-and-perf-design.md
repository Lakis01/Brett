# Preparation UX, Multi-Source Streaming, Recent Games Navigation, Eval Bar & Mega Performance — Design Spec

**Date:** 2026-11-07  
**Author:** Gemini 4 Argon (`plan` architect)  
**Target Executor:** `opencode/deepseek-v4.1-flash-free` via OpenCode

---

## 1. Intent & Why

Real-world opening preparation against `Kern, Johannes` with `Mega Database 2026.2cbh` (11.74M games) exposed severe performance bottlenecks and UX gaps:
1. **Mega Bottlenecks:** Starting preparation triggered both a 398-second all-core `.brettpos` rebuild over 5.2M games (`ensure_reference_for_player`) and a 256-second full-database player scan (`TwoCbhLibrary::accelerated_players` decoding all 600k players instead of filtering `.2lid` names first).
2. **Opaque Multi-Source Compilation:** While compiling games from Current DB, Reference DB, ChessABC, and Lichess/Chess.com, the UI gave no step-by-step progress and withheld merged games until the entire batch finished.
3. **1-Time Repetition Forever Loop:** `PlayerBook::add_game`, `PositionIndex`, and `RecentIndex` broke out of the game loop on the second visit to a position (`!seen.insert(key)`), dropping the post-repetition continuation move and trapping preparation in a cycle.
4. **Online & ChessABC Confirmations:** Adding an account refetched all existing accounts; ChessABC lacked a graphical `✓` confirmation, used a verbose label, and missed truncated/enumerated profiles (`Kern, J1`) due to alphanumeric tokenizing and full-name-only API queries. Online fetches were artificially capped at 300 games (and 100 for probes).
5. **Game List & Recent Games Ergonomics:** The Players-tab game list ignored merged prep games; game-type colors used a side dot instead of font color; Recent Games lacked an uncapped ("All") option, didn't show continuation moves inline, couldn't be browsed on hover with arrow keys, and opened at ply 0 instead of the current preparation position.
6. **Evaluation Bar Continuity & Sources:** The eval bar only consulted Local Engine, Lichess Cloud Eval, and PGN `[%eval]`, ignoring `chessdb.cn` and local/reference DB aggregate win rates, and snapped jarring to `0.00` while awaiting an evaluation.

---

## 2. Goals, Non-Goals & Constraints

### Goals
- **Instant Mega Player Extraction (<50ms):** Filter `.2lid` / player name tables before decoding `.2lgd` / accelerator postings via `GameLibrary::accelerated_games_for_player`.
- **One-Time Reference Indexing & Book Support:** Never rebuild per-opponent rating-window `.brettpos` indexes on multi-million-game databases; use a single shared opening index (`max_plies: 36`, `filter: None`) or a native Opening Book (`.bin`, `.ctg`, `.abk`, `.bkt`) for the General Reference Tree and OTB prior.
- **Multi-Layered Progress & Live Streaming:** Stream `PrepLoadMessage` updates after each source (Current DB → Reference → ChessABC → Online Accounts → Extra File) with a visible step-by-step progress banner and live incremental book/list updates. Allow adding/editing sources inside active preparation without closing it.
- **Repetition Loop Elimination:** Track `(position_key, move_code)` edges in `PlayerBook`, `PositionIndex`, and `RecentIndex` so post-repetition game continuations are indexed; flag moves that loop back to an ancestor in the current preparation line.
- **Uncapped Online & Recent Games + Smart Account Checks:** Default online account fetches to uncapped (`None`), cache probed games so Start doesn't re-download, only check newly added/changed accounts, rename to `"ChessABC games"`, show `✓`/`✗` confirmation for ChessABC, and match truncated/enumerated names (`Kern, J1`) in both local tokenization and ChessABC API queries.
- **Schnellschach Classification:** Recognize `(?i)schnellschach` as `TimeControlClass::Rapid` and OTB.
- **Font-Colored Game Rows & Hover-Key Browsing:** Color game rows by font color (`Classical OTB` emerald, `Rapid/Blitz OTB` sky, `Online` violet) instead of a side dot; update both left Games and Player profile lists with merged prep games; show continuation moves in Recent Games; allow hovering any game row + pressing `←`/`→` to step through its moves on the board preview; open clicked recent/prep games at the current/browsed position instead of ply 0.
- **Composite Eval Bar with Hold-Last-Reliable Value:** Aggregate Local Engine → Lichess Cloud → `chessdb.cn` → PGN `[%eval]` → DB aggregate win-% logistic cp, and hold the most recent reliable eval from the current game line while a new ply's eval is pending.

### Non-Goals
- Modifying native `.2cbh` or `.si4` binary file formats on disk.
- Requiring network access for offline tests.

---

## 3. Architecture & Component Design

### 3.1 Fast Per-Player Accelerator Lookup & One-Time Reference / Book Tree
- **`GameLibrary::accelerated_games_for_player(&self, player_key: &str, canonical_name: &str) -> Option<Vec<usize>>`** (`src/library.rs`, `src/twocbh/mod.rs`, `src/scid.rs`, `src/cbh/mod.rs`):
  - In `TwoCbhLibrary`, iterate `entity in 0..self.entity_bound() as u32`, inspect `self.names.player(entity).name` first, and only call `acc.player_games(entity)` when `players::player_key(&name).as_deref() == Some(player_key)` or `players::matches_truncated_or_enumerated_name(canonical_name, &name)`.
  - `player_games_from_library` (`src/app.rs`) calls `library.accelerated_games_for_player(player_key, canonical_name)` before falling back to `accelerated_player_games()` or header scanning.
- **One-Time Reference Cache & Book Reference Tree** (`src/app.rs`):
  - For databases with `library.len() > 500_000` (e.g. Mega), `ensure_reference_for_player` uses a single player-independent cache path `position::reference_cache_path(&path, None)` with `max_plies: 36` and `rating_filter: None`, so an existing `.brettpos` is reused across all opponents without triggering a 400s rebuild per player.
  - If the configured reference path or General Reference Tree picker selects an opening book (`*.bin`, `*.ctg`, `*.abk`, `*.bkt`), open it via `Book::open` into `self.reference_book: Option<Book>` and serve its moves in both `prior_for_position` and the `📚 General Reference Tree` UI.

### 3.2 Repetition Continuation Indexing & Loop Prevention
- In `PlayerBook::add_game` (`src/players.rs:1780`), `PositionIndex` (`src/position.rs:597, 1654`), and `RecentIndex` (`src/recent.rs:746`):
  - Replace `if !seen.insert(key) { break; }` with:
    - `seen_positions: HashSet<u64>` to guard per-game position-level entries (`depths.entry(key).or_insert(depth)` and `game_refs.entry(key)` pushed at most once per game).
    - `seen_edges: HashSet<(u64, u32)>` (with safety cap `depth < 400`): if `!seen_edges.insert((key, code))`, skip recording the duplicate edge observation, **but continue walking `node = child`** so the continuation move played on the second visit to `key` (and the rest of the game) is fully indexed!
- In `src/app.rs` (`prep_tree`), compute `ancestor_keys: HashSet<u64>` along `self.node`'s parent chain in `self.current`. For each candidate move row from `pos`, check if `pos.clone().play(mv)` lands on an `after_key` in `ancestor_keys`. If non-looping moves exist at `pos`, sort non-looping moves ahead of looping moves and append a `↺` indicator tooltip `"Repeats an earlier position in this line"`.

### 3.3 ChessABC Name Matching, Uncapped Online Fetches & Incremental Compilation
- **Tokenization & Search (`src/players.rs`, `src/online.rs`):**
  - Update `name_tokens(name: &str)` in `src/players.rs` to split on `!c.is_alphabetic()` (stripping digits attached to initials like `"J1"` → `"j"`), so `matches_truncated_or_enumerated_name("Kern, Johannes", "Kern, J1")` returns `true`.
  - In `chessabc_search_queries(query: &str)` (`src/online.rs`), append a `"<Surname> <FirstInitial>"` query (e.g. `"Kern J"` from `"Kern, Johannes"`) so ChessABC's substring search returns truncated/enumerated profiles (`Kern, J`, `Kern, J1`).
- **Schnellschach (`src/players.rs`):**
  - In `tc_from_event_keywords`, match `lower.contains("rapid") || lower.contains("schnellschach")` → `Some(TimeControlClass::Rapid)`.
  - In `looks_online`, never treat an event containing `"schnellschach"` as online when `Site` is `"?"` unless an explicit online platform keyword (`lichess`, `chess.com`, `tornelo`, `online`, `internet`) is present.
- **Uncapped Online Fetch & Cached Account Checks (`src/online.rs`, `src/app.rs`):**
  - Change `PREP_ONLINE_MAX` and `PREP_CHESSABC_MAX` to `usize::MAX` (uncapped by default). In `online::download_player_pgn`:
    - For Lichess: omit `&max=...` when `max_games >= 5000` (or `usize::MAX`) so Lichess streams all games for the account.
    - For Chess.com: iterate all archive URLs when `max_games >= 5000` (removing the `take(MAX_ARCHIVES)` cap when uncapped).
    - For ChessABC: page through all `totalPages` when uncapped.
  - Store `games: Arc<Vec<Game>>` inside `AccountCheckResult`. In `refresh_prep_account_checks`, skip any request whose `key` is already in `prep_setup_account_checks` (and not failed), only re-evaluating `kept` locally when `account.filter` changes!
  - In `poll_chessabc_candidates`, call `self.refresh_prep_account_checks()` when candidates resolve, and display the `✓ <N> games` / `⏳` / `✗` check status next to `"ChessABC games"`.
- **Streaming `PrepLoadMessage` & Live Source Editing (`src/app.rs`):**
  - `start_smart_prep_load` sends `PrepLoadMessage::Progress { step, total, active_source, completed_notes, partial_book, partial_games }` after each source finishes, updating `self.player_book`, `self.prep_merged_games`, and `self.player_book_status` live.
  - Add an `"Add / Edit sources…"` button in `prep_tree` that opens `prep_setup_open = true` while keeping `self.prep` active; clicking `"Update preparation"` (when `self.prep.is_some()`) updates sources/accounts and re-runs `start_smart_prep_load` without resetting `self.current` or `self.node`.

### 3.4 Game Lists, Font Color Coding, Move Previews & Hover-Key Browsing
- **Font Color Coding (`src/app.rs`):**
  - Add `text_color: Option<Color32>` to `game_row` (or `game_row_colored`), coloring the primary player names with `type_color` (`Classical OTB` emerald, `Rapid/Blitz OTB` sky, `Online` violet) and removing `paint_game_type_dot`.
- **Merged Games in Left & Player Profile Lists (`src/app.rs`):**
  - In `players_content`, when `self.prep` matches `player.key` and `!self.prep_merged_games.is_empty()`, render the player's games from `self.prep_merged_games` (matching the left `game_list`).
- **Recent Games Uncapped + Continuation Moves + Hover Arrow-Key Browsing + Click at Position (`src/app.rs`):**
  - Add an `"All"` checkbox/toggle (`prep_recent_all: bool`) next to `Show last [N] games`.
  - In `PrepRecentRow`, compute `continuation: String` (up to 6 plies from `key` in `game`, e.g. `12...Nf6 13. Nc3 Be7`) and render it on the row.
  - Add `hovered_game_browse: Option<HoveredGameBrowse>` (`{ source_key, game, anchor_node, cursor_node }`). When hovering any row in Recent Games or the Left Game List:
    - Resolve the game's node matching the current preparation position `key` (`find_node_for_position_key(&game, key).unwrap_or(0)`).
    - Set `self.hover_preview` to `cursor_node`'s position.
    - While a row is hovered, `Key::ArrowLeft` and `Key::ArrowRight` in `handle_keys` step `cursor_node` backward/forward in the hovered game (updating `hover_preview`) instead of moving `self.node` in the preparation tree!
  - When clicking a Recent Game or Left Prep Game row, call `open_game_preview_at(game, label, target_node)` where `target_node` is `hovered_game_browse.cursor_node` (if matching) or `find_node_for_position_key(&game, current_key).unwrap_or(0)`.

### 3.5 Evaluation Bar Sources & Hold-Last-Reliable Eval
- **`EvalSource` (`src/eval.rs`):**
  - Extend `EvalSource` with `ChessDb` and `DbAggregate`:
    - Authority rank: `Local (5) > Cloud (4) > ChessDb (3) > Pgn (2) > DbAggregate (1)`.
    - Add `BoardEval::from_chessdb(white_cp: i32) -> Self` and `BoardEval::from_db_aggregate(white_wins: u64, draws: u64, black_wins: u64) -> Option<Self>` (requires `total >= 5`; converts `w = ((white + 0.5 * draw) / total).clamp(0.05, 0.95)` via inverse logistic `cp = (-1.0 / 0.00368208 * ((1.0 / w) - 1.0).ln()).round() as i32`).
- **Hold Most Recent Reliable Value (`src/app.rs`, `src/ui/eval_bar.rs`):**
  - If `eval_for_node(self.node)` is `None`, walk `parent` pointers in `self.current` from `self.node` to find the nearest ancestor with `Some(BoardEval)`. Pass `(eval, is_held)` to `eval_bar::draw` so the bar stays at that ancestor's evaluation (with `(held from previous move — computing…)` in the tooltip) instead of jumping to `0.00`.

