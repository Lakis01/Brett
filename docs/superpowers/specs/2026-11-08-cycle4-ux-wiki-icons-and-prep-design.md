# Cycle 4 Design Spec: Game & Player List Polish, Venue Flags, Wikidata Enrichment, URL-Paste & Incremental Online Cache, and App Icon

**Date:** 2026-11-08  
**Project:** `/home/gratis01/Programs/Brett`  
**Planner:** Gemini 4 Argon High (`antigravity-ide`)  
**Implementer:** `opencode` (`crouton` / Muse Spark 1.4)

---

## 1. Goals & Problem Statement

This cycle addresses 12 concrete UX, data-enrichment, and performance items reported after testing `v0.1.7`:

1. **Application Icon (`assets/brett.webp`, `src/main.rs`):**
   - Crop `/home/gratis01/Pictures/Brett Logo.jpg` to its rounded-corner charcoal square with transparent outer corners (already generated at `assets/brett.webp` and `assets/brett.png`, plus `~/.local/share/icons/hicolor/256x256/apps/brett.png` and `~/.local/share/applications/brett.desktop`).
   - Embed `assets/brett.webp` in `src/main.rs` (`include_bytes!("../assets/brett.webp")`), decode via `image::load_from_memory`, and pass `egui::IconData` to `ViewportBuilder::with_icon(...)` along with `.with_app_id("brett")` so both X11 and KDE Plasma Wayland taskbars display the Brett icon.

2. **Welcome / Default Reference Dialog Button Labels (`src/app.rs`):**
   - In `reference_setup_dialog` (`src/app.rs:4569-4579`), rename:
     - `"Save & Close"` → **`"Set"`**
     - `"Not now"` → **`"Not right now"`**
     - `"No & don't ask again"` → **`"Never"`**

3. **Remove Redundant Preview Notation (`src/app.rs`):**
   - In `prep_unified_recent_rows` (`src/app.rs:15588-15614`), stop appending ` · ▶ {continuation}` to `subtitle`.
   - In `prep_tree` (`src/app.rs:16659-16729`), remove the bottom `Preview: 1. e4 e5 ...` text box below Recent Games (since hovering already previews on the main board and overlays moves in the right-hand `Moves` panel).

4. **Online Account Probe Clarity (`200+ games`) & Default Bullet Inclusion (`src/app.rs`, `src/players.rs`):**
   - `TimeControlMask::default()` (`src/players.rs:1164-1173`) previously had `bullet: false`, which silently filtered out Bullet/UltraBullet games on Lichess/Chess.com accounts even when `AllTime` was selected—causing the user to see `200 games (87 match filters)` when they thought all games were included. Change `TimeControlMask::default()` to set `bullet: true`.
   - In `account_check_text` and `account_check_badge` (`src/app.rs:558-583`), when `check.probe_capped` is `true` (`total >= App::PREP_ACCOUNT_PROBE_MAX`), display `"✓ Found · 200+ games ({}+ match filters · all fetched on Start)"` (and `"✓ 200+ games"` in `account_check_badge`) so the user never thinks exports are truncated at 200 games.

5. **Incremental Disk Cache for Online Account Games (`src/online.rs`, `src/app.rs`):**
   - Downloading 17,733 Lichess games across two accounts took `746.11s` because Lichess throttles unauthenticated PGN streams to ~24 games/sec and Brett previously re-downloaded from scratch on every session.
   - Persist downloaded raw games per `(source, username)` in `~/.cache/brett/online/<service>_<username>.pgn`.
   - For Lichess, when a cached PGN file exists and `max_games > 500`, determine the latest game's date (`YYYY.MM.DD`), fetch only new games since `latest_date - 1 day` (`since=<unix_ms>`), prepend/deduplicate with the cached games, and write the updated cache back to disk. Repeat preparations against a 10,000-game Lichess account complete in **< 1 second**.

