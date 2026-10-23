# Third-party notices

## cburnett chess piece set

The chess piece artwork in `assets/pieces/` is the **cburnett** set.

- **Author:** Colin M. L. Burnett
- **Licence:** Creative Commons Attribution-ShareAlike 3.0 Unported
  (CC BY-SA 3.0) — <https://creativecommons.org/licenses/by-sa/3.0/>
- **Source:** <https://github.com/lichess-org/lila/tree/master/public/piece/cburnett>
- **Modifications:** rasterised from the SVG originals to 160 × 160 transparent
  PNGs with `rsvg-convert`.

The piece artwork remains under CC BY-SA 3.0 and is not covered by the
project's MIT licence. See `assets/pieces/README.md` for details.

## DejaVu Sans font

`assets/fonts/DejaVuSans.ttf` is embedded as a fallback font for chess
figurines and annotation symbols.

- **Source:** <https://github.com/dejavu-fonts/dejavu-fonts>
- **Licence:** Bitstream Vera Fonts Copyright, with DejaVu changes in the public
  domain. Glyphs imported from Arev fonts are Copyright (c) 2006 Tavmjong Bah.
  The complete licence text is in `assets/fonts/LICENSE`.
- **Modifications:** none. The upstream TrueType file is copied verbatim and
  used after the primary UI fonts.

The font is not covered by the project's MIT licence; redistribute it only with
the accompanying licence text. See `assets/fonts/README.md`.

## SCID format reference

The native SCID reader in `src/scid.rs` is an independent implementation
written from the published SCID on-disk format. The GPL SCID source tree and
documentation were used as a format reference only.

- **SCID project:** <https://scid.sourceforge.net/>
- **Relationship:** no SCID source code is linked, copied, or distributed in
  Brett. Brett does not invoke `scidpgn` or any other SCID executable at
  runtime.
- **Test fixture:** `fixtures/scid/opera.*` was generated from the
  public-domain Opera Game in `fixtures/opera.pgn` using the `pgnscid` build
  tool. The generated database contains only that public-domain game and
  Brett-authored annotation markers. The SCID tools themselves are not
  redistributed.

The SCID trademark and project names belong to their respective owners. This
notice does not imply endorsement by the SCID project.

## Classic ChessBase CBH format reference

The native classic CBH reader in `src/cbh/` was written from independent
format references. No ChessBase installation, Wine runtime, or converter
executable is used.

- **cbvault** by Itshak (MIT): <https://github.com/itshak/cbvault>. Used as the
  independent validation oracle and as the source of the four move-encoding
  translation tables. Its documentation attributes parts of its format work
  to `asdfjkl/cbh2pgn` (MIT) and to `oschess-cb-bridge`.
- **cbh2pgn** by asdfjkl (MIT): <https://github.com/asdfjkl/cbh2pgn>. Used as
  an additional public format reference and cross-check.
- **oschess-cb-bridge** by asavis: <https://github.com/asavis/oschess-cb-bridge>.
  Its published format notes were consulted as format facts only; its code is
  AGPL and is not copied, linked, or distributed here.

Brett's decoder, annotation mapping, entity-name handling, and tests are an
independent implementation. The reference validation sample is a personal
database and is not redistributed; `src/cbh/tests.rs` generates a synthetic
classic CBH database at test time.

The move-encoding tables are distributed under cbvault's MIT licence:

```text
MIT License

Copyright (c) 2026 cbvault Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

The ChessBase name and format names belong to their respective owners. This
notice does not imply endorsement by ChessBase or by the referenced projects.

## Rust dependencies

Brett depends on third-party Rust crates (including `eframe`, `egui`,
`shakmaty`, `pgn-reader`, `image`, `memmap2`, `serde_json`, and `ureq`). Their
licences are listed in the crate metadata and in `Cargo.lock`; redistributed
binaries should include the licence texts required by those crates.
