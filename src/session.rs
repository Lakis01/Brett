//! An editable, ordered view over one or more game sources.
//!
//! The original database is never modified in place. Each visible row is a
//! slot: it either points lazily into the opened database, points lazily into
//! a merged database, or owns a copied game. Reordering slots changes only the
//! session; saving materialises the games in the chosen order.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use crate::domain::Game;
use crate::library::{GameLibrary, SharedStr};

struct LocalSlot {
    game: Game,
    headers: Vec<(SharedStr, SharedStr)>,
    search: String,
}

struct HeaderOverride {
    headers: Vec<(SharedStr, SharedStr)>,
    search: String,
}

enum SlotSource {
    Primary {
        library: Arc<dyn GameLibrary>,
        index: usize,
    },
    External {
        library: Arc<dyn GameLibrary>,
        index: usize,
    },
    Local(Box<LocalSlot>),
}

/// One game row in the editable session.
///
/// Primary and merged database rows point at the source index instead of
/// copying their headers. A multi-million-game ChessBase database would
/// otherwise duplicate every tag pair (and its search text) in memory.
pub struct GameSlot {
    pub id: usize,
    source: SlotSource,
    edited: Option<Box<HeaderOverride>>,
    cached_headers: std::sync::OnceLock<Vec<(SharedStr, SharedStr)>>,
}

impl GameSlot {
    fn primary(id: usize, library: Arc<dyn GameLibrary>, index: usize) -> Self {
        Self {
            id,
            source: SlotSource::Primary { library, index },
            edited: None,
            cached_headers: std::sync::OnceLock::new(),
        }
    }

    fn external(id: usize, library: Arc<dyn GameLibrary>, index: usize) -> Self {
        Self {
            id,
            source: SlotSource::External { library, index },
            edited: None,
            cached_headers: std::sync::OnceLock::new(),
        }
    }

    fn local(id: usize, game: Game) -> Self {
        let headers = shared_headers(&game.headers);
        let search = search_text(&headers);
        Self {
            id,
            source: SlotSource::Local(Box::new(LocalSlot {
                game,
                headers,
                search,
            })),
            edited: None,
            cached_headers: std::sync::OnceLock::new(),
        }
    }

    pub fn headers(&self) -> &[(SharedStr, SharedStr)] {
        if let Some(edited) = &self.edited {
            return &edited.headers;
        }
        match &self.source {
            SlotSource::Primary { library, index } | SlotSource::External { library, index } => {
                if let Some(game) = library.games().get(*index) {
                    return game.headers();
                }
                self.cached_headers
                    .get_or_init(|| library.game_headers(*index))
                    .as_slice()
            }
            SlotSource::Local(local) => &local.headers,
        }
    }

    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers()
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    }

    pub fn white(&self) -> &str {
        self.header("White").unwrap_or("?")
    }

    pub fn black(&self) -> &str {
        self.header("Black").unwrap_or("?")
    }

    pub fn event(&self) -> &str {
        self.header("Event").unwrap_or("?")
    }

    pub fn date(&self) -> &str {
        self.header("Date").unwrap_or("????.??.??")
    }

    pub fn result(&self) -> &str {
        self.header("Result").unwrap_or("*")
    }

    pub fn source_index(&self) -> Option<usize> {
        match &self.source {
            SlotSource::Primary { index, .. } => Some(*index),
            _ => None,
        }
    }

    fn matches(&self, needle: &str) -> bool {
        if let Some(edited) = &self.edited {
            return edited.search.contains(needle);
        }
        match &self.source {
            SlotSource::Primary { library, index } | SlotSource::External { library, index } => {
                if let Some(game) = library.games().get(*index) {
                    return game.matches(needle);
                }
                self.headers()
                    .iter()
                    .any(|(_, v)| v.to_lowercase().contains(needle))
            }
            SlotSource::Local(local) => local.search.contains(needle),
        }
    }

    fn refresh_headers(&mut self, game: &Game) {
        let headers = shared_headers(&game.headers);
        let search = search_text(&headers);
        if let SlotSource::Local(local) = &mut self.source {
            local.headers = headers;
            local.search = search;
            self.edited = None;
        } else {
            self.edited = Some(Box::new(HeaderOverride { headers, search }));
        }
    }
}

