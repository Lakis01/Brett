# Printing a game with Typst

Brett exports the current game as print-ready [Typst](https://typst.app/)
source, with real board diagrams and an optional locally compiled PDF. Typst is
not bundled with Brett; the generated source is a normal text file that can be
kept, edited, or compiled later.

## Exporting

1. Open a game.
2. Right-click a move and choose **Add diagram after this move** to mark a
   position. A gold `▣` appears beside marked moves. The final position is
   included by default; the export dialog can turn it off.
3. Choose **File → Export current game as Typst…**.
4. Pick the `.typ` path and, if wanted, enable **Compile a PDF with Typst**.
5. Select **Export**.

The default path is `<export-folder>/<White - Black>.typ` (for example
`Documents/Carlsen, Magnus - Nakamura, Hikaru.typ`), avoiding naming conflicts
when exporting several games from one database. If player names are missing or
placeholders (`?`), Brett falls back to `game.typ`. Brett remembers the last
folder chosen for a print export across sessions, falling back to
`$HOME/Documents` (or `$HOME`) on first use. A compiled PDF is written beside the
`.typ` source, keeping their filenames aligned. Diagram markers are cleared
when another game is selected, so they always belong to the game currently on
screen.

## Output layout

Exporting `Mega-print.typ` writes:

```text
Mega-print.typ
Mega-print_diagrams/
  diagram-01.svg
  diagram-02.svg
  …
```

The Typst source references the diagram folder by a relative path, so keep the
source and its folder together when copying or archiving the export. Re-exporting
removes stale `diagram-*.svg` files from that folder only; other files are left
alone. The `.typ` source contains no `(Diagram)` placeholder — every marked
position produces an actual board.

## Printed layout

- A4 portrait, two-column body flow.
- A slim running header with the database/source name, owner, date, and page
  number.
- A compact game heading with color squares, stacked White and Black rows with
  their Elos, and event, site, round, and date. The ECO code appears by itself
  in the right-hand gutter, without an `ECO` prefix.
- Mainline moves in bold and wrapped as prose; variations are indented,
  bracketed, and numbered; comments and clock/evaluation notes are indented
  below the move they belong to.
- Board diagrams with file/rank coordinates and a side-to-move marker. A
  diagram is kept as one unbreakable block. If it does not fit at the bottom of
  a column, it moves to the next column or page while the surrounding text
  keeps its reading order.

## Compiling a PDF

Compile the generated file directly:

```sh
typst compile Mega-print.typ Mega-print.pdf
```

Or enable **Compile a PDF with Typst** in the export dialog. Brett runs the
compiler only when that option is enabled. It uses `BRETT_TYPST` when set,
otherwise `typst` from `PATH`:

```sh
BRETT_TYPST=/opt/typst/typst ./brett
```

If the compiler is missing or fails, the `.typ` source and diagrams are still
written, and the dialog reports the error. Brett never downloads Typst and
never needs a network connection for an export.

## Fonts

The generated source asks for DejaVu Serif and then Liberation Serif. If neither
is installed, Typst falls back to its default serif font. No font files are
embedded in the export, and the game content is plain Unicode text.

## What is preserved

The exporter walks Brett's parsed game tree, so the printed game matches the
moves, comments, and annotations on screen:

- all PGN headers, including player names, Elos, event, site, round, date, and
  result;
- comments, NAGs, clock and evaluation annotations;
- nested variations with their move numbers;
- custom start positions and Chess960 games, including a start-position note in
  the heading.

One game is exported per file. The current game is the source of truth, so
unsaved edits on screen are included.

## Attribution

The board renderer is Brett-authored MIT code. The piece artwork is the
**cburnett** set by Colin M. L. Burnett, licensed under
[CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/). The SVG
originals in `assets/pieces/` are embedded into every generated diagram, and
each SVG carries an attribution comment. The printed page does not show a
visible attribution line. See
[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) and
[`PROVENANCE.md`](PROVENANCE.md) before redistributing exported diagrams.

No ChessBase code, binary, print template, or token-table dump is used. The
layout is an independent implementation of the requested ChessBase-style
blueprint.

## LaTeX

The Typst target is implemented first. The backend-neutral document model in
`src/print/mod.rs` is shared, and the same SVG diagrams can feed a future LaTeX
renderer. LaTeX export is deferred; Typst is not an intermediate format for it.
