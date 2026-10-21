# Position index

Brett's opening tree is backed by a position index derived from the loaded PGN.
This document describes the design, the on-disk format, and the current limits.

## What is counted

- Every **mainline** move of every game. Variations are skipped: they are
  analysis, not games played in the database.
- For each move, the position **before** the move is hashed with a stable
  Zobrist key from `shakmaty`.
- A game is counted at most once per position. If a game repeats a position,
  indexing stops at the repetition.
- Result (`1-0`, `0-1`, `1/2-1/2`, `*`), `WhiteElo`/`BlackElo`, and `Date` tags
  are aggregated into the record.
- Games with illegal SAN, a null move, or an invalid setup are skipped after the
  last valid move. The opening tree therefore degrades gracefully instead of
  inventing statistics.

Position totals shown in the UI are the sum of the outgoing move records. A
position with no outgoing move (the final position of a game) is not stored;
the opening tree is about moves that were actually played.

## On-disk layout

All integers are little-endian.

```
header (80 bytes)
  magic                8   "BRETTPOS"
  version              4   1
  flags                4   reserved
  source length        8   bytes of the PGN at build time
  source mtime seconds 8
  source mtime nanos   4
  max plies            4   u32::MAX when unlimited
  edge count           8
  game count           8
  position count       8
  build milliseconds   8
  reserved             8

edge records (48 bytes each, sorted by key then move)
  position key         8   Zobrist64
  move code            4   from | to << 6 | promotion << 12
  games                4
  white wins           4
  draws                4
  black wins           4
  Elo sum              8
  Elo count            4
  first date           4   YYYYMMDD, 0 if unknown
  last date            4   YYYYMMDD, 0 if unknown
```

The move code is 16 bits of squares (`Square::to_u32()` is 0..63) plus a
promotion code in bits 12..14: `0` queen, `1` rook, `2` bishop, `3` knight,
`7` no promotion. Castling is encoded as the king's from-square and the king's
destination square (`g1`/`c1`); decoding searches the legal moves for the match.
The file is memory-mapped read-only. A lookup is two binary searches over the
sorted edge array, then a linear scan of the matching run.

## Cache

The index is never written next to the PGN or a database directory. It lives at:

```
$XDG_CACHE_HOME/brett/index/<fnv1a-of-canonical-path>.brettpos
```

or `~/.cache/brett/index/` when `XDG_CACHE_HOME` is unset. The filename depends
only on the canonical source path; the header stores the source length and
modification time. A stale index is ignored and rebuilt. Builds write to a
temporary file in the cache directory and atomically rename it into place, so a
crash cannot leave a half-written index under the final name.

## Performance

On the 49,780-game / 32.9 MiB SCID test export (3.86 M mainline positions):

- build: **8.9 s** in the development profile
- index: **137.6 MiB** for 2.94 M positions and 3.01 M move records
- start position: 48,619 games with a move available

Query cost is independent of the number of games for the binary searches;
returning the moves at a position is linear in the number of distinct moves
there.

## Current limits / next steps

- The build aggregates in `HashMap`s in memory. That is comfortable for 50k
  games, but a multi-million-game Mega Database will need external sorting or a
  chunked merge.
- The PGN header table is still in RAM. Moving it on disk is tracked as M2b.
- An index is built for one PGN at a time. Reference-database merging (several
  files into one opening tree) comes with opponent preparation in M6.