fn shared_headers(headers: &[(String, String)]) -> Vec<(SharedStr, SharedStr)> {
    headers
        .iter()
        .map(|(key, value)| {
            (
                SharedStr::from(key.as_str()),
                SharedStr::from(value.as_str()),
            )
        })
        .collect()
}

/// Result of merging one database into another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub added: usize,
    pub skipped: usize,
}

/// The ordered list of rows currently shown in the Games tab.
pub struct GameList {
    primary: Option<Arc<dyn GameLibrary>>,
    slots: Vec<GameSlot>,
    /// Stable slot id -> current position. `usize::MAX` marks a removed row.
    /// A multi-million-game database cannot afford a linear id search on every
    /// board click.
    positions: Vec<usize>,
    /// Primary source index -> stable slot id.
    source_slots: Vec<usize>,
    next_id: usize,
}

impl GameList {
    pub fn empty() -> Self {
        Self {
            primary: None,
            slots: Vec::new(),
            positions: Vec::new(),
            source_slots: Vec::new(),
            next_id: 0,
        }
    }

    pub fn from_library(library: Arc<dyn GameLibrary>) -> Self {
        let mut list = Self {
            primary: Some(library.clone()),
            slots: Vec::with_capacity(library.len()),
            positions: (0..library.len()).collect(),
            source_slots: (0..library.len()).collect(),
            next_id: library.len(),
        };
        for index in 0..library.len() {
            list.slots
                .push(GameSlot::primary(index, library.clone(), index));
        }
        list
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn slots(&self) -> &[GameSlot] {
        &self.slots
    }

    pub fn slot(&self, id: usize) -> Option<&GameSlot> {
        let position = *self.positions.get(id)?;
        if position == usize::MAX {
            return None;
        }
        self.slots.get(position)
    }

    pub fn slot_mut(&mut self, id: usize) -> Option<&mut GameSlot> {
        let position = *self.positions.get(id)?;
        if position == usize::MAX {
            return None;
        }
        self.slots.get_mut(position)
    }

    pub fn position_of(&self, id: usize) -> Option<usize> {
        self.positions
            .get(id)
            .copied()
            .filter(|position| *position != usize::MAX)
    }

    pub fn id_at(&self, position: usize) -> Option<usize> {
        self.slots.get(position).map(|slot| slot.id)
    }

    pub fn source_slot(&self, source_index: usize) -> Option<usize> {
        let id = *self.source_slots.get(source_index)?;
        self.position_of(id).map(|_| id)
    }

    pub fn filter(&self, needle: &str) -> Vec<usize> {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return self.slots.iter().map(|slot| slot.id).collect();
        }
        self.slots
            .iter()
            .filter(|slot| slot.matches(&needle))
            .map(|slot| slot.id)
            .collect()
    }

    pub fn load(&self, id: usize, edited: &HashMap<usize, Game>) -> Result<Game> {
        if let Some(game) = edited.get(&id) {
            return Ok(game.clone());
        }
        let slot = self
            .slot(id)
            .with_context(|| format!("game row {id} is gone"))?;
        match &slot.source {
            SlotSource::Primary { index, .. } => self
                .primary
                .as_ref()
                .context("no primary database is open")?
                .load(*index)
                .with_context(|| format!("loading source game {}", index + 1)),
            SlotSource::External { library, index } => library
                .load(*index)
                .with_context(|| format!("loading merged game {}", index + 1)),
            SlotSource::Local(local) => Ok(local.game.clone()),
        }
    }

