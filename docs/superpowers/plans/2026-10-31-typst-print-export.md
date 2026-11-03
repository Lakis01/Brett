# Typst Print Export Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Export the current game as a print-ready Typst document with a two-column A4 layout, bold mainline prose, indented variations and comments, and real board diagrams at user-chosen plies.

**Architecture:** A backend-neutral `src/print/` document model is built from Brett's existing game tree. A Typst renderer consumes that model and writes a `.typ` source plus SVG board assets; the same model can later feed a LaTeX renderer. The app adds per-game diagram markers from the move context menu and a small export dialog. Compilation is opt-in and invokes a local `typst` binary only when requested.

**Tech Stack:** Rust 2024, existing `cozy-chess` facade and `domain::Game`, embedded cburnett SVG pieces, Typst 0.15 source generation, optional local `typst compile`.

**Spec:** `docs/BACKLOG.md` item 25, including the local ChessBase '26 printout blueprint.

## Global Constraints

- MIT-clean: no ChessBase code, tables, binaries, token dumps, or copied output.
- Typst first; LaTeX is a later target from the same document model.
- A4 portrait, two-column body flow, slim page header, compact game heading.
- Mainline moves bold and wrapped as prose; variations and comments indented below the move.
- `(Diagram)` is never emitted as a placeholder; the actual board is always emitted, and Typst moves it to the next column/page when it does not fit.
- Diagram positions are user-chosen; final position is included by default and can be disabled.
- Source databases are read-only; exports write only to the chosen output path and its diagram folder.
- A local compiler runs only when the user asks for a PDF; a missing compiler is reported clearly.
- Generated output must be deterministic for the same game, metadata, and diagram set.
- Preserve headers, comments, NAGs, clock/eval annotations, variations, custom starts, and Chess960.
- No network access.

## Review Focus

- Typst string escaping for names, comments, URLs, quotes, backslashes, and Unicode.
- Nested variation depth and variation order.
- Custom starts and Chess960 FENs and board diagrams.
- Diagram blocks that do not fit at the bottom of a column.
- Missing `typst` binary and failed compilation.
- Empty games and games with no moves.
- Deterministic asset names and ordering.
- Attribution for the CC BY-SA cburnett piece set.

---

### Task 1: Print document model and game-tree builder

**Files:**
- Create: `src/print/mod.rs`
- Create: `src/print/tests.rs`
- Modify: `src/lib.rs`
- Test: `src/print/tests.rs`

**Interfaces:**
- Produces:
  - `pub struct PrintMeta { pub source: String, pub owner: String, pub date: String }`
  - `pub struct PrintDocument { pub meta: PrintMeta, pub game: PrintGame, pub diagrams: Vec<Diagram> }`
  - `pub struct PrintGame { pub headers: Vec<(String, String)>, pub start_fen: Option<String>, pub variant: Option<String>, pub items: Vec<PrintItem>, pub result: String }`
  - `pub enum PrintItem { Move(PrintMove), Comment(String), Annotation(String), Variation(Vec<PrintItem>), Diagram(usize) }`
  - `pub struct PrintMove { pub text: String, pub bold: bool }`
  - `pub struct Diagram { pub node: usize, pub fen: String, pub caption: String, pub svg: String }`
  - `pub fn build_document(game: &Game, meta: PrintMeta, selected: &BTreeSet<usize>, include_final: bool) -> PrintDocument`

- [ ] **Step 1: Write the failing tests**

Cover a normal game, nested variations, custom starts, Chess960, NAGs, clock/eval annotations, selected diagram order, final-position default, and deterministic output.

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test --lib print::tests`
Expected: compile failure because `crate::print` does not exist.

- [ ] **Step 3: Implement the model and builder**

Walk the first child as the mainline; each later child becomes `PrintItem::Variation`. Emit move text with `number.` for White and `number...` only when a Black move starts a line or variation. Append NAG symbols from `domain::nag_symbol`. Add root comments first, then move comments and clock/eval annotations. Sort diagram nodes, deduplicate, and include the final mainline node when requested. Use `Fen::from_position(..., EnPassantMode::Legal)` for custom-start and diagram FENs.

- [ ] **Step 4: Run the tests and verify they pass**

Run: `cargo test --lib print::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/print src/lib.rs
git commit -m "feat(print): build a backend-neutral print document"
```

### Task 2: SVG board diagrams

**Files:**
- Create: `src/print/board.rs`
- Modify: `src/print/mod.rs`
- Test: `src/print/tests.rs`

**Interfaces:**
- Consumes: `PrintDocument`, `Diagram`.
- Produces: `pub(super) fn board_svg(pos: &Chess) -> String` and diagram SVG generation in `build_document`.

- [ ] **Step 1: Write the failing tests**

Assert that a board SVG contains 64 square rectangles, the eight file letters and rank numbers, a side-to-move marker, and embedded piece artwork. Assert the SVG includes the cburnett attribution comment.

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test --lib print::tests::board`
Expected: FAIL because the renderer is missing.

