# Brett

A native Linux chess preparation workspace. It is a functional re-implementation
of the parts of ChessBase that matter for opening preparation — not a clone of
its UI, and not a Wine package.

## Current state (M2a)

- Native `egui`/`eframe` window (X11 and Wayland).
- Streaming PGN index: opening a file records every game's headers and byte
  range, then builds a full tree only for the game you select. The 49,780-game
  (32.9 MiB) SCID test export indexes in **0.6 s** and uses ~36 MiB for headers,
  instead of ~1 GiB for retained trees; any game loads lazily in ~0.1–0.3 ms.
- Position index and opening tree: a Zobrist key is computed for every mainline
  position, and `(position, move)` statistics are stored in a sorted, memory-
  mapped file. The 49,780-game / 3.86 M-position test corpus builds in **8.9 s**
  and produces a **137.6 MiB** index with 2.94 M positions and 3.01 M move
  records.
- Tree tab shows games, white/draw/black counts, score, average Elo,
  year range, and lets you play any indexed move. The index is built in the
  background on first open and cached under `$XDG_CACHE_HOME/brett/index/`.
  See [docs/POSITION_INDEX.md](docs/POSITION_INDEX.md) for the format and limits.
- Full game-tree import: recursive variations, comments, NAGs, `SetUp`/`FEN`
  positions, and analysis null moves (`--`).
- 2D board with the cburnett piece set, coordinates, legal-move hints, click or
  drag-and-drop to play moves and branch into variations.
- Move list with variations in PGN order, comments, copyable FEN, and keyboard
  navigation (`←`/`→`/`Home`/`End`, `↑`/`↓` for games, `F` to flip, `T`/`M`
  for the tree / move list).
- Verified against a 49,780-game SCID export (3.8 M nodes) with zero import
  errors.

## Development

```sh
cargo test                                   # unit + fixture smoke tests
cargo clippy --all-targets
cargo run --example report -- file.pgn          # import statistics
cargo run --example index_report -- file.pgn    # index + lazy-load timing
cargo run --example position_report -- file.pgn # opening-tree index benchmark
scripts/scid-to-pgn.sh "/path/to/db" /tmp/opencode/out.pgn
```

`scripts/scid-to-pgn.sh` copies the SCID triple to `/tmp/opencode` first; it
never writes to the source database.

## Roadmap

- **M1 — database index (done).** Header-only first pass, lazy per-game trees,
  and a byte-offset table so a 50k-game PGN does not cost 1 GiB of RAM. (A
  retained tree costs ~280 bytes per position; the 49,780-game test corpus is
  3.86 M positions.)
- **M2a — position index and opening tree (done).** Zobrist key per position,
  on-disk sorted/mmap store, win/draw/loss, Elo and date aggregates, move
  statistics, and a clickable tree tab. Mainlines only; variations are
  analysis, not database games.
- **M2b — Mega-scale ingest.** The current build uses in-memory aggregation and
  keeps the PGN header table in RAM. External sorting, an on-disk header table,
  and incremental updates are needed for multi-million-game databases.
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
