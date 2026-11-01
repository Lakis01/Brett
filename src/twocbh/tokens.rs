//! Independent reconstruction of the 2CBH move-token table.
//!
//! Every token is eight little-endian 16-bit fields:
//!
//! `[from, to, piece, aux, b3, b2, special6, special7]`
//!
//! The table is generated from the move geometry rather than copied from a
//! ChessBase binary.  The first 45,356 entries are ordinary piece and pawn
//! moves; the last 3,840 entries are Chess960 castling moves.
//!
//! Coordinates in the table are ChessBase's *stored* squares: `file * 8 + rank`.
//! They are converted to Brett squares at decode time.

use std::collections::HashMap;
use std::ops::Index;
use std::sync::LazyLock;

const KING_DIRS: [(i8, i8); 8] = [
    (-1, -1),
    (-1, 0),
    (-1, 1),
    (0, -1),
    (0, 1),
    (1, -1),
    (1, 0),
    (1, 1),
];

const KNIGHT_DIRS: [(i8, i8); 8] = [
    (-2, -1),
    (-2, 1),
    (2, -1),
    (2, 1),
    (-1, -2),
    (-1, 2),
    (1, -2),
    (1, 2),
];

const SLIDER_DIRS: [(i8, i8); 8] = [
    (-1, -1),
    (1, -1),
    (1, 1),
    (-1, 1),
    (-1, 0),
    (0, -1),
    (1, 0),
    (0, 1),
];

/// A generated token table with one spare slot for token zero.
pub(super) struct TokenTable {
    entries: Box<[[u16; 8]]>,
    by_entry: HashMap<[u16; 8], u16>,
}