- [ ] **Step 3: Implement the SVG renderer**

Use a 420×420 viewBox, a 360×360 board at (24, 24), light `#EBECD0`, dark `#779656`, coordinates around the board, and a filled circle at the right edge for the side to move. Embed the inner content of the twelve `assets/pieces/*.svg` files with `include_str!` and a `translate/scale` transform. Add an XML comment crediting Colin M. L. Burnett and CC BY-SA 3.0.

- [ ] **Step 4: Run the tests and verify they pass**

Run: `cargo test --lib print::tests::board`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/print
git commit -m "feat(print): render board diagrams as SVG"
```

### Task 3: Typst renderer and bundle writer

**Files:**
- Create: `src/print/typst.rs`
- Modify: `src/print/mod.rs`
- Test: `src/print/tests.rs`

**Interfaces:**
- Consumes: `PrintDocument`.
- Produces:
  - `pub fn source(document: &PrintDocument) -> String`
  - `pub struct PrintBundle { pub source: PathBuf, pub diagrams: Vec<PathBuf> }`
  - `pub fn write_bundle(path: &Path, document: &PrintDocument) -> Result<PrintBundle>`
  - `pub fn compile_pdf(typst_path: &Path, pdf_path: &Path) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

Assert A4 two-column page setup, page header, compact heading, bold move text, bracketed indented variations, nested indentation, comments, annotations, result, diagram image references, no `(Diagram)` placeholder, escaping of Typst string literals, deterministic output, and bundle files on disk.

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test --lib print::tests::typst`
Expected: FAIL because the renderer is missing.

- [ ] **Step 3: Implement the renderer and writer**

Use `#set page(paper: "a4", columns: 2, margin: ..., header: ...)`; `#set text` with a DejaVu/Liberation fallback; `#text(weight: "bold", "...")` for moves; `#text("...")` for all user text; `#block(inset: (left: 1.2em * depth))` for variations; `#block(breakable: false)[#image("...", width: 86%)]` for diagrams. Write `diagram-01.svg`, `diagram-02.svg`, … into `<stem>_diagrams/`, overwriting only `diagram-*.svg` files. Do not add a visible cburnett attribution line; each SVG keeps its attribution comment. Compile with `BRETT_TYPST` or `typst` only from `compile_pdf`.

- [ ] **Step 4: Run the tests and verify they pass**

Run: `cargo test --lib print::tests::typst`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/print
git commit -m "feat(print): generate Typst source and diagram bundles"
```

### Task 4: App diagram selection and export dialog

**Files:**
- Modify: `src/app.rs`
- Test: `src/app.rs` unit tests if practical; otherwise rely on library tests plus manual UI check.

**Interfaces:**
- Consumes: `crate::print::{build_document, PrintMeta, typst}`.
- Produces: current-game diagram state, move context-menu toggle, `File → Export current game as Typst…`, and an export dialog with final-position and PDF-compile options.

- [ ] **Step 1: Write the failing tests**

Add a unit test for the default print path and for diagram-toggle state if the state can be isolated without an `App`; otherwise document the manual check.

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test --lib`
Expected: FAIL for the new helper, or pass with a documented manual check if no isolated helper is added.

- [ ] **Step 3: Implement the UI**

Add `print_diagrams: BTreeSet<usize>`, `print_export_open`, `print_export_include_final`, `print_export_compile_pdf`, and `print_export_status`. Clear diagram markers when a different game is selected. Add “Add diagram here” / “Remove diagram here” to the move context menu. Add the File menu entry and a dialog that picks the `.typ` path, builds the document, writes the bundle, and optionally compiles a PDF. Use the library stem plus `-print.typ` as the default path. Report missing `typst` without failing the source export.

- [ ] **Step 4: Run the tests and verify they pass**

Run: `cargo test --lib` and `cargo clippy --all-targets --all-features`
Expected: PASS and no warnings.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): export the current game as Typst"
```

### Task 5: Documentation, manual compile validation, and final checks

**Files:**
- Create: `docs/PRINT.md`
- Modify: `docs/BACKLOG.md`
- Modify: `docs/PROVENANCE.md` if the SVG asset use needs a new row
- Modify: `THIRD_PARTY_NOTICES.md` only if attribution is not already covered

- [ ] **Step 1: Document the feature**

Describe the Typst target, diagram selection, output folder layout, compiler requirement, font fallback, cburnett attribution, and the deferred LaTeX target. Mark backlog item 25 as Typst implemented.

- [ ] **Step 2: Generate and compile a sample**

Run a local unit test or a small example that writes a `.typ` file from the inline Opera Game in `src/test_data.rs`, then run `typst compile` manually. Inspect the PDF for two columns, bold mainline, indented variations, and a real board. Confirm no `(Diagram)` placeholder is emitted.

- [ ] **Step 3: Run the full verification**

Run: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features`, `cargo test`.
Expected: all clean.

- [ ] **Step 4: Commit and push**

```bash
git add docs src
git commit -m "docs(print): document Typst export"
git push origin main
```
