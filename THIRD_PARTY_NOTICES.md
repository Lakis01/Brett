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

## Rust dependencies

Brett depends on third-party Rust crates (including `eframe`, `egui`,
`shakmaty`, `pgn-reader`, `image`, `memmap2`, `serde_json`, and `ureq`). Their
licences are listed in the crate metadata and in `Cargo.lock`; redistributed
binaries should include the licence texts required by those crates.
