# 2CBH container-first writer

Date: 2026-10-31
Status: design for approval

## Intent

Brett can already read ChessBase's current multi-file 2CBH format, including
moves, variations, names, Chess960 starts, and the full 11.7-million-game Mega
archive through the low-level reader. It cannot write 2CBH. This project adds a
native writer that produces a database ChessBase can open, so a Brett user can
export a session to the current ChessBase format and test the result directly in
ChessBase.

The first release is deliberately **container-first**: it writes the complete
container, names, headers, moves, variations, and Chess960 starts, but writes
empty annotation records. Comments, NAGs, clocks, evaluations, and drawing
tokens are a second pass after ChessBase accepts the container. This isolates the
highest-risk compatibility question from the still-undecoded `.2cba`
annotation-item layout.

The writer is independent Rust code. It does not link to or copy ChessBase code,
binaries, tables, or token-table dumps. The generated token table already in
`src/twocbh/tokens.rs` remains the only move-token source.

## Goals

- Write a valid four-file 2CBH set: `.2cbh`, `.2cbg`, `.2cba`, and `.2lid`.
- Preserve the metadata Brett already reads from 2CBH: event, site, date, round,
  result, players, Elo, FIDE IDs, source title/publisher, game ID, beauty value,
  deleted state, and Chess960 start.
- Preserve the full move tree: mainline, nested variations, sibling variations,
  promotions, en passant, castling, null moves, and Chess960 castling.
- Stream games instead of holding the whole database in memory.
- Replace an existing database atomically and remove stale derived
  `.2lcd`/`.2lgd` indexes so ChessBase does not use mismatched caches.
- Make 2CBH a writable format in Brett's Save database flow.
- Produce a small sample database for manual ChessBase validation.
- Keep the reader's existing public API and local real-file tests working.

## Non-goals

- Decoding or writing non-empty `.2cba` annotation records in this pass.
- Preserving comments, NAGs, clocks, evaluations, or drawing tokens in this pass.
- Writing arbitrary FEN start positions. 2CBH supports standard chess and
  Chess960 start positions only; anything else is rejected.
- Writing text records, database icons, `.ini` files, or ChessBase's derived
  `.2lcd`/`.2lgd` indexes.
- Changing the large-database indexing caps or the in-memory game-list model.
- Desktop Open With integration or packaging. Those are separate projects.
- Reproducing ChessBase's proprietary unknown header fields exactly. The design
  writes the structural fields observed in real files and leaves opaque counters
  at safe defaults until manual validation says otherwise.

## Constraints

- MIT-only code and data. No ChessBase code, binaries, tables, or token-table
  dumps may be added.
- 2CBH sources stay read-only. The app must not overwrite the open source.
- Preserve the ICCF notice and existing provenance documents.
- The writer must reject a game rather than silently write a corrupted move tree
  or a custom start position.
- The first ChessBase validation is manual on the user's machine. Automated tests
  can prove round-trip consistency, not ChessBase acceptance.
- Existing `TwoCbhFile` and `TwoCbhLibrary` readers must be able to read every
  file the writer produces.

## Approaches considered

### A. Container first, annotations second (chosen)

Write headers, names, moves, variations, Chess960, and empty `.2cba` records.
Hand the result to the user for a ChessBase test. Add comments/NAGs/evals after
the container is accepted.

Trade-off: annotation data is not preserved yet. The first build answers the
compatibility question early and keeps the annotation reverse-engineering work
from blocking a usable writer.

### B. Full annotations in one pass

Decode `.2cba` comment/NAG/eval items and write everything before the first
ChessBase test.

Trade-off: more complete on day one, but slower and riskier. A basic header or
name-block mismatch could stay hidden behind annotation work, and the `.2cba`
item layout is still only partly understood.

### C. Headers and moves only, permanently

Ship a minimal writer that never preserves annotations.

Trade-off: smallest implementation, but visibly incomplete and a poor foundation
for the second pass. Rejected.

