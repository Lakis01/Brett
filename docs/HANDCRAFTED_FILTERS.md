# Handcrafted Structure Search Filters (Locked Presets)

The following **six** `StructurePreset` variants in [`src/search.rs`](../src/search.rs) were hand-tuned directly by the repository owner (matching `~/.config/brett/custom_structures.json`).

Every one of them returns `true` from `StructurePreset::is_handcrafted(self)` and carries a `// HANDCRAFTED (DO NOT OVERWRITE IN BATCH CHANGES — see docs/HANDCRAFTED_FILTERS.md)` comment at both its `to_pattern_components` and `matches_structure` arms.

**Policy:** Never overwrite or tighten these 6 definitions during batch improvements to other structure presets unless explicitly instructed for that specific preset.

| Preset Variant | Display Label | Persp. | Handcrafted Criteria (normalized to perspective side) |
|---|---|---|---|
| `MaroczyBindNoFianchetto` | `Maróczy — No K-side fianchetto` | White | Own pawns `a2, c4, e4, g2`; `b2\|b3`, `f2\|f3`, `h2\|h3`; `no d-pawn`. Opp pawns `b7, d6, f7, g6`; `a7\|a6`, `e7\|e6`, `h7\|h6`; `no c-pawn`. |
| `MaroczyBindFianchetto` | `Maróczy — K-side fianchetto (g3+Bg2)` | White | Own `a2, c4, f2, g3, Bg2`; `b2\|b3`, `e2\|e3\|e4`, `h2\|h3`; `no d-pawn`. Opp `b7, d6, f7, g6, Bg7, h7`; `a7\|a6`, `e7\|e6`; `no c-pawn`. |
| `Botvinnik` | `Botvinnik` | White | Own `b2, c4, d3, e4, f2, g3, h2, Nc3, Ne2, Bg2`; `a2\|a3`. |
| `HangingPawnsClassic` | `Hanging Pawns (c4+d4)` | White | Own `c4, d4`; `no b-pawn`, `no e-pawn`. Opp `no c-pawn`, `no d-pawn`; `b7\|b6`, `e7\|e6`. |
| `CarlsbadAny` | `Carlsbad (core)` | White | Own `h2, e3, d4`; `no c-pawn`; `f2\|f3`, `g2\|g3`, `b2\|b3\|b4`, `a2\|a3\|a4`. Opp `d5, c6`; `no e-pawn`; `b7\|b6`, `a7\|a6\|a5`. |
| `CarlsbadMinorityAttack` | `Carlsbad + Minority Attack (b4/b5)` | White | Own `e3, d4, b5`; `no c-pawn`. Opp `d5, c6`; `no e-pawn`. |