    /// Add one owned game. `insert_at` is a position in the visible order,
    /// where `len()` means append.
    pub fn insert_game(&mut self, game: Game, insert_at: Option<usize>) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.insert_slot(GameSlot::local(id, game), insert_at);
        id
    }

    /// Copy an existing row into a new independent row.
    pub fn duplicate_game(
        &mut self,
        source_id: usize,
        edited: &mut HashMap<usize, Game>,
        insert_at: Option<usize>,
    ) -> Result<usize> {
        let game = self.load(source_id, edited)?;
        // Keep the edited copy attached to the source row. Removing it here
        // used to discard the user's changes as soon as a row was duplicated.
        Ok(self.insert_game(game, insert_at))
    }

    /// Append every distinct game from another database. Header candidates are
    /// checked first, then the first moves are compared so two different games
    /// between the same players on the same date are both retained.
    pub fn merge_library(
        &mut self,
        library: Arc<dyn GameLibrary>,
        edited: &HashMap<usize, Game>,
    ) -> Result<MergeReport> {
        let mut report = MergeReport::default();
        let count = library.len();
        for index in 0..count {
            let headers_vec;
            let headers = if let Some(indexed) = library.games().get(index) {
                indexed.headers()
            } else {
                headers_vec = library.game_headers(index);
                &headers_vec
            };
            let candidates = duplicate_candidates(&self.slots, headers);
            if !candidates.is_empty() {
                let incoming = library
                    .load(index)
                    .with_context(|| format!("loading merged game {}", index + 1))?;
                let mut duplicate = false;
                for id in candidates {
                    let existing = self.load(id, edited)?;
                    if same_game(&existing, &incoming) {
                        duplicate = true;
                        break;
                    }
                }
                if duplicate {
                    report.skipped += 1;
                    continue;
                }
            }
            let id = self.next_id;
            self.next_id += 1;
            self.push_slot(GameSlot::external(id, library.clone(), index));
            report.added += 1;
        }
        Ok(report)
    }

    /// Append every game from another database lazily. The returned ids are in
    /// source order and can be checked for duplicates by the caller.
    pub fn append_library(&mut self, library: Arc<dyn GameLibrary>) -> Vec<usize> {
        let mut ids = Vec::with_capacity(library.len());
        for index in 0..library.len() {
            let id = self.next_id;
            self.next_id += 1;
            self.push_slot(GameSlot::external(id, library.clone(), index));
            ids.push(id);
        }
        ids
    }

    pub fn move_slot(&mut self, id: usize, insertion_index: usize) -> bool {
        let Some(from) = self.position_of(id) else {
            return false;
        };
        let insertion_index = insertion_index.min(self.slots.len());
        let target = if from < insertion_index {
            insertion_index.saturating_sub(1)
        } else {
            insertion_index
        };
        if target == from {
            return false;
        }
        let slot = self.slots.remove(from);
        let target = target.min(self.slots.len());
        self.slots.insert(target, slot);
        let (start, end) = if from < target {
            (from, target)
        } else {
            (target, from)
        };
        for position in start..=end {
            if let Some(slot) = self.slots.get(position) {
                self.positions[slot.id] = position;
            }
        }
        true
    }

    pub fn remove(&mut self, id: usize) -> Option<GameSlot> {
        let position = self.position_of(id)?;
        let slot = self.slots.remove(position);
        self.positions[id] = usize::MAX;
        for (position, slot) in self.slots.iter().enumerate().skip(position) {
            self.positions[slot.id] = position;
        }
        if let Some(source) = slot.source_index()
            && let Some(mapped) = self.source_slots.get_mut(source)
            && *mapped == id
        {
            *mapped = usize::MAX;
        }
        Some(slot)
    }

    pub fn update_headers(&mut self, id: usize, game: &Game) {
        if let Some(slot) = self.slot_mut(id) {
            slot.refresh_headers(game);
        }
    }

    fn insert_slot(&mut self, slot: GameSlot, insert_at: Option<usize>) {
        let id = slot.id;
        let at = insert_at.unwrap_or(self.slots.len()).min(self.slots.len());
        self.positions.resize(self.next_id.max(id + 1), usize::MAX);
        self.slots.insert(at, slot);
        for (position, slot) in self.slots.iter().enumerate().skip(at) {
            self.positions[slot.id] = position;
        }
    }

    fn push_slot(&mut self, slot: GameSlot) {
        let id = slot.id;
        self.positions.resize(self.next_id.max(id + 1), usize::MAX);
        self.slots.push(slot);
        self.positions[id] = self.slots.len() - 1;
    }
}

