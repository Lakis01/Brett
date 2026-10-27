//! Shredder `.bkt` opening-book reader.
//!
//! A BKT file is a two-word Zobrist hash table.  The first 256 buckets are
//! selected by seven hash bits plus the side to move; additional overflow
//! buckets are appended and reached through a footer in each 1552-byte bucket.
//! Every bucket contains 32 slots of 48 bytes followed by four little-endian
//! `i32` values: an unused word, an overflow mask and two next-bucket offsets.
//!
//! Each slot holds the two hash words and a 40-byte move record.  The record
//! contains Shredder's packed move, a quality nibble, average-rating sums and
//! White-perspective win/draw/loss totals.  The reader below is independent
//! Rust code written from the published format layout; the Zobrist constants
//! in `tables.rs` are fixed format data.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use memmap2::Mmap;
use shakmaty::uci::UciMove;
use shakmaty::{Chess, Color, Position, Role, Square};

use super::{BookMove, move_code};

mod tables;

const BUCKET_SIZE: usize = 1552;
const SLOT_SIZE: usize = 48;
const SLOTS: usize = 32;
const BASE_BUCKETS: usize = 256;
const ENTRY_SIZE: usize = 40;

/// The fixed nibble-to-piece mapping used by BKT's packed moves.
const PIECE_NIBBLE: [u8; 16] = [0, 1, 6, 5, 0, 4, 3, 2, 0, 7, 12, 11, 0, 10, 9, 8];

pub(super) struct BktBook {
    mmap: Mmap,
    buckets: usize,
    entries: usize,
}

impl BktBook {
    pub(super) fn open(mmap: Mmap) -> Result<Self> {
        if mmap.len() < BASE_BUCKETS * BUCKET_SIZE || !mmap.len().is_multiple_of(BUCKET_SIZE) {
            bail!("BKT book is not a whole number of 1552-byte buckets");
        }
        let buckets = mmap.len() / BUCKET_SIZE;
        let mut entries = 0usize;
        for bucket in 0..buckets {
            let base = bucket * BUCKET_SIZE;
            for slot in 0..SLOTS {
                let at = base + slot * SLOT_SIZE;
                if mmap[at..at + SLOT_SIZE].iter().any(|&byte| byte != 0) {
                    entries += 1;
                }
            }
        }
        Ok(Self {
            mmap,
            buckets,
            entries,
        })
    }

    pub(super) fn summary(&self) -> String {
        format!("{} buckets · {} entries", self.buckets, self.entries)
    }

    pub(super) fn moves(&self, pos: &Chess) -> Result<Vec<BookMove>> {
        let (hash_a, hash_b) = position_hash(pos);
        let mut offset = bucket_offset(hash_a, pos.turn());
        let mut moves: BTreeMap<u32, BookMove> = BTreeMap::new();
        let mut hops = 0usize;

        while offset + BUCKET_SIZE <= self.mmap.len() && hops < 256 {
            for slot in 0..SLOTS {
                let at = offset + slot * SLOT_SIZE;
                if read_i32(&self.mmap, at) != hash_a || read_i32(&self.mmap, at + 4) != hash_b {
                    continue;
                }
                if let Some(entry) = decode_entry(&self.mmap, at + 8, pos) {
                    let code = move_code(entry.mv);
                    match moves.entry(code) {
                        std::collections::btree_map::Entry::Vacant(vacant) => {
                            vacant.insert(entry);
                        }
                        std::collections::btree_map::Entry::Occupied(mut occupied) => {
                            // Multiple overflow buckets can theoretically
                            // repeat a position; combine their statistics.
                            let existing = occupied.get_mut();
                            existing.weight = existing.weight.saturating_add(entry.weight);
                            existing.games = existing.games.saturating_add(entry.games);
                            existing.white = existing.white.saturating_add(entry.white);
                            existing.draw = existing.draw.saturating_add(entry.draw);
                            existing.black = existing.black.saturating_add(entry.black);
                        }
                    }
                }
            }

            let footer = offset + SLOTS * SLOT_SIZE;
            let _unknown = read_i32(&self.mmap, footer);
            let mask = read_i32(&self.mmap, footer + 4);
            let next_zero = read_i32(&self.mmap, footer + 8);
            let next_nonzero = read_i32(&self.mmap, footer + 12);
            let next = if (hash_b & mask) != 0 {
                next_nonzero
            } else {
                next_zero
            };
            if next <= 0 {
                break;
            }
            offset = next as usize;
            hops += 1;
        }

        let mut moves: Vec<BookMove> = moves.into_values().collect();
        moves.sort_unstable_by(|a, b| {
            b.games
                .cmp(&a.games)
                .then(b.weight.cmp(&a.weight))
                .then_with(|| move_code(a.mv).cmp(&move_code(b.mv)))
        });
        Ok(moves)
    }
}

