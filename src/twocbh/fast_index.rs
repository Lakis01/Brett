//! Zero-allocation mainline move streamer for 2CBH (`.2cbg`).
//!
//! Unlike [`super::moves::decode_moves`], which constructs a full [`crate::domain::Game`]
//! AST with variations, comments, and annotations, this module decodes only the
//! mainline moves directly on a stack-allocated [`Chess`] position, emitting
//! `(position_key, move_code)` pairs to a caller-supplied closure.

use anyhow::{Context, Result};

use super::{moves, tokens};
use crate::chess::{Chess, Position};
use crate::position::encode_move;

const END_LINE: u16 = 0xffff;
const VARIATION: u16 = 0xfffd;
const START: u16 = 0xfffc;
const START_960: u16 = 0xfffb;

/// Stream mainline moves from a raw 2CBH token slice without allocating game trees.
///
/// Calls `consumer(position_key, move_code)` for each mainline move up to `max_plies`.
pub fn stream_2cbh_mainline<F>(
    start_pos: &Chess,
    tokens: &[u16],
    _chess960: bool,
    max_plies: u32,
    consumer: &mut F,
) -> Result<()>
where
    F: FnMut(u64, u32),
{
    let mut current_pos = start_pos.clone();
    let mut plies = 0u32;

    for &token in tokens {
        if token == END_LINE || plies >= max_plies {
            break;
        }
        match token {
            VARIATION | START | START_960 => continue,
            0 => {
                current_pos = current_pos
                    .swap_turn()
                    .context("null move leaves the side to move in check")?;
                plies += 1;
            }
            code => {
                let entry = tokens::table().entry(code);
                let mv = moves::move_from_token(&current_pos, entry)
                    .with_context(|| format!("decoding 2CBH token {code}"))?;
                let key = current_pos.position_hash();
                if let Some(move_code) = encode_move(mv) {
                    consumer(key, move_code);
                }
                current_pos = current_pos
                    .play(mv)
                    .with_context(|| format!("playing 2CBH token {code}"))?;
                plies += 1;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Game;

    fn sample_tokens() -> Vec<u16> {
        // e4, c5, Nf3, d6, d4, cxd4
        vec![44351, 45101, 12895, 33643, 15463, 44731]
    }

    #[test]
    fn stream_2cbh_mainline_matches_decode_moves() {
        let mut tokens = sample_tokens();
        tokens.push(END_LINE);

        let start_pos = Chess::default();
        let mut game = Game::default();
        moves::decode_moves(&mut game, &tokens, false).unwrap();

        let mut expected = Vec::new();
        let mut curr = 0;
        while let Some(&child) = game.nodes[curr].children.first() {
            let parent_pos = &game.nodes[curr].pos;
            let ply = game.nodes[child].mv.as_ref().unwrap();
            let ucimove = crate::chess::UciMove::from_ascii(ply.uci.as_bytes()).unwrap();
            let mv = ucimove.to_move(parent_pos).unwrap();
            expected.push((parent_pos.position_hash(), encode_move(mv).unwrap()));
            curr = child;
        }

        let mut streamed = Vec::new();
        stream_2cbh_mainline(&start_pos, &tokens, false, u32::MAX, &mut |key, code| {
            streamed.push((key, code));
        })
        .unwrap();

        assert_eq!(streamed, expected);
        assert_eq!(streamed.len(), 6);
    }

    #[test]
    fn stream_2cbh_mainline_ignores_variations_after_terminator() {
        let mut tokens = sample_tokens();
        // Add variation marker, terminator, and variation moves
        tokens.push(VARIATION);
        tokens.push(END_LINE);
        tokens.extend_from_slice(&[35113, 45354, VARIATION, END_LINE, 44258]);

        let start_pos = Chess::default();
        let mut streamed = Vec::new();
        stream_2cbh_mainline(&start_pos, &tokens, false, u32::MAX, &mut |key, code| {
            streamed.push((key, code));
        })
        .unwrap();

        assert_eq!(
            streamed.len(),
            6,
            "must decode exactly the 6 mainline moves and stop at END_LINE"
        );
    }

    #[test]
    fn stream_2cbh_mainline_honours_max_plies() {
        let mut tokens = sample_tokens();
        tokens.push(END_LINE);

        let start_pos = Chess::default();
        let mut streamed = Vec::new();
        stream_2cbh_mainline(&start_pos, &tokens, false, 3, &mut |key, code| {
            streamed.push((key, code));
        })
        .unwrap();

        assert_eq!(streamed.len(), 3, "must halt exactly at max_plies");
    }

    #[test]
    fn stream_2cbh_mainline_handles_chess960() {
        // Scharnagl start 518 (standard chess is 518)
        let pos = moves::start_position_960(518).unwrap();
        let mut tokens = sample_tokens();
        tokens.push(END_LINE);

        let mut streamed = Vec::new();
        stream_2cbh_mainline(&pos, &tokens, true, u32::MAX, &mut |key, code| {
            streamed.push((key, code));
        })
        .unwrap();

        assert_eq!(streamed.len(), 6);
    }
}

