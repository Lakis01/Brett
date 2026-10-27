# ICCF correspondence reference

This directory carries the compiled ICCF correspondence reference used by
Brett's Engine tab. Downloading it is optional: Brett runs without it, and all
other database formats work without it. It is included here because a local
correspondence reference makes preparation materially better, and because the
database is refreshed between releases rather than tied to a particular build.

The three files form one read-only SCID database:

| File | Size | Contents |
| --- | ---: | --- |
| `Correspondence_2350.si4` | 4.0 MB | SCID index |
| `Correspondence_2350.sn4` | 72 KB | Player, event, site, and round names |
| `Correspondence_2350.sg4` | 7.2 MB | Games, comments, and variations |

Snapshot facts:

- **87,927 games**, **6,187,801 mainline moves**;
- dates from **2021-01-01** through **2026-09-25**;
- every game has `Site "ICCF"`;
- `WhiteElo` and `BlackElo` both range from **2350 to 2724**;
- every game has both ratings and at least one move;
- no ECO or annotation records are present in this export.

The ICCF note accompanying the export says the January–March 2026 compilation
is missing and will be added when the year-end compilation is published. Later
2026 games are present, so this is a gap in the source compilation rather than
a database-wide cut-off.

## Licence and non-sale notice

> **The ICCF database is provided for personal use only and may not be sold or
> re-sold to any third party.**

This notice applies to this directory and to any copy or derived data made from
it. It does not change the MIT licence of Brett's source code. See
[`ICCF_NOTICE.md`](../../ICCF_NOTICE.md) and
[`docs/CORRESPONDENCE_DATABASE.md`](../../docs/CORRESPONDENCE_DATABASE.md).

Brett opens the triple read-only. It writes only derived position indexes under
its cache directory and never writes beside these files.

## Using the bundled copy

In a source checkout, Brett finds `databases/iccf/Correspondence_2350.si4`
automatically when no explicit correspondence path has been saved. If you
already keep a newer copy elsewhere, set **Settings → Correspondence database**
or `BRETT_CORRESPONDENCE`; those take precedence.

To use a copy outside the checkout, copy all three files together. SCID
databases are a triple, not a single file.

## Refreshing between releases

1. Obtain a newer ICCF archive export. Keep the source files unchanged.
2. Apply the inclusive filter `WhiteElo >= 2350` and `BlackElo >= 2350` on both
   sides. No upper bound is currently applied.
3. De-duplicate by player names, date, result, and move sequence.
4. Write a new `.si4`/`.sn4`/`.sg4` triple with the same base name, then replace
   the three files here together. Never leave an index from one snapshot beside
   games from another.
5. Open the new triple in Brett, verify that it decodes with zero errors, and
   let the position index rebuild. The cache key is the canonical `.si4` path
   and the combined file metadata, so replacing the triple invalidates the old
   cache.
6. Update the snapshot facts above and the counts in
   [`docs/CORRESPONDENCE_DATABASE.md`](../../docs/CORRESPONDENCE_DATABASE.md).

Do not refresh only one file of the triple, and do not rewrite a snapshot in
place while a Brett instance has it open.
