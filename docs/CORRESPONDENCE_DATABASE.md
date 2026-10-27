# Correspondence database provenance and refresh

This note records the active correspondence reference, the older 2016 sample,
and the procedure for refreshing the database. The sources are read-only; Brett
writes only derived position indexes under its cache directory.

## Licence and non-sale notice

> **The ICCF database is provided for personal use only and may not be sold or
> re-sold to any third party.**

Brett does not bundle or redistribute the ICCF database. Obtain it from ICCF,
or compile it from ICCF server games under ICCF's terms, and comply with ICCF's
terms. This notice applies to the database and to any copy or derived data made
from it; it does not change the MIT licence of Brett's source code. See
[../ICCF_NOTICE.md](../ICCF_NOTICE.md).

## Active source

```text
~/Downloads/Brett Resources/Correspondence Database/Correspondence_2350.si4
~/Downloads/Brett Resources/Correspondence Database/Correspondence_2350.sn4
~/Downloads/Brett Resources/Correspondence Database/Correspondence_2350.sg4
```

The native SCID reader opens the triple with zero decode errors:

- **99,606 games**, **6,924,478 mainline moves**;
- dates from **2015-12-30** through **2026-09-25**;
- every game has `Site "ICCF"`;
- `WhiteElo` and `BlackElo` both range from **2350 to 2724**;
- **27** games have no moves;
- no ECO or annotation records are present in this export.

The accompanying ICCF note says the January–March 2026 compilation is missing
and will be added when the year-end compilation is published. The export still
contains later 2026 games, so this is a gap in the source compilation rather
than a database-wide cut-off.

The active database is used by the Engine tab as a local correspondence
reference. Inclusion and the path are configured in **Settings → Correspondence
database**; the setting is stored in `preparation.json` beside the other
preparation preferences. `BRETT_CORRESPONDENCE` overrides the saved path. The
first open builds a position index in the background and caches it under
`$XDG_CACHE_HOME/brett/index/`; later opens memory-map the cached `.brettpos`
file. The source triple is never modified.

The database is not part of the repository. Keep it in the local Brett
Resources directory (or another local path) and never commit it; the repository
ships only the notice and the code that reads a user-supplied copy.

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
- 17,943 games have an `ECO` tag. The 6,922 without one are concentrated in
  the newer ICCF server games (2015 and early 2016).
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
   current export keeps 27 zero-move games.
4. De-duplicate by player names, date, result, and move sequence rather than
   by file position.
5. Write a new `.si4`/`.sn4`/`.sg4` triple to a new path under
   `~/Downloads/Brett Resources/Correspondence Database/`. Never overwrite the
   source or the historical 2016 database.
6. Open the new triple in Brett, verify that it decodes with zero errors, and
   let the position index rebuild. The cache key is the canonical `.si4` path,
   so a new filename gets a new cache entry automatically.

## Remaining questions

- How often should the ICCF reference be refreshed?
- Should the historical mixed 2016 database be kept as a separate optional
  reference, or archived?
- Should future exports retain zero-move games?
