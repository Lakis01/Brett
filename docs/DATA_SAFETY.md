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

## Practical workflow

1. `cp -p` the complete database triple (`.si4`, `.sn4`, `.sg4`) or CBH file
   into `/tmp/opencode/…`.
2. Run readers/converters there.
3. Keep derived fixtures out of version control: `fixtures/local/` is
   git-ignored.

`fixtures/opera.pgn` is public domain and safe to commit. Everything under
`fixtures/local/` is for local testing only and must never be redistributed.

## Enforcement ideas (later)

- Import code opens source files read-only (`File::open`, no write flags).
- A debug assertion refuses paths under `Chess Databases` for any write API.
- Tests never touch the real database directory; they use copies.
