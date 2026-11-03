# Data safety

## The rule

The local source collections — conventionally `$HOME/Chess Databases/` and
`$HOME/Downloads/CBH/` — are **read-only** for this project and for every agent
session working on them. Brett protects those two locations automatically when
they exist. Add extra roots with `BRETT_READ_ONLY_ROOTS`, a platform-separated
path list.

Allowed:

- Reading files.
- Copying files **out** of either read-only location into `/tmp/opencode/` or
  `Brett/fixtures/local/`.
- Converting those copies in the destination.

Never allowed:

- Deleting, moving, or renaming anything in the source tree.
- Writing, truncating, or creating files there (including lock files, journals,
  thumbnails, or converted exports).
- Running tools in-place that open databases for writing (`scid`, ChessBase,
  `pgnscid`, etc.). Export only from copies.

## Why

The databases are the user's irreplaceable reference material. Several are
commercial products (Mega Database, ChessBase opening trees, annotated book
collections) and cannot be re-downloaded. A single accidental write or lock
file can corrupt a SCID index or a CBH container.

## Native readers

Brett's PGN, SCID, and classic CBH readers open source files with read-only
handles.

The SCID reader:

- opens the `.si4`/`.si3` index and reads its header, entries, and companion
  `.sn`/`.sg` files;
- never creates a missing companion file;
- never writes to the database directory;
- decodes one `.sg` record at a time on demand.

The classic CBH reader:

- opens `.cbh` and `.cbg`, and reads optional `.cba`, `.cbp`, `.cbt`, `.cbc`,
  `.cbs`, and `.cbj` files;
- never creates a missing companion file or a `.cbj`;
- never writes to the database directory;
- decodes one `.cbg` and `.cba` record at a time on demand.

No SCID installation, ChessBase installation, `scidpgn`, or converter
subprocess is involved. The independent format references and fixture
provenance are documented in [SCID.md](SCID.md), [CBH.md](CBH.md), and
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

## Derived data

Player and reference position indexes live only under:

```text
$XDG_CACHE_HOME/brett/index/       (or ~/.cache/brett/index/)
```

Online imports live only under:

```text
$XDG_CACHE_HOME/brett/online/      (or ~/.cache/brett/online/)
```

User preferences, including the selected UCI engine path, thread count, hash
size, display choices, reference-database settings, correspondence-database
settings, and the optional Lichess token, live under:

```text
$XDG_CONFIG_HOME/brett/engine.json       (or ~/.config/brett/engine.json)
$XDG_CONFIG_HOME/brett/display.json      (or ~/.config/brett/display.json)
$XDG_CONFIG_HOME/brett/preparation.json  (or ~/.config/brett/preparation.json)
$XDG_CONFIG_HOME/brett/references.json   (or ~/.config/brett/references.json)
```

`references.json` can contain a Lichess token and account names. Never commit
it. Prefer `BRETT_LICHESS_TOKEN` for the token so it never reaches the file.
The repository's `.gitignore` covers these filenames, but it cannot untrack a
file that is already tracked; check `git status` before committing.

Nothing is written beside a source database. A cache index is keyed by the
canonical source path and validated against the PGN file's length/mtime, the
combined SCID triple fingerprint, or the combined classic CBH file-set
fingerprint. Builds use a temporary file in the cache directory and an atomic
rename.

Player books and edited game trees are in memory for the session. They are not
written back to the source database.

## Practical workflow

1. `cp -p` the complete database triple (`.si4`, `.sn4`, `.sg4`) or the
   complete classic CBH file set (at least `.cbh` and `.cbg`, plus `.cba` and
   name files when present) into `/tmp/opencode/…`.
2. Run readers/converters there.
3. Keep derived fixtures out of version control: `fixtures/local/` is
   git-ignored.

No PGN or SCID database fixtures are committed. Tests embed the public-domain
Opera Game inline in `src/test_data.rs` and generate SCID databases in
temporary directories at test time. Everything under `fixtures/local/` is for
local testing only and must never be redistributed.

## Enforcement

- Import code opens source files read-only (`File::open`, no write flags).
- SCID and CBH companion files are checked for existence but never created.
- Tests never touch the real database directory; they use inline public-domain
  test data, synthetic databases, or temporary copies.
- The only tools allowed to touch the source tree are read-only inspectors
  (`ls`, `stat`, checksum tools). Converters run on copies.