6. **Preparation Setup UX: Hide Empty `Confirm`, Support URL Pasting, Fuzzy/Underscore Player Search, and No-DB ChessABC Search (`src/online.rs`, `src/players.rs`, `src/app.rs`):**
   - **Hide `Confirm` when 0 ChessABC candidates are found:** In `prep_setup_dialog` (`src/app.rs:8252`), only render the `"Confirm"` button when `!self.prep_setup_chessabc_candidates.is_empty()`.
   - **Paste Full URLs (`src/online.rs::parse_account_or_profile_input`):**
     - Support pasting `https://lichess.org/@/Makefrench` (or `https://lichess.org/@/Makefrench/all`) → `(Some(OnlineSource::Lichess), "Makefrench")`.
     - Support pasting `https://www.chess.com/member/hikaru` (or `https://chess.com/players/hikaru`) → `(Some(OnlineSource::ChessCom), "hikaru")`.
     - Support pasting `https://www.chessabc.com/player/4604776` or `https://ratings.fide.com/profile/4604776` → `(Some(OnlineSource::ChessAbc), "4604776")`.
     - In `prep_setup_dialog`, when adding an online account or pasting a URL into the account/search field, auto-detect the service and extract the clean username or FIDE ID! If a ChessABC/FIDE URL or raw numeric FIDE ID is pasted into the opponent search or ChessABC section, add/select that FIDE ID directly and queue its check.
   - **Fuzzy & Punctuation-Tolerant Player Search (`src/players.rs::PlayerDirectory::filter`):**
     - Normalize `_`, `-`, `,`, `.`, and diacritics (`deunicode`) into spaces/tokens so typing `"Makefrench"` matches `"Make_french"` or `"Make-french"`, and rank exact/substring matches first followed by bounded edit-distance (`levenshtein <= 1` for tokens of length 4..=6, `<= 2` for length >= 7) so minor typos still surface the most probable opponent while typing.
   - **ChessABC Search When No Local DB Is Open (`src/app.rs`):**
     - In `prep_setup_dialog` when `show_search` is active, provide a `"Search ChessABC"` button (and trigger on `Enter` in the opponent search box) that calls `refresh_chessabc_candidates(typed, None)` and expands the ChessABC candidate picker directly below the search input so users can search and pick any FIDE/ChessABC player even with no database open or when the player is absent from the open database.

7. **Remove FIDE IDs from All Game & Player Lists; Prominent FIDE Badge in Player Profile Only (`src/app.rs`):**
   - Never display FIDE IDs in `game_row` (`games_content`, `players_content`, `prep_tree` Recent Games, `online_reference`) or `player_row` (`player_list`).
   - Display FIDE ID only in the **Player Profile Card** (`players_content`, `src/app.rs:12148-12156`), rendered as a prominent clickable badge pill with a custom vector FIDE/globe-knight emblem (`paint_fide_badge_button`), styled with a subtle gold/slate border and `"FIDE <id> ↗"`, opening `https://ratings.fide.com/profile/<id>`.

8. **Restructure Player's Game List Rows (`src/app.rs`):**
   - In `players_content` (`src/app.rs:12489-12505` and `12656-12678`) and `online_reference` (`src/app.rs:11653-11663`), change row formatting from:
     - `prefix = "vs "`, `white = Opponent`, `suffix = " · as White"`
     - to:
     - `prefix = &format!("as {color} against ")`, `white = Opponent`, `suffix = ""`
     - producing `"as White against GM Magnus Carlsen · 🇳🇴 NOR"`.

9. **Bare Elo Number & Country Flags in Database `Players` List (`src/app.rs`):**
   - In `player_row` (`src/app.rs:21308-21318`), format the rating as `  {elo}` (e.g. `2830`) instead of `  Elo {elo}`.
   - Ensure the player's country flag badge (`🇳🇴 NOR`) is always rendered on the right of the row (including when populated via Wikipedia/Wikidata enrichment).

10. **Country Flag from Conclusive `Site` Tag (`src/country.rs`, `src/app.rs`):**
    - Add `country::from_site_tag(site: Option<&str>) -> Option<Country>` in `src/country.rs`:
      - Rejects online sites (`lichess`, `chess.com`, `chessabc`, `int`, `internet`, `online`, `tornelo`).
      - Checks if the whole trimmed `Site` string or its trailing whitespace/comma-separated token (e.g. `"Wijk aan Zee NED"`, `"Toronto CAN"`, `"Portoroz SLO"`, `"Saint Louis, USA"`, `"Stavanger, Norway"`) resolves via `country::normalize` (excluding ambiguous 2-letter US state abbreviations unless the whole tag is a 3-letter FIDE code or full country name, or the trailing token is a 3-letter FIDE/ISO3 code or full country name).
    - When `country::from_site_tag(site)` returns `Some(venue_country)`, prefix the event in game subtitles (`games_content`, `players_content`, `prep_tree` Recent Games) with `"{} {}", venue_country.flag, event` (e.g. `"🇳🇱 86th Tata Steel Masters"`).

