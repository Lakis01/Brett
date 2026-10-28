# Removing the GPL/AGPL dependency exposure

**Status:** analysis only. No dependency or source change is implied by this
document. This is an engineering and licensing-risk assessment, not legal
advice; a lawyer should confirm the distribution model before a release.

## Current exposure

Brett's original code is MIT. Two runtime dependencies are
**GPL-3.0-or-later**:

| Crate | Version | Used for |
| --- | --- | --- |
| `shakmaty` | 0.30.2 | Board representation, legal moves, SAN, FEN, UCI conversion, Zobrist hashing, Chess960/custom starts |
| `pgn-reader` | 0.29.1 | Streaming PGN tokenization, tags, comments, NAGs, variations, null moves |

A distributed Brett binary that links these crates is a combined work under the
GPL. Keeping the top-level `LICENSE` as MIT does **not** remove that obligation:
the MIT grant applies to Brett's original code, while the combined binary must
still be distributed under GPL-compatible terms, with the corresponding source
and notices. Private use is unaffected; the question only becomes acute when a
binary, AppImage, package, or installer is published.

`r-efi` and `self_cell` also appear in the dependency graph with permissive
options; they are not hard blockers. The two crates above are.

## What a replacement must preserve

- Legal move generation and check/checkmate/stalemate handling.
- SAN parsing and formatting, including disambiguation, promotions, checks,
  mates, null moves, and figurine input.
- FEN parsing/formatting, custom starts, and Chess960 castling.
- UCI move conversion used by the engine and review paths.
- A stable position key for the position index (or a deliberate cache-format
  bump).
- Streaming PGN performance and tolerance for real-world files: tags,
  comments, NAGs, recursive variations, clock/eval tokens, malformed games,
  and multi-million-game archives.
- The existing SCID/classic-CBH readers, which are independent and should not
  be destabilized.

## Options

### 1. Adopt GPL-3.0-or-later for the combined distribution

Keep the dependencies and make the project's distribution licence explicit.

- **Work:** update `LICENSE`, `Cargo.toml`, `README.md`, and
  `THIRD_PARTY_NOTICES.md`; add a source offer or point at the public
  repository; ensure the MIT notices remain for the original code.
- **Pros:** zero rewrite risk; fastest; legally clean; matches the existing
  upstream licences; no correctness or performance regression.
- **Cons:** the distributed work is copyleft. Downstream users cannot take the
  combined binary into a proprietary product. Some contributors and packagers
  prefer MIT. The original MIT code can remain available under MIT, but the
  combined binary cannot be described as MIT-only.
- **When it wins:** if "full open source" means "unambiguously licensed open
  source", not "permissive for closed reuse". This is the lowest-risk path.

### 2. Ask the upstream authors for a permissive or dual licence

`shakmaty` and `pgn-reader` share an author. A dual-licence grant, a commercial
licence, or a permissive relicense would solve the problem with no rewrite.

- **Work:** a clear request describing Brett, the distribution model, and the
  requested terms; keep the current GPL path until a written grant exists.
- **Pros:** cheapest if granted; no code churn; upstream may welcome a
  permissive client.
- **Cons:** entirely outside Brett's control; may be refused, slow, or
  expensive; a verbal answer is not enough for a release.
- **When it wins:** run this in parallel with every other option. It can
  collapse the decision at any time.

### 3. Replace the stack with permissive components

This is the main engineering option. It can be staged so the project keeps
working while the GPL crates are removed.

#### 3a. Rules: `cozy-chess` (MIT)

`cozy-chess` is an MIT-licensed, bitboard-based legal move generator with
Chess960/DFRC support and an incremental Zobrist hash. It is the closest
permissive match to the parts of `shakmaty` Brett uses.

- **Work:** add a thin Brett-owned facade (`Position`, `Move`, `Color`,
  `Square`, SAN/FEN helpers) so the 22 files that import `shakmaty` do not all
  change at once. Implement SAN parsing/formatting on top of legal moves, FEN
  formatting, UCI adapters (including Chess960 king-captures-rook notation),
  and a position-key adapter.
- **Hidden cost:** the position index is keyed by the current hash. A new
  Zobrist table requires a cache-format version bump and a rebuild of existing
  indexes. Budget for that migration rather than silently reusing stale keys.
- **Risk:** SAN edge cases and Chess960 castling are the usual sources of
  subtle bugs. Perft suites and the existing SCID/CBH fixtures are the safety
  net.
- **Pros:** MIT, fast, Chess960, no new copyleft.
- **Cons:** not a drop-in `shakmaty` replacement; SAN/FEN layers are ours to
  write and test.

`chess` 3.2.0 is also MIT and fast, but it does not advertise Chess960 support;
Brett's classic-CBH path needs Chess960, so `cozy-chess` is the better fit.

