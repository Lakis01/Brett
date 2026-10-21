# Brett

A native Linux chess preparation workspace. It is a functional re-implementation
of the parts of ChessBase that matter for opening preparation — not a clone of
its UI, and not a Wine package.

## Current state (M1)

- Native `egui`/`eframe` window (X11 and Wayland).
- Streaming PGN index: opening a file records every game's headers and byte
  range, then builds a full tree only for the game you select. The 49,780-game
  (32.9 MiB) SCID test export indexes in **0.6 s** and uses ~36 MiB for headers,
  instead of ~1 GiB for retained trees; any game loads lazily in ~0.1–0.3 ms.
- Opens PGN files by drag & drop, the path field, a native zenity/kdialog
  dialog, or a CLI argument.
- Full game-tree import: recursive variations, comments, NAGs, `SetUp`/`FEN`
  positions, and analysis null moves (`--`).
- 2D board with the cburnett piece set, coordinates, legal-move hints, click to
  play moves and branch into variations.
- Move list with variations in PGN order, comments, copyable FEN, and keyboard
  navigation (`←`/`→`/`Home`/`End`, `↑`/`↓` for games, `F` to flip).
- Verified against a 49,780-game SCID export (3.8 M nodes) with zero import
  errors.

## Development

```sh
cargo test                                   # unit + fixture smoke tests
cargo clippy --all-targets
cargo run --example report -- file.pgn       # import statistics
cargo run --example index_report -- file.pgn # index + lazy-load timing
scripts/scid-to-pgn.sh "/path/to/db" /tmp/opencode/out.pgn
```

`scripts/scid-to-pgn.sh` copies the SCID triple to `/tmp/opencode` first; it
never writes to the source database.

## Roadmap

- **M1 — database index (done).** Header-only first pass, lazy per-game trees,
  and a byte-offset table so a 50k-game PGN does not cost 1 GiB of RAM. (A
  retained tree costs ~280 bytes per position; the 49,780-game test corpus is
  3.86 M positions.)
- **M2 — position index and opening tree.** Zobrist key per position, on-disk
  sorted/mmap store, win/draw/loss, Elo and date aggregates, move statistics.
  The header table moves on disk too, so a multi-million-game Mega Database does
  not have to fit in RAM.
- **M3 — CBH/2CBH import** behind the `cbvault` adapter (fixtures pending).
- **M4 — UCI engine** (Stockfish at `/usr/bin/stockfish`), analysis lines on the
  board.
- **M5 — cloud evaluation** (Lichess cloud eval, ChessDB.cn) with a local
  Stockfish fallback.
- **M6 — opponent preparation reports** built from the same position index.

## Build and run

```sh
cargo run --release -- path/to/file.pgn
```

Without an argument, open a file from the UI. Stockfish is detected separately
(engine support lands in a later milestone).

## Scope

In: PGN and CBH import, Zobrist position index, opening tree, UCI engine, cloud
evaluation (Lichess / ChessDB.cn), opponent preparation reports, 2D board.

Out: 3D board, video, speech annotation, Playchess/cloud sync, publishing and
printing, licensing/serial subsystems.

## Data safety

**`/home/gratis01/Chess Databases/` is treated as read-only.** Nothing in this
project ever deletes, renames, or writes there. See
[docs/DATA_SAFETY.md](docs/DATA_SAFETY.md).

## Third-party assets

The piece images in `assets/pieces/` are the **cburnett** set by
Colin M. L. Burnett, licensed **CC BY-SA 3.0**, converted from the SVG sources
in the [Lichess repository](https://github.com/lichess-org/lila/tree/master/public/piece/cburnett).
See `assets/pieces/README.md`.

## Licence

MIT (application code) — see [LICENSE](LICENSE). The piece artwork remains
CC BY-SA 3.0; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