/// Header-based duplicate candidates. Names are compared order-insensitively
/// and transliterated so `Müller, Jörg` matches `Joerg Mueller`, and a Cyrillic
/// spelling can match its Latin transliteration. Date and result must also
/// agree; callers can verify the moves before rejecting a game.
pub fn duplicate_candidates(slots: &[GameSlot], headers: &[(SharedStr, SharedStr)]) -> Vec<usize> {
    let key = duplicate_key(headers);
    slots
        .iter()
        .filter(|slot| duplicate_key(slot.headers()) == key)
        .map(|slot| slot.id)
        .collect()
}

fn duplicate_key(headers: &[(SharedStr, SharedStr)]) -> String {
    let header = |key: &str| {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
            .unwrap_or("")
    };
    format!(
        "{}|{}|{}|{}",
        normalized_name(header("White")),
        normalized_name(header("Black")),
        header("Date"),
        header("Result")
    )
}

/// Normalise a player name for duplicate detection.
pub fn normalized_name(name: &str) -> String {
    let mut words: Vec<String> = name
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(normalize_word)
        .filter(|word| !word.is_empty())
        .collect();
    words.sort();
    words.join(" ")
}

fn normalize_word(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    for ch in word.chars() {
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        match lower {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            'á' | 'à' | 'â' | 'å' | 'ã' => out.push('a'),
            'é' | 'è' | 'ê' | 'ë' => out.push('e'),
            'í' | 'ì' | 'î' | 'ï' => out.push('i'),
            'ó' | 'ò' | 'ô' | 'õ' | 'ø' => out.push('o'),
            'ú' | 'ù' | 'û' => out.push('u'),
            'ç' => out.push('c'),
            'ñ' => out.push('n'),
            'ý' | 'ÿ' => out.push('y'),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'đ' => out.push('d'),
            'ł' => out.push('l'),
            'ž' => out.push('z'),
            'š' => out.push('s'),
            'č' => out.push('c'),
            'ř' => out.push('r'),
            // Common Cyrillic transliteration. This intentionally covers the
            // letters that occur in chess player names rather than attempting
            // a language-specific scholarly scheme.
            'а' => out.push('a'),
            'б' => out.push('b'),
            'в' => out.push('v'),
            'г' => out.push('g'),
            'д' => out.push('d'),
            'е' | 'ё' => out.push('e'),
            'ж' => out.push_str("zh"),
            'з' => out.push('z'),
            'и' => out.push('i'),
            'й' => out.push('i'),
            'к' => out.push('k'),
            'л' => out.push('l'),
            'м' => out.push('m'),
            'н' => out.push('n'),
            'о' => out.push('o'),
            'п' => out.push('p'),
            'р' => out.push('r'),
            'с' => out.push('s'),
            'т' => out.push('t'),
            'у' => out.push('u'),
            'ф' => out.push('f'),
            'х' => out.push_str("kh"),
            'ц' => out.push_str("ts"),
            'ч' => out.push_str("ch"),
            'ш' => out.push_str("sh"),
            'щ' => out.push_str("shch"),
            'ъ' | 'ь' => {}
            'ы' => out.push('y'),
            'э' => out.push('e'),
            'ю' => out.push_str("yu"),
            'я' => out.push_str("ya"),
            _ if lower.is_ascii_alphanumeric() => out.push(lower),
            _ => {}
        }
    }
    out
}

fn search_text(headers: &[(SharedStr, SharedStr)]) -> String {
    let mut search = String::with_capacity(128);
    for key in ["White", "Black", "Event", "Site", "Date", "ECO", "Opening"] {
        if let Some((_, value)) = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
        {
            search.push_str(&value.to_lowercase());
            search.push(' ');
        }
    }
    search
}

/// Verify that two games have a matching move prefix. Used after the header
/// key matches, so games between the same players on the same date are only
/// rejected when their moves really agree.
pub fn same_game(a: &Game, b: &Game) -> bool {
    let a = mainline(a, 12);
    let b = mainline(b, 12);
    !a.is_empty() && a == b
}

fn mainline(game: &Game, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut node = 0usize;
    while out.len() < limit {
        let Some(&child) = game.nodes[node].children.first() else {
            break;
        };
        let Some(ply) = &game.nodes[child].mv else {
            break;
        };
        out.push(ply.uci.clone());
        node = child;
    }
    out
}

