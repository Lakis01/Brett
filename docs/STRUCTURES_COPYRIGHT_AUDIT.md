# Copyright Audit: Middlegame Structure Presets & Plan Illustrations

**Audit date:** 2026-11-25  
**Scope:** `src/search.rs`, `src/structure_plans.rs`, `README.md`, `CONTRIBUTING.md`, `assets/`, and `.github/workflows/release.yml` (including release binary/package/AppImage/DMG/ZIP artifacts).  
**Status:** Passed for `v0.3.2` release. This is an engineering provenance and copyright-hygiene record, not formal legal advice.

## 1. Why This Audit Exists

Middlegame pawn-structure taxonomy (IQP, Hanging Pawns, Carlsbad, Stonewall, Maróczy Bind, Hedgehog, Najdorf/Scheveningen/Dragon/Boleslavsky, Semi-Slav, Grünfeld, French & King's Indian chains, Benoni) belongs to centuries of public chess theory and is discussed in classic works including Mauricio Flores Ríos's *Chess Structures: A Grandmaster Guide* (Quality Chess, 2015).

Under copyright law, **ideas, chess rules, strategic concepts, and standard opening/pawn-structure classifications are not copyrightable**, whereas **specific prose explanations, chapter/label schemes unique to a book, curated game-example selections, and copyrighted diagram collections** are protected expression.

## 2. Audit Checks & Remediations Performed

1. **Label & Taxonomy Hygiene (`src/search.rs`, `README.md`):**
   - Removed the remaining reference to `*Chess Structures*` in `README.md`.
   - Replaced book-specific Roman-numeral chapter labels (`"Najdorf Type I"`, `"Najdorf Type II"`, `"Najdorf Type III"`) in `src/search.rs` and `README.md` with descriptive standard chess terminology (`"Najdorf — Classical / Flexible (a6-d6, e5/e6)"`, `"Najdorf — Queenside ...b5 (a6-b5-d6)"`, `"Najdorf — ...e5 vs e4+f4 (a6-d6-e5)"`).
   - Verified zero occurrences of `"Flores"`, `"Rios"`, `"Ríos"`, `"Quality Chess"`, `"Type I"`, `"Type II"`, `"Type III"`, or `"Chess Structures"` in shipped source code (`src/`), `README.md`, `CONTRIBUTING.md`, or `.github/workflows/`.
2. **Originality of Strategic Plan Prose & Diagrams (`src/structure_plans.rs`):**
   - Inspected every `StructurePlan` entry across all 9 `StructureFamily` groups (64 plans total: Stonewall 6, Badewanne 4, Carlsbad 7, IQP & Panov 9, Hanging Pawns 7, Maróczy & Hedgehog 7, Sicilian 8, Slav/Triangle/Grünfeld 8, Closed Chains 8).
   - Confirmed every `title` and `summary` is freshly authored, concise, factual chess description using standard domain terms (`minority attack`, `outpost`, `blockade`, `fianchetto`, `rook lift`, `minority push`, `central break`).
   - Confirmed every `setup_fen` is a clean, synthetic/generic middlegame schema created specifically to satisfy the corresponding Brett `StructurePreset` bitboard predicate and demonstrate a standard legal move/maneuver, without citing or reproducing annotated book games or diagram collections.
   - Automated unit tests (`every_structure_plan_has_valid_balanced_fen_legal_moves_and_matches_its_preset`) validate every FEN for legality, balanced material (`|W - B| <= 1`), preset matching, legal SAN moves, and empty maneuver waypoints.
3. **ECO Data Provenance (`src/eco.rs`, `assets/eco/`):**
   - Uses only the CC0 1.0 Public Domain `lichess-org/chess-openings` TSV dataset (`assets/eco/a.tsv`..`e.tsv`).
4. **Release Build & Packaging Audit (`.github/workflows/release.yml`):**
   - Verified Linux (`.tar.gz`, `.deb`, `.AppImage`, `PKGBUILD`), Windows (`.zip`), and macOS (`.dmg`) release steps bundle only the compiled `brett` binary, desktop/AppStream metadata, icon, `README.md`, and `LICENSE`.
   - Verified no player photo database (`assets/photos.bin`), third-party book file, or local personal config (`custom_structures.json`, `references.json`) is committed or bundled.

## 3. Verification Commands

```sh
rg -n "Chess Structures|Mauricio|Flores|Quality Chess|Type I|Type II|Type III" src/ README.md CONTRIBUTING.md .github/
git grep -nE 'lip_[A-Za-z0-9]{20,}|Bearer [A-Za-z0-9]'
cargo test --all-targets
```

All grep checks returned zero unintended matches, and the full test suite passed after the final edits.
