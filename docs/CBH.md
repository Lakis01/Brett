# Native classic ChessBase CBH support

Brett reads the older multi-file ChessBase database format directly. This is
the classic `.cbh` format, not CBH2/2CBH. Opening a database does not require
ChessBase, Wine, or a converter, and never writes to the source files.

The implementation is native Rust in `src/cbh/`. It was written from the
published on-disk layout and independent format references; no GPL converter
code is linked or copied. Translation-table and reference provenance is in
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

## Files

A classic database is a set of files sharing one base name:

| File | Contents |
| --- | --- |
| `.cbh` | 46-byte header record for each game, including move/annotation offsets |
| `.cbg` | Move records: encoding mode, optional start position, move stream |
| `.cba` | Annotation records: comments, NAGs, drawing tokens, times, evaluations |
| `.cbp` | Players |
| `.cbt` | Tournaments (event title and site) |
| `.cbc` | Annotators |
| `.cbs` | Sources |
| `.cbj` | Optional 64-bit offsets for `.cbg`/`.cba` files over 4 GiB |

The derived `.cbb`/`.cbgi` boosters are not needed. `.cbe` teams and other
metadata files are ignored. Brett requires `.cbh` and `.cbg`; the name and
annotation files are optional but are validated when present.

Entity files use a little-endian header and fixed-size records. Entity ids in
the `.cbh` are zero-based. Brett reads `.cbp` records with 30-byte last-name and
20-byte first-name fields, `.cbt` with 40-byte title and 30-byte place fields,
and the corresponding annotator/source fields. Deleted or missing entities are
omitted or resolve to `?`.

## Opening and indexing

`CbhLibrary::open` accepts the `.cbh` path, a `.cbg`/`.cba` companion, or the
bare base name. It:

- validates the 46-byte record size and file length;
- reads every header into the same `IndexedGame` rows used by PGN and SCID;
- resolves player, tournament, annotator, and source names once;
- checks deleted (`0x80`) and guiding-text (`0x02`) flags;
- records the source paths and sizes used by the position-index fingerprint.

Only headers are decoded at open time. `load(index)` reads exactly one `.cbg`
record and its optional `.cba` record, so opening a large database is cheap and
selecting a game is independent of database size.

## Move decoding

The implemented encodings are modes **0, 4, 5, and 10**:

- mode 0: compact one-byte codes and two-byte square moves, pre-translated by
  the ply count;
- mode 4: compact codes, post-translated;
- mode 5: two-byte square moves with variation/end flags;
- mode 10: compact codes for Chess960, with an explicit start and named king
  and rook squares.

Mode 11 is not decoded. Such a game opens with its headers and a visible
"not supported" error; the rest of the database remains usable.

The walker supports promotions, en passant, null moves, recursive variations,
castling (standard and Chess960), and custom start positions. Nodes are
appended in stored depth-first order without de-duplicating equal moves. This
is required because `.cba` annotation positions are indices into that exact
stored order.

## Annotations

Annotation items name a stored position, a type, and a payload. Brett decodes:

- text before (`0x82`) and after (`0x02`) the move;
- NAGs (`0x03`), in ChessBase's export order: prefix, move, position;
- coloured squares (`0x04`, `[%csl …]`) and arrows (`0x05`, `[%cal …]`);
- time spent (`0x07`, `[%emt …]`);
- engine evaluations (`0x21`, `[%eval …]`);
- medals (`0x22`, `[%mdl …]`).

Drawing and evaluation tokens are stored in the normal comment string, in the
same order ChessBase's own PGN export uses. Other annotation kinds are skipped
by their own size. Positions past the last stored move attach to the main
line's last move, matching ChessBase's export.

## Headers

The base tags are Event, Site, Date, Round, White, Black, Result, ECO,
WhiteElo, BlackElo, Annotator, and Source. Games with no moves receive
`PlyCount "0"`. Custom starts add `SetUp`, `FEN`, and, for Chess960, `Variant`.
Dates use `????`/`??` for unknown parts; rounds use `?`, `N`, `N.M`, or `?.M`.
ECO codes cover A00-E99.

## Validation

The reader was validated against the independent MIT-licensed `cbvault` reader
on the personal reference database (the database itself is not committed):

- **24,865 games** and **2,003,054 nodes** decoded with **zero errors**;
- full export comparison: no tree-path, SAN, or NAG mismatches;
- all **168 annotated games**: comments and NAGs match after drawing,
  time, and evaluation tokens are included;
- headers match except 90 player names where `cbvault`'s current name-buffer
  path drops a final non-ASCII byte; Brett preserves it. Brett also emits an
  `Annotator` tag for 134 games that `cbvault` does not.

No real mode-4 or mode-5 games occur in that sample. Those paths, mode 0, mode
10 Chess960, comments, NAGs, drawing tokens, custom starts, deleted games,
empty move records, wide `.cbj` offsets, mode-11 errors, truncated records, bad
headers, and position indexing are covered by the synthetic byte-level writer
in [`src/cbh/tests.rs`](../src/cbh/tests.rs). The synthetic fixture is generated
at test time, so no personal database is redistributed.

Run the CBH tests with:

```sh
cargo test --lib cbh::tests
```

## Position index integration

`CbhLibrary` implements `GameLibrary`, so `PositionIndex::build_cached_for`
uses the generic loader for CBH just as it does for SCID. The index format is
identical to PGN and SCID. The cache is keyed by the canonical `.cbh` path and
its fingerprint combines the lengths and newest modification time of the
header, move, annotation, name, and wide-offset files under the base name.

## Limits

- Read-only. Editing, deleting, or compacting a classic CBH database is out of
  scope.
- Mode 11 games are header-only with a visible error.
- Mode 4 and mode 5 are implemented and synthetically tested but are not
  represented in the local reference sample.
- Richer annotation kinds (quotations, clocks, media, mainline `[%evp]`) are
  skipped. The common drawing, time, per-move evaluation, and medal tokens are
  preserved as comment text.
- `.cbe` teams and the derived `.cbb`/`.cbgi` boosters are not read.
- A malformed individual game is reported when it is loaded (as a game error
  or a load error); it does not prevent the rest of the database from opening.

## Data safety

The conventional `$HOME/Downloads/CBH/` sample directory and the user's chess
database tree are strictly read-only. Brett opens every source file with
`File::open` and writes only derived position indexes under
`$XDG_CACHE_HOME/brett/index/`. Add extra protected roots with
`BRETT_READ_ONLY_ROOTS`. For external comparisons, copy the database to
`/tmp/opencode/` first; see [DATA_SAFETY.md](DATA_SAFETY.md).