fn bucket_offset(hash_a: i32, turn: Color) -> usize {
    let mut bucket = (hash_a as u32 & 0x7f) as usize;
    if turn == Color::Black {
        bucket += 128;
    }
    bucket * BUCKET_SIZE
}

fn decode_entry(mmap: &[u8], at: usize, pos: &Chess) -> Option<BookMove> {
    if at + ENTRY_SIZE > mmap.len() {
        return None;
    }
    let packed = read_i32(mmap, at) as u32;
    let from = (packed & 0x7f) as i32 - 1;
    let to = ((packed >> 7) & 0x7f) as i32 - 1;
    if !(0..64).contains(&from) || !(0..64).contains(&to) {
        return None;
    }
    let promotion = PIECE_NIBBLE[((packed >> 14) & 0xf) as usize];
    let promotion = role_from_code(promotion);
    let uci = UciMove::Normal {
        from: Square::new(from as u32),
        to: Square::new(to as u32),
        promotion,
    };
    let mv = uci.to_move(pos).ok()?;

    let quality = match mmap[at + 6] & 0x0f {
        1 => Some('a'),
        2 => Some('b'),
        3 => Some('c'),
        4 => Some('d'),
        5 => Some('e'),
        6 => Some('f'),
        7 => Some('g'),
        _ => None,
    };
    let white_wins = read_i32(mmap, at + 24).max(0) as u64;
    let draws = read_i32(mmap, at + 28).max(0) as u64;
    let white_losses = read_i32(mmap, at + 32).max(0) as u64;
    let games = white_wins
        .saturating_add(draws)
        .saturating_add(white_losses);
    let (wins, losses) = if pos.turn() == Color::White {
        (white_wins, white_losses)
    } else {
        (white_losses, white_wins)
    };

    Some(BookMove {
        mv,
        weight: games,
        games,
        white: wins,
        draw: draws,
        black: losses,
        annotation: None,
        recommendation: None,
        quality,
    })
}

fn role_from_code(code: u8) -> Option<Role> {
    match code {
        2 | 8 => Some(Role::Queen),
        3 | 9 => Some(Role::Rook),
        4 | 10 => Some(Role::Bishop),
        5 | 11 => Some(Role::Knight),
        _ => None,
    }
}

fn read_i32(mmap: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(mmap[at..at + 4].try_into().expect("four bytes"))
}

/// Compute the two BKT hash words for a position.
///
/// BKT uses a1 = 0 through h8 = 63.  White pieces use rows 1–6 and black
/// pieces rows 7–12; empty squares contribute nothing.
pub(crate) fn position_hash(pos: &Chess) -> (i32, i32) {
    let mut hash_a = 0i32;
    let mut hash_b = 0i32;
    for square in Square::ALL {
        let Some(piece) = pos.board().piece_at(square) else {
            continue;
        };
        let index = usize::from(square.file()) + usize::from(square.rank()) * 8;
        let row = piece_row(piece.role, piece.color);
        hash_a ^= tables::ZOBRIST_A[row][index];
        hash_b ^= tables::ZOBRIST_B[row][index];
    }
    (hash_a, hash_b)
}

fn piece_row(role: Role, color: Color) -> usize {
    let base = match role {
        Role::King => 1,
        Role::Queen => 2,
        Role::Rook => 3,
        Role::Bishop => 4,
        Role::Knight => 5,
        Role::Pawn => 6,
    };
    if color == Color::White {
        base
    } else {
        base + 6
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_position_hash_matches_the_bkt_table() {
        assert_eq!(
            position_hash(&Chess::default()),
            (0x7623_eebc_u32 as i32, 0x5fd4_2fda_u32 as i32)
        );
    }
}
