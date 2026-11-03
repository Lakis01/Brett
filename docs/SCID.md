# Native SCID support

Brett reads and writes SCID databases directly. Opening a `.si4` or `.si3` file
does not require SCID to be installed, does not convert the database to PGN,
and never writes to the source files. Exporting creates a new SCID4 triple; the
writer is a separate, explicit **Save database as… → SCID4** action.

Both implementations are independent: they were written from the published
on-disk layout and the SCID project's format documentation. No SCID source code
is linked or copied. See [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

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
  `FEN`, `Variant`, and other custom tags, including `WhiteTitle`/`BlackTitle`
  if the record explicitly carries them);
- encoded mainline moves, including castling, en passant, and promotions;
- promotion and under-promotion flags;
- null moves (`--`), which advance the side to move;
- recursive variations, with piece-list state restored at variation end;
- textual comments, assigned in SCID's PGN traversal order;
- NAGs, including the standard `!`, `?`, `!?`, `+=`, `=+`, and positional
  symbols.

The namebase stores player names only; it has no separate title field, and
SCID's own `scidpgn` export of the reference corpus emitted no
`WhiteTitle`/`BlackTitle` tags. Brett therefore does not synthesize titles for
SCID sources. A title tag stored explicitly in a game record is still decoded
and displayed like any other custom tag.

Custom starting positions are reconstructed from the record's FEN tag. The
piece list used by SCID's compact move encoding is rebuilt from that position,
so games that do not start from the standard array decode correctly.

## Writing a SCID database

**File → Save database as… → SCID4** writes the current session, in its current
order, as a new SCID4 triple. The writer is native Rust in
[`src/scid/writer.rs`](../src/scid/writer.rs); it does not invoke SCID, link
SCID code, or convert through PGN.

- The target must end in `.si4`. Brett writes the matching `.sn4` and `.sg4`
  companions beside it.
- Headers, custom tags, comments, clocks, NAGs, recursive variations, custom
  start FENs, castling, en passant, promotions, under-promotions, and null
  moves are preserved.
- The namebase is built with dense, byte-sorted IDs and front-coded names. The
  index records the standard flags, offsets, results, dates, Elos, ECO codes,
  final material signatures, half-move counts, and home-pawn data.
- Game records are streamed to a temporary file, with a compact provisional
  index. The completed triple is installed by renaming the companions first and
  the index last.
- If an existing triple is present, Brett asks for confirmation, moves the old
  files aside, and restores them if any step of the replacement fails.
- The source database cannot be selected as the export target. Exporting is
  disabled while no database is open.
- A single record is limited to 131,071 bytes, the game file to 4 GiB, and a
  database to 16,777,214 games. Chess960 and other non-standard variants are
  rejected instead of being written incorrectly.

For diagnostics and format work, the helper exports any PGN file:

```sh
cargo run --example scid_export -- input.pgn /tmp/opencode/output.si4
```

## Validation

The reader and writer tests generate a SCID4 database at test time from the
public-domain Opera Game embedded inline in [`src/test_data.rs`](../src/test_data.rs).
The round trip exercises a comment, a variation, a NAG, castling, promotion, and
the normal header/namebase path, so no database fixture is committed.

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

The writer is validated in two independent ways:

- every writer test round-trips through `ScidLibrary` and compares the decoded
  mainline SAN, headers, comments, NAGs, variations, clocks, and custom starts;
- a five-game rich fixture (comments, clocks, a NAG, a variation, custom FEN,
  castling, en passant, under-promotion, and a null move) was exported with
  `scid_export`, then read by SCID vs. PC **4.27** `scidt` and `scidpgn` with
  all five games and all moves intact. Brett's reader decoded the same file
  with zero errors.

The SCID executables are used only as external validation oracles; Brett never
invokes them at runtime.

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
  `chess960` are rejected with a clear error instead of being decoded or
  written incorrectly.
- **SCID5 and newer layouts are not supported.**
- Brett reads existing databases and exports a new SCID4 triple. It does not
  edit games in place, add annotations to a source database, delete games, or
  compact databases.
- The writer currently targets the SCID4 (version 400) layout only. It does not
  write SCID3, and it does not implement Chess960.
- Clock/evaluation annotations are not separately modelled; clocks are
  preserved as `[%clk]` text inside comments, and only textual comments and
  standard NAGs are represented in the game tree.
- A malformed game can fail individually; it does not prevent the rest of the
  database from opening. The game list still shows its headers, and selecting
  the game reports the decode error.

## Data safety

The conventional `$HOME/Chess Databases/` tree and every directory below it
are strictly read-only, as are extra roots in `BRETT_READ_ONLY_ROOTS`. Brett
opens the source triple directly for reading, and derived indexes are written
only under the user's cache directory. **Save database as… → SCID4** writes a
new triple at an explicit path; the app refuses to overwrite the open source
database and asks before replacing an existing triple. If you need to run
SCID's own tools for comparison, copy the triple to `/tmp/opencode` first; see
[`scripts/scid-to-pgn.sh`](../scripts/scid-to-pgn.sh) and
[DATA_SAFETY.md](DATA_SAFETY.md).

## Code map

| Path | Purpose |
| --- | --- |
| `src/scid.rs` | Index/namebase/record parsing, `ScidLibrary`, `PieceLists` |
| `src/scid/writer.rs` | Native SCID4 writer, namebase/index construction, rollback-safe triple commit |
| `src/library.rs` | `GameLibrary`, `IndexedGame`, source metadata |
| `src/position.rs` | Generic position-index builder and cache fingerprint |
| `src/players.rs` | Player directory and preparation books |
| `src/app.rs` | Format detection, opening, game selection |
| `src/test_data.rs` | Inline public-domain Opera Game used by parser and SCID tests |
| `examples/scid_report.rs` | Full-corpus decode check |
