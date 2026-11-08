# Product

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

## Stack

Rust (`edition = "2024"`, `eframe` / `egui` 0.37 native Linux desktop application)

## Users

Tournament and club chess players preparing for upcoming over-the-board (OTB) or serious online games, analyzing their own games (`Eigene Partien`), and browsing large reference corpora (`Mega Database 2026.2cbh`, SCID `.si4`, ChessBase `.cbh`/`.2cbh`, PGN, and live cloud books).

## Product Purpose

Brett is a fast, native Linux chess database and opponent-preparation workstation. It compiles an opponent's complete playing history across local databases, reference corpora, ChessABC OTB tournament feeds, Lichess, and Chess.com accounts into a unified empirical-Bayes repertoire model, paired with live reference trees, opening popularity trends, and multi-source evaluation.

## Positioning

Unlike browser tools that fragment OTB databases and online accounts or legacy desktop suites with cluttered multi-window chrome, Brett unifies multi-format local databases (`.2cbh`, `.cbh`, `.si4`, `.pgn`, `.bin`, `.ctg`, `.abk`, `.bkt`) and live cloud sources (`ChessABC`, `Lichess Masters`, `Lichess All`, `chessdb.cn`) into a single, keyboard-driven, zero-latency preparation surface.

## Operating Context

- Used on high-DPI Linux laptops (2560x1600, KDE Plasma, Wayland/X11) before tournament rounds and during post-game analysis.
- Operates in **Operate** mode: scanability, tabular numerical alignment, instant hover-to-preview keyboard scrubbing, and calm visual hierarchy outrank decorative chrome.

## Capabilities and Constraints

- Native `eframe`/`egui` immediate-mode UI with three primary spatial zones:
  1. **Left — Database & Players Navigation** (`Games`, `Players`, top Player Profile card with direct `Prepare` color-square triggers).
  2. **Center — Board Canvas** (warm wood/stone board, slim left evaluation bar, compact top/bottom player strips with photo, title badge, latest OTB Elo, country flag, and clock).
  3. **Right — Analysis & Preparation Workspace** (`Moves`, `Tree` / `Preparation`, `Engine`, `Review`).
- Game-category identity is communicated exclusively via user-configurable font colors (differentiating OTB Classical/Rapid/Blitz, Online Classical/Rapid/Blitz/Bullet, and Correspondence) rather than noisy text badges.
- All W/D/B statistics render as compact horizontal stacked bars (White wins = white, Draws = neutral slate/grey, Black wins = dark charcoal/black).

## Product Principles

1. **Data density without chrome bloat:** One unified table beats three stacked tables; color-coding and horizontal bars replace repetitive text labels.
2. **Zero-commit hover exploration:** Hovering any game row in any list previews its board position, player strips, and move tree in the right-hand Moves section with arrow-key stepping (`←`/`→`/`Home`/`End`), committing only on click.
3. **Resilient background streaming:** Multi-source compilation streams incremental results without blocking the UI or stretching layout containers.
4. **Earned familiarity:** Controls, typography, and spacing follow strict `Operate`-mode discipline—no nested card-in-card clutter, no decorative gradients, and consistent 1px structural hairlines.

