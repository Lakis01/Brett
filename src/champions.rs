//! World-champion lookup for the gold crown badge.
//!
//! Brett supports two documented lineages:
//!
//! - **Classical/PCA**: the uninterrupted line from Steinitz to the present.
//! - **FIDE/inclusive**: the classical line plus the four FIDE knockout world
//!   champions of 1999–2006 (Khalifman, Ponomariov, Kasimdzhanov, Topalov).
//!
//! Name matching is deliberately conservative: only names in this table are
//! crowned, and the comparison folds accents and ignores word order and
//! punctuation. A player with an unknown or merely similar name is never
//! crowned.

use crate::players::player_key;
use crate::settings::ChampionLineage;

/// One recognized world champion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Champion {
    /// Display name used in documentation and tests.
    pub name: &'static str,
    /// Accepted spellings, including the display name itself.
    pub aliases: &'static [&'static str],
    /// Championship years, including split reigns.
    pub reign: &'static str,
    /// How the reign is described, e.g. `World Champion`,
    /// `Classical World Champion`, or `FIDE World Champion`.
    pub title: &'static str,
    /// One factual sentence about the reign, shown in the crown tooltip.
    pub fact: &'static str,
    /// True for the four FIDE knockout champions that are absent from the
    /// classical/PCA line.
    pub fide_only: bool,
}

impl Champion {
    /// Hover text for the gold crown. It names the title line, the reign, and
    /// one factual note so the badge is never just an unexplained symbol.
    pub fn tooltip(&self) -> String {
        let verb = if self.reign.contains("present") {
            "is"
        } else {
            "was"
        };
        format!(
            "{} {verb} {} {}.\n{}",
            self.name, self.title, self.reign, self.fact
        )
    }
}

