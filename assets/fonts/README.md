# Fonts

## DejaVu Sans

`DejaVuSans.ttf` is embedded as a fallback font for chess figurines and
annotation symbols that the default egui fonts do not contain.

- **Source:** <https://github.com/dejavu-fonts/dejavu-fonts>
- **Licence:** Bitstream Vera Fonts Copyright and DejaVu public-domain changes.
  The full licence text is in [LICENSE](LICENSE).
- **Modifications:** none; the upstream TrueType file is copied verbatim.

The font is used only as a fallback after the UI's primary fonts.

## Emoji fallback fonts (bundled by egui)

Brett enables the `egui/monochrome_emoji_fonts` feature so the gold
world-champion crown (👑, U+1F451) renders without relying on a system emoji
font. The fonts come from the `epaint_default_fonts` crate; no font files are
copied into this directory.

- **NotoEmoji-Regular.ttf** — Noto Emoji, SIL Open Font License 1.1.
  Source: <https://github.com/googlefonts/noto-emoji>
- **emoji-icon-font.ttf** — Copyright (c) 2014 John Slegers, MIT.
  Source: <https://github.com/jslegers/emoji-icon-font>

Both are used only after the primary UI fonts. See
[`THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md) for the full notices.