## Format contract

The layout below is the contract the writer and tests share. All multi-byte
integers are little-endian unless marked otherwise.

### File set

| File | Purpose |
| --- | --- |
| `.2cbh` | 192-byte record 0 plus one 192-byte header per game |
| `.2cbg` | 12-byte file header plus one framed move record per game |
| `.2cba` | 12-byte file header plus one framed annotation record per game |
| `.2lid` | 0xB8-byte header plus fixed-stride name blocks |

A database is addressed by its bare stem or any of those four extensions. The
writer normalizes the target to `.2cbh` and writes the other three beside it.

### `.2cbh`

Record 0 is 192 bytes:

| Offset | Size | Value |
| --- | --- | --- |
| `0x00` | 8 | zero |
| `0x08` | 1 | generation `0x26` |
| `0x09` | 1 | zero |
| `0x0a` | 2 | record size `192` |
| `0x0c` | 2 | version `0x0500` |
| `0x0e` | 2 | zero |
| `0x10` | 4 | record count including record 0 |
| `0x14` | 172 | zero |

Each game record uses these fields. Unlisted bytes are zero except the
`0x40..0x50` run, which is `0xff`.

| Offset | Size | Meaning |
| --- | --- | --- |
| `0x00` | 1 | flags: `0x01` normal, `0x81` deleted |
| `0x01` | 3 | `[0, 1, 1]` |
| `0x08` | 8 | `.2cbg` record offset |
| `0x10` | 8 | `.2cba` record offset |
| `0x18` | 4 | white player id |
| `0x20` | 4 | black player id |
| `0x28` | 4 | event id |
| `0x30` | 4 | observed normal-game value `2` |
| `0x38` | 4 | source id |
| `0x40` | 16 | `0xff` |
| `0x58` | 4 | `(round << 16) | result_code` |
| `0x5c` | 4 | subround |
| `0x60` | 4 | white Elo |
| `0x70` | 4 | black Elo |
| `0x80` | 4 | Chess960 start `0xFC40 + id`, or zero for standard |
| `0x98` | 8 | game id |
| `0xb0` | 8 | beauty value |
| `0xbc` | 4 | packed date |

Result codes are `0` for `0-1`, `1` for `1/2-1/2`, `2` for `1-0`, and `3` for
`*` or an unknown result. The date packing is
`(year << 9) | (month << 5) | day`, with missing components zero.

### `.2cbg`

The 12-byte file header is:

| Offset | Size | Value |
| --- | --- | --- |
| `0x00` | 8 | final file length |
| `0x08` | 2 | `12` |
| `0x0a` | 2 | `0x0500` |

Each record is:

| Part | Size | Contents |
| --- | --- | --- |
| magic | 8 | `88 77 66 55 44 33 22 11` |
| `A` | 4 | content length: kind word plus move tokens, excluding the framing terminator |
| `B` | 4 | zero padding after the content terminator |
| opaque key | 8 | deterministic non-zero 64-bit record key |
| content | `A` | `u16` kind followed by `u16` move tokens |
| terminator | 2 | `ff ff` |
| padding | `B` | zero |
| trailer | 8 | total record size (`34 + A + B`) |

`B` is chosen so the next record starts on an 8-byte boundary. The observed
non-empty formula is `B = 98 + alignment`; the empty-move record uses
`B = 194 + alignment`. The writer computes the alignment from the current file
offset, not from a hard-coded first-record assumption.

The kind word is `1` for standard chess and `2` for Chess960. Standard content
starts with `ff fc`. Chess960 content starts with `ff fb`, the Scharnagl start
id, then `ff fc`. Move tokens follow.

### Move tokens

Tokens are little-endian `u16`. The existing generated table in
`src/twocbh/tokens.rs` supplies every ordinary move and Chess960 castling
entry. The writer adds an inverse lookup rather than guessing token numbers.

- Ordinary move: choose the table entry whose geometry matches the move.
- Non-pawn and quiet pawn moves: use the zero-aux entry.
- Pawn capture: use the entry whose aux names the captured role and the moving
  side's color bit.