/// Classical/PCA champions, oldest first, followed by the FIDE-only champions.
pub const CHAMPIONS: &[Champion] = &[
    Champion {
        name: "Wilhelm Steinitz",
        aliases: &["Wilhelm Steinitz", "William Steinitz"],
        reign: "1886–1894",
        title: "World Champion",
        fact: "Won the first official World Championship match, against Johannes Zukertort in 1886.",
        fide_only: false,
    },
    Champion {
        name: "Emanuel Lasker",
        aliases: &["Emanuel Lasker"],
        reign: "1894–1921",
        title: "World Champion",
        fact: "Held the title for 27 years, the longest reign in World Championship history.",
        fide_only: false,
    },
    Champion {
        name: "José Raúl Capablanca",
        aliases: &["José Raúl Capablanca", "Jose Raul Capablanca"],
        reign: "1921–1927",
        title: "World Champion",
        fact: "Won the title from Emanuel Lasker in 1921 and lost it to Alexander Alekhine in 1927.",
        fide_only: false,
    },
    Champion {
        name: "Alexander Alekhine",
        aliases: &[
            "Alexander Alekhine",
            "Alexandre Alekhine",
            "Alexander Alexandrovich Alekhine",
        ],
        reign: "1927–1935, 1937–1946",
        title: "World Champion",
        fact: "Lost the title to Max Euwe in 1935, regained it in 1937, and held it until his death in 1946.",
        fide_only: false,
    },
    Champion {
        name: "Max Euwe",
        aliases: &["Max Euwe", "Machgielis Euwe"],
        reign: "1935–1937",
        title: "World Champion",
        fact: "The only Dutch World Champion; beat Alexander Alekhine in 1935 and lost the 1937 rematch.",
        fide_only: false,
    },
    Champion {
        name: "Mikhail Botvinnik",
        aliases: &["Mikhail Botvinnik", "Mikhail Moiseyevich Botvinnik"],
        reign: "1948–1957, 1958–1960, 1961–1963",
        title: "World Champion",
        fact: "The first Soviet World Champion; won the 1948 tournament and lost and regained the title twice.",
        fide_only: false,
    },
    Champion {
        name: "Vasily Smyslov",
        aliases: &["Vasily Smyslov", "Vassily Smyslov", "Vasili Smyslov"],
        reign: "1957–1958",
        title: "World Champion",
        fact: "Beat Mikhail Botvinnik in 1957 and lost the 1958 rematch.",
        fide_only: false,
    },
    Champion {
        name: "Mikhail Tal",
        aliases: &["Mikhail Tal", "Mikhail Nekhemyevich Tal"],
        reign: "1960–1961",
        title: "World Champion",
        fact: "At 23, the youngest World Champion at the time; beat Botvinnik in 1960 and lost the 1961 rematch.",
        fide_only: false,
    },
    Champion {
        name: "Tigran Petrosian",
        aliases: &["Tigran Petrosian", "Tigran Vartanovich Petrosian"],
        reign: "1963–1969",
        title: "World Champion",
        fact: "Beat Botvinnik in 1963 and lost the title to Boris Spassky in 1969.",
        fide_only: false,
    },
    Champion {
        name: "Boris Spassky",
        aliases: &["Boris Spassky", "Boris Vasilievich Spassky"],
        reign: "1969–1972",
        title: "World Champion",
        fact: "Beat Tigran Petrosian in 1969 and lost the 1972 match to Bobby Fischer.",
        fide_only: false,
    },
    Champion {
        name: "Bobby Fischer",
        aliases: &[
            "Bobby Fischer",
            "Robert Fischer",
            "Robert J. Fischer",
            "Robert James Fischer",
        ],
        reign: "1972–1975",
        title: "World Champion",
        fact: "The only American-born World Champion; won in 1972 and forfeited the title in 1975 when he refused to defend against Anatoly Karpov.",
        fide_only: false,
    },
    Champion {
        name: "Anatoly Karpov",
        aliases: &[
            "Anatoly Karpov",
            "Anatoli Karpov",
            "Anatoly Yevgenyevich Karpov",
        ],
        reign: "1975–1985",
        title: "World Champion",
        fact: "Awarded the title in 1975 after Fischer declined to defend; lost it to Garry Kasparov in 1985.",
        fide_only: false,
    },
    Champion {
        name: "Garry Kasparov",
        aliases: &["Garry Kasparov", "Gary Kasparov", "Garry Kimovich Kasparov"],
        reign: "1985–2000",
        title: "Classical World Champion",
        fact: "Beat Karpov in 1985 and held the classical title until 2000; formed the PCA in 1993.",
        fide_only: false,
    },
    Champion {
        name: "Vladimir Kramnik",
        aliases: &["Vladimir Kramnik", "Vladimir Borisovich Kramnik"],
        reign: "2000–2007",
        title: "Classical World Champion",
        fact: "Beat Kasparov in 2000 and unified the classical and FIDE titles by beating Veselin Topalov in 2006.",
        fide_only: false,
    },
    Champion {
        name: "Viswanathan Anand",
        aliases: &["Viswanathan Anand", "Vishy Anand"],
        reign: "2007–2013",
        title: "World Champion",
        fact: "The first World Champion from Asia; won the unified title in 2007 and held it until 2013.",
        fide_only: false,
    },
    Champion {
        name: "Magnus Carlsen",
        aliases: &[
            "Magnus Carlsen",
            "Sven Magnus Øen Carlsen",
            "Magnus Oen Carlsen",
        ],
        reign: "2013–2023",
        title: "World Champion",
        fact: "Beat Anand in 2013 and won all five of his World Championship matches before choosing not to defend in 2023.",
        fide_only: false,
    },
    Champion {
        name: "Ding Liren",
        aliases: &["Ding Liren", "Liren Ding"],
        reign: "2023–2024",
        title: "World Champion",
        fact: "Won the 2023 title match against Ian Nepomniachtchi and lost it to Gukesh Dommaraju in 2024.",
        fide_only: false,
    },
    Champion {
        name: "Gukesh Dommaraju",
        aliases: &[
            "Gukesh Dommaraju",
            "Dommaraju Gukesh",
            "Gukesh D",
            "D Gukesh",
        ],
        reign: "2024–present",
        title: "World Champion",
        fact: "At 18, the youngest undisputed World Champion; beat Ding Liren in 2024.",
        fide_only: false,
    },
    Champion {
        name: "Alexander Khalifman",
        aliases: &["Alexander Khalifman", "Alexandre Khalifman"],
        reign: "1999–2000",
        title: "FIDE World Champion",
        fact: "Won the 1999 FIDE knockout championship in Las Vegas.",
        fide_only: true,
    },
    Champion {
        name: "Ruslan Ponomariov",
        aliases: &["Ruslan Ponomariov", "Ruslan Ponomariv"],
        reign: "2002–2004",
        title: "FIDE World Champion",
        fact: "At 18, the youngest FIDE World Champion; won the 2002 knockout championship.",
        fide_only: true,
    },
    Champion {
        name: "Rustam Kasimdzhanov",
        aliases: &[
            "Rustam Kasimdzhanov",
            "Rustam Kasimjanov",
            "Rustam Qosimjonov",
        ],
        reign: "2004–2005",
        title: "FIDE World Champion",
        fact: "Won the 2004 FIDE knockout championship in Tripoli.",
        fide_only: true,
    },
    Champion {
        name: "Veselin Topalov",
        aliases: &["Veselin Topalov"],
        reign: "2005–2006",
        title: "FIDE World Champion",
        fact: "Won the 2005 FIDE championship in San Luis and lost the 2006 unification match to Vladimir Kramnik.",
        fide_only: true,
    },
];

fn normalized(name: &str) -> Option<String> {
    player_key(&deunicode::deunicode(name))
}

/// Look up a player in the selected lineage.
pub fn champion(name: &str, lineage: ChampionLineage) -> Option<&'static Champion> {
    let key = normalized(name)?;
    CHAMPIONS.iter().find(|champion| {
        (!champion.fide_only || lineage == ChampionLineage::Fide)
            && champion
                .aliases
                .iter()
                .any(|alias| normalized(alias).as_deref() == Some(key.as_str()))
    })
}

