# Native ChessBase CBH and 2CBH support

Brett reads and writes the current multi-file ChessBase 2CBH container (moves,
variations, and metadata) and reads and writes the older classic CBH format
directly. 2CBH comments, NAGs, clocks, and evaluations are not written yet.
Opening a database does not require ChessBase, Wine, or a converter, and
opening a database never writes to the source files.

The classic implementation is native Rust in `src/cbh/`. The 2CBH reader and
writer are native Rust in `src/twocbh/`. Both were written from observed
on-disk layout and independent format references; no ChessBase code, binary,
or token-table dump is linked or copied. Translation-table and reference
provenance is in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) and
[PROVENANCE.md](PROVENANCE.md).

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

Classic `.cbp` player records have **no title field**: the 50-byte record is
only the 30-byte last name followed by the 20-byte first name. A CBH database
therefore cannot supply FIDE or historical player titles, and Brett does not
infer one from a rating. Title badges appear only when the title comes from
another source, such as an imported PGN tag.

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
on the personal reference database (the database itself is not committed).
The code-level provenance, including the MIT `cbvault-format` dependency and
its `oschess-cb-bridge` port, is recorded in
[PROVENANCE.md](PROVENANCE.md). Validation results:

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

## Current ChessBase 2CBH support (`src/twocbh/`)

2CBH is a different container from classic CBH. A database is a set of files
sharing one base name:

| File | Contents |
| --- | --- |
| `.2cbh` | 192-byte header records; record 0 is the file header |
| `.2cbg` | Move records |
| `.2cba` | Annotation records (framing validated; item structure not decoded) |
| `.2lid` | Inline player, event, and source names |
| `.2lcd`, `.2lgd` | ChessBase's own derived indexes, ignored |

`TwoCbhFile::open` accepts the `.2cbh` path or a companion file. It validates
the 192-byte record size, the record count at `0x10` (which includes record 0),
the `0x26` generation, and the move/annotation file headers. Opening a
database never writes to it.

Each game header record supplies the player, event, and source name ids, flags,
round/subround, result, Elos, Chess960 start marker, game id, beauty score, and
packed date. Player names are stored as last name, first name; a two-letter
first name gets the conventional trailing period. Elo `0` is omitted. A packed
date with a zero month or day renders `??`; the file does not always carry a
full date (Mega record 3, for example, stores only `1475`).

`TwoCbhLibrary` builds the normal `IndexedGame` list and is wired into the
application's Open dialog and `open_library` dispatch. It refuses to index more
than `MAX_INDEXED_GAMES = 2_000_000` slots, because the current in-memory game
list is not viable for the 11.7-million-game Mega database. The full Mega file
remains readable one record at a time through `TwoCbhFile::load_game`.

### Move records

A `.2cbg` record starts with a magic, a content length `A`, a trailing length
`B`, and eight unknown bytes. Content begins at `+24` with a little-endian kind
word:

- kind `1`: standard chess, followed by the `0xfffc` start marker and tokens;
- kind `2`: Chess960, followed by `0xfffb`, the Scharnagl start id, `0xfffc`,
  and tokens.

`A` counts the kind word and the tokens *before* the final `0xffff`; that final
terminator sits immediately after the content area, followed by `B` zero bytes
and a 64-bit trailer equal to `34 + A + B`. `0xffff` ends a line, `0xfffd`
marks the move that the following variation replaces, and `0x0000` is a null
move. Variation blocks follow the mainline terminator, deepest last; they are
replayed in reverse marker order so the stored order becomes PGN order.

The token table has 65,536 entries of eight little-endian 16-bit fields:
`[from, to, piece, aux, b3, b2, special6, special7]`. Brett generates entries
1–49,196 independently from move geometry:

- ordinary king, queen, knight, bishop, and rook moves for both colors;
- pawn pushes, captures, promotions, and en passant;
- orthodox castling tokens 45,353–45,356;
- Chess960 castling tokens 45,357–49,196, two per start id and color.

Squares are stored as `file * 8 + rank` and converted at decode time. Castling
is recognized by `special6 & 7 == 5`; the rook comes from the position's
castling rights, not from the king destination, because a Chess960 `b3` token
can otherwise name the wrong rook. Promotions use `special6`, and en passant is
identified by `aux == 0`, `b2 == b3`, a nonzero `b3`, and a nonzero `special7`.

### Validation

The 2CBH reader was checked against local databases that are not committed:

- **MegaBase2026 Update 01:** 10,738 games, zero mainline move mismatches and
  zero header mismatches against the complete PGN export.