- En passant: use the dedicated entry whose special fields name the captured
  pawn square.
- Promotion: use the entry whose special fields name the promoted role and the
  moving side's color bit. For a promotion capture, use the aux that names the
  captured role when one exists. The generated table has no captured-pawn
  promotion entry, so a pawn capture uses the queen-capture aux as the
  canonical fallback; the reader ignores aux for promotions, and manual
  validation confirms whether ChessBase accepts that fallback.
- Orthodox castling: use the four fixed table entries after the ordinary moves.
- Chess960 castling: index the generated block directly:
  `45357 + start_id * 4 + color_offset + side_offset`, where white is `0`,
  black is `2`, queenside is `0`, and kingside is `1`.
- Null move: token `0`.

Line markers are `ff ff` (end line), `ff fd` (the preceding move has a
variation), and `ff fc`/`ff fb` (start markers). The record framing supplies the
final `ff ff`; the writer collects tokens with line terminators, then removes the
last terminator before writing the record.

Variation ordering must exactly invert the reader. A **branch point** is a move
whose parent has more than one child: the move is the mainline move being
replaced by its siblings. The `ff fd` marker belongs immediately after that
move, because the reader records the most recent move as the replaced move and
attaches the following variation line to that move's parent.

The writer uses one line encoder with a `skip_first_branch` flag:

1. Write the chosen first child, then follow its first-child chain.
2. After each move, if it is a branch point and it is not the skipped first move
   of a variation line, emit one `ff fd` for every sibling and record the branch
   point in line order.
3. End the line with `ff ff`.
4. Process recorded branch points in reverse line order. For each branch point,
   emit one variation line per sibling in reverse child order. Each such line
   starts with that sibling, uses the branch point's parent as its parent, and
   sets `skip_first_branch` so the parent's own markers are not duplicated.
5. The mainline call sets `skip_first_branch = false`, so root alternatives such
   as `1. e4 (1. d4)` are preserved: the marker follows the first mainline move
   and the variation starts at the root.

The final `ff ff` is removed before framing, because the `.2cbg` record
terminator supplies the last line terminator. This order is required because the
reader collects branch markers in line order and replays them in reverse. A
round-trip test must cover root alternatives, multiple branch points on one
line, and multiple variations at one node.

### `.2cba`

The 12-byte file header is:

| Offset | Size | Value |
| --- | --- | --- |
| `0x00` | 8 | final file length |
| `0x08` | 2 | `12` |
| `0x0a` | 2 | `0` |

The first-pass writer emits one empty annotation record per game. The record is:

| Part | Size | Contents |
| --- | --- | --- |
| magic | 8 | `88 77 66 55 44 33 22 11` |
| `A` | 4 | `4` |
| `B` | 4 | zero padding |
| opaque key | 8 | `00 00 00 00 7f ff ff ff` |
| content | 4 | `00 20 ff ff` |
| terminator | 2 | `ff 7f` |
| padding | `B` | zero |
| trailer | 8 | total record size (`34 + A + B`) |

The empty record uses the same 8-byte alignment rule as `.2cbg`, with base
padding `194`. Every `.2cbh` annotation offset points at its game's empty
record. No game points at offset zero in this pass.

### `.2lid`

The header is `0xB8` bytes. The writer fills the structural fields observed in
real files and leaves unknown counters at zero:

| Offset | Size | Value |
| --- | --- | --- |
| `0x00` | 4 | `0xB8` big-endian |
| `0x04` | 4 | `6` big-endian |
| `0x08` | 4 | `0x400` big-endian |
| `0x0c` | 4 | zero |
| `0x10` | 4 | allocated block count plus one, big-endian |
| `0x14` | 8 | `0xffffffff` twice |
| `0x1c` | 4 | `0x460` big-endian |
| `0x24` | 4 | interned event count, big-endian |
| `0x38` | 4 | `2` big-endian |
| `0x44` | 4 | `0x400` big-endian |
| `0x74` | 4 | `1` big-endian |
| `0x80` | 4 | `0xffffffff` big-endian |
| other | — | zero |

