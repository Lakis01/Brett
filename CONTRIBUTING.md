# Contributing to Brett

Brett is a native Linux desktop application. The project is written in Rust and
uses `eframe`/`egui` for the window, `shakmaty` for rules and SAN, and native
readers for PGN, SCID, and classic ChessBase CBH databases.

## Platform status

Linux is the supported and tested platform. Development and visual checks run
on KDE Plasma Wayland with Xwayland, and the binary is built with both the X11
and Wayland backends. Windows and macOS are **not currently supported or
tested**: the file dialogs shell out to `zenity`/`kdialog`, configuration uses
XDG directories, and the windowing/rendering stack has not been validated
there. A port is possible, but treat it as new work rather than an assumed
build.

## Prerequisites

- Rust 1.97 or newer (edition 2024; `shakmaty`/`pgn-reader` set the floor).
- A C toolchain and the usual X11/Wayland development packages required by
  `eframe`/`winit`.
- `zenity` or `kdialog` for native file dialogs. Without one, **Open…** and
  **Choose…** cannot show a picker.
- Optional: a UCI engine such as Stockfish. Brett looks for `$BRETT_ENGINE`,
  then `/usr/bin/stockfish`; any UCI engine can be selected in the Engine tab.
- Optional: a Lichess API token for the Lichess opening explorer. See
  [Secrets](#secrets-and-personal-settings).

## Build, run, and test

```sh
cargo fmt --all
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings

cargo run --release -- fixtures/opera.pgn
cargo run --release -- /path/to/database.si4
```

Useful reports:

```sh
cargo run --example report -- file.pgn
cargo run --example index_report -- file.pgn
cargo run --example position_report -- file.pgn
cargo run --example scid_report -- file.si4
```

`fixtures/opera.pgn` is public domain. The generated SCID fixture under
`fixtures/scid/` is safe to commit. Everything under `fixtures/local/` is local
test data and is git-ignored; never redistribute it.

## Secrets and personal settings

Brett keeps personal state outside the repository:

```text
$XDG_CONFIG_HOME/brett/references.json   (or ~/.config/brett/references.json)
$XDG_CONFIG_HOME/brett/display.json
$XDG_CONFIG_HOME/brett/preparation.json
$XDG_CONFIG_HOME/brett/engine.json
$XDG_CACHE_HOME/brett/                  (or ~/.cache/brett/)
```

`references.json` can contain a Lichess token and account names.
`preparation.json` can contain local database paths. `engine.json` can contain
a local engine path. None of these should ever be committed.

The repository's `.gitignore` covers the usual copies, but **`.gitignore` does
not untrack a file that is already tracked**. Before committing:

```sh
git status --short
git diff
git grep -nE 'lip_[A-Za-z0-9]{20,}|Bearer [A-Za-z0-9]'
```

Prefer the environment variable for the token:

```sh
export BRETT_LICHESS_TOKEN='lip_...'
```

The variable takes precedence over the saved token and keeps the secret out of
`references.json` entirely. If a token is ever exposed, revoke it at
<https://lichess.org/account/oauth/token> immediately; deleting the commit is
not enough because the value may already have been copied.

Other useful environment variables:

- `BRETT_ENGINE` — default UCI engine path.
- `BRETT_REFERENCE` — default preparation reference database.
- `BRETT_CORRESPONDENCE` — default correspondence database.
- `BRETT_READ_ONLY_ROOTS` — platform-separated list of extra directories that
  PGN export must never write into. The conventional `$HOME/Chess Databases`
  and `$HOME/Downloads/CBH` locations are protected automatically when they
  exist.

## Licensing

Brett's original code is MIT, but the current build links `shakmaty` and
`pgn-reader` under GPL-3.0-or-later. A distributed binary is therefore GPL,
not MIT-only. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Do not add
GPL/AGPL source code or assets; if you can replace the two GPL crates with
permissively licensed equivalents without regressing correctness or
large-database performance, that is a welcome contribution.

## Source databases

Source PGN, SCID, and classic CBH collections are **read-only**. Brett opens
them with read-only handles, writes derived indexes only under the cache
directory, and refuses to export into the protected source roots. Convert or
inspect copies in `/tmp/opencode/`, never in place. See
[docs/DATA_SAFETY.md](docs/DATA_SAFETY.md).

## ICCF database

Brett does not redistribute the ICCF correspondence database. If you use one,
read [ICCF_NOTICE.md](ICCF_NOTICE.md). The database is for personal use and may
not be sold or re-sold to a third party.

## Pull-request checklist

- `cargo fmt --all` has been run.
- `cargo check --all-targets`, `cargo test --all-targets`, and strict clippy
  pass.
- New UI behavior has a short note in `README.md` or the relevant `docs/` file.
- No local database, token, personal path, or config file is staged.
- Source collections were not modified.
- The change keeps the read-only and large-database guards intact.