11. **Concise `Prepare` Control in Player Profile Card (`src/app.rs`):**
    - In `players_content` (`src/app.rs:12260-12296`), replace the full-width bottom bar with a compact, inline pill (`Prepare` label + `18x18` White square + `18x18` Black square right next to each other with `6px` spacing) placed directly on the profile header row (right-aligned) so it takes zero extra vertical space and keeps the white/black buttons immediately adjacent to `"Prepare"`.

12. **Wikipedia & Wikidata Player Enrichment (`src/country.rs`, `src/wiki.rs`, `src/players.rs`, `src/app.rs`):**
    - Extend `wiki::Summary` with:
      - `pub countries: Vec<String>` (canonical 3-letter FIDE codes ordered oldest → latest, so `countries.last()` is the **latest/current** federation)
      - `pub fide_id: Option<String>`
      - `pub birth_year: Option<i32>`
      - `pub peak_elo: Option<u16>`
    - Populate these fields from:
      1. Lead-sentence / description nationality demonyms in `Summary::from_value` (e.g. `"Iranian-born French chess grandmaster"` → `["IRI", "FRA"]`, `"Soviet and Russian chess grandmaster"` → `["URS", "RUS"]`, `"Norwegian chess grandmaster"` → `["NOR"]`).
      2. Wikidata entity claims (`https://www.wikidata.org/wiki/Special:EntityData/{wikibase_item}.json`) fetched in `lookup_with` when `wikibase_item` (`"Q..."`) is present on the Wikipedia summary response:
         - `P1440` → `fide_id`
         - `P1532` (country for sport) / `P27` (country of citizenship) mapped via `country::from_wikidata_qid` → `countries` (sorted so claims without an end date `P582` or with `preferred` rank come last as the latest federation)
         - `P569` → `birth_year`
         - `P1087` → `peak_elo`
    - When `App::apply_wiki_result` receives `Lookup::Summary(summary)`:
      - Update the matching `PlayerSummary` in `self.players`:
        - Store `player.wiki_countries` (all resolved `Country` items).
        - Set `player.country = summary.latest_country().or(player.country)` so the Database `Players` list (`player_list`) shows the **latest** country flag (`🇫🇷 FRA`), while the **Player Profile Card** displays **all** countries (`🇫🇷 FRA · 🇮🇷 IRI`).
        - If `player.fide_id.is_none()`, set `player.fide_id = summary.fide_id.clone()` so the clickable FIDE badge appears in the Player Profile Card!
        - Also show `Born <birth_year>` and `(peak <peak_elo>)` in the Player Profile Card when available.
    - Add a quick `"Sample DB"` button in the top bar / `File` menu (`databases/samples/grandmaster_showcase.pgn`) so the user can load the rich sample database in one click.

---

## 2. Amendments (these override §1 where they conflict)

### 2.1 Flags are currently NOT rendered as flags — use a bundled flag atlas
`egui` 0.37 (with `monochrome_emoji_fonts`) does no glyph shaping, so a regional-indicator pair such as `🇳🇴` is drawn as two separate boxed letters, not as a flag. Every `country.badge` / `country.flag` string in the UI is therefore effectively unreadable. This explains the user's question "are flags included here?".

- **Asset already generated by the planner:** `assets/flags_4x3.webp` (80 KB, lossless, 195 flags, 48×36 px each, 16 columns, row-major), `assets/flags_4x3.txt` (one FIDE code per line in atlas order; `#` lines are comments), `assets/flags_LICENSE.txt` (flag-icons 7.6.0, MIT).
- Historical federations without a modern flag (`URS`, `YUG`, `GDR`, `TCH`, `SCG`) and `FID` are not in the atlas. Render them as a small muted text pill (`URS`) of identical height.
- New module `src/flags.rs` loads the atlas once (`include_bytes!` + `image::load_from_memory` → `ctx.load_texture("brett-flags", …, TextureOptions::LINEAR)`) and exposes UV lookup plus two painters: one for `Painter`-drawn rows and one returning an `egui::Response` for widget layouts.
- Display size: 16×12 logical px in list rows, 20×15 in the profile card, with a 1 px `#00000040` hairline so white flags (JPN, etc.) stay visible on light surfaces.
- Remove the `🇳🇴 NOR` emoji badge text wherever a flag image is now drawn. Keep the 3-letter code only as hover text (`Norway (NOR)`).

