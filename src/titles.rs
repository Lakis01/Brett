//! Player-title normalization for display.
//!
//! PGN `WhiteTitle`/`BlackTitle` tags and imported metadata are free-form
//! strings. The stored tag is never rewritten; this module only maps a raw
//! value to a canonical short badge for the UI. Unknown or empty values fall
//! back to no badge, and a rating is never used to infer a title.

/// A recognized chess title, ordered by display strength.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerTitle {
    Gm,
    Igm,
    Cgm,
    Im,
    Sim,
    Ccm,
    Fm,
    Cm,
    Wgm,
    Wim,
    Wfm,
    Wcm,
    Nm,
    Lm,
}

impl PlayerTitle {
    /// Canonical short form used for badges and exports.
    pub const fn short(self) -> &'static str {
        match self {
            Self::Gm => "GM",
            Self::Igm => "IGM",
            Self::Cgm => "CGM",
            Self::Im => "IM",
            Self::Sim => "SIM",
            Self::Ccm => "CCM",
            Self::Fm => "FM",
            Self::Cm => "CM",
            Self::Wgm => "WGM",
            Self::Wim => "WIM",
            Self::Wfm => "WFM",
            Self::Wcm => "WCM",
            Self::Nm => "NM",
            Self::Lm => "LM",
        }
    }

    /// Full human-readable title name used in hover tooltips.
    pub const fn full_name(self) -> &'static str {
        match self {
            Self::Gm => "Grandmaster",
            Self::Igm => "International Grandmaster",
            Self::Cgm => "Correspondence Grandmaster",
            Self::Im => "International Master",
            Self::Sim => "Senior International Master",
            Self::Ccm => "Correspondence Candidate Master",
            Self::Fm => "FIDE Master",
            Self::Cm => "Candidate Master",
            Self::Wgm => "Woman Grandmaster",
            Self::Wim => "Woman International Master",
            Self::Wfm => "Woman FIDE Master",
            Self::Wcm => "Woman Candidate Master",
            Self::Nm => "National Master",
            Self::Lm => "Life Master",
        }
    }

    /// Rough ordering used when a player's games carry different titles.
    /// Open titles outrank women's titles, and correspondence titles sit
    /// between the corresponding open title and the next one down.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Gm => 100,
            Self::Igm => 95,
            Self::Cgm => 90,
            Self::Im => 80,
            Self::Sim => 75,
            Self::Ccm => 55,
            Self::Fm => 60,
            Self::Cm => 50,
            Self::Wgm => 45,
            Self::Wim => 40,
            Self::Wfm => 30,
            Self::Wcm => 20,
            Self::Nm => 15,
            Self::Lm => 10,
        }
    }
}

/// A title together with the year it was awarded, when the source states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TitleAward {
    pub title: PlayerTitle,
    pub year: Option<i32>,
}

/// Strongest title in a history, preferring the highest rank. Used for the
/// current profile badge so a GM who was once an IM still reads `GM`.
pub fn strongest(awards: &[TitleAward]) -> Option<PlayerTitle> {
    awards
        .iter()
        .map(|award| award.title)
        .max_by_key(|title| title.rank())
}

/// Strongest title that had already been awarded in `year`. Undated awards are
/// deliberately ignored: they must never be guessed onto an earlier game.
pub fn at_year(awards: &[TitleAward], year: u32) -> Option<PlayerTitle> {
    awards
        .iter()
        .filter(|award| award.year.is_some_and(|awarded| year >= awarded as u32))
        .map(|award| award.title)
        .max_by_key(|title| title.rank())
}

/// Map a raw title tag to a canonical title, or `None` when it is absent,
/// empty, or not a recognized chess title.
pub fn normalize(raw: &str) -> Option<PlayerTitle> {
    let mut cleaned = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            // Periods and apostrophes are dropped so `G.M.` and `Women's`
            // collapse onto the same tokens as their plain spellings.
            '.' | '\'' | '’' => {}
            '-' | '_' | ',' | ';' | '/' | '|' | '&' | '(' | ')' | '[' | ']' => {
                cleaned.push(' ');
            }
            _ => cleaned.push(ch),
        }
    }
    let normalized = cleaned
        .split_whitespace()
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return None;
    }

    if let Some(title) = abbreviation(&normalized) {
        return Some(title);
    }
    for (name, title) in FULL_NAMES {
        if full_name_matches(&normalized, name) {
            return Some(*title);
        }
    }

    // A few sources put the abbreviation first and an annotation after it,
    // for example `GM (FIDE)`.
    abbreviation(normalized.split(' ').next()?)
}

