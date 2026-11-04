# Correspondence database provenance and refresh

This note records the active correspondence reference, the older 2016 sample,
and the procedure for refreshing the database. The sources are read-only; Brett
writes only derived position indexes under its cache directory.

## Licence and non-sale notice

> **The ICCF database is provided for personal use only and may not be sold or
> re-sold to any third party.**

A compiled snapshot is redistributed in this repository under
[`../databases/iccf/`](../databases/iccf/). Downloading or using it is optional,
but encouraged for correspondence preparation. The snapshot is refreshed
between releases. This notice applies to the database and to any copy or
derived data made from it; it does not change the MIT licence of Brett's source
code. See [../ICCF_NOTICE.md](../ICCF_NOTICE.md).

The database is not sold as part of Brett and must not be sold or re-sold to a
third party. Obtain newer snapshots from ICCF, or compile them from ICCF server
games under ICCF's terms, and comply with ICCF's terms.

## Active source

The redistributed snapshot is a SCID triple:

```text
databases/iccf/Correspondence_2350.si4
databases/iccf/Correspondence_2350.sn4
databases/iccf/Correspondence_2350.sg4
```

A local working copy may also live at:

```text
~/Downloads/Brett Resources/Correspondence Database/Correspondence_2350.si4
~/Downloads/Brett Resources/Correspondence Database/Correspondence_2350.sn4
~/Downloads/Brett Resources/Correspondence Database/Correspondence_2350.sg4
```

The native SCID reader opens the triple with zero decode errors:

- **87,927 games**, **5,421,540 mainline moves** in the bundled 70-ply
  snapshot (the untruncated ICCF export has **6,187,801**);
- every branch is capped at **70 plies**; deeper moves and their annotations
  are dropped when the snapshot is prepared;
- dates from **2021-01-01** through **2026-09-25**;
- every game has `Site "ICCF"`;
- `WhiteElo` and `BlackElo` both range from **2350 to 2724**;
- every game has both ratings and at least one move;
- no ECO or annotation records are present in this export.

The accompanying ICCF note says the January–March 2026 compilation is missing
and will be added when the year-end compilation is published. The export still
contains later 2026 games, so this is a gap in the source compilation rather
than a database-wide cut-off.

The active database is used by the Engine tab as a local correspondence
reference. Inclusion and the path are configured in **Settings → Correspondence
database**; the setting is stored in `preparation.json` beside the other
preparation preferences. `BRETT_CORRESPONDENCE` overrides the saved path. With
no saved choice, Brett looks for the bundled snapshot in a source checkout and
then for the conventional local copy. The first open builds a position index in
the background and caches it under `$XDG_CACHE_HOME/brett/index/`; later opens
memory-map the cached `.brettpos` file. At runtime the source triple is never
modified; snapshot maintenance rewrites it only before a release.

The database is optional: Brett and every other format work without it. The
snapshot is committed so a clone or repository download includes it; release
packages may omit it to keep the download small, in which case users can copy
the triple from the repository or obtain a newer ICCF export.

## Historical 2016 sample

```text
~/Downloads/CBH/Pimped Correspondence Database.*
```

The classic multi-file database was last written on 2016-07-02. Its title is
“Pimped Correspondence Database” and its `.ini` also names a “Pimped
Correspondence Opening Reference Base 2015”.

What the older sample contains:

- **24,865 games**, **2,003,054 nodes**.
- Game dates run from **2009-02-15** through **2016-02-08**. The file date is
  later than the last game, so the archive snapshot is probably from early
  2016 rather than July 2016.
- Both `WhiteElo` and `BlackElo` are present in every game.
- The minimum on **both sides is exactly 2400**; the maximum is 2733. No game
  has a player below 2400. 183 games have at least one player at exactly 2400,
  including 2 games where both players are 2400. This supports an inclusive
  `Elo >= 2400` filter for each side independently.
- There is no evidence of an upper cutoff. If one existed, it was at least
  2733 and therefore had no effect on this snapshot.
- 17,943 games have an `ECO` tag. The 6,922 without one are concentrated in the
  newer ICCF server games (2015 and early 2016).
- 298 games have no moves (`PlyCount "0"`), two games are Chess960, and 168
  games carry annotations.

### Historical source tags

| `Site` | Games |
| --- | ---: |
| `ICCF email` | 16,104 |
| `ICCF` | 6,921 |
| `Remote email` | 1,419 |
| `LSS email` | 292 |
| `FICGS email` | 127 |
| `ICCF corr`, `GER email` | 2 |

The ICCF records use real ICCF event names such as `EU/TC9/final`,
`RCCAC Gold`, `CCO16/F`, `CL/2015/...`, and `WC24/final`. Their header style
matches current ICCF PGN downloads (`Site "ICCF"`, player names, Elo, round,
date, result). The source is therefore almost certainly the official **ICCF
Games Archive** (or an export made while logged in to ICCF), not a generic PGN
website.

The non-ICCF records are older LSS, FICGS, and Remote games. They are not part
of the ICCF archive, so the older database is likely an aggregate of an ICCF
download plus one or more earlier correspondence-server collections.

## What changed

- Source access is no longer a blocker. The active `Correspondence_2350`
  triple is an ICCF-only export.
- The active rating floor is **2350** on both sides, replacing the older
  2400 floor.
- The active export has no ECO tags and no annotations. This is expected for
  the current ICCF export and does not affect the move statistics.
- The older mixed 2016 database is retained as provenance. It should not be
  silently merged into the active ICCF-only reference.

## Refresh procedure

1. Obtain a newer ICCF archive export. Keep the source files unchanged.
2. Apply the inclusive filter `WhiteElo >= 2350` and `BlackElo >= 2350` on
   both sides. No upper bound is currently applied.
3. Decide the end date and whether to keep unfinished/zero-move games. The
   current export has no zero-move games.
4. De-duplicate by player names, date, result, and move sequence rather than
   by file position.
5. Cap every branch at 70 plies. In a source checkout:
   `cargo run --release --example truncate_scid -- path/to/input.si4 70`.
   The command is idempotent and reports when the database is already within
   the limit.
6. Write a new `.si4`/`.sn4`/`.sg4` triple to a new path. Never overwrite the
   source or the historical 2016 database.
7. Replace all three files under `databases/iccf/` together, then open the new
   triple in Brett, verify that it decodes with zero errors, and let the
   position index rebuild. The cache key includes the canonical `.si4` path and
   the combined file metadata, so a replaced triple invalidates the old cache.
8. Update the snapshot facts in this file, `databases/iccf/README.md`, and the
   counts in the README if they are mentioned there.

Do not refresh only one file of the triple, and do not rewrite a snapshot in
place while a Brett instance has it open.

## Remaining questions

- How often should the ICCF reference be refreshed between releases?
- Should the historical mixed 2016 database be kept as a separate optional
  reference, or archived?
- Should future exports retain zero-move games?
