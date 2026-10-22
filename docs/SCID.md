# Native SCID support

Brett reads SCID databases directly. Opening a `.si4` or `.si3` file does not
require SCID to be installed, does not convert the database to PGN, and never
writes to the source files.

The implementation is independent: it was written from the published on-disk
layout and the SCID project's format documentation. No SCID source code is
linked or copied. See [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

## Files and versions

A SCID database is three files sharing a base name:

| File | Contents |
| --- | --- |
| `.si4` / `.si3` | Index header and one fixed-size record per game |
| `.sn4` / `.sn3` | Namebase: players, events, sites, rounds |
| `.sg4` / `.sg3` | Game records: tags, moves, comments, variations |

Brett accepts SCID versions **300 through 399** (SCID3 layout) and version
**400** (SCID4 layout). The suffix used for the namebase and game file follows
the version in the index header, not the filename extension. SCID5 and other
version numbers are rejected with an explicit error.

The `.si` file is opened read-only. The companion files must exist beside it.
Brett never creates a missing companion file and never opens a database in
write mode.

## What is decoded

Opening a database reads only the index header, the fixed-size game index, and
the namebase. This produces the same `IndexedGame` header rows used by the PGN
library, so the Games tab, player directory, filters, and game list work
without decoding any moves.

`ScidLibrary::load(index)` then reads exactly one `.sg` record and builds a full
`Game` tree:

- the ten base header fields from the index (White, Black, Event, Site, Date,
  Round, Result, ECO, Elo, EventDate), with names resolved through the
  compressed namebase;
- additional tags stored in the game record (`Annotator`, `PlyCount`, `SetUp`,
  `FEN`, `Variant`, and other custom tags);
- encoded mainline moves, including castling, en passant, and promotions;
- promotion and under-promotion flags;
- null moves (`--`), which advance the side to move;
- recursive variations, with piece-list state restored at variation end;
- textual comments, assigned in SCID's PGN traversal order;
- NAGs, including the standard `!`, `?`, `!?`, `+=`, `=+`, and positional
  symbols.

Custom starting positions are reconstructed from the record's FEN tag. The
piece list used by SCID's compact move encoding is rebuilt from that position,
so games that do not start from the standard array decode correctly.

## Validation

The committed fixture is generated from the public-domain Opera Game in
[`fixtures/opera.pgn`](../fixtures/opera.pgn) and exercises a comment, a
variation, a NAG, castling, promotion, and the normal header/namebase path.
Regeneration is documented in
[`fixtures/scid/README.md`](../fixtures/scid/README.md).

The reader has also been run against every `.si4` database in the local
reference corpus. The largest is `Frese Sammlung`:

- **49,780 games**, 8,047,585 bytes across the three files;
- header/namebase index in **0.28 s**;
- **3,805,440 mainline moves** decoded, **zero errors**;
- position index in **16.5 s**, producing a 137.6 MiB index with 2.94 M
  positions and 3.01 M move records.

All local `.si4` databases decoded with zero errors. SCID3 is implemented from
the older 46-byte entry layout and 128-byte header, but there is no real SCID3
database in the local corpus yet, so that path is structurally tested rather
than corpus-tested.

Use the report helper after changing the decoder:

```sh
cargo run --example scid_report -- path/to/database.si4
```

It decodes every game and exits non-zero if any record fails. A successful run
prints the game count, file size, indexing time, move count, and error count.

## Position index integration

`ScidLibrary` implements the same `GameLibrary` trait as `PgnLibrary`.
`PositionIndex::build_cached_for` uses the streaming PGN visitor when the
library is a PGN file and the generic library loader for SCID. The resulting
`.brettpos` format is identical.

The cache is keyed by the canonical `.si` path. Its fingerprint combines the
lengths of all three files and the newest modification time. Changing any part
of the triple therefore invalidates the cached position index. Cache files live
under `$XDG_CACHE_HOME/brett/index/` (or `~/.cache/brett/index/`); nothing is
stored beside the database.

## Limits

- **Chess960 is not supported.** Games with a `Variant` tag containing `960` or
  `chess960` are rejected with a clear error instead of being decoded
  incorrectly.
- **SCID5 and newer layouts are not supported.**
- Brett is a reader. Editing games, adding annotations to the source database,
  deleting games, and compacting databases are out of scope.
- Clock/evaluation annotations are not separately modelled; only textual
  comments and standard NAGs are represented in the game tree.
- A malformed game can fail individually; it does not prevent the rest of the
  database from opening. The game list still shows its headers, and selecting
  the game reports the decode error.

## Data safety

`/home/gratis01/Chess Databases/` and every directory below it are strictly
read-only. Brett opens the source triple directly for reading, and derived
indexes are written only under the user's cache directory. If you need to run
SCID's own tools for comparison, copy the triple to `/tmp/opencode` first; see
[`scripts/scid-to-pgn.sh`](../scripts/scid-to-pgn.sh) and
[DATA_SAFETY.md](DATA_SAFETY.md).

## Code map

| Path | Purpose |
| --- | --- |
| `src/scid.rs` | Index/namebase/record parsing, `ScidLibrary`, `PieceLists` |
| `src/library.rs` | `GameLibrary`, `IndexedGame`, source metadata |
| `src/position.rs` | Generic position-index builder and cache fingerprint |
| `src/players.rs` | Player directory and preparation books |
| `src/app.rs` | Format detection, opening, game selection |
| `fixtures/scid/` | Public-domain generated SCID fixture |
| `examples/scid_report.rs` | Full-corpus decode check |