fn abbreviation(value: &str) -> Option<PlayerTitle> {
    Some(match value {
        "GM" => PlayerTitle::Gm,
        "IGM" => PlayerTitle::Igm,
        "CGM" => PlayerTitle::Cgm,
        "IM" => PlayerTitle::Im,
        "SIM" => PlayerTitle::Sim,
        "CCM" => PlayerTitle::Ccm,
        "FM" => PlayerTitle::Fm,
        "CM" => PlayerTitle::Cm,
        "WGM" => PlayerTitle::Wgm,
        "WIM" => PlayerTitle::Wim,
        "WFM" => PlayerTitle::Wfm,
        "WCM" => PlayerTitle::Wcm,
        "NM" => PlayerTitle::Nm,
        "LM" => PlayerTitle::Lm,
        _ => return None,
    })
}

fn full_name_matches(normalized: &str, name: &str) -> bool {
    if normalized == name {
        return true;
    }
    normalized.strip_prefix(name).is_some_and(|rest| {
        rest.starts_with(' ')
            && rest
                .split_whitespace()
                .all(|token| abbreviation(token).is_some() || token == "FIDE")
    })
}

/// Longest, most specific spellings first.
const FULL_NAMES: &[(&str, PlayerTitle)] = &[
    ("CORRESPONDENCE CANDIDATE MASTER", PlayerTitle::Ccm),
    ("CORRESPONDENCE GRANDMASTER", PlayerTitle::Cgm),
    ("SENIOR INTERNATIONAL MASTER", PlayerTitle::Sim),
    ("INTERNATIONAL GRANDMASTER", PlayerTitle::Igm),
    ("INTERNATIONAL MASTER", PlayerTitle::Im),
    ("FIDE CANDIDATE MASTER", PlayerTitle::Cm),
    ("FIDE MASTER", PlayerTitle::Fm),
    ("WOMAN INTERNATIONAL MASTER", PlayerTitle::Wim),
    ("WOMANS INTERNATIONAL MASTER", PlayerTitle::Wim),
    ("WOMEN INTERNATIONAL MASTER", PlayerTitle::Wim),
    ("WOMENS INTERNATIONAL MASTER", PlayerTitle::Wim),
    ("WOMAN CANDIDATE MASTER", PlayerTitle::Wcm),
    ("WOMANS CANDIDATE MASTER", PlayerTitle::Wcm),
    ("WOMEN CANDIDATE MASTER", PlayerTitle::Wcm),
    ("WOMENS CANDIDATE MASTER", PlayerTitle::Wcm),
    ("WOMAN GRANDMASTER", PlayerTitle::Wgm),
    ("WOMANS GRANDMASTER", PlayerTitle::Wgm),
    ("WOMEN GRANDMASTER", PlayerTitle::Wgm),
    ("WOMENS GRANDMASTER", PlayerTitle::Wgm),
    ("WOMAN FIDE MASTER", PlayerTitle::Wfm),
    ("WOMANS FIDE MASTER", PlayerTitle::Wfm),
    ("WOMEN FIDE MASTER", PlayerTitle::Wfm),
    ("WOMENS FIDE MASTER", PlayerTitle::Wfm),
    ("CANDIDATE MASTER", PlayerTitle::Cm),
    ("GRANDMASTER", PlayerTitle::Gm),
    ("NATIONAL MASTER", PlayerTitle::Nm),
    ("LIFE MASTER", PlayerTitle::Lm),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn check(cases: &[(&str, PlayerTitle)]) {
        for (raw, expected) in cases {
            assert_eq!(normalize(raw), Some(*expected), "raw title {raw:?}");
        }
    }

    #[test]
    fn recognizes_fide_titles_and_spellings() {
        check(&[
            ("GM", PlayerTitle::Gm),
            ("gm", PlayerTitle::Gm),
            ("G.M.", PlayerTitle::Gm),
            ("GM.", PlayerTitle::Gm),
            (" Grandmaster ", PlayerTitle::Gm),
            ("International Grandmaster", PlayerTitle::Igm),
            ("IM", PlayerTitle::Im),
            ("International Master", PlayerTitle::Im),
            ("FM", PlayerTitle::Fm),
            ("FIDE Master", PlayerTitle::Fm),
            ("CM", PlayerTitle::Cm),
            ("Candidate Master", PlayerTitle::Cm),
            ("FIDE Candidate Master", PlayerTitle::Cm),
        ]);
    }

    #[test]
    fn recognizes_womens_titles_and_spellings() {
        check(&[
            ("WGM", PlayerTitle::Wgm),
            ("Woman Grandmaster", PlayerTitle::Wgm),
            ("Women's Grandmaster", PlayerTitle::Wgm),
            ("WIM", PlayerTitle::Wim),
            ("Woman International Master", PlayerTitle::Wim),
            ("WFM", PlayerTitle::Wfm),
            ("Women FIDE Master", PlayerTitle::Wfm),
            ("WCM", PlayerTitle::Wcm),
            ("Woman Candidate Master", PlayerTitle::Wcm),
        ]);
    }

    #[test]
    fn recognizes_correspondence_and_historical_titles() {
        check(&[
            ("CGM", PlayerTitle::Cgm),
            ("Correspondence Grandmaster", PlayerTitle::Cgm),
            ("SIM", PlayerTitle::Sim),
            ("Senior International Master", PlayerTitle::Sim),
            ("CCM", PlayerTitle::Ccm),
            ("Correspondence Candidate Master", PlayerTitle::Ccm),
            ("IGM", PlayerTitle::Igm),
            ("NM", PlayerTitle::Nm),
            ("National Master", PlayerTitle::Nm),
            ("LM", PlayerTitle::Lm),
            ("Life Master", PlayerTitle::Lm),
        ]);
    }

    #[test]
    fn accepts_multiple_or_annotated_titles() {
        assert_eq!(normalize("GM, IM"), Some(PlayerTitle::Gm));
        assert_eq!(normalize("WGM/WIM"), Some(PlayerTitle::Wgm));
        assert_eq!(normalize("GM (FIDE)"), Some(PlayerTitle::Gm));
        assert_eq!(normalize("Woman Grandmaster (WGM)"), Some(PlayerTitle::Wgm));
    }

    #[test]
    fn rejects_empty_unknown_and_rating_like_values() {
        for raw in [
            "",
            "   ",
            "GMX",
            "2500",
            "Candidate",
            "Grandmaster of Chess?",
        ] {
            assert_eq!(normalize(raw), None, "raw title {raw:?}");
        }
    }

    #[test]
    fn short_forms_are_canonical() {
        assert_eq!(PlayerTitle::Gm.short(), "GM");
        assert_eq!(PlayerTitle::Wcm.short(), "WCM");
        assert_eq!(PlayerTitle::Ccm.short(), "CCM");
        assert!(PlayerTitle::Gm.rank() > PlayerTitle::Im.rank());
        assert!(PlayerTitle::Im.rank() > PlayerTitle::Wgm.rank());
    }

    #[test]
    fn strongest_and_dated_titles_are_chosen_by_rank_and_year() {
        let awards = [
            TitleAward {
                title: PlayerTitle::Im,
                year: Some(2003),
            },
            TitleAward {
                title: PlayerTitle::Gm,
                year: Some(2004),
            },
            TitleAward {
                title: PlayerTitle::Fm,
                year: None,
            },
        ];
        assert_eq!(strongest(&awards), Some(PlayerTitle::Gm));
        assert_eq!(at_year(&awards, 2000), None);
        assert_eq!(at_year(&awards, 2003), Some(PlayerTitle::Im));
        assert_eq!(at_year(&awards, 2004), Some(PlayerTitle::Gm));
        assert_eq!(at_year(&awards, 2010), Some(PlayerTitle::Gm));
    }
}
