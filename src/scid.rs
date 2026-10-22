//! Native reader for SCID databases (`.si4`/`.sn4`/`.sg4` and the older
//! `.si3`/`.sn3`/`.sg3` trio).
//!
//! The format is documented by the SCID project. This is an independent
//! implementation written from the published on-disk layout; no SCID source
//! code is linked or copied. Games are decoded lazily: opening a database
//! reads only the index and the namebase, and [`ScidLibrary::load`] decodes
//! one record on demand.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use shakmaty::fen::Fen;
use shakmaty::{CastlingMode, Chess, Color, File as ChessFile, Move, Position, Role, Square};

use crate::domain::{Game, Node, Ply};
use crate::library::{GameLibrary, IndexedGame, SourceMeta};

const INDEX_MAGIC: &[u8; 8] = b"Scid.si\0";
const NAME_MAGIC: &[u8; 8] = b"Scid.sn\0";

const INDEX_HEADER_V3: usize = 128;
const INDEX_HEADER_V4: usize = 182;
const ENTRY_SIZE_V3: usize = 46;
const ENTRY_SIZE_V4: usize = 47;

const COMMON_TAGS: [&str; 10] = [
    "WhiteCountry",
    "BlackCountry",
    "Annotator",
    "PlyCount",
    "EventDate",
    "Opening",
    "Variation",
    "Setup",
    "Source",
    "SetUp",
];

/// One game's index record. Only the fields Brett needs are kept.
#[derive(Clone, Debug)]
struct Entry {
    offset: u64,
    length: u32,
    white: u32,
    black: u32,
    event: u32,
    site: u32,
    round: u32,
    date: u32,
    event_date: u32,
    result: u8,
    eco: u16,
    white_elo: u16,
    black_elo: u16,
}

/// A SCID database opened through the common [`GameLibrary`] interface.
pub struct ScidLibrary {
    path: PathBuf,
    name_path: PathBuf,
    game_path: PathBuf,
    game_len: u64,
    file_len: u64,
    entries: Vec<Entry>,
    games: Vec<IndexedGame>,
    indexed_in: Duration,
    #[allow(dead_code)]
    version: u16,
    #[allow(dead_code)]
    description: String,
}

impl ScidLibrary {
    /// Open a `.si4`/`.si3` index and its accompanying namebase and game file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let started = Instant::now();
        let path = path.as_ref();
        let mut index =
            File::open(path).with_context(|| format!("opening SCID index {}", path.display()))?;
        let index_len = index.metadata()?.len();

        let mut head = [0u8; INDEX_HEADER_V3];
        index
            .read_exact(&mut head)
            .with_context(|| format!("reading SCID header from {}", path.display()))?;
        if &head[0..8] != INDEX_MAGIC {
            bail!("{} is not a SCID index", path.display());
        }
        let version = be_u16(&head[8..10]);
        if !(300..=400).contains(&version) {
            bail!(
                "{} uses unsupported SCID format version {version}",
                path.display()
            );
        }
        let game_count = be_u24(&head[14..17]) as usize;
        let description = String::from_utf8_lossy(&head[20..128])
            .trim_end_matches(['\0', ' '])
            .to_string();
        let (header_len, entry_size) = if version >= 400 {
            (INDEX_HEADER_V4, ENTRY_SIZE_V4)
        } else {
            (INDEX_HEADER_V3, ENTRY_SIZE_V3)
        };
        let entries_len = game_count
            .checked_mul(entry_size)
            .context("SCID game count is too large")?;
        if index_len < (header_len + entries_len) as u64 {
            bail!(
                "{} is truncated: {} games need {} bytes but the file has {}",
                path.display(),
                game_count,
                header_len + entries_len,
                index_len
            );
        }

        index
            .seek(SeekFrom::Start(header_len as u64))
            .context("seeking to SCID index entries")?;
        let mut raw = vec![0u8; entries_len];
        index
            .read_exact(&mut raw)
            .with_context(|| format!("reading SCID game index from {}", path.display()))?;
        let entries: Vec<Entry> = raw
            .chunks_exact(entry_size)
            .map(|buf| decode_entry(buf, version))
            .collect();

        let suffix = if version >= 400 { "4" } else { "3" };
        let name_path = path.with_extension(format!("sn{suffix}"));
        let game_path = path.with_extension(format!("sg{suffix}"));
        if !name_path.exists() {
            bail!(
                "SCID namebase {} is missing (expected beside {})",
                name_path.display(),
                path.display()
            );
        }
        if !game_path.exists() {
            bail!(
                "SCID game file {} is missing (expected beside {})",
                game_path.display(),
                path.display()
            );
        }
        let name_bytes = fs::read(&name_path)
            .with_context(|| format!("reading SCID namebase {}", name_path.display()))?;
        let names = parse_namebase(&name_bytes)
            .with_context(|| format!("reading SCID namebase {}", name_path.display()))?;
        let game_len = fs::metadata(&game_path)?.len();
        let name_len = fs::metadata(&name_path)?.len();

