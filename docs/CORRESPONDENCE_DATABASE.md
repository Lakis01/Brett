# Correspondence database provenance and rebuild

This note records the evidence recovered from the personal 2016 classic CBH
database and the questions that must be answered before an updated copy is
generated. It is analysis only; the original database remains untouched.

## Sample

```text
/home/gratis01/Downloads/CBH/Pimped Correspondence Database.*
```

The classic multi-file database was last written on 2016-07-02. Its title is
“Pimped Correspondence Database” and its `.ini` also names a “Pimped
Correspondence Opening Reference Base 2015”.

## What the games say

- **24,865 games**, **2,003,054 nodes**.
- Game dates run from **2009-02-15** through **2016-02-08**. The file date is
  later than the last game, so the archive snapshot is probably from early
  2016 rather than July 2016.
- Both `WhiteElo` and `BlackElo` are present in every game.
- The minimum on **both sides is exactly 2400**; the maximum is 2733. No game
  has a player below 2400. 183 games have at least one player at exactly 2400,
  including 2 games where both players are 2400. This rules out an exclusive
  `> 2400` filter and strongly supports inclusive `Elo >= 2400` for each side
  independently.
- There is no evidence of an upper cutoff. If one existed, it was at least
  2733 and therefore had no effect on this snapshot.
- 17,943 games have an `ECO` tag. The 6,922 without one are concentrated in
  the newer ICCF server games (2015 and early 2016).
- 298 games have no moves (`PlyCount "0"`), two games are Chess960, and 168
  games carry annotations.

## Source tags

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
of the ICCF archive, so the complete database is likely an aggregate of an
ICCF download plus one or more earlier correspondence-server collections. The
headers alone do not identify which download supplied those older games.

## Rebuild plan

1. Obtain a current ICCF archive export (the Games Archive download links are
   shown only to registered ICCF members) and, if they are still wanted, the
   current LSS/FICGS/Remote collections.
2. Confirm whether the rebuilt database should contain ICCF only or retain
   the older multi-server games.
3. Keep the separate inclusive filter `WhiteElo >= 2400` and
   `BlackElo >= 2400`; confirm whether an upper bound is desired.
4. Decide the end date and whether to keep unfinished/zero-move games.
5. De-duplicate by player names, date, result, and move sequence rather than
   by file position.
6. Write to a new output location. Never overwrite
   `/home/gratis01/Downloads/CBH/` or anything under
   `/home/gratis01/Chess Databases/`.

## Open questions

- Was the original source the ICCF Games Archive, or a mixed archive assembled
  elsewhere?
- Should the 2016 cut-off be replaced with “all games through today”, or is a
  particular date range wanted?
- Should LSS/FICGS/Remote be retained and, if so, which current archives
  should be used?
- Was there an upper Elo bound, a minimum number of games, or any event/date
  filter besides the two-sided rating filter?