/// Whether a player should receive the gold crown in the selected lineage.
pub fn is_champion(name: &str, lineage: ChampionLineage) -> bool {
    champion(name, lineage).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASSICAL: [&str; 18] = [
        "Wilhelm Steinitz",
        "Emanuel Lasker",
        "José Raúl Capablanca",
        "Alexander Alekhine",
        "Max Euwe",
        "Mikhail Botvinnik",
        "Vasily Smyslov",
        "Mikhail Tal",
        "Tigran Petrosian",
        "Boris Spassky",
        "Bobby Fischer",
        "Anatoly Karpov",
        "Garry Kasparov",
        "Vladimir Kramnik",
        "Viswanathan Anand",
        "Magnus Carlsen",
        "Ding Liren",
        "Gukesh Dommaraju",
    ];

    const FIDE_ONLY: [&str; 4] = [
        "Alexander Khalifman",
        "Ruslan Ponomariov",
        "Rustam Kasimdzhanov",
        "Veselin Topalov",
    ];

    #[test]
    fn classical_line_contains_every_undisputed_champion() {
        for name in CLASSICAL {
            assert!(
                is_champion(name, ChampionLineage::Classical),
                "{name} should be a classical champion"
            );
            assert!(
                is_champion(name, ChampionLineage::Fide),
                "{name} should also be a FIDE/inclusive champion"
            );
        }
    }

    #[test]
    fn fide_lineage_adds_the_four_knockout_champions() {
        for name in FIDE_ONLY {
            assert!(
                !is_champion(name, ChampionLineage::Classical),
                "{name} is not part of the classical line"
            );
            assert!(
                is_champion(name, ChampionLineage::Fide),
                "{name} is part of the FIDE/inclusive line"
            );
        }
    }

    #[test]
    fn name_order_accents_and_aliases_match() {
        for name in [
            "Capablanca, José Raúl",
            "Jose Raul Capablanca",
            "Alekhine, Alexander",
            "Alexandre Alekhine",
            "Bobby Fischer",
            "Robert J. Fischer",
            "Vishy Anand",
            "Gary Kasparov",
            "Gukesh D",
            "D Gukesh",
            "Dommaraju Gukesh",
            "Liren Ding",
            "Magnus Oen Carlsen",
        ] {
            assert!(
                is_champion(name, ChampionLineage::Classical),
                "{name} should match a classical champion alias"
            );
        }
    }

    #[test]
    fn non_champions_are_never_crowned() {
        for name in [
            "Hikaru Nakamura",
            "Magnus Carlsen Jr",
            "Ding Liren Fan",
            "Unknown Player",
            "",
            "?",
        ] {
            assert!(
                !is_champion(name, ChampionLineage::Fide),
                "{name} must not be crowned"
            );
        }
    }

    #[test]
    fn champion_lookup_returns_display_name_and_reign() {
        let carlsen = champion("Carlsen, Magnus", ChampionLineage::Fide).unwrap();
        assert_eq!(carlsen.name, "Magnus Carlsen");
        assert_eq!(carlsen.reign, "2013–2023");
        assert!(!carlsen.fide_only);

        let khalifman = champion("Alexander Khalifman", ChampionLineage::Fide).unwrap();
        assert_eq!(khalifman.reign, "1999–2000");
        assert!(khalifman.fide_only);
    }

    #[test]
    fn tooltips_state_the_title_reign_and_a_fact() {
        let carlsen = champion("Magnus Carlsen", ChampionLineage::Fide).unwrap();
        let tooltip = carlsen.tooltip();
        assert!(tooltip.contains("Magnus Carlsen"));
        assert!(tooltip.contains("World Champion"));
        assert!(tooltip.contains("2013–2023"));
        assert!(tooltip.contains("all five"));
        assert!(!tooltip.contains("2013-2020"));

        let kasparov = champion("Garry Kasparov", ChampionLineage::Classical).unwrap();
        assert!(kasparov.tooltip().contains("Classical World Champion"));

        let khalifman = champion("Alexander Khalifman", ChampionLineage::Fide).unwrap();
        assert!(khalifman.tooltip().contains("FIDE World Champion"));
        assert!(khalifman.tooltip().contains("1999–2000"));

        for champion in CHAMPIONS {
            let tooltip = champion.tooltip();
            assert!(tooltip.contains(champion.name), "{}", champion.name);
            assert!(tooltip.contains(champion.reign), "{}", champion.name);
            assert!(!champion.fact.is_empty(), "{}", champion.name);
            assert!(champion.fact.ends_with('.'), "{}", champion.name);
            assert!(!champion.title.is_empty(), "{}", champion.name);
        }
    }

    #[test]
    fn every_table_entry_is_unique_and_complete() {
        let mut names = std::collections::HashSet::new();
        for champion in CHAMPIONS {
            assert!(names.insert(champion.name), "duplicate {}", champion.name);
            assert!(!champion.reign.is_empty());
            assert!(!champion.aliases.is_empty());
            assert!(
                champion.aliases.contains(&champion.name),
                "{} must accept its display name",
                champion.name
            );
        }
        assert_eq!(names.len(), CLASSICAL.len() + FIDE_ONLY.len());
    }
}