        let mut games = Vec::with_capacity(entries.len());
        for entry in &entries {
            let headers = base_headers(entry, &names);
            games.push(IndexedGame::scid(headers, entry.offset, entry.length));
        }

        Ok(Self {
            path: path.to_path_buf(),
            name_path,
            game_path,
            game_len,
            file_len: index_len + name_len + game_len,
            entries,
            games,
            indexed_in: started.elapsed(),
            version,
            description,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    pub fn indexed_in(&self) -> Duration {
        self.indexed_in
    }

    pub fn games(&self) -> &[IndexedGame] {
        &self.games
    }

    pub fn get(&self, index: usize) -> Option<&IndexedGame> {
        self.games.get(index)
    }

    pub fn filter(&self, needle: &str) -> Vec<usize> {
        crate::library::filter_games(&self.games, needle)
    }

    /// Decode one game record from the `.sg4`/`.sg3` file.
    pub fn load(&self, index: usize) -> Result<Game> {
        let entry = self
            .entries
            .get(index)
            .with_context(|| format!("SCID game index {index} is out of range"))?;
        if entry.length == 0 {
            bail!("SCID game {index} has an empty record");
        }
        let end = entry
            .offset
            .checked_add(u64::from(entry.length))
            .context("SCID game offset overflow")?;
        if end > self.game_len {
            bail!(
                "SCID game {index} points outside {} ({}..{})",
                self.game_path.display(),
                entry.offset,
                end
            );
        }

        let mut file = File::open(&self.game_path)
            .with_context(|| format!("opening SCID game file {}", self.game_path.display()))?;
        file.seek(SeekFrom::Start(entry.offset))
            .with_context(|| format!("seeking to SCID byte {}", entry.offset))?;
        let mut bytes = vec![0u8; entry.length as usize];
        file.read_exact(&mut bytes)
            .with_context(|| format!("reading SCID game {index}"))?;

        let headers = self
            .games
            .get(index)
            .map(|game| game.headers().to_vec())
            .unwrap_or_default();
        decode_record(&bytes, headers).with_context(|| format!("decoding SCID game {index}"))
    }
}

impl GameLibrary for ScidLibrary {
    fn path(&self) -> &Path {
        ScidLibrary::path(self)
    }

    fn file_len(&self) -> u64 {
        ScidLibrary::file_len(self)
    }

    fn len(&self) -> usize {
        ScidLibrary::len(self)
    }

    fn indexed_in(&self) -> Duration {
        ScidLibrary::indexed_in(self)
    }

    fn games(&self) -> &[IndexedGame] {
        ScidLibrary::games(self)
    }

    fn load(&self, index: usize) -> Result<Game> {
        ScidLibrary::load(self, index)
    }

    fn source_meta(&self) -> Result<SourceMeta> {
        let parts = [
            SourceMeta::read(&self.path)?,
            SourceMeta::read(&self.name_path)?,
            SourceMeta::read(&self.game_path)?,
        ];
        Ok(SourceMeta::combine(&parts))
    }

    fn filter(&self, needle: &str) -> Vec<usize> {
        crate::library::filter_games(&self.games, needle)
    }
}

/// Parse a namebase. The caller must have checked the magic already.
#[derive(Default)]
struct Names {
    players: Vec<String>,
    events: Vec<String>,
    sites: Vec<String>,
    rounds: Vec<String>,
}

fn parse_namebase(bytes: &[u8]) -> Result<Names> {
    if bytes.len() < 36 {
        bail!("namebase header is truncated");
    }
    if &bytes[0..8] != NAME_MAGIC {
        bail!("namebase has the wrong magic");
    }
    let counts = [
        be_u24(&bytes[12..15]) as usize,
        be_u24(&bytes[15..18]) as usize,
        be_u24(&bytes[18..21]) as usize,
        be_u24(&bytes[21..24]) as usize,
    ];
    let max_freq = [
        be_u24(&bytes[24..27]) as usize,
        be_u24(&bytes[27..30]) as usize,
        be_u24(&bytes[30..33]) as usize,
        be_u24(&bytes[33..36]) as usize,
    ];

    let mut cursor = 36usize;
    let mut names = Names {
        players: Vec::new(),
        events: Vec::new(),
        sites: Vec::new(),
        rounds: Vec::new(),
    };
    for kind in 0..4 {
        let count = counts[kind];
        if count == 0 {
            continue;
        }
        let mut list: Vec<String> = Vec::with_capacity(count);
        let id_width = if count >= 65536 { 3 } else { 2 };
        let freq_width = if max_freq[kind] >= 65536 {
            3
        } else if max_freq[kind] >= 256 {
            2
        } else {
            1
        };
        let mut previous: Vec<u8> = Vec::new();
        for i in 0..count {
            let id = read_width(bytes, &mut cursor, id_width)? as usize;
            let _freq = read_width(bytes, &mut cursor, freq_width)?;
            let length = *bytes.get(cursor).context("truncated name length")? as usize;
            cursor += 1;
            let prefix = if i == 0 {
                0
            } else {
                let value = *bytes.get(cursor).context("truncated name prefix")? as usize;
                cursor += 1;
                value
            };
            if prefix > length || prefix > previous.len() {
                bail!("invalid name prefix {prefix} for name {i}");
            }
            let tail_len = length - prefix;
            let tail = bytes
                .get(cursor..cursor + tail_len)
                .context("truncated name text")?;
            cursor += tail_len;
            let mut name = Vec::with_capacity(length);
            name.extend_from_slice(&previous[..prefix]);
            name.extend_from_slice(tail);
            previous = name.clone();
            let name = String::from_utf8_lossy(&name).into_owned();
            if list.len() <= id {
                list.resize(id + 1, String::new());
            }
            list[id] = name;
        }
        match kind {
            0 => names.players = list,
            1 => names.events = list,
            2 => names.sites = list,
            _ => names.rounds = list,
        }
    }
    Ok(names)
}

fn read_width(bytes: &[u8], cursor: &mut usize, width: usize) -> Result<u32> {
    let slice = bytes
        .get(*cursor..*cursor + width)
        .context("truncated SCID namebase field")?;
    *cursor += width;
    Ok(match width {
        1 => u32::from(slice[0]),
        2 => u32::from(be_u16(slice)),
        3 => be_u24(slice),
        _ => unreachable!(),
    })
}

fn base_headers(entry: &Entry, names: &Names) -> Vec<(String, String)> {
    let mut headers = Vec::with_capacity(12);
    headers.push(("Event".to_string(), lookup(&names.events, entry.event)));
    headers.push(("Site".to_string(), lookup(&names.sites, entry.site)));
    headers.push(("Date".to_string(), date_string(entry.date)));
    headers.push(("Round".to_string(), lookup(&names.rounds, entry.round)));
    headers.push(("White".to_string(), lookup(&names.players, entry.white)));
    headers.push(("Black".to_string(), lookup(&names.players, entry.black)));
    headers.push((
        "Result".to_string(),
        result_string(entry.result).to_string(),
    ));
    if let Some(eco) = eco_string(entry.eco) {
        headers.push(("ECO".to_string(), eco));
    }
    if entry.white_elo > 0 {
        headers.push(("WhiteElo".to_string(), entry.white_elo.to_string()));
    }
    if entry.black_elo > 0 {
        headers.push(("BlackElo".to_string(), entry.black_elo.to_string()));
    }
    if entry.event_date > 0 {
        headers.push(("EventDate".to_string(), date_string(entry.event_date)));
    }
    headers
}

fn lookup(names: &[String], id: u32) -> String {
    names
        .get(id as usize)
        .filter(|name| !name.is_empty())
        .cloned()
        .unwrap_or_else(|| "?".to_string())
}

fn date_string(date: u32) -> String {
    if date == 0 {
        return "????.??.??".to_string();
    }
    let year = (date >> 9) & 0x7FFF;
    let month = (date >> 5) & 0x0F;
    let day = date & 0x1F;
    let year = if year == 0 {
        "????".to_string()
    } else {
        format!("{year:04}")
    };
    let month = if (1..=12).contains(&month) {
        format!("{month:02}")
    } else {
        "??".to_string()
    };
    let day = if (1..=31).contains(&day) {
        format!("{day:02}")
    } else {
        "??".to_string()
    };
    format!("{year}.{month}.{day}")
}

fn result_string(result: u8) -> &'static str {
    match result {
        1 => "1-0",
        2 => "0-1",
        3 => "1/2-1/2",
        _ => "*",
    }
}

fn eco_string(code: u16) -> Option<String> {
    if code == 0 {
        return None;
    }
    let index = u32::from(code) - 1;
    let basic = index / 131;
    if basic > 499 {
        return None;
    }
    let letter = (b'A' + (basic / 100) as u8) as char;
    Some(format!("{letter}{:02}", basic % 100))
}

fn decode_entry(buf: &[u8], version: u16) -> Entry {
    let offset = u64::from(be_u32(&buf[0..4]));
    let len_low = u32::from(be_u16(&buf[4..6]));
    let (length, flags, mut p) = if version >= 400 {
        let len_flags = buf[6];
        let length = ((u32::from(len_flags) & 0x80) << 9) | len_low;
        let flags = ((u32::from(len_flags) & 0x3F) << 16) | u32::from(be_u16(&buf[7..9]));
        (length, flags, 9usize)
    } else {
        (len_low, u32::from(be_u16(&buf[6..8])), 8usize)
    };
    let _ = flags;

    let white_black_high = u32::from(buf[p]);
    p += 1;
    let white = ((white_black_high & 0xF0) << 12) | u32::from(be_u16(&buf[p..p + 2]));
    p += 2;
    let black = ((white_black_high & 0x0F) << 16) | u32::from(be_u16(&buf[p..p + 2]));
    p += 2;

    let event_site_round_high = u32::from(buf[p]);
    p += 1;
    let event = ((event_site_round_high & 0xE0) << 11) | u32::from(be_u16(&buf[p..p + 2]));
    p += 2;
    let site = ((event_site_round_high & 0x1C) << 14) | u32::from(be_u16(&buf[p..p + 2]));
    p += 2;
    let round = ((event_site_round_high & 0x03) << 16) | u32::from(be_u16(&buf[p..p + 2]));
    p += 2;

    let var_counts = be_u16(&buf[p..p + 2]);
    p += 2;
    let result = ((var_counts >> 12) & 0x0F) as u8;

    let eco = be_u16(&buf[p..p + 2]);
    p += 2;

    let date_edate = be_u32(&buf[p..p + 4]);
    p += 4;
    let date = date_edate & 0x000F_FFFF;
    let edate_raw = date_edate >> 20;
    let event_date = decode_event_date(date, edate_raw);

    let white_elo_raw = be_u16(&buf[p..p + 2]);
    p += 2;
    let white_elo = white_elo_raw & 0x0FFF;
    let black_elo_raw = be_u16(&buf[p..p + 2]);
    let black_elo = black_elo_raw & 0x0FFF;

    Entry {
        offset,
        length,
        white,
        black,
        event,
        site,
        round,
        date,
        event_date,
        result,
        eco,
        white_elo,
        black_elo,
    }
}

fn decode_event_date(date: u32, packed: u32) -> u32 {
    let eyear = (packed >> 9) & 0x07;
    if eyear == 0 {
        return 0;
    }
    let year = date >> 9;
    let mut year = year + eyear;
    year = year.saturating_sub(4);
    (year << 9) | (packed & 0x01E0) | (packed & 0x1F)
}

fn be_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be_u24(bytes: &[u8]) -> u32 {
    (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Decode a game record. `headers` already contains the standard tags taken
/// from the index; tags stored in the record augment or replace them.
fn decode_record(bytes: &[u8], mut headers: Vec<(String, String)>) -> Result<Game> {
    if bytes.is_empty() {
        bail!("empty game record");
    }
    let mut cursor = 0usize;
    for (key, value) in read_tags(bytes, &mut cursor)? {
        set_header(&mut headers, &key, value);
    }

    if let Some(variant) = header_value(&headers, "Variant") {
        let lower = variant.to_ascii_lowercase();
        if lower.contains("960") || lower.contains("chess960") {
            bail!("Chess960 SCID games are not supported yet");
        }
    }

    let start_flags = *bytes
        .get(cursor)
        .context("truncated start-position flags")?;
    cursor += 1;
    let mut custom_start = false;
    let mut start_pos = Chess::default();
    if start_flags & 1 != 0 {
        let fen = read_cstring(bytes, &mut cursor)?;
        if fen.trim().is_empty() {
            bail!("SCID game has a custom start but no FEN");
        }
        start_pos = parse_fen(&fen)?;
        set_header(&mut headers, "FEN", fen.trim().to_string());
        set_header(&mut headers, "SetUp", "1".to_string());
        custom_start = true;
    } else if let Some(fen) = header_value(&headers, "FEN").map(str::to_string) {
        start_pos = parse_fen(&fen)?;
        set_header(&mut headers, "SetUp", "1".to_string());
        custom_start = true;
    }

    let mut game = Game {
        headers,
        ..Game::default()
    };
    game.nodes[0].pos = start_pos.clone();

    let mut lists = if custom_start {
        PieceLists::from_fen(&start_pos)
    } else {
        PieceLists::standard()
    };
    let mut node_lists = vec![lists.clone()];

    let mut current = 0usize;
    let mut stack: Vec<usize> = Vec::new();
    let mut comment_marks: Vec<usize> = Vec::new();
    let mut moves = 0usize;

    loop {
        let val = *bytes
            .get(cursor)
            .context("SCID move stream ended without an end marker")?;
        cursor += 1;
        match val {
            11 => {
                let nag = *bytes.get(cursor).context("truncated NAG")?;
                cursor += 1;
                if nag != 0 {
                    let nags = &mut game.nodes[current].nags;
                    if !nags.contains(&nag) {
                        nags.push(nag);
                    }
                }
            }
            12 => comment_marks.push(current),
            13 => {
                let parent = game.nodes[current]
                    .parent
                    .context("variation starts before the first move")?;
                stack.push(current);
                current = parent;
                lists = node_lists[current].clone();
            }
            14 => {
                current = stack.pop().context("variation end without a start")?;
                lists = node_lists[current].clone();
            }
            15 => {
                if !stack.is_empty() {
                    bail!("game ended inside an unterminated variation");
                }
                break;
            }
            _ => {
                let parent_pos = game.nodes[current].pos.clone();
                let decoded = decode_move(&parent_pos, &lists, val, bytes, &mut cursor)
                    .with_context(|| {
                        format!(
                            "ply {} (SCID byte {val:#04x}, FEN {})",
                            moves + 1,
                            Fen::from_position(&parent_pos, shakmaty::EnPassantMode::Legal)
                        )
                    })?;
                let mv = match decoded.special {
                    Special::Null => {
                        current = add_null_move(&mut game, current, &parent_pos)?;
                        node_lists.push(lists.clone());
                        moves += 1;
                        if moves > 100_000 {
                            bail!("move stream is implausibly long");
                        }
                        continue;
                    }
                    Special::Castle(kingside) => find_castle(&parent_pos, decoded.from, kingside)
                        .with_context(|| {
                        format!(
                            "illegal castling move from {} ({})",
                            decoded.from,
                            if kingside { "kingside" } else { "queenside" }
                        )
                    })?,
                    Special::Normal => {
                        let to = decoded.to.context("move has no destination")?;
                        find_normal(&parent_pos, decoded.from, to, decoded.promotion).with_context(
                            || {
                                format!(
                                    "illegal SCID move {} -> {} (promotion {:?})",
                                    decoded.from, to, decoded.promotion
                                )
                            },
                        )?
                    }
                };
                let child = game.add_move(current, mv);
                if child < node_lists.len() {
                    lists = node_lists[child].clone();
                } else {
                    lists
                        .apply(&parent_pos, mv)
                        .context("updating SCID piece list")?;
                    node_lists.push(lists.clone());
                }
                current = child;
                moves += 1;
                if moves > 100_000 {
                    bail!("move stream is implausibly long");
                }
            }
        }
    }

    let comments = read_comment_strings(bytes, cursor);
    apply_comments(&mut game, &comment_marks, &comments);

    Ok(game)
}

fn read_tags(bytes: &[u8], cursor: &mut usize) -> Result<Vec<(String, String)>> {
    let mut tags = Vec::new();
    loop {
        let tag_len = *bytes.get(*cursor).context("truncated tag section")? as usize;
        *cursor += 1;
        if tag_len == 0 {
            break;
        }
        let (name, id) = if tag_len > 240 {
            (None, Some(tag_len - 241))
        } else {
            let raw = bytes
                .get(*cursor..*cursor + tag_len)
                .context("truncated tag name")?;
            *cursor += tag_len;
            (Some(String::from_utf8_lossy(raw).into_owned()), None)
        };
        let value_len = if tag_len == 255 {
            3
        } else {
            let value = *bytes.get(*cursor).context("truncated tag value length")? as usize;
            *cursor += 1;
            value
        };
        let raw = bytes
            .get(*cursor..*cursor + value_len)
            .context("truncated tag value")?;
        *cursor += value_len;
        let Some(name) = name.or_else(|| {
            id.and_then(|id| COMMON_TAGS.get(id).copied())
                .map(str::to_string)
        }) else {
            // Spare/common tag IDs that are not textual, and the old SCID2
            // compact EventDate encoding, are simply skipped.
            continue;
        };
        tags.push((name.to_string(), String::from_utf8_lossy(raw).into_owned()));
    }
    Ok(tags)
}

fn read_cstring(bytes: &[u8], cursor: &mut usize) -> Result<String> {
    let tail = bytes
        .get(*cursor..)
        .context("truncated null-terminated string")?;
    let end = tail
        .iter()
        .position(|&byte| byte == 0)
        .context("unterminated null-terminated string")?;
    let value = String::from_utf8_lossy(&tail[..end]).into_owned();
    *cursor += end + 1;
    Ok(value)
}

fn read_comment_strings(bytes: &[u8], mut cursor: usize) -> Vec<String> {
    let mut comments = Vec::new();
    while cursor < bytes.len() {
        if bytes[cursor] == 0 {
            cursor += 1;
            continue;
        }
        let Some(end) = bytes[cursor..].iter().position(|&byte| byte == 0) else {
            break;
        };
        comments.push(String::from_utf8_lossy(&bytes[cursor..cursor + end]).into_owned());
        cursor += end + 1;
    }
    comments
}

fn apply_comments(game: &mut Game, marks: &[usize], comments: &[String]) {
    if comments.is_empty() {
        return;
    }
    let order = pgn_order(game);
    let mut used = 0usize;
    if marks.is_empty() {
        for (&node, comment) in order.iter().zip(comments.iter()) {
            set_comment(game, node, comment);
            used += 1;
        }
    } else {
        for &node in marks {
            let Some(comment) = comments.get(used) else {
                break;
            };
            set_comment(game, node, comment);
            used += 1;
        }
        if used < comments.len()
            && let Some(last) = marks.last()
            && let Some(position) = order.iter().position(|node| node == last)
        {
            for &node in order.iter().skip(position + 1) {
                let Some(comment) = comments.get(used) else {
                    break;
                };
                set_comment(game, node, comment);
                used += 1;
            }
        }
    }
    if used < comments.len() {
        game.errors.push(format!(
            "{} SCID comment(s) could not be attached to a move",
            comments.len() - used
        ));
    }
}

fn set_comment(game: &mut Game, node: usize, comment: &str) {
    let comment = comment.trim();
    if comment.is_empty() {
        return;
    }
    let target = &mut game.nodes[node].comment;
    if target.is_empty() {
        target.push_str(comment);
    } else {
        target.push(' ');
        target.push_str(comment);
    }
}

/// Nodes in PGN order: a move, then alternatives to that move, then the
/// continuation of the move. This is the order SCID uses when storing comments
/// without explicit markers.
fn pgn_order(game: &Game) -> Vec<usize> {
    fn visit(game: &Game, start: usize, out: &mut Vec<usize>) {
        out.push(start);
        let mut node = start;
        loop {
            let children = &game.nodes[node].children;
            let Some(&main) = children.first() else {
                break;
            };
            out.push(main);
            for &variation in &children[1..] {
                visit(game, variation, out);
            }
            node = main;
        }
    }
    let mut order = Vec::with_capacity(game.nodes.len());
    visit(game, 0, &mut order);
    order
}

fn set_header(headers: &mut Vec<(String, String)>, key: &str, value: String) {
    if key.is_empty() {
        return;
    }
    if let Some(pair) = headers
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(key))
    {
        pair.1 = value;
    } else {
        headers.push((key.to_string(), value));
    }
}

fn header_value<'a>(headers: &'a [(String, String)], key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
}

fn parse_fen(fen: &str) -> Result<Chess> {
    let fen = Fen::from_ascii(fen.trim().as_bytes()).context("invalid FEN")?;
    fen.into_position(CastlingMode::Standard)
        .context("illegal FEN position")
}

fn add_null_move(game: &mut Game, parent: usize, parent_pos: &Chess) -> Result<usize> {
    let after = parent_pos
        .clone()
        .swap_turn()
        .context("illegal null move")?;
    let id = game.nodes.len();
    game.nodes.push(Node {
        mv: Some(Ply {
            san: "--".to_string(),
            uci: "0000".to_string(),
            number: parent_pos.fullmoves().get(),
            color: parent_pos.turn(),
        }),
        comment: String::new(),
        nags: Vec::new(),
        parent: Some(parent),
        children: Vec::new(),
        pos: after,
    });
    game.nodes[parent].children.push(id);
    Ok(id)
}

enum Special {
    Normal,
    Null,
    Castle(bool),
}

struct DecodedMove {
    from: Square,
    to: Option<Square>,
    promotion: Option<Role>,
    special: Special,
}

fn decode_move(
    pos: &Chess,
    lists: &PieceLists,
    val: u8,
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<DecodedMove> {
    let color = color_index(pos.turn());
    let piece_index = usize::from(val >> 4);
    let code = val & 0x0F;
    let from = lists
        .get(color, piece_index)
        .with_context(|| format!("SCID piece index {piece_index} is out of range"))?;
    let piece = pos
        .board()
        .piece_at(from)
        .with_context(|| format!("SCID refers to an empty square {from}"))?;
    if piece.color != pos.turn() {
        bail!("SCID piece list points at an opponent piece on {from}");
    }

    let (to, promotion, special) = match piece.role {
        Role::Pawn => {
            const DIFF: [i32; 16] = [7, 8, 9, 7, 8, 9, 7, 8, 9, 7, 8, 9, 7, 8, 9, 16];
            let diff = DIFF[usize::from(code)];
            let from = from.to_u32() as i32;
            let target = if pos.turn() == Color::White {
                from + diff
            } else {
                from - diff
            };
            let promotion = match code {
                3..=5 => Some(Role::Queen),
                6..=8 => Some(Role::Rook),
                9..=11 => Some(Role::Bishop),
                12..=14 => Some(Role::Knight),
                _ => None,
            };
            (Some(square_from_i32(target)?), promotion, Special::Normal)
        }
        Role::Bishop => {
            let fylediff = i32::from(code & 7) - (from.to_u32() as i32 & 7);
            let from_i = from.to_u32() as i32;
            let target = if code >= 8 {
                from_i - 7 * fylediff
            } else {
                from_i + 9 * fylediff
            };
            (Some(square_from_i32(target)?), None, Special::Normal)
        }
        Role::Knight => {
            const DIFF: [i32; 16] = [0, -17, -15, -10, -6, 6, 10, 15, 17, 0, 0, 0, 0, 0, 0, 0];
            let target = from.to_u32() as i32 + DIFF[usize::from(code)];
            (Some(square_from_i32(target)?), None, Special::Normal)
        }
        Role::Queen => {
            if u32::from(code) == (from.to_u32() & 7) {
                let extra = *bytes
                    .get(*cursor)
                    .context("two-byte queen move is truncated")?;
                *cursor += 1;
                let target = i32::from(extra) - 64;
                (Some(square_from_i32(target)?), None, Special::Normal)
            } else {
                (
                    Some(rook_or_queen_target(from, code)?),
                    None,
                    Special::Normal,
                )
            }
        }
        Role::Rook => (
            Some(rook_or_queen_target(from, code)?),
            None,
            Special::Normal,
        ),
        Role::King => match code {
            0 => (None, None, Special::Null),
            1..=8 => {
                const DIFF: [i32; 9] = [0, -9, -8, -7, -1, 1, 7, 8, 9];
                let target = from.to_u32() as i32 + DIFF[usize::from(code)];
                (Some(square_from_i32(target)?), None, Special::Normal)
            }
            9 => (None, None, Special::Castle(false)),
            10 => (None, None, Special::Castle(true)),
            _ => bail!("invalid SCID king move code {code}"),
        },
    };

    Ok(DecodedMove {
        from,
        to,
        promotion,
        special,
    })
}

fn rook_or_queen_target(from: Square, code: u8) -> Result<Square> {
    let from = from.to_u32();
    let file = from & 7;
    let rank = from >> 3;
    let target = if code >= 8 {
        (u32::from(code - 8) << 3) | file
    } else {
        (rank << 3) | u32::from(code)
    };
    square_from_i32(target as i32)
}

fn square_from_i32(value: i32) -> Result<Square> {
    if !(0..64).contains(&value) {
        bail!("SCID move leaves the board ({value})");
    }
    Ok(Square::new(value as u32))
}

fn find_normal(pos: &Chess, from: Square, to: Square, promotion: Option<Role>) -> Option<Move> {
    pos.legal_moves().iter().copied().find(|mv| {
        matches!(mv, Move::Normal { .. } | Move::EnPassant { .. })
            && mv.from() == Some(from)
            && mv.to() == to
            && mv.promotion() == promotion
    })
}

fn find_castle(pos: &Chess, king: Square, kingside: bool) -> Option<Move> {
    pos.legal_moves().iter().copied().find(|mv| match *mv {
        Move::Castle { king: k, rook } => k == king && (rook.file() > k.file()) == kingside,
        _ => false,
    })
}

fn color_index(color: Color) -> usize {
    if color == Color::White { 0 } else { 1 }
}

/// SCID's per-colour piece lists. The index encoded in the high nibble of a
/// move is a position in this list, not a piece type.
#[derive(Clone)]
struct PieceLists {
    squares: [[u8; 16]; 2],
    counts: [usize; 2],
    pos: [u8; 64],
}

impl PieceLists {
    fn standard() -> Self {
        const WHITE: [u8; 16] = [
            4, 0, 1, 2, 3, 5, 6, 7, // K, QR, QN, QB, Q, KB, KN, KR
            8, 9, 10, 11, 12, 13, 14, 15, // pawns
        ];
        const BLACK: [u8; 16] = [
            60, 56, 57, 58, 59, 61, 62, 63, // K, QR, QN, QB, Q, KB, KN, KR
            48, 49, 50, 51, 52, 53, 54, 55, // pawns
        ];
        let mut lists = Self::empty();
        lists.squares[0] = WHITE;
        lists.squares[1] = BLACK;
        lists.counts = [16, 16];
        for squares in [WHITE, BLACK] {
            for (index, square) in squares.into_iter().enumerate() {
                lists.pos[usize::from(square)] = index as u8;
            }
        }
        lists
    }

    fn empty() -> Self {
        Self {
            squares: [[0; 16]; 2],
            counts: [0; 2],
            pos: [u8::MAX; 64],
        }
    }

    /// Reconstruct the list exactly as SCID does after parsing a FEN: pieces
    /// are added scanning from rank 8 to rank 1, and a king takes slot 0,
    /// moving whatever was there to the end.
    fn from_fen(pos: &Chess) -> Self {
        let mut lists = Self::empty();
        for rank in (0..8).rev() {
            for file in 0..8 {
                let square = Square::new(rank * 8 + file);
                let Some(piece) = pos.board().piece_at(square) else {
                    continue;
                };
                let color = color_index(piece.color);
                let count = lists.counts[color];
                if piece.role == Role::King {
                    if count > 0 {
                        let old = lists.squares[color][0];
                        lists.squares[color][count] = old;
                        lists.pos[usize::from(old)] = count as u8;
                    }
                    lists.squares[color][0] = square.to_u32() as u8;
                    lists.pos[usize::from(square)] = 0;
                } else {
                    lists.squares[color][count] = square.to_u32() as u8;
                    lists.pos[usize::from(square)] = count as u8;
                }
                lists.counts[color] += 1;
            }
        }
        lists
    }

    fn get(&self, color: usize, index: usize) -> Option<Square> {
        if index >= self.counts[color] {
            return None;
        }
        Some(Square::new(u32::from(self.squares[color][index])))
    }

    fn relocate(&mut self, color: usize, from: Square, to: Square) {
        let slot = usize::from(self.pos[usize::from(from)]);
        self.squares[color][slot] = to.to_u32() as u8;
        self.pos[usize::from(to)] = slot as u8;
        self.pos[usize::from(from)] = u8::MAX;
    }

    fn remove(&mut self, color: usize, square: Square) -> Result<()> {
        let slot = usize::from(self.pos[usize::from(square)]);
        if slot >= self.counts[color] {
            bail!("SCID piece list is inconsistent (capture on {square})");
        }
        let last_index = self.counts[color] - 1;
        let last_square = self.squares[color][last_index];
        self.squares[color][slot] = last_square;
        self.pos[usize::from(last_square)] = slot as u8;
        self.counts[color] -= 1;
        self.pos[usize::from(square)] = u8::MAX;
        Ok(())
    }

    fn apply(&mut self, pos: &Chess, mv: Move) -> Result<()> {
        let color = color_index(pos.turn());
        let opponent = 1 - color;
        match mv {
            Move::Normal {
                from, to, capture, ..
            } => {
                if capture.is_some() {
                    self.remove(opponent, to)?;
                }
                self.relocate(color, from, to);
            }
            Move::EnPassant { from, to } => {
                let captured = if pos.turn() == Color::White {
                    to.to_u32() - 8
                } else {
                    to.to_u32() + 8
                };
                self.remove(opponent, Square::new(captured))?;
                self.relocate(color, from, to);
            }
            Move::Castle { king, rook } => {
                let backrank = king.rank();
                let kingside = rook.file() > king.file();
                let (king_file, rook_file) = if kingside {
                    (ChessFile::G, ChessFile::F)
                } else {
                    (ChessFile::C, ChessFile::D)
                };
                let king_to = Square::from_coords(king_file, backrank);
                let rook_to = Square::from_coords(rook_file, backrank);
                self.squares[color][0] = king_to.to_u32() as u8;
                self.pos[usize::from(king_to)] = 0;
                let rook_slot = usize::from(self.pos[usize::from(rook)]);
                self.squares[color][rook_slot] = rook_to.to_u32() as u8;
                self.pos[usize::from(rook_to)] = rook_slot as u8;
                self.pos[usize::from(king)] = u8::MAX;
                self.pos[usize::from(rook)] = u8::MAX;
            }
            Move::Put { .. } => bail!("piece drops are not valid in a SCID game"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/scid")
            .join(name)
    }

    #[test]
    fn opens_a_scid4_database() {
        let library = ScidLibrary::open(fixture("opera.si4")).unwrap();
        assert_eq!(library.len(), 1);
        let game = library.get(0).unwrap();
        assert_eq!(game.white(), "Paul Morphy");
        assert_eq!(game.black(), "Duke Karl and Count Isouard");
        assert_eq!(game.result(), "1-0");
    }

    #[test]
    fn decodes_the_reference_game() {
        let library = ScidLibrary::open(fixture("opera.si4")).unwrap();
        let game = library.load(0).unwrap();
        assert!(game.errors.is_empty(), "{:?}", game.errors);
        let sans: Vec<_> = game
            .mainline()
            .into_iter()
            .map(|node| game.nodes[node].san().unwrap_or("--"))
            .collect();
        assert_eq!(
            sans,
            [
                "e4", "e5", "Nf3", "d6", "d4", "Bg4", "dxe5", "Bxf3", "Qxf3", "dxe5", "Bc4", "Nf6",
                "Qb3", "Qe7", "Nc3", "c6", "Bg5", "b5", "Nxb5", "cxb5", "Bxb5+", "Nbd7", "O-O-O",
                "Rd8", "Rxd7", "Rxd7", "Rd1", "Qe6", "Bxd7+", "Nxd7", "Qb8+", "Nxb8", "Rd8#"
            ]
        );
        let d6 = game
            .mainline()
            .into_iter()
            .find(|&node| game.nodes[node].san() == Some("d6"))
            .unwrap();
        assert_eq!(game.nodes[d6].comment.trim(), "Philidor's Defense.");
        assert_eq!(
            game.nodes[0].comment.trim(),
            "The Opera Game, played in 1858 in the Italian Opera House in Paris."
        );
    }

    #[test]
    fn decodes_comments_nags_and_variations() {
        let library = ScidLibrary::open(fixture("opera.si4")).unwrap();
        let game = library.load(0).unwrap();
        assert!(game.errors.is_empty(), "{:?}", game.errors);
        assert!(game.mainline_len() > 30);
        let nxb5 = game
            .mainline()
            .into_iter()
            .find(|&node| game.nodes[node].san() == Some("Nxb5"))
            .unwrap();
        assert_eq!(game.nodes[nxb5].nags, [1]);
        let exd4 = game
            .nodes
            .iter()
            .position(|node| node.san() == Some("exd4"))
            .unwrap();
        assert!(
            game.nodes
                .iter()
                .any(|node| node.san() == Some("d4") && node.children.len() == 2)
        );
        assert!(
            game.nodes
                .iter()
                .any(|node| { node.parent == Some(exd4) && node.san() == Some("Nxd4") })
        );
        assert!(
            game.nodes.iter().any(
                |node| node.san() == Some("Nc3") && node.comment.contains("usual continuation")
            )
        );
    }

    #[test]
    fn decode_event_date_round_trips() {
        let date = (1957 << 9) | (10 << 5) | 13;
        let packed = (4u32 << 9) | (10 << 5) | 13;
        assert_eq!(decode_event_date(date, packed) >> 9, 1957);
    }
}
