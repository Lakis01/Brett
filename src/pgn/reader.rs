//! Streaming PGN reader with logical byte-offset accounting.
//!
//! [`Reader`] consumes a byte stream one game at a time without loading an
//! entire multi-gigabyte database into memory. [`Reader::stream_position`]
//! returns the exact byte offset of the unconsumed stream position (excluding
//! buffered read-ahead), so callers can record `[start, end)` byte ranges for
//! lazy random-access loading.

use std::io::{self, Read};
use std::ops::ControlFlow;

use crate::chess::{San, SanPlus};

const BUFFER_CAP: usize = 64 * 1024;

/// Visitor callbacks invoked by [`Reader::read_game`].
pub trait Visitor {
    /// Accumulator constructed while reading header tag pairs.
    type Tags;
    /// Accumulator constructed while reading movetext.
    type Movetext;
    /// Final value returned for one game.
    type Output;

    /// Called when a new game starts before its first tag (or movetext if
    /// tagless).
    fn begin_tags(&mut self) -> ControlFlow<Self::Output, Self::Tags>;

    /// Called for each `[Name "Value"]` header tag pair.
    fn tag(&mut self, tags: &mut Self::Tags, name: &str, value: &str) -> ControlFlow<Self::Output> {
        let _ = (tags, name, value);
        ControlFlow::Continue(())
    }

    /// Called once all tags for the game have been read.
    fn begin_movetext(&mut self, tags: Self::Tags) -> ControlFlow<Self::Output, Self::Movetext>;

    /// Whether the reader should invoke movetext callbacks (`san`, `nag`,
    /// `comment`, `begin_variation`, `end_variation`). When `false`, the
    /// reader still tokenizes and advances to the next game boundary.
    fn wants_movetext(&self) -> bool {
        true
    }

    /// Called for each SAN move token (including null moves `--`, `Z0`, `z0`).
    fn san(&mut self, mt: &mut Self::Movetext, san: SanPlus) -> ControlFlow<Self::Output> {
        let _ = (mt, san);
        ControlFlow::Continue(())
    }

    /// Called for each numeric annotation glyph (`$n` or suffix `!`/`?`/etc.).
    fn nag(&mut self, mt: &mut Self::Movetext, nag: u8) -> ControlFlow<Self::Output> {
        let _ = (mt, nag);
        ControlFlow::Continue(())
    }

    /// Called for each `{...}` or `;...` comment.
    fn comment(&mut self, mt: &mut Self::Movetext, comment: &str) -> ControlFlow<Self::Output> {
        let _ = (mt, comment);
        ControlFlow::Continue(())
    }

    /// Called when a `(` variation starts.
    fn begin_variation(&mut self, mt: &mut Self::Movetext) -> ControlFlow<Self::Output> {
        let _ = mt;
        ControlFlow::Continue(())
    }

    /// Called when a `)` variation ends (or is auto-closed at game end / EOF).
    fn end_variation(&mut self, mt: &mut Self::Movetext) -> ControlFlow<Self::Output> {
        let _ = mt;
        ControlFlow::Continue(())
    }

    /// Called at the end of the game to produce the final output.
    fn end_game(&mut self, mt: Self::Movetext) -> Self::Output;
}

/// Streaming PGN parser over any [`Read`] source.
pub struct Reader<R> {
    reader: R,
    buf: Vec<u8>,
    pos: usize,
    stream_start: u64,
    eof: bool,
    line_start: bool,
}

