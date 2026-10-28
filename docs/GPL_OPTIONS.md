# Removing the GPL/AGPL dependency exposure

**Status:** decision recorded (2026-10-28); implementation planned. This is an
engineering and licensing-risk assessment, not legal advice; a lawyer should
confirm the distribution model before a release.

**Decision (2026-10-28):** do not send the upstream licence request. Replace
`shakmaty` and `pgn-reader` with MIT components and keep Brett MIT/permissive:
`cozy-chess` 0.3.4 for rules (legal moves, SAN/UCI helpers, FEN/Shredder FEN,
Chess960/DFRC, Zobrist), a Brett-owned streaming PGN parser, and a Brett-owned
Polyglot Zobrist for `.bin` books. The position-index cache version will be
bumped because `cozy-chess` hashes differ from the current Polyglot-compatible
keys. The MIT `chess` crate is rejected because it lacks Chess960; a clean-room
move generator is rejected as an unreasonable rewrite.

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
  `Square`, SAN/FEN helpers) so the 16 files that import `shakmaty` do not all
  change at once. `cozy-chess` already supplies
  `util::{parse_san_move, display_san_move, parse_uci_move, display_uci_move}`;
  the facade adapts its flat `Move` representation and king-captures-rook
  castling notation to Brett's tree, SCID, and CBH code. It also supplies
  FEN/Shredder FEN and `BoardBuilder` for custom starts.
- **Hidden cost:** the position index is keyed by the current
  Polyglot-compatible hash. `cozy-chess`'s `Board::hash()` uses a different
  Zobrist table, so bump `src/position.rs::VERSION` and rebuild existing
  indexes. Polyglot `.bin` books need a separate Brett-owned Polyglot hash.
- **Risk:** SAN edge cases and Chess960 castling are the usual sources of
  subtle bugs. Perft suites, SAN/FEN round-trip tests, and the existing
  SCID/CBH fixtures are the safety net.
- **Pros:** MIT, fast, Chess960, no new copyleft.
- **Cons:** not a drop-in `shakmaty` replacement; the facade and adapters are
  ours to test.

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
- **Mitigation:** validate the new parser on the public fixtures and large
  local archives, and compare game trees move-for-move before removing the GPL
  crates. No GPL-backed feature ships in the default binary.

#### Staged sequence

1. Add `cozy-chess` and a Brett-owned facade, keeping the current call sites on
   the facade types. Use `shakmaty` temporarily only for differential tests.
2. Replace `pgn-reader` with the Brett-owned streaming parser. It touches three
   files (`domain.rs`, `library.rs`, `position.rs`) and has a narrower surface
   than the rules engine.
3. Switch the facade to `cozy-chess`, migrate Polyglot book lookup to the
   Brett-owned hash, and bump the position-index cache version.
4. Remove the GPL dependencies and update `THIRD_PARTY_NOTICES.md`; run the
   full fixture, SCID/CBH, and large-archive suites plus a streaming benchmark.

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

The project selected **option 3** on 2026-10-28: keep Brett MIT/permissive and
replace both GPL crates.

1. **Implement the `cozy-chess` facade first**, with `shakmaty` retained only as
   a test oracle. Gate it on perft, SAN/FEN round-trip, and the existing
   PGN/SCID/CBH fixtures.
2. **Replace `pgn-reader` with the Brett-owned streaming parser**, validated on
   public fixtures and a large local archive before removal.
3. **Bump the position-index version** and rebuild caches; add the Brett-owned
   Polyglot hash for `.bin` books.
4. **Remove the GPL dependencies and update the notices.** Do not ship a binary
   that links GPL code while describing the whole binary as MIT, and do not
   rely on process isolation or an optional feature as a legal workaround.

The upstream request is no longer part of the plan; it is retained below only
as a record of the option considered.

## Draft upstream request (unsent and obsolete — do not send)

**To:** Niklas Fiekas, author of `shakmaty` and `pgn-reader`
**Channel:** the contact route published on
<https://github.com/niklasf/shakmaty>
**Status:** drafted 2026-10-28; **not sent**. The 2026-10-28 decision replaced
both crates instead, so this request is retained only as a record. Do not send
it.

> Hello Niklas,
>
> I maintain Brett, a native Rust desktop chess database and preparation app
> (<https://github.com/Lakis01/Brett>). Brett's original code is MIT, but it
> links `shakmaty` 0.30 and `pgn-reader` 0.29, both GPL-3.0-or-later, so a
> distributed binary is a combined GPL work.
>
> Would you consider dual-licensing these two crates under a permissive
> licence (MIT or Apache-2.0), or granting Brett a permissive licence for its
> use? I am happy to keep the existing attribution, contribute fixes upstream,
> or discuss a commercial arrangement if that is the better fit.
>
> If the answer is no, I will either adopt GPL-3.0-or-later for the combined
> binary or replace the crates; either way I would like to record your answer
> accurately. Thank you for the work you have put into both libraries.
>
> [name / contact]