impl TokenTable {
    fn new() -> Self {
        let mut entries = vec![[0u16; 8]; 65_536];
        let mut at = 1usize;
        build_main(&mut entries, &mut at);
        build_castling_960(&mut entries, &mut at);
        debug_assert_eq!(at, 49_197);
        let mut by_entry = HashMap::new();
        for token in 1..=49_196u16 {
            // Keep the first token for duplicate geometry.  The orthodox castling
            // entries are generated before the Chess960 block, so they win.
            by_entry.entry(entries[usize::from(token)]).or_insert(token);
        }
        Self {
            entries: entries.into_boxed_slice(),
            by_entry,
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn entry(&self, token: u16) -> [u16; 8] {
        self.entries[usize::from(token)]
    }

    pub(super) fn token_for(&self, entry: [u16; 8]) -> Option<u16> {
        self.by_entry.get(&entry).copied()
    }
}

impl Index<usize> for TokenTable {
    type Output = [u16; 8];

    fn index(&self, index: usize) -> &Self::Output {
        &self.entries[index]
    }
}

static TABLE: LazyLock<TokenTable> = LazyLock::new(TokenTable::new);

pub(super) fn table() -> &'static TokenTable {
    &TABLE
}

fn build_main(entries: &mut [[u16; 8]], at: &mut usize) {
    // Non-pawn pieces: white king, queen, knight, bishop, rook; then black.
    for piece in [1u16, 2, 3, 4, 5, 9, 10, 11, 12, 13] {
        let role = piece & 7;
        let color = piece & 8;
        for file in 0..8i8 {
            for rank in 0..8i8 {
                let from = square(file, rank);
                for (to_file, to_rank) in piece_moves(role, file, rank) {
                    let to = square(to_file, to_rank);
                    for value in [0u16, 2, 3, 4, 5, 6] {
                        let aux = if value == 0 {
                            0
                        } else {
                            (if color == 0 { 8 } else { 0 }) | value
                        };
                        push(entries, at, [from, to, piece, aux, 0, 0, 0, 0]);
                    }
                }
            }
        }
    }

    // Pawns.
    for piece in [6u16, 14] {
        let color = piece & 8;
        for file in 0..8i8 {
            for rank in 0..8i8 {
                if rank == 0 || rank == 7 {
                    continue;
                }
                let from = square(file, rank);
                for (to, capture) in pawn_moves(color, file, rank) {
                    let to_file = to / 8;
                    let to_rank = to % 8;
                    let promotion = (color == 0 && to_rank == 7) || (color == 8 && to_rank == 0);
                    if promotion {
                        if capture {
                            for value in [2u16, 3, 4, 5] {
                                let aux = (if color == 0 { 8 } else { 0 }) | value;
                                for promoted in [2u16, 3, 4, 5] {
                                    let special6 = promoted | color;
                                    push(
                                        entries,
                                        at,
                                        [from, to, piece, aux, to, to, special6, piece],
                                    );
                                }
                            }
                        } else {
                            for promoted in [2u16, 3, 4, 5] {
                                let special6 = promoted | color;
                                push(entries, at, [from, to, piece, 0, to, to, special6, piece]);
                            }
                        }
                    } else if capture {
                        for value in [2u16, 3, 4, 5, 6] {
                            let aux = (if color == 0 { 8 } else { 0 }) | value;
                            push(entries, at, [from, to, piece, aux, 0, 0, 0, 0]);
                            if value == 6
                                && ((color == 0 && rank == 4) || (color == 8 && rank == 3))
                            {
                                let captured = (to_file * 8) | rank as u16;
                                push(
                                    entries,
                                    at,
                                    [
                                        from,
                                        to,
                                        piece,
                                        0,
                                        captured,
                                        captured,
                                        0,
                                        if color == 0 { 14 } else { 6 },
                                    ],
                                );
                            }
                        }
                    } else {
                        push(entries, at, [from, to, piece, 0, 0, 0, 0, 0]);
                    }
                }
            }
        }
    }

    // Orthodox castling: queenside and kingside for each color.
    push(entries, at, [32, 16, 1, 0, 0, 24, 5, 0]);
    push(entries, at, [32, 48, 1, 0, 56, 40, 5, 0]);
    push(entries, at, [39, 23, 9, 0, 7, 31, 13, 0]);
    push(entries, at, [39, 55, 9, 0, 63, 47, 13, 0]);
}

fn build_castling_960(entries: &mut [[u16; 8]], at: &mut usize) {
    for id in 0..960 {
        let (king, rook_q, rook_k) = scharnagl(id);
        for color in [0u16, 8] {
            let rank = if color == 0 { 0 } else { 7 };
            let king_from = square(king, rank);
            let king_q = square(2, rank);
            let king_k = square(6, rank);
            let rook_q_from = square(rook_q, rank);
            let rook_k_from = square(rook_k, rank);
            let rook_q_to = square(3, rank);
            let rook_k_to = square(5, rank);
            let piece = 1 | color;
            let rook_flags = 5 | color;
            push(
                entries,
                at,
                [
                    king_from,
                    king_q,
                    piece,
                    0,
                    rook_q_from,
                    rook_q_to,
                    rook_flags,
                    0,
                ],
            );
            push(
                entries,
                at,
                [
                    king_from,
                    king_k,
                    piece,
                    0,
                    rook_k_from,
                    rook_k_to,
                    rook_flags,
                    0,
                ],
            );
        }
    }
}

fn push(entries: &mut [[u16; 8]], at: &mut usize, entry: [u16; 8]) {
    entries[*at] = entry;
    *at += 1;
}

fn square(file: i8, rank: i8) -> u16 {
    (file as u16) * 8 + rank as u16
}

fn on(file: i8, rank: i8) -> bool {
    (0..8).contains(&file) && (0..8).contains(&rank)
}

fn piece_moves(role: u16, file: i8, rank: i8) -> Vec<(i8, i8)> {
    let mut out = Vec::new();
    match role {
        1 => {
            for (df, dr) in KING_DIRS {
                let (to_file, to_rank) = (file + df, rank + dr);
                if on(to_file, to_rank) {
                    out.push((to_file, to_rank));
                }
            }
        }
        3 => {
            for (df, dr) in KNIGHT_DIRS {
                let (to_file, to_rank) = (file + df, rank + dr);
                if on(to_file, to_rank) {
                    out.push((to_file, to_rank));
                }
            }
        }
        2 | 4 | 5 => {
            for (df, dr) in SLIDER_DIRS {
                if role == 4 && (df == 0 || dr == 0) {
                    continue;
                }
                if role == 5 && df != 0 && dr != 0 {
                    continue;
                }
                let (mut to_file, mut to_rank) = (file + df, rank + dr);
                while on(to_file, to_rank) {
                    out.push((to_file, to_rank));
                    to_file += df;
                    to_rank += dr;
                }
            }
        }
        _ => {}
    }
    out
}

/// Pawn destinations in generator order.  `color` is 0 for White and 8 for
/// Black.  The boolean marks a capture.
fn pawn_moves(color: u16, file: i8, rank: i8) -> Vec<(u16, bool)> {
    let mut out = Vec::new();
    if color == 0 {
        if rank == 1 {
            out.push((square(file, rank + 2), false));
        }
        if rank < 7 {
            out.push((square(file, rank + 1), false));
        }
        if rank < 7 {
            if file > 0 {
                out.push((square(file - 1, rank + 1), true));
            }
            if file < 7 {
                out.push((square(file + 1, rank + 1), true));
            }
        }
    } else {
        if rank == 6 {
            out.push((square(file, rank - 2), false));
        }
        if rank > 0 {
            out.push((square(file, rank - 1), false));
        }
        if rank > 0 {
            if file > 0 {
                out.push((square(file - 1, rank - 1), true));
            }
            if file < 7 {
                out.push((square(file + 1, rank - 1), true));
            }
        }
    }
    out
}

/// Scharnagl number to `(king_file, queenside_rook_file, kingside_rook_file)`.
pub(super) fn scharnagl(id: u16) -> (i8, i8, i8) {
    let mut n = usize::from(id);
    let light = [1usize, 3, 5, 7];
    let dark = [0usize, 2, 4, 6];
    let light_bishop = light[n % 4];
    n /= 4;
    let dark_bishop = dark[n % 4];
    n /= 4;
    let mut remaining: Vec<usize> = (0..8)
        .filter(|&file| file != light_bishop && file != dark_bishop)
        .collect();
    let _queen = remaining.remove(n % 6);
    n /= 6;
    let knight_pair = combinations(&remaining, n);
    let mut rest: Vec<usize> = remaining
        .into_iter()
        .filter(|file| !knight_pair.contains(file))
        .collect();
    rest.sort_unstable();
    (rest[1] as i8, rest[0] as i8, rest[2] as i8)
}

/// The `index`-th lexicographic pair from `files`.
fn combinations(files: &[usize], index: usize) -> [usize; 2] {
    let mut seen = 0usize;
    for first in 0..files.len() {
        for second in first + 1..files.len() {
            if seen == index {
                return [files[first], files[second]];
            }
            seen += 1;
        }
    }
    [files[0], files[1]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scharnagl_boundaries() {
        assert_eq!(scharnagl(0), (6, 5, 7));
        assert_eq!(scharnagl(518), (4, 0, 7));
        assert_eq!(scharnagl(959), (1, 0, 2));
    }

    #[test]
    fn generated_table_has_the_expected_size() {
        assert_eq!(table().len(), 65_536);
        assert_eq!(table().entry(0), [0; 8]);
        assert_eq!(table().entry(49_196), [15, 55, 9, 0, 23, 47, 13, 0]);
        assert_eq!(table().entry(49_197), [0; 8]);
    }

    #[test]
    fn inverse_lookup_round_trips_every_generated_entry() {
        for token in 1..=49_196u16 {
            let entry = table().entry(token);
            let got = table()
                .token_for(entry)
                .expect("generated entry has a token");
            assert_eq!(table().entry(got), entry, "token {token}");
        }
        assert_eq!(table().token_for([0; 8]), None);
    }

    #[test]
    fn orthodox_castling_uses_the_fixed_tokens() {
        assert_eq!(table().token_for([32, 16, 1, 0, 0, 24, 5, 0]), Some(45_353));
        assert_eq!(
            table().token_for([32, 48, 1, 0, 56, 40, 5, 0]),
            Some(45_354)
        );
        assert_eq!(
            table().token_for([39, 23, 9, 0, 7, 31, 13, 0]),
            Some(45_355)
        );
        assert_eq!(
            table().token_for([39, 55, 9, 0, 63, 47, 13, 0]),
            Some(45_356)
        );
    }
}