### 2.2 Game rows
- `RowName.country` becomes `Option<Country>` (not a `&str` badge); `RowName.fide_id` and `with_fide_id` are deleted, and `append_player_name` loses its `fide_id` parameter. FIDE ids appear nowhere in any list.
- Because a `LayoutJob` cannot embed images, `game_row` lays out the line as consecutive segments: `[prefix galley][title+name galley][flag][" – " galley][title+name galley][flag]`, advancing `x` after each one and stopping (with `…`) once the available width runs out.
- Player-centric rows (Player view games list and the online-reference list) read **`as White against GM Carlsen, Magnus [🇳🇴]`**: prefix `as {color} against `, no suffix. `color` is the prepared/viewed player's colour in that game.
- Subtitle = `[venue flag] {event} · {date} · {result}`. Draw the venue flag whenever `country::from_site_tag(site)` is conclusive. Online sites never yield a flag.

### 2.3 Account probes and the "200 games" message
- The setup dialog probes at most `App::PREP_ACCOUNT_PROBE_MAX = 200` games for every account and ChessABC profile. That probe only proves the account exists. Pressing **Start** downloads uncapped (the user's log shows `Lichess 10000/10000 + Lichess 7733/7733`).
- New wording: `✓ Found · 200+ games · full history loads on Start` when the probe is capped; `✓ Found · 143 games · 120 pass filters (23 bullet)` when not capped. The parenthetical comes from `AccountFilter::exclusion_breakdown`, which shows exactly which filter removed games.
- `TimeControlMask::default().bullet = true`. **Migration:** accounts saved before this change hold `"bullet": false` without a `"filter_version"` key. On load, set `bullet = true` once and write `"filter_version": 2`.
- If one Lichess export ends at exactly 10 000 games, keep paging backwards with `until=<oldest game ms>` until a page returns no new games, then dedupe by game id. Whether Lichess caps exports server-side is not verified, and the extra request is cheap.

### 2.4 Incremental online cache (this replaces the §1 item 5 details)
- Layout: `$XDG_CACHE_HOME/brett/online/lichess/<lowercase user>.pgn` stores the **unfiltered** full history, newest first. Chess.com caches immutable past-month archives at `…/chesscom/<user>/<YYYY-MM>.pgn`; the current and previous months are always refetched.
- Lichess delta: read the newest cached game's `UTCDate`/`UTCTime`, request `since=<ms + 1000>` **without** `perfType`/`rated` parameters, prepend the new games, and dedupe by the id in `[Site "https://lichess.org/<id>"]` (or a `GameId` tag). The `AccountFilter` is always applied locally afterwards.
- The probe reads the cache first. Its status reads `✓ 10 000 games cached · checking for new…`, then `✓ 10 012 games (12 new)`.
- Settings → Online gets a `Clear online game cache (NN MB)` button.
- ChessABC stays uncached this cycle because it is already fast.

### 2.5 Player Profile Card
- Header row, right-aligned: a compact segmented pill `Prepare ⬜ ⬛`. The squares are 16×16. **Square colour = the colour YOU play**, which matches the setup dialog's "You play". Tooltips: `You play White — prepare against {name}'s Black games` and the mirror. The current label button (which silently opened a White prep) is removed.
- The `FIDE Rating` line becomes `{elo}` in monospace strong plus `peak {n}` muted. No "Elo"/"FIDE Rating" words.
- FIDE badge: a pill button with a gold hairline border. It holds a painted globe glyph (circle + 2 meridians + equator, 12 px, gold) followed by `FIDE · 1503014 ↗` and opens `fide::profile_url(id)`. No FIDE logo raster (trademark); the word FIDE used descriptively is fine.
- Federation line: every known country as `[flag] Norway`, separated by ` · `, current federation first.
- `Born 1990` muted when Wikidata supplies it.

### 2.6 Fuzzy search: performance guard
`PlayerDirectory::filter` runs every frame and is O(n) over 476 k Mega players. Add a one-entry memo `(query, Vec<usize>)` behind `RefCell`/`Mutex`. Fuzzy (edit-distance) matching runs only when exact/substring matches number < 8 and the normalized query has ≥ 4 chars. It must also stay under a 25 ms budget, measured in a test on a synthetic 500 k directory in release mode, or just bounded by early-exit. On directories > 150 k players, run fuzzy only on Enter.

### 2.7 Stretch: PlayerDirectory sidecar for large databases
`PlayerDirectory built: 476038 players in 49.98s` on every Mega open violates the "build accelerators once" principle. Persist the built directory to `<db>.brettplayers`, keyed by the DB file size + mtime + format version, and load it in < 2 s. Do this only after Tasks 1–6 are green.

