# Design System — Brett

## 1. Visual Direction & Mode (`Operate`)

Brett is a precision analytical instrument (`Operate` mode). The interface recedes so the chessboard, move tree, and statistical distributions lead.

- **Theme Polarity:** Intentional dark carbon-slate workspace (`#111317` canvas ground, `#171A20` elevated panel surface, `#1E222A` interactive/card surface) chosen for long pre-round preparation sessions, paired with a warm natural wood/stone chessboard (`#DFCFB8` light squares, `#9E7B56` dark squares) that anchors the center viewport.
- **No Card-in-Card Nesting:** Panels are separated by `1px` subtle structural borders (`#262B35`). Tables and lists sit flush inside their section without double-boxed frames.
- **Equal Content Edges & Stable Splitters:** Every panel enforces horizontal text truncation or wrapping so long status strings never force a panel wider than its splitter allocation.

## 2. Color Tokens

| Token | Hex | Role |
|---|---|---|
| `BG_CANVAS` | `#111317` | Root window background & central board stage |
| `SURFACE_PANEL` | `#171A20` | Left database sidebar, right workspace panel, top bar |
| `SURFACE_ELEVATED` | `#1E222A` | Player profile header card, active table header, hover row fill |
| `SURFACE_SUBTLE` | `#14171C` | Striped table row alternate fill |
| `BORDER_SUBTLE` | `#262B35` | `1px` panel dividers and card hairlines |
| `TEXT_PRIMARY` | `#ECEFF4` | Primary labels, moves, player names |
| `TEXT_SECONDARY` | `#9AA3B2` | Subtitles, events, dates, secondary counts |
| `TEXT_MUTED` | `#656E7E` | Disabled controls, empty-state hints |
| `ACCENT_PRIMARY` | `#4C9AFF` | Active tab indicator, primary selection |
| `GOLD_TITLE` | `#E6B450` | FIDE title pill (`GM`, `IM`, `FM`, etc.) |
| `WDL_WHITE` | `#E8ECF2` | White wins segment in horizontal W/D/B bars |
| `WDL_DRAW` | `#6C7380` | Draws segment in horizontal W/D/B bars |
| `WDL_BLACK` | `#20242C` | Black wins segment in horizontal W/D/B bars (with `#383E4C` 1px stroke) |

### Configurable Game-Category Font Palette (`GameCategoryColors`)

Game rows never print text category labels (`Classical OTB`, `Online`, etc.); the player names' font color encodes the category and is fully customizable in **Settings → Appearance**:

| Category | Default Hex | Visual Character |
|---|---|---|
| `otb_classical` | `#7ED38A` | Crisp emerald green |
| `otb_rapid` | `#56B6C2` | Teal cyan |
| `otb_blitz` | `#61AFEF` | Sky blue |
| `online_classical` | `#B48EAD` | Muted plum |
| `online_rapid` | `#C678DD` | Orchid violet |
| `online_blitz` | `#E5C07B` | Warm amber |
| `online_bullet` | `#E06C75` | Coral rose |
| `correspondence` | `#D19A66` | Copper |
| `unknown` | `#ECEFF4` | Default primary text |

## 3. Typography & Hierarchy

- **Single Sans Family + Tabular Monospace Data:** Proportional sans for player names, headings, and controls; monospace for SAN moves, Elo ratings, percentages, evaluations, and clocks.
- **Tight Scale:**
  - Section / Profile Heading: `15.5px` strong
  - Primary Row / Player Name: `13.5px` strong
  - Data Table Cell: `12.5px` tabular
  - Metadata / Subtitle: `11.5px` secondary (`#9AA3B2`)
  - Title Badge Pill (`GM`, `IM`): `10.5px` bold dark text on `#E6B450` gold pill (`corner_radius: 3px`)

## 4. Component Grammar

1. **Board & Player Strips (`CentralPanel`):**
   - Left vertical evaluation bar (`14px` wide, smooth fill, score label at top/bottom).
   - Top & bottom player strips matched to board width: `[Portrait 32x32 rounded 4px]` `[Name]` `[(Elo)]` `[Gold Title Pill]` `[Country Flag]` — and right-aligned monospace `[Clock 14:32]` or move counter.
2. **Database Players List & Top Player Profile Card:**
   - **Player List Row:** `[Portrait (optional)]` `[Gold Title Pill]` `[Player Name]` `[Latest OTB Elo (prominent)]` on the top line, with `N games · score X% · YYYY–YYYY` below, and the **Country Flag** right-aligned.
   - **Selected Player Profile Card (top of Players view):**
     - Left: Square portrait (`64x64`, rounded `6px`, silhouette fallback).
     - Right: `[Gold Title Pill]` `[Name]` `[Country Flag]`, prominent `FIDE Rating` (latest OTB Elo), `W/D/B` horizontal stacked bar + win/draw/loss counts, and a single **`Prepare`** action bar with clickable **White (`⬜`)** and **Black (`⬛`)** squares that immediately launch preparation against that player with the chosen color.
3. **Reference and Preparation Tables:**
   - **Reference** selects one human-data source at a time—local database,
     Lichess, ChessABC, or a native book—and keeps its move statistics, trends,
     and available games together. Persistent source paths live in Settings.
   - **Prep** keeps opponent probabilities and game history in its own tab so
     starting preparation never replaces the general Reference view.
   - Scannable move grids keep `Move` | `Prob / Share` | `Games` | `W/D/B Bar`
     (`White`/`Grey`/`Black` horizontal bar) | `Reference stats` | `Trend` |
     `Eval` aligned where the active workflow supports them.
4. **Hover-to-Preview Overlay:**
   - Hovering any game row in `Games`, `Players`, or `Recent Games` highlights the row, steps moves with `←`/`→`/`Home`/`End`, updates the board + player strips, and overlays the hovered game's move sheet and header metadata in the right-hand Moves view with a subtle `Previewing hovered game` pill.
