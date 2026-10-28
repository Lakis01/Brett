# Provenance audit: SCID, CBH, CBV, and fixtures

**Audit date:** 2026-10-28
**Scope:** the native SCID reader/writer, the classic ChessBase CBH reader,
the `.cbv` container path, the photo-pack converter, and the committed format
fixtures.
**Status:** complete for the current tree. This is an engineering provenance
record, not legal advice.

## Why this exists

Brett is MIT-licensed and reads several proprietary or GPL-adjacent database
formats. The audit answers three questions for each component:

1. Was any third-party **code** copied, linked, or ported?
2. If code was reused, was it under a licence compatible with Brett's MIT
   distribution?
3. Are the committed **fixtures** redistributable, and do they contain only
   generated data or public-domain content?

Format facts — field layouts, byte offsets, table values, and observed
behaviour — are not copyrightable expression. Code that implements those facts
is. This audit separates the two deliberately.

## Component table

| Component | Path | Origin and method | Code reused | Licence status |
| --- | --- | --- | --- | --- |
| Native SCID reader/writer | `src/scid.rs` | Independent implementation from the published SCID on-disk format; GPL SCID source and docs used as a format reference only | None | Brett MIT. No SCID code linked, copied, or invoked at runtime. |
| SCID fixtures | `fixtures/scid/opera.{si4,sn4,sg4}` | Generated with the `pgnscid` build tool from the public-domain Opera Game in `fixtures/opera.pgn` | Generated data only | Committed fixture contains one public-domain game plus Brett-authored annotation markers. SCID tools are not redistributed. |
| Classic CBH reader | `src/cbh/` | Independent decoder, annotation mapping, entity-name handling, and tests; validated against the MIT `cbvault` reader | Four 256-byte move-encoding tables from MIT `cbvault` (`src/cbh/tables.rs`) | Brett MIT plus the MIT table notice in `THIRD_PARTY_NOTICES.md`. |
| Classic CBH fixtures | `src/cbh/tests.rs` | Synthetic database generated at test time; the personal validation sample lives under git-ignored `fixtures/local/` and is never redistributed | Generated data only | No third-party database content committed. |
| `.cbv` / `.cbz` container path | `src/cbv.rs` | Uses the `cbvault-format` 0.1.4 crate to materialise members, then opens the extracted classic database through `src/cbh/` | Links `cbvault-format` (MIT) | MIT. `cbvault-format`'s classic reader is a port of `cbformat` in `oschess-cb-bridge` at commit `ca9e8f8e`, which is MIT. |
| Photo-pack converter | `scripts/convert_spf.py` | Brett-authored BPF1/WebP converter; input is Scid's `Player_photos.zip` | None | Converter is Brett MIT. The photo data has no explicit redistribution licence and remains local-only. |
| Champion data and crowns | `src/champions.rs`, `docs/CHAMPIONS.md` | Hand-authored from public championship records | None | Brett MIT. |
| Clock parsing and trend engine | `src/domain.rs`, `src/recent.rs` | Brett-authored | None | Brett MIT. |

## The `cbvault-format` chain

This is the only place where third-party **code** is linked into Brett, so the
chain is recorded explicitly.

1. `cbvault-format` 0.1.4 is a direct dependency (`Cargo.toml`) and is MIT
   ("Copyright (c) 2026 cbvault Contributors").
2. Its README states that the classic-format reader is ported from `cbformat`
   in `oschess-cb-bridge` (MIT, "Copyright (c) 2026 the oschess-cb-bridge
   contributors"), and that ported files keep the upstream notice.
3. The ported files name upstream commit `ca9e8f8e`. The `LICENSE` at that
   commit is the MIT licence, and the upstream README states that releases up
   to and including v1.0.3 were published under MIT and keep it.
4. Current `oschess-cb-bridge` `main` is AGPL-3.0-only. Brett does not link,
   copy, or distribute that AGPL code; the MIT-era commit is the relevant
   source for the port.

**Correction recorded:** an earlier revision of `THIRD_PARTY_NOTICES.md` said
`oschess-cb-bridge` was AGPL and that none of its code was copied, linked, or
distributed. That was inaccurate because Brett links `cbvault-format`, which
contains an MIT port of `cbformat`. The notice now states the dependency, the
MIT-era commit, and the AGPL boundary explicitly.

## What was checked

- `git log --follow` for `src/scid.rs` and `src/cbh/` to identify the commits
  that introduced each reader and writer.
- Source headers and comments in `src/scid.rs`, `src/cbh/`, and `src/cbv.rs`
  for attribution, porting, or copied-code markers.
- `cargo tree` and `Cargo.lock` for every SCID/CBH-related crate in the
  dependency graph. The only match is `cbvault-format`.
- The `cbvault-format` source tree for upstream notices and the exact ported
  files.
- The upstream `oschess-cb-bridge` `LICENSE` at the ported commit and on
  `main`.
- Fixture READMEs and generation scripts for the committed SCID database.
- `THIRD_PARTY_NOTICES.md`, `docs/CBH.md`, and `docs/PHOTOS.md` for statements
  that no longer matched the dependency graph.

## Audit limitations

- This was a targeted manual audit, not a formal similarity or provenance
  scan. It did not run a code-similarity tool against SCID, Scidb, cbvault, or
  oschess-cb-bridge.
- Format facts may legitimately resemble other implementations. The audit
  distinguishes copied expression (tables, comments, code structure) from
  independently written code that implements the same published layout.
- The personal classic-CBH validation sample is not in the repository, so its
  contents cannot be audited from a clean checkout; only the synthetic test
  database is reproducible.
- Licence texts and upstream statements were read on 2026-10-28. A future
  upstream relicence does not retroactively change the MIT grant at
  `ca9e8f8e`, but new ports must be re-audited.
- This document does not replace legal review before a public release.

## Maintenance rules

- When adding a new format reader or writer, record its source, the licence of
  any reused code, and whether the fixture is generated or third-party.
- Do not copy code from an AGPL/GPL project into Brett. Use published format
  facts and independent tests instead, or isolate the dependency with a
  compatible licence and record it here.
- Keep personal databases under `fixtures/local/` and out of commits and
  release artefacts.
- Re-run this audit before the classic CBH writer lands and before any release
  that adds a new database format.