impl<R: Read> Reader<R> {
    /// Creates a new streaming PGN reader.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buf: Vec::with_capacity(BUFFER_CAP),
            pos: 0,
            stream_start: 0,
            eof: false,
            line_start: true,
        }
    }

    /// Logical byte offset in the underlying stream, excluding buffered
    /// lookahead.
    #[must_use]
    pub fn stream_position(&self) -> u64 {
        self.stream_start + self.pos as u64
    }

    /// Ensures at least `n` unconsumed bytes are available in `self.buf[self.pos..]`,
    /// unless EOF is reached first.
    fn ensure(&mut self, n: usize) -> io::Result<()> {
        if self.buf.len() - self.pos >= n || self.eof {
            return Ok(());
        }
        if self.pos > 0 {
            self.stream_start += self.pos as u64;
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let mut scratch = [0u8; 8192];
        while self.buf.len() - self.pos < n && !self.eof {
            let needed = (BUFFER_CAP.max(n)).saturating_sub(self.buf.len());
            let to_read = needed.clamp(1, scratch.len());
            let read = self.reader.read(&mut scratch[..to_read])?;
            if read == 0 {
                self.eof = true;
                break;
            }
            self.buf.extend_from_slice(&scratch[..read]);
        }
        Ok(())
    }

    fn peek_byte(&mut self) -> io::Result<Option<u8>> {
        self.ensure(1)?;
        Ok(self.buf.get(self.pos).copied())
    }

    fn peek_n(&mut self, n: usize) -> io::Result<&[u8]> {
        self.ensure(n)?;
        let avail = (self.buf.len() - self.pos).min(n);
        Ok(&self.buf[self.pos..self.pos + avail])
    }

    fn next_byte(&mut self) -> io::Result<Option<u8>> {
        self.ensure(1)?;
        let Some(&b) = self.buf.get(self.pos) else {
            return Ok(None);
        };
        self.pos += 1;
        if b == b'\n' || b == b'\r' {
            self.line_start = true;
        } else if !b.is_ascii_whitespace() {
            self.line_start = false;
        }
        Ok(Some(b))
    }

    fn skip_bom_at_start(&mut self) -> io::Result<()> {
        if self.stream_position() == 0 && self.peek_n(3)? == [0xEF, 0xBB, 0xBF] {
            self.pos += 3;
        }
        Ok(())
    }

    fn skip_line(&mut self) -> io::Result<()> {
        while let Some(b) = self.next_byte()? {
            if b == b'\n' {
                break;
            }
        }
        self.line_start = true;
        Ok(())
    }

    fn skip_brace_comment(&mut self) -> io::Result<()> {
        while let Some(b) = self.next_byte()? {
            if b == b'}' {
                break;
            }
        }
        Ok(())
    }

    fn read_brace_comment(&mut self) -> io::Result<String> {
        let mut bytes = Vec::new();
        while let Some(b) = self.next_byte()? {
            if b == b'}' {
                break;
            }
            bytes.push(b);
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn read_line_comment(&mut self) -> io::Result<String> {
        let mut bytes = Vec::new();
        while let Some(b) = self.next_byte()? {
            if b == b'\n' {
                break;
            }
            if b != b'\r' {
                bytes.push(b);
            }
        }
        self.line_start = true;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Reads the next game from the stream, returning `Ok(None)` at EOF.
    pub fn read_game<V: Visitor>(&mut self, visitor: &mut V) -> io::Result<Option<V::Output>> {
        self.skip_bom_at_start()?;

        // Skip leading whitespace and `%` escape lines.
        loop {
            let Some(b) = self.peek_byte()? else {
                return Ok(None);
            };
            if b.is_ascii_whitespace() {
                self.next_byte()?;
            } else if self.line_start && b == b'%' {
                self.skip_line()?;
            } else {
                break;
            }
        }

        if self.peek_byte()?.is_none() {
            return Ok(None);
        }

        let mut tags = match visitor.begin_tags() {
            ControlFlow::Continue(tags) => tags,
            ControlFlow::Break(output) => {
                self.skip_remaining_tags()?;
                self.skip_movetext()?;
                self.skip_intergame_space()?;
                return Ok(Some(output));
            }
        };

        // Parse consecutive tag lines until a blank line or a non-`[` token.
        while self.peek_byte()? == Some(b'[') {
            if let Some((name, value)) = self.read_tag()?
                && let ControlFlow::Break(output) = visitor.tag(&mut tags, &name, &value)
            {
                self.skip_remaining_tags()?;
                self.skip_movetext()?;
                self.skip_intergame_space()?;
                return Ok(Some(output));
            }
            if self.skip_post_tag_space()? {
                break;
            }
        }

        let mut movetext = match visitor.begin_movetext(tags) {
            ControlFlow::Continue(mt) => mt,
            ControlFlow::Break(output) => {
                self.skip_movetext()?;
                self.skip_intergame_space()?;
                return Ok(Some(output));
            }
        };

        if visitor.wants_movetext() {
            if let ControlFlow::Break(output) = self.parse_movetext(visitor, &mut movetext)? {
                self.skip_movetext()?;
                self.skip_intergame_space()?;
                return Ok(Some(output));
            }
        } else {
            self.skip_movetext()?;
        }

        // Advance past inter-game whitespace so `stream_position()` points directly
        // at the next game's `[` (or EOF).
        self.skip_intergame_space()?;

        Ok(Some(visitor.end_game(movetext)))
    }

    fn skip_intergame_space(&mut self) -> io::Result<()> {
        while let Some(b) = self.peek_byte()? {
            if b.is_ascii_whitespace() {
                self.next_byte()?;
            } else if self.line_start && b == b'%' {
                self.skip_line()?;
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Skips horizontal space and line endings after a tag, returning `true` if
    /// a blank line was encountered (which terminates the tag section). Always
    /// stops before `[` so the next tag or game boundary is preserved.
    fn skip_post_tag_space(&mut self) -> io::Result<bool> {
        let mut newlines = 0usize;
        loop {
            let Some(b) = self.peek_byte()? else {
                return Ok(true);
            };
            match b {
                b' ' | b'\t' => {
                    self.next_byte()?;
                }
                b'\r' => {
                    self.next_byte()?;
                    if self.peek_byte()? == Some(b'\n') {
                        self.next_byte()?;
                    }
                    newlines += 1;
                }
                b'\n' => {
                    self.next_byte()?;
                    newlines += 1;
                }
                b'%' if self.line_start => {
                    self.skip_line()?;
                    newlines = newlines.max(1);
                }
                _ => break,
            }
        }
        Ok(newlines >= 2)
    }

    fn skip_remaining_tags(&mut self) -> io::Result<()> {
        while self.peek_byte()? == Some(b'[') {
            let _ = self.read_tag()?;
            if self.skip_post_tag_space()? {
                break;
            }
        }
        Ok(())
    }

    /// Parses `[Name "Value"]` with `\"` and `\\` escapes and UTF-8 lossy values.
    /// Malformed tags are tolerated without aborting the game.
    fn read_tag(&mut self) -> io::Result<Option<(String, String)>> {
        // Consume '['.
        self.next_byte()?;

        // Skip whitespace before tag name.
        while let Some(b) = self.peek_byte()? {
            if b == b' ' || b == b'\t' {
                self.next_byte()?;
            } else {
                break;
            }
        }

        // Read tag name until whitespace, '"', ']', or line end.
        let mut name_bytes = Vec::new();
        while let Some(b) = self.peek_byte()? {
            if b.is_ascii_whitespace() || b == b'"' || b == b']' {
                break;
            }
            self.next_byte()?;
            name_bytes.push(b);
        }

        // Skip whitespace before opening quote.
        while let Some(b) = self.peek_byte()? {
            if b == b' ' || b == b'\t' {
                self.next_byte()?;
            } else {
                break;
            }
        }

        // Expect opening quote; if malformed, skip to ']' or end of line.
        if self.peek_byte()? != Some(b'"') {
            while let Some(b) = self.next_byte()? {
                if b == b']' || b == b'\n' {
                    break;
                }
            }
            return Ok(None);
        }
        self.next_byte()?; // consume '"'

        let mut value_bytes = Vec::new();
        while let Some(b) = self.next_byte()? {
            match b {
                b'"' => break,
                b'\\' => {
                    if let Some(escaped) = self.next_byte()? {
                        if escaped == b'"' || escaped == b'\\' {
                            value_bytes.push(escaped);
                        } else {
                            value_bytes.push(b'\\');
                            value_bytes.push(escaped);
                        }
                    } else {
                        value_bytes.push(b'\\');
                        break;
                    }
                }
                b'\n' | b'\r' => break,
                _ => value_bytes.push(b),
            }
        }

        // Consume until closing ']' or newline.
        while let Some(b) = self.peek_byte()? {
            if b == b'\n' || b == b'\r' {
                break;
            }
            self.next_byte()?;
            if b == b']' {
                break;
            }
        }

        if name_bytes.is_empty() {
            return Ok(None);
        }

        let name = String::from_utf8_lossy(&name_bytes).into_owned();
        let value = String::from_utf8_lossy(&value_bytes).into_owned();
        Ok(Some((name, value)))
    }

    fn parse_movetext<V: Visitor>(
        &mut self,
        visitor: &mut V,
        mt: &mut V::Movetext,
    ) -> io::Result<ControlFlow<V::Output>> {
        let mut depth = 0usize;
        let mut token_buf = Vec::with_capacity(16);

        while let Some(b) = self.peek_byte()? {
            if b.is_ascii_whitespace() {
                self.next_byte()?;
                continue;
            }

            if self.line_start && b == b'%' {
                self.skip_line()?;
                continue;
            }

            // A '[' at the start of a line begins the next game's tag section.
            if self.line_start && b == b'[' {
                break;
            }

            match b {
                b'{' => {
                    self.next_byte()?;
                    let text = self.read_brace_comment()?;
                    if let ControlFlow::Break(out) = visitor.comment(mt, &text) {
                        return Ok(ControlFlow::Break(out));
                    }
                }
                b';' => {
                    self.next_byte()?;
                    let text = self.read_line_comment()?;
                    if let ControlFlow::Break(out) = visitor.comment(mt, &text) {
                        return Ok(ControlFlow::Break(out));
                    }
                }
                b'(' => {
                    self.next_byte()?;
                    depth += 1;
                    if let ControlFlow::Break(out) = visitor.begin_variation(mt) {
                        return Ok(ControlFlow::Break(out));
                    }
                }
                b')' => {
                    self.next_byte()?;
                    if depth > 0 {
                        depth -= 1;
                        if let ControlFlow::Break(out) = visitor.end_variation(mt) {
                            return Ok(ControlFlow::Break(out));
                        }
                    }
                }
                b'$' => {
                    self.next_byte()?;
                    let mut val = 0u32;
                    let mut any = false;
                    while let Some(d) = self.peek_byte()? {
                        if d.is_ascii_digit() {
                            self.next_byte()?;
                            val = val.saturating_mul(10).saturating_add(u32::from(d - b'0'));
                            any = true;
                        } else {
                            break;
                        }
                    }
                    if any
                        && let Ok(nag) = u8::try_from(val)
                        && let ControlFlow::Break(out) = visitor.nag(mt, nag)
                    {
                        return Ok(ControlFlow::Break(out));
                    }
                }
                b'*' => {
                    self.next_byte()?;
                }
                _ => {
                    token_buf.clear();
                    self.read_word(&mut token_buf)?;
                    if let ControlFlow::Break(out) = Self::emit_token(visitor, mt, &token_buf)? {
                        return Ok(ControlFlow::Break(out));
                    }
                }
            }
        }

        while depth > 0 {
            depth -= 1;
            if let ControlFlow::Break(out) = visitor.end_variation(mt) {
                return Ok(ControlFlow::Break(out));
            }
        }

        Ok(ControlFlow::Continue(()))
    }

    fn skip_movetext(&mut self) -> io::Result<()> {
        while let Some(b) = self.peek_byte()? {
            if b.is_ascii_whitespace() {
                self.next_byte()?;
                continue;
            }
            if self.line_start && b == b'%' {
                self.skip_line()?;
                continue;
            }
            if self.line_start && b == b'[' {
                break;
            }
            match b {
                b'{' => {
                    self.next_byte()?;
                    self.skip_brace_comment()?;
                }
                b';' => {
                    self.next_byte()?;
                    self.skip_line()?;
                }
                _ => {
                    self.next_byte()?;
                }
            }
        }
        Ok(())
    }

    fn read_word(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        while let Some(b) = self.peek_byte()? {
            if b.is_ascii_whitespace() || matches!(b, b'{' | b'}' | b'(' | b')' | b';' | b'$') {
                break;
            }
            self.next_byte()?;
            out.push(b);
        }
        Ok(())
    }

    fn emit_token<V: Visitor>(
        visitor: &mut V,
        mt: &mut V::Movetext,
        raw: &[u8],
    ) -> io::Result<ControlFlow<V::Output>> {
        let mut slice = raw;

        // Strip leading move number (`1.`, `1...`, `12.e4`).
        if let Some(first) = slice.first()
            && first.is_ascii_digit()
        {
            let digits_end = slice
                .iter()
                .position(|b| !b.is_ascii_digit())
                .unwrap_or(slice.len());
            let after_digits = &slice[digits_end..];
            if after_digits.starts_with(b".") {
                let dots_end = after_digits
                    .iter()
                    .position(|&b| b != b'.')
                    .unwrap_or(after_digits.len());
                slice = &after_digits[dots_end..];
            } else if slice == b"1-0" || slice == b"0-1" || slice == b"1/2-1/2" {
                return Ok(ControlFlow::Continue(()));
            } else if after_digits.is_empty() {
                // Bare move number.
                return Ok(ControlFlow::Continue(()));
            }
        }

        // Strip any remaining leading dots (e.g. `...e5`).
        while let Some(b'.') = slice.first() {
            slice = &slice[1..];
        }

        if slice.is_empty() {
            return Ok(ControlFlow::Continue(()));
        }

        // Result tokens and ignored en-passant markers.
        if matches!(slice, b"1-0" | b"0-1" | b"1/2-1/2" | b"*" | b"e.p." | b"ep") {
            return Ok(ControlFlow::Continue(()));
        }

        // Standalone NAG glyphs (`!`, `?`, `!!`, `??`, `!?`, `?!`).
        if let Some(nag) = glyph_nag(slice) {
            return Ok(visitor.nag(mt, nag));
        }

        // Null moves (`--`, `Z0`, `z0`).
        if matches!(slice, b"--" | b"Z0" | b"z0") {
            let san = SanPlus {
                san: San::Null,
                text: "--".to_owned(),
            };
            return Ok(visitor.san(mt, san));
        }

        // Split trailing annotation glyphs (`!`, `?`, `!!`, `??`, `!?`, `?!`) off the SAN token.
        let (san_bytes, trailing_nag) = split_trailing_nag(slice);
        if san_bytes.is_empty() {
            if let Some(nag) = trailing_nag {
                return Ok(visitor.nag(mt, nag));
            }
            return Ok(ControlFlow::Continue(()));
        }

        // Re-check null move if a glyph was attached to `--` / `Z0` / `z0`.
        if matches!(san_bytes, b"--" | b"Z0" | b"z0") {
            let san = SanPlus {
                san: San::Null,
                text: "--".to_owned(),
            };
            if let ControlFlow::Break(out) = visitor.san(mt, san) {
                return Ok(ControlFlow::Break(out));
            }
            if let Some(nag) = trailing_nag
                && let ControlFlow::Break(out) = visitor.nag(mt, nag)
            {
                return Ok(ControlFlow::Break(out));
            }
            return Ok(ControlFlow::Continue(()));
        }

        let raw_text = String::from_utf8_lossy(san_bytes);
        let text = normalize_castling(&raw_text);
        if text.is_empty() {
            return Ok(ControlFlow::Continue(()));
        }

        let san = SanPlus {
            san: San::Raw(text.clone()),
            text,
        };
        if let ControlFlow::Break(out) = visitor.san(mt, san) {
            return Ok(ControlFlow::Break(out));
        }
        if let Some(nag) = trailing_nag
            && let ControlFlow::Break(out) = visitor.nag(mt, nag)
        {
            return Ok(ControlFlow::Break(out));
        }

        Ok(ControlFlow::Continue(()))
    }
}

fn glyph_nag(bytes: &[u8]) -> Option<u8> {
    match bytes {
        b"!" => Some(1),
        b"?" => Some(2),
        b"!!" => Some(3),
        b"??" => Some(4),
        b"!?" => Some(5),
        b"?!" => Some(6),
        b"=" => Some(10),
        b"~" => Some(13),
        b"+=" | b"+/=" => Some(14),
        b"=+" | b"=/+" => Some(15),
        b"+/-" => Some(16),
        b"-/+" => Some(17),
        b"+-" => Some(18),
        b"-+" => Some(19),
        b"+--" => Some(20),
        b"--+" => Some(21),
        b"@/=" | b"=/~" | b"~/=" => Some(44),
        b"N" => None,
        _ => match std::str::from_utf8(bytes).ok()? {
            "□" => Some(7),
            "∞" => Some(13),
            "⩲" => Some(14),
            "⩱" => Some(15),
            "±" => Some(16),
            "∓" => Some(17),
            "+−" => Some(18),
            "−+" => Some(19),
            "⨀" | "◯" => Some(22),
            "↑" => Some(36),
            "→" => Some(40),
            "⯹" => Some(44),
            "⊕" => Some(138),
            "⇆" => Some(132),
            "∆" => Some(140),
            _ => None,
        },
    }
}

fn split_trailing_nag(bytes: &[u8]) -> (&[u8], Option<u8>) {
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], b'!' | b'?') {
        end -= 1;
    }
    let suffix = &bytes[end..];
    if suffix.is_empty() {
        (bytes, None)
    } else {
        (&bytes[..end], glyph_nag(suffix))
    }
}

fn normalize_castling(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("0-0-0") {
        format!("O-O-O{rest}")
    } else if let Some(rest) = text.strip_prefix("0-0") {
        format!("O-O{rest}")
    } else {
        text.to_owned()
    }
}
