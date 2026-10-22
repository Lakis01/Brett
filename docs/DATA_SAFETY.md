# Data safety

## The rule

`/home/gratis01/Chess Databases/` — and every database directory inside it — is
**read-only** for this project and for every agent session working on it.

Allowed:

- Reading files.
- Copying files **out** of the directory into `/tmp/opencode/` or
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

Brett's PGN and SCID readers open source files with read-only handles. The SCID
reader:

- opens the `.si4`/`.si3` index and reads its header, entries, and companion
  `.sn`/`.sg` files;
- never creates a missing companion file;
- never writes to the database directory;
- decodes one `.sg` record at a time on demand.

No SCID installation or `scidpgn` subprocess is involved. The independent
format reference and fixture provenance are documented in
[SCID.md](SCID.md) and [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

## Derived data

Position indexes live only under:

```text
$XDG_CACHE_HOME/brett/index/       (or ~/.cache/brett/index/)
```

Online imports live only under:

```text
$XDG_CACHE_HOME/brett/online/      (or ~/.cache/brett/online/)
```

User preferences, including the selected UCI engine path, thread count, and hash
size, live under:

```text
$XDG_CONFIG_HOME/brett/engine.json (or ~/.config/brett/engine.json)
```

Nothing is written beside a source database. A cache index is keyed by the
canonical source path and validated against the PGN file's length/mtime or the
combined SCID triple fingerprint. Builds use a temporary file in the cache
directory and an atomic rename.

Player books and edited game trees are in memory for the session. They are not
written back to the source database.

## Practical workflow

1. `cp -p` the complete database triple (`.si4`, `.sn4`, `.sg4`) or CBH file
   into `/tmp/opencode/…`.
2. Run readers/converters there.
3. Keep derived fixtures out of version control: `fixtures/local/` is
   git-ignored.

`fixtures/opera.pgn` is public domain and safe to commit. The small SCID
fixture under `fixtures/scid/` is generated from it with `pgnscid`; see
`fixtures/scid/README.md`. Everything under `fixtures/local/` is for local
testing only and must never be redistributed.

## Enforcement

- Import code opens source files read-only (`File::open`, no write flags).
- SCID companion files are checked for existence but never created.
- Tests never touch the real database directory; they use committed fixtures or
  copies.
- The only tools allowed to touch the source tree are read-only inspectors
  (`ls`, `stat`, checksum tools). Converters run on copies.