#### 3b. PGN: in-house streaming reader (or a small MIT parser)

`pgn_parser` and `w-pgn` are MIT, but they are small, non-streaming parsers
with limited variation/NAG/tolerance coverage. They are useful as grammar
references, not as a drop-in replacement for `pgn-reader`.

- **Work:** a byte-oriented streaming parser that emits the same raw events
  Brett already consumes: tags, SAN tokens, comments, NAGs, variations, results,
  and null moves. SAN validation stays in the rules layer.
- **Pros:** full control over tolerance and performance; no copyleft; can be
  optimized for the header-index path.
- **Cons:** real-world PGN is messy; this is where most of the compatibility
  risk lives. A naive parser will be slower than `pgn-reader` on multi-million
  game archives.
- **Mitigation:** keep the current `pgn-reader` path behind a feature while the
  new parser is validated on the public fixtures and large local archives;
  compare game trees move-for-move before switching the default.

#### Staged sequence

1. Put the current GPL crates behind a non-default `gpl` feature and make the
   rest of the code depend on Brett-owned facade types.
2. Replace `pgn-reader` first. It touches four files and has a narrower surface
   than the rules engine.
3. Replace `shakmaty` with `cozy-chess` plus SAN/FEN/UCI adapters; bump the
   position-index cache version.
4. Flip the default build to the permissive backend; keep the GPL backend as an
   optional compatibility feature for a release or two.
5. Remove the GPL feature and dependencies once the permissive path has passed
   the full fixture, SCID/CBH, and large-archive suites.

- **Effort:** roughly 4–8 focused weeks for a careful implementation,
  validation, and performance pass; more if SAN/Chess960 edge cases are worse
  than expected.
- **When it wins:** if Brett must remain MIT/permissive for downstream use and
  upstream will not relicense.

### 4. Clean-room rewrite of both libraries

Same as option 3, but without `cozy-chess`: write the move generator, SAN/FEN,
Zobrist, and PGN parser from scratch.

- **Pros:** no third-party licence at all; complete control.
- **Cons:** significantly more work and risk than 3a; a correct legal move
  generator is a substantial project. `cozy-chess` already supplies the part
  that is hardest to get right.
- **When it wins:** only if `cozy-chess` fails a required feature or its
  maintenance becomes a problem. Otherwise this is avoidable duplication.

### 5. Dual build / feature split

Ship a permissive default build and an optional GPL build with the current
dependencies. Users who need the old behavior can enable the feature; the
default binary stays MIT/permissive.

- **Pros:** incremental; preserves a compatibility path; lets the rewrite land
  gradually.
- **Cons:** two backends and two test matrices; feature combinations are easy
  to break; the GPL build still needs GPL compliance; users may not understand
  which binary they have.
- **Important:** the default distributed binary must genuinely exclude the GPL
  crates. A Cargo feature that is enabled by default, or a build that still
  links them, does not help.

### 6. Process isolation or a helper binary

Move the GPL code into a separate executable and communicate over pipes.

- **Pros:** superficially separates the works.
- **Cons:** the legal boundary is uncertain and fact-specific; the helper is
  still GPL and must be distributed with source; IPC adds latency to the
  hottest paths (PGN parsing and position indexing); packaging and sandboxing
  become harder.
- **Verdict:** not a reliable licensing strategy and a poor engineering fit.
  Do not use this as a dodge.

### 7. Stop distributing binaries

Keep the source MIT and let users build locally, or distribute only source.

- **Pros:** avoids distributing a combined binary.
- **Cons:** defeats the purpose of a desktop application; most users will not
  build it; distro packagers still need a coherent licence story.
- **Verdict:** not a product strategy.

## Recommendation

1. **In parallel, ask upstream for a dual/permissive licence.** It is cheap and
   could remove the problem outright.
2. **If permissive distribution matters, choose option 3** and fund a short
   spike first: `cozy-chess` for rules, a prototype streaming PGN parser, and a
   facade that hides both. Gate the full rewrite on perft, SAN/FEN round-trip,
   the existing PGN/SCID/CBH fixtures, and a large-archive benchmark.
3. **If permissive distribution does not matter, choose option 1 now.** Adopt
   GPL-3.0-or-later for the combined distribution, keep the MIT notices for the
   original code, and stop spending engineering time on a replacement that the
   project does not need.
4. **Do not** ship a binary that links GPL code while describing the whole
   binary as MIT, and do not rely on process isolation or an optional feature
   as a legal workaround.

The decision gate is a product decision, not a technical one: is Brett's value
in being permissively reusable, or in being open source with the least legal and
engineering risk? The current code can support either, but not both without the
rewrite.