The observed `0x10` value is the allocated block count plus one (the name-file
header block). The observed `0x24` value matches the number of events. The
observed `0x30`, `0x58`, `0x60`, and `0x6c` values do not match local player,
event, or source counts, so their meaning is not assumed; this writer leaves
them zero.

Blocks have stride `0x108A` and start at `0xB8`. Each block holds three
independent, length-prefixed name records:

| Offset | Record |
| --- | --- |
| `+0x000` | player |
| `+0x400` | event |
| `+0x860` | source |

Player record: `u32 length`, `u32 last_len`, last name, `u32 first_len`, first
name, then a 20-byte tail. The tail is eight zero bytes, `u32 1`, `u32 8`, and
the FIDE id (`u32::MAX` when absent). The total length is
`32 + last_len + first_len`.

Event record: `u32 length`, `u32 place_len`, place, `u32 title_len`, title, then
104 reserved bytes. The total length is `12 + place_len + title_len + 104`.
The existing synthetic fixture uses a shorter event total; it is a test-only
simplification and will be corrected to this observed length. The reader maps
place to `Site` and title to `Event`.

Source record: `u32 length`, `u32 title_len`, title, `u32 publisher_len`,
publisher, then eight reserved bytes. The total length is
`20 + title_len + publisher_len`. The reader maps title to `SourceTitle` and
publisher to `Source`.

Names are UTF-8, matching the real Mega archive's non-ASCII names and the
reader's first-choice decoder. A name that would overflow its sub-record space
is an error, not a truncation: player strings share 0x400 bytes, event strings
share 0x460 bytes, and source strings share 0x82A bytes.

Players are interned on `(last, first, fide)`, so two same-name players with
different FIDE ids get separate blocks and both ids survive. Events are interned
on `(place, title)`, sources on `(title, publisher)`. A block is created for the
highest id used by any of the three tables; unused sub-records stay zero. The
file is extended to `0xB8 + block_count * 0x108A` at finish. An empty export
still writes one blank block so the name file has a valid header and stride.

## Writer design

### Public API

Add `src/twocbh/writer.rs` and expose it from `src/twocbh/mod.rs`:

```rust
pub const CORE_EXTENSIONS: [&str; 4] = ["2cbh", "2cbg", "2cba", "2lid"];
pub const DERIVED_EXTENSIONS: [&str; 2] = ["2lcd", "2lgd"];
pub const ALL_EXTENSIONS: [&str; 6] = [
    "2cbh", "2cbg", "2cba", "2lid", "2lcd", "2lgd",
];

pub struct TwoCbhWriteReport {
    pub games: usize,
    pub bytes: u64,
}

pub struct TwoCbhWriteOptions {
    pub allow_annotation_loss: bool,
}

pub fn write_database<I>(path: &Path, games: I) -> Result<TwoCbhWriteReport>
where
    I: IntoIterator<Item = Result<Game>>;

pub fn write_database_with_options<I>(
    path: &Path,
    options: TwoCbhWriteOptions,
    games: I,
) -> Result<TwoCbhWriteReport>
where
    I: IntoIterator<Item = Result<Game>>;

pub fn existing_companion_files(path: &Path) -> Vec<PathBuf>;
```

`write_database` uses `allow_annotation_loss: false`. The app uses the options
form after a confirmation when any game has annotations.

The target must end in `.2cbh` or be a bare stem. A path ending in another
2CBH extension is normalized; a missing parent directory is an error.

### Streaming and finish

`TwoCbhWriter` creates four temporary files beside the target with a unique
suffix and a `TempGuard`. It writes record 0 and the three file headers first,
then streams games:

1. Resolve the game's move tree and encode its `.2cbg` content.
2. Write the framed `.2cbg` record at the current offset.
3. Write the empty `.2cba` record at the current offset.
4. Intern players, event, and source; write their `.2lid` sub-records at the
   fixed block offsets.
5. Write the `.2cbh` game record with both offsets and all metadata.
6. Update running counts and offsets.

At finish:

1. Flush all writers.
2. Patch the `.2cbh` record count.
3. Patch the `.2cbg` and `.2cba` file lengths.
4. Extend and patch the `.2lid` header.
5. Install the four files with the same backup/rollback pattern as the classic
   CBH writer.
6. Back up and remove existing `.2lcd`/`.2lgd` files on success so ChessBase
   rebuilds them. Leave unrelated `.ini` files alone.

The report counts only the four core files in `bytes`.

### Move encoding and validation

The writer walks `Game::nodes` rather than trusting PGN text. Each `Ply` is
resolved with the existing strategy: try UCI, then SAN. A move that cannot be
resolved or played in the parent position is an error naming the game and ply.
`Move::Put` is rejected.

Start positions:

- Standard: the root must equal `Chess::default()`. Otherwise reject with a
  message that 2CBH cannot store arbitrary FEN starts.
- Chess960: the root must equal one of the 960 generated start positions with
  Chess960 castling. Find the Scharnagl id by comparing the generated position
  and reject if no id matches. `Variant` containing `960` or `Fischer`, or a
  root using `CastlingMode::Chess960`, selects Chess960.
- A `Variant` naming any other variant is rejected.
- A standard board with Chess960 castling is written as kind `2` with the
  matching id (for example id `518`), not as standard chess.

### Metadata mapping

| 2CBH field | Source |
| --- | --- |
| white/black | `White`/`Black`, split at the first comma into last/first |
| FIDE | `WhiteFideId`/`BlackFideId` |
| event | `Event` |
| site | `Site` |
| source title | `SourceTitle` |
| source publisher | `Source` |
| date | `Date` |
| round/subround | `Round`, with `Board` as the fallback subround |
| result | `Result` |
| Elo | `WhiteElo`/`BlackElo` |
| game id | `GameId` |
| beauty | `Beauty` |
| deleted | `Deleted` or a reader error containing the deleted marker |

Missing or `?` strings become empty name fields. A blank player/event/source is
still interned so every game has a valid id. Elo, game id, and beauty are zero
when absent or unparsable. ECO is not stored in this pass.

### Annotation-loss guard

Before writing a game, check every node for a non-empty comment, NAGs, clock, or
evaluation. If any exists and `allow_annotation_loss` is false, stop with an
error naming the game and the first unsupported feature. The app will show a
confirmation that the first 2CBH writer preserves moves and metadata only, then
retry with `allow_annotation_loss: true`.

This guard is intentional: the chosen container-first scope accepts annotation
loss, but it must be visible, not silent.

### App integration

`DatabaseFormat` changes:

- Add `TwoCbh` to `WRITABLE` (now four formats).
- Label it `ChessBase 2CBH database (.2cbh)`.
- Remove the read-only wording from the save dialog and the disabled radio.
- `is_writable` becomes always true; keep the helper if other code still reads it.
- `start_format_save` dispatches `TwoCbh` to a new `start_two_cbh_save`.

Add `PendingTwoCbhExport` and a confirmation dialog mirroring the classic CBH
flow. The dialog lists the four core files and the stale derived indexes that
will be replaced. `start_two_cbh_save`:

- normalizes the extension to `.2cbh`;
- rejects read-only export targets;
- refuses to overwrite the open source (all six companion extensions);
- collects existing companion files;
- writes immediately when none exist, otherwise asks for confirmation;
- calls `crate::twocbh::write_database` with the session's games and the
  annotation-loss option;
- updates the status line and calls `finish_saved_database`.

If the session has annotations, the app asks once for annotation-loss
confirmation before writing. The first-pass UI must not claim comments or NAGs
are preserved.

## Testing

### Automated

