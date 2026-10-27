# Position index

Brett's opening tree is backed by a position index derived from the currently
loaded PGN, SCID, or classic CBH database, or from a separately configured
opening-tree file. This document describes what is counted, the on-disk
format, cache invalidation, and current limits.

## What is counted

- Every **mainline** move of every game. Variations are skipped: they are
  analysis, not games played in the database.
- For each move, the position **before** the move is hashed with a stable
  Zobrist key from `shakmaty`.
- A game is counted at most once per position. If a game repeats a position,
  indexing stops at the repetition.
- Result (`1-0`, `0-1`, `1/2-1/2`, `*`), `WhiteElo`/`BlackElo`, and `Date` tags
  are aggregated into the record.
- Indexing stops at the last valid move when it encounters a null move, an
  illegal move, a malformed game, or an invalid setup. The opening tree
  therefore degrades gracefully instead of inventing statistics.

Position totals shown in the UI are the sum of the outgoing move records. A
position with no outgoing move (the final position of a game) is not stored;
the opening tree is about moves that were actually played.

PGN files use the streaming `pgn-reader` visitor: tags and byte ranges are
indexed once, and the position builder reads the file directly without building
a full game tree. SCID and classic CBH databases implement the shared
`GameLibrary` interface and use the generic builder, which loads one decoded
game at a time through the native reader. Both paths produce the same edge
records.

## On-disk layout

All integers are little-endian.

```text
header (80 bytes)
  magic                8   "BRETTPOS"
  version              4   1
  flags                4   reserved
  source length        8   combined source bytes at build time
  source mtime seconds 8   newest source modification time
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

## Cache and invalidation

The index is never written next to the PGN or a database directory. It lives at:

```text
$XDG_CACHE_HOME/brett/index/<fnv1a-of-canonical-path>.brettpos
```

or `~/.cache/brett/index/` when `XDG_CACHE_HOME` is unset. The filename depends
on the canonical path of the PGN, the SCID `.si` file, or the classic CBH
`.cbh` file.

The header stores a source fingerprint. For PGN it is the file length and
modification time. For SCID it combines the lengths of `.si`, `.sn`, and `.sg`
and uses the newest modification time among them; changing any part of the
triple therefore invalidates the cache. For classic CBH it combines the
lengths of `.cbh`, `.cbg`, and every present companion (`.cba`, name files,
`.cbj`) and uses the newest modification time; changing any part of the file
set invalidates the cache. A stale or corrupt index is ignored and rebuilt.
Builds write to a temporary file in the cache directory and atomically rename
it into place, so a crash cannot leave a half-written index under the final
name.

The fingerprint is metadata-based. Replacing a source with one of exactly the
same total length while also preserving its modification time would not be
detected; normal edits change at least one of those values.

The cache is already the persistent index: reopening a database calls
`PositionIndex::open_cached_for`, memory-maps the existing `.brettpos` file,
and does **not** re-index it. Re-indexing happens only when the cache is
missing, stale, or corrupt. The large-database bottleneck is therefore the
**first** build, not repeated startup: the builder currently aggregates every
edge in an in-memory `HashMap` and keeps the full header table in RAM. M2b
addresses those two costs with external sorting and an on-disk header table;
incremental updates are a further optimization, not a prerequisite for using
the cache.

## Performance

On the native 49,780-game `Frese Sammlung` SCID database (8,047,585 bytes,
3.8 M mainline moves), in the development profile:

- header/namebase index: **0.28 s**
- position index: **16.5 s**
- output: **137.6 MiB**, 2.94 M positions, 3.01 M move records
- start position: 48,619 games with a move available

Query cost is independent of the number of games for the binary searches;
returning the moves at a position is linear in the number of distinct moves
there.

## Current limits / next steps

- The build aggregates in `HashMap`s in memory. That is comfortable for 50k
  games, but a multi-million-game Mega Database will need external sorting or a
  chunked merge. Automatic indexing therefore stops at 250,000 games; the Tree
  tab offers a warned manual build up to 1,000,000 games and refuses a full
  build above that until the external-sort indexer exists.
- The header table for PGN, SCID, and classic CBH is still in RAM. Repeated
  header strings are interned and primary/external session rows resolve their
  headers lazily from the source library. Opening the 11,151,119-game Mega
  Database 2025 in the development profile now stabilizes around 6.4 GiB RSS
  instead of exhausting RAM, and does not start a position index
  automatically. A compact/on-disk header table is still tracked as M2b.
- An index is built for one database at a time. The Tree tab can point at a
  separate opening-tree file, but merging several files into one opening tree
  and a static, rating-matched background distribution for preparation are
  future work.
- Chess960 SCID games are rejected by the reader before indexing. Classic CBH
  mode-10 Chess960 games are supported.