- **`9601.2cbh`:** 21/21 mainlines match the GMT-M Chess960 PGN, covering seven
  distinct Scharnagl start positions. The generated start id matches every
  game; three FENs differ only in castling spelling (`HAha` versus `KQkq`)
  while the positions and moves match.
- **Nested-variation game:** the mainline and the complete 16-variation tree
  match the PGN tree, ignoring variation order; all 112 tokens are consumed.
- **Mega Database 2026:** record 3 loads directly without indexing the file,
  proving targeted access to the 11.7-million-record container.
- **Synthetic suite:** `src/twocbh/tests.rs` generates byte-level fixtures for
  standard moves, promotions, en passant, Chess960, nested variations, headers,
  deleted/text records, and malformed records.

The env-gated `tests/twocbh_local.rs` reproduces the real-file checks when the
local paths are supplied. No database content is committed.

### Writing 2CBH

`twocbh::write_database` and `write_database_with_options` in
`src/twocbh/writer.rs` write a fresh database at a `.2cbh` path or bare stem.
An export produces the four core files:

| File | Contents |
| --- | --- |
| `.2cbh` | 192-byte header records |
| `.2cbg` | Encoded move records |
| `.2cba` | Framed annotation records (empty annotation content) |
| `.2lid` | Inline player, event, and source names |

The writer streams to temporary files beside the target, replaces the existing
companion set atomically with rollback, and removes stale derived `.2lcd` and
`.2lgd` indexes so ChessBase rebuilds them. The writer emits only 2CBH
generation `0x26`. It preserves moves, variations, headers, names, standard
chess and Chess960 starts, castling, promotions, en passant, null moves, and
the deleted flag. Comments, NAGs, clocks, and evaluations are
not written, so the library refuses annotation loss unless
`TwoCbhWriteOptions { allow_annotation_loss: true }` is passed.

Generate a database for manual ChessBase validation with:

```sh
cargo run --example twocbh_sample -- /tmp/brett-2cbh-sample.2cbh
```

The sample writes six games (metadata, variations, promotion, Chess960 on the
standard board, a deleted game, and a null move), reopens the result, asserts
six games and the four core files, and prints the absolute path plus a short
ChessBase checklist.

### Limits

- Only 2CBH generation `0x26` is read and written. The older `0x22` generation is not
  verified.
- `.2cba` annotation items are not decoded; comments and NAGs are not
  fabricated. The annotation offset is recorded so the framing can be added
  without changing the game model.
- Writing 2CBH is implemented in `src/twocbh/writer.rs`; annotation writing is
  deferred.
- The library index is capped at 2,000,000 slots; targeted `TwoCbhFile` access
  is the path for larger databases.
- Unknown `.2cbg` record kinds and damaged streams are reported as game errors
  or load errors without preventing the rest of the database from opening.

## Exporting and writing (`src/cbh/writer.rs`)

**File → Save database as… → Classic CBH** (and `cbh::write_database`) writes a
complete classic multi-file database from the ordered session:

- streams `.cbh` (46-byte header records), `.cbg` (move streams in mode 0, 4,
  5, or 10), and `.cba` (annotations in depth-first stored move order) to
  temporary files;
- interns `.cbp` (players), `.cbt` (tournaments and sites), `.cbc`
  (annotators), and `.cbs` (sources) with zero-based entity IDs;
- writes a `.cbj` 64-bit wide-offset companion when `.cbg` or `.cba` reaches
  4 GiB (or when `CbhWriteOptions::force_wide` is set);
- encodes 1-byte `PieceLists` compact move codes and `235` 2-byte square
  escapes, promotions, en passant, null moves, recursive variations, custom
  start positions, and Chess960;
- preserves text comments, NAGs (`0x03`), evaluations (`0x21`, from both
  `Node::eval` and `[%eval cp,depth]`), `[%emt]` (`0x07`), `[%csl]` (`0x04`),
  `[%cal]` (`0x05`), and `[%mdl]` (`0x22`);
- atomically replaces existing companion files with rollback and removes stale
  derived `.cbb`/`.cbgi` boosters so ChessBase does not read mismatched index
  accelerators;
- round-trips cleanly through both `CbhLibrary` and the independent
  `cbvault-format` reader (`Headers`, `Entities`, `Batch`, `Annotations`, and
  `Wide`).

## Limits

- Exporting writes a fresh classic database via Save As; in-place record
  patching or compacting of an open source database is intentionally avoided.
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