/// The function used by a full database merge. It returns the first clean
/// duplicate, or `None` when all candidate rows are distinct.
pub fn find_duplicate(
    list: &GameList,
    headers: &[(SharedStr, SharedStr)],
    game: &Game,
    edited: &HashMap<usize, Game>,
) -> Result<Option<usize>> {
    for id in duplicate_candidates(list.slots(), headers) {
        let existing = list.load(id, edited)?;
        if same_game(&existing, game) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Development guard: a save routine must never write a slot twice by mistake.
pub fn ensure_unique_ids(list: &GameList) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for slot in list.slots() {
        if !seen.insert(slot.id) {
            bail!("internal error: duplicate game row id {}", slot.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::PgnLibrary;

    fn game(white: &str, black: &str, date: &str, san: &str) -> Game {
        let pgn = format!(
            "[Event \"Test\"]\n[White \"{white}\"]\n[Black \"{black}\"]\n[Date \"{date}\"]\n[Result \"1-0\"]\n\n{san} 1-0\n"
        );
        let mut parsed = crate::domain::parse_pgn(pgn.as_bytes()).unwrap();
        parsed.remove(0)
    }

    #[test]
    fn names_ignore_order_umlauts_and_cyrillic() {
        assert_eq!(
            normalized_name("Müller, Jörg"),
            normalized_name("Joerg Mueller")
        );
        assert_eq!(
            normalized_name("Tal, Mikhail"),
            normalized_name("Mikhail Tal")
        );
        assert_eq!(
            normalized_name("Михаил Таль"),
            normalized_name("Mikhail Tal")
        );
        assert_ne!(
            normalized_name("Mikhail Tal"),
            normalized_name("Mikhail Botvinnik")
        );
    }

    #[test]
    fn duplicate_candidates_require_the_same_pair_date_and_result() {
        let mut list = GameList::empty();
        let first = list.insert_game(
            game("Müller, Jörg", "Tal, Mikhail", "1970.01.01", "1. e4 e5"),
            None,
        );
        let second = list.insert_game(
            game("Joerg Mueller", "Mikhail Tal", "1970.01.01", "1. d4 d5"),
            None,
        );
        let other = game("Other", "Player", "1970.01.01", "1. c4 c5");
        let mut candidates = duplicate_candidates(list.slots(), &shared_headers(&other.headers));
        assert!(candidates.is_empty());
        candidates = duplicate_candidates(list.slots(), list.slot(first).unwrap().headers());
        assert_eq!(candidates, vec![first, second]);
        assert!(
            find_duplicate(
                &list,
                list.slot(second).unwrap().headers(),
                &game("Joerg Mueller", "Mikhail Tal", "1970.01.01", "1. d4 d5"),
                &HashMap::new()
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn move_prefix_distinguishes_same_header_key() {
        let a = game("A", "B", "2020.01.01", "1. e4 e5 2. Nf3");
        let b = game("A", "B", "2020.01.01", "1. e4 e5 2. Bc4");
        let c = game("A", "B", "2020.01.01", "1. e4 e5 2. Nf3");
        assert!(!same_game(&a, &b));
        assert!(same_game(&a, &c));
    }

    #[test]
    fn slots_can_be_reordered_without_changing_ids() {
        let mut list = GameList::empty();
        let a = list.insert_game(game("A", "B", "2020.01.01", "1. e4"), None);
        let b = list.insert_game(game("C", "D", "2020.01.01", "1. d4"), None);
        let c = list.insert_game(game("E", "F", "2020.01.01", "1. c4"), None);
        assert!(list.move_slot(c, 0));
        assert_eq!(
            list.slots().iter().map(|slot| slot.id).collect::<Vec<_>>(),
            vec![c, a, b]
        );
        assert_eq!(list.slot(c).unwrap().white(), "E");
        assert!(list.remove(a).is_some());
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn source_backed_slots_keep_headers_and_positions_after_reordering() {
        let path = std::env::temp_dir().join(format!(
            "brett-session-lazy-{}-{}.pgn",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let pgn = concat!(
            "[Event \"Test\"]\n[White \"Alpha\"]\n[Black \"Beta\"]\n",
            "[Date \"2020.01.01\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n\n",
            "[Event \"Test\"]\n[White \"Gamma\"]\n[Black \"Delta\"]\n",
            "[Date \"2020.01.02\"]\n[Result \"0-1\"]\n\n1. d4 d5 0-1\n\n",
            "[Event \"Test\"]\n[White \"Epsilon\"]\n[Black \"Zeta\"]\n",
            "[Date \"2020.01.03\"]\n[Result \"1/2-1/2\"]\n\n1. c4 c5 1/2-1/2\n"
        );
        std::fs::write(&path, pgn).unwrap();
        let library: Arc<dyn GameLibrary> = Arc::new(PgnLibrary::build(&path).unwrap());
        let mut list = GameList::from_library(library);

        assert_eq!(list.slot(0).unwrap().white(), "Alpha");
        assert_eq!(list.source_slot(1), Some(1));

        assert!(list.move_slot(2, 0));
        assert_eq!(list.position_of(2), Some(0));
        assert_eq!(list.slot(2).unwrap().white(), "Epsilon");
        assert_eq!(list.slot(0).unwrap().white(), "Alpha");
        assert_eq!(list.source_slot(0), Some(0));
        assert_eq!(list.source_slot(2), Some(2));

        assert!(list.remove(1).is_some());
        assert!(list.slot(1).is_none());
        assert!(list.source_slot(1).is_none());
        assert_eq!(list.slot(2).unwrap().white(), "Epsilon");

        let inserted = list.insert_game(game("Local", "Row", "2020.01.04", "1. e4"), Some(0));
        assert_eq!(list.position_of(inserted), Some(0));
        assert_eq!(list.slot(inserted).unwrap().white(), "Local");
        assert_eq!(list.slot(2).unwrap().white(), "Epsilon");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn duplicating_an_edited_row_keeps_the_source_edits() {
        let mut list = GameList::empty();
        let source = list.insert_game(
            game("Müller, Jörg", "Tal, Mikhail", "1970.01.01", "1. e4 e5"),
            None,
        );
        let mut edited = HashMap::new();
        let mut changed = list.load(source, &HashMap::new()).unwrap();
        changed
            .headers
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case("Event"))
            .unwrap()
            .1 = "Edited event".to_string();
        edited.insert(source, changed);

        let duplicate = list.duplicate_game(source, &mut edited, None).unwrap();
        assert!(edited.contains_key(&source));
        assert_eq!(
            list.load(source, &edited).unwrap().header("Event"),
            Some("Edited event")
        );
        assert_eq!(
            list.load(duplicate, &edited).unwrap().header("Event"),
            Some("Edited event")
        );
        assert_ne!(source, duplicate);
    }

    #[test]
    fn merge_library_skips_duplicates_but_keeps_distinct_games() {
        let path = std::env::temp_dir().join(format!(
            "brett-session-merge-{}-{}.pgn",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let source_pgn = concat!(
            "[Event \"Test\"]\n[White \"Joerg Mueller\"]\n[Black \"Mikhail Tal\"]\n",
            "[Date \"1970.01.01\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n\n",
            "[Event \"Test\"]\n[White \"Müller, Jörg\"]\n[Black \"Tal, Mikhail\"]\n",
            "[Date \"1970.01.01\"]\n[Result \"1-0\"]\n\n1. d4 d5 1-0\n"
        );
        std::fs::write(&path, source_pgn).unwrap();
        let library: Arc<dyn GameLibrary> = Arc::new(PgnLibrary::build(&path).unwrap());

        let mut list = GameList::empty();
        list.insert_game(
            game("Müller, Jörg", "Tal, Mikhail", "1970.01.01", "1. e4 e5"),
            None,
        );
        let report = list.merge_library(library, &HashMap::new()).unwrap();
        assert_eq!(
            report,
            MergeReport {
                added: 1,
                skipped: 1
            }
        );
        assert_eq!(list.len(), 2);
        let _ = std::fs::remove_file(path);
    }
}
