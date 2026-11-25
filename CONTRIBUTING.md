# Contributing to Brett

Brett is a native Linux desktop application. The project is written in Rust and
uses `eframe`/`egui` for the window, `cozy-chess` for rules and SAN, a
Brett-owned streaming parser for PGN, and native readers for PGN, SCID, and
classic ChessBase CBH databases.

## Platform status

Linux is the supported and tested platform. Development and visual checks run
on Arch Linux with KDE Plasma Wayland plus Xwayland, and the binary is built
with both the X11 and Wayland backends. Other mainstream distributions should
work with the prerequisites below, but they have not been validated; package
names and desktop integration vary. Windows and macOS are **not currently
supported or tested**: the file dialogs shell out to `zenity`/`kdialog`,
configuration uses XDG directories, and the windowing/rendering stack has not
been validated there. A port is possible, but treat it as new work rather than
an assumed build.

## Prerequisites

- Rust 1.97 or newer (edition 2024; `eframe`/`egui` set the floor).
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

cargo run --release
cargo run --release -- /path/to/database.si4
```

Useful reports:

```sh
cargo run --example report -- file.pgn
cargo run --example index_report -- file.pgn
cargo run --example position_report -- file.pgn
cargo run --example scid_report -- file.si4
```

No PGN or SCID fixtures are committed. Tests embed the public-domain Opera
Game inline in `src/test_data.rs` and generate SCID databases in temporary
directories with Brett's writer at test time. Everything under
`fixtures/local/` is local test data and is git-ignored; never redistribute it.

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

Brett's original code is MIT, and the current build links only MIT/permissive
dependencies (`cozy-chess` for rules, a Brett-owned streaming PGN parser, and a
Brett-owned Polyglot hash). See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
Do not add GPL/AGPL source code, dependencies, or assets.

## Source databases

Source PGN, SCID, and classic CBH collections are **read-only**. Brett opens
them with read-only handles, writes derived indexes only under the cache
directory, and refuses to export into the protected source roots. Convert or
inspect copies in `/tmp/opencode/`, never in place. See
[docs/DATA_SAFETY.md](docs/DATA_SAFETY.md).

## ICCF database

Brett redistributes a compiled ICCF correspondence snapshot under
[`databases/iccf/`](databases/iccf/). Read [ICCF_NOTICE.md](ICCF_NOTICE.md).
The database is for personal use and may not be sold or re-sold to a third
party. Keep the three SCID files together when refreshing or copying the
snapshot.

## Contributing position search filters & structure presets

Brett's built-in **position search filters** and **strategic plan illustrations** (`src/search.rs` and `src/structure_plans.rs`) are preliminary and actively evolving. If you hand-craft sharper pawn/piece filters using the **Board Pattern** editor (`OR` / `AND` / `NONE` conjunctions, file-absence guards, and piece brushes), Brett saves them to:

```text
$XDG_CONFIG_HOME/brett/custom_structures.json
```

Community contributions of handcrafted filter definitions and original plan illustrations are very welcome! Feel free to open a GitHub Issue or Pull Request with your exported JSON preset entries (and brief notes on edge cases they catch or exclude). Please ensure any contributed prose or sample FENs are original and do not copy text or diagrams from copyrighted chess books.

## Pull-request checklist

- `cargo fmt --all` has been run.
- `cargo check --all-targets`, `cargo test --all-targets`, and strict clippy
  pass.
- New UI behavior has a short note in `README.md` or the relevant `docs/` file.
- No local database, token, personal path, or config file is staged.
- Source collections were not modified.
- The change keeps the read-only and large-database guards intact.