Add module tests in `src/twocbh/tests.rs` (or a writer test submodule) that use
only synthetic bytes and generated games:

- Standard game with headers, Elo, FIDE, game id, beauty, source, deleted flag.
- Mainline plus root alternatives, multiple sibling variations, and multiple
  branch points on one line; compare the full tree after reopening, not just the
  mainline.
- Promotion, en passant, castling, and null move.
- Chess960 start id, Chess960 castling, and the standard-board/id-518 case.
- Empty game and empty database.
- Byte-level checks for `.2cbh` record 0, record fields, `.2cbg` framing and
  alignment, `.2cba` empty framing, and `.2lid` block lengths.
- Reject custom FEN, unsupported variant, unresolvable move, and annotation loss
  by default.
- Replacement removes stale `.2lcd`/`.2lgd` and preserves unrelated files.
- Round-trip through `TwoCbhLibrary` for every synthetic fixture.

Add an env-gated test in `tests/twocbh_local.rs`: when a local real database is
available, load a small sample, write it with annotation loss allowed, reopen it,
and compare headers and full trees. This is development-only and does not add
real data to the repository.

### Manual ChessBase validation

Generate a sample set containing:

1. a normal standard game with headers and no annotations;
2. a game with nested and sibling variations;
3. a promotion and an en passant game;
4. a Chess960 game;
5. a deleted game;
6. a game with a null move, if ChessBase accepts it.

Checklist for the user:

- ChessBase opens the `.2cbh` without a repair prompt.
- The game list shows names, event, site, date, round, result, Elo, FIDE ids,
  game id, and beauty values.
- Games open with the correct mainline and variation tree.
- Castling, promotions, en passant, and Chess960 castling replay correctly.
- The annotation pane is empty but does not crash.
- ChessBase creates `.2lcd`/`.2lgd` indexes without errors.
- Saving or closing in ChessBase does not corrupt the database.

If ChessBase rejects a file, the failing file and step are recorded in the
design's follow-up notes before changing the layout.

## Risks and unknowns

- The 8-byte opaque key in `.2cbg` records is not decoded. The writer uses a
  deterministic non-zero value. Manual validation decides whether ChessBase
  requires a specific checksum.
- The `.2lid` header contains counters whose meaning is not fully decoded. The
  writer fills the structural fields and leaves unknown counters zero. Manual
  validation decides whether more fields are required.
- ChessBase may require a specific `.2cba` empty-record key or may accept a
  shared empty record. This pass uses the observed per-game empty record.
- Annotation loss is a known limitation and is guarded by an explicit option and
  app confirmation.
- The writer streams core files but interns names in memory. This is appropriate
  for session-sized exports; the separate large-database project will revisit
  memory bounds if 2CBH export of millions of games becomes a requirement.

## Follow-up after container acceptance

1. Decode `.2cba` comment, NAG, clock, evaluation, and drawing-token items.
2. Add annotation encoding and round-trip tests against real annotated games.
3. Remove the annotation-loss guard or make it a warning.
4. Revisit the `.2lid` header and opaque record keys with the manual findings.
5. Add the 2CBH writer to the large-database export path if needed.

## Builder calls

These are implementation choices made here, not requirements the user already
settled:

- Per-game empty `.2cba` records, matching observed real files, instead of a
  shared empty record.
- Deterministic non-zero `.2cbg` opaque keys, with manual validation as the gate.
- `.2lid` unknown counters zeroed, structural fields filled; `0x10` is block
  count plus one and `0x24` is the event count.
- Players interned on `(last, first, fide)` so FIDE ids survive duplicate names.
- `.2lid` names use UTF-8; overlong names are rejected instead of truncated.
- Captured-pawn promotion uses the queen-capture aux fallback because the
  generated table has no captured-pawn promotion entry.
- Annotation loss blocked by default at the library level and confirmed in the
  app.
- Stale `.2lcd`/`.2lgd` files are backed up and removed; `.ini` files are left
  alone.
- ECO is not written in this pass.
