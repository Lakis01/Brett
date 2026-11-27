//! Bridge for importing PGN games from German, regional, international, and
//! custom chess league/tournament providers.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::domain::{Game, parse_pgn};

pub const LIGAMANAGER_BASE_URL: &str = "https://ligamanager.schachbund-bayern.de";
pub const DSB_BASE_URL: &str = "https://ergebnisdienst.schachbund.de";
pub const CHESS_RESULTS_POST_PREFIX: &str = "brett-cr-post:";

const USER_AGENT: &str = "Brett/0.3 (+https://github.com/Lakis01/Brett)";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LeaguePortal {
    #[default]
    LigaManagerBayern,
    DsbErgebnisdienst,
}

impl LeaguePortal {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LigaManagerBayern => "LigaManager Bayern",
            Self::DsbErgebnisdienst => "DSB Ergebnisdienst",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    LigaManagerBayern,
    DsbErgebnisdienst,
    LichessBroadcast,
    ChessResultsFederation,
    RegionalHtmlPortal,
    CustomUrl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChessResultsTournamentType {
    #[default]
    TeamRoundRobin,
    TeamSwiss,
    All,
    IndividualSwiss,
    IndividualRoundRobin,
}

impl ChessResultsTournamentType {
    pub const ALL: [Self; 5] = [
        Self::TeamRoundRobin,
        Self::TeamSwiss,
        Self::All,
        Self::IndividualSwiss,
        Self::IndividualRoundRobin,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TeamRoundRobin => "2",
            Self::TeamSwiss => "3",
            Self::All => "5",
            Self::IndividualSwiss => "0",
            Self::IndividualRoundRobin => "1",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::TeamRoundRobin => "Team Leagues (RR)",
            Self::TeamSwiss => "Team Swiss",
            Self::All => "All Tournaments",
            Self::IndividualSwiss => "Individual Swiss",
            Self::IndividualRoundRobin => "Individual RR",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChessResultsTournament {
    pub tnr: String,
    pub title: String,
    pub fed: String,
    pub last_update: String,
    pub start_date: String,
    pub end_date: String,
    pub bulk_pgn_url: String,
}

#[must_use]
pub fn parse_cr_default_query(default_query: Option<&str>) -> (String, ChessResultsTournamentType) {
    let raw = default_query.unwrap_or("").trim();
    if raw.is_empty() {
        return ("-".to_string(), ChessResultsTournamentType::All);
    }
    let (fed_raw, kind_raw) = raw.split_once(':').unwrap_or((raw, "all"));
    let fed = if fed_raw.trim().is_empty() {
        "-".to_string()
    } else {
        fed_raw.trim().to_ascii_uppercase()
    };
    let tour_type = match kind_raw.trim().to_ascii_lowercase().as_str() {
        "team" | "rr" => ChessResultsTournamentType::TeamRoundRobin,
        "teamswiss" => ChessResultsTournamentType::TeamSwiss,
        "swiss" => ChessResultsTournamentType::IndividualSwiss,
        "indrr" => ChessResultsTournamentType::IndividualRoundRobin,
        _ => ChessResultsTournamentType::All,
    };
    (fed, tour_type)
}

#[derive(Clone, Copy, Debug)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub region: &'static str,
    pub country_code: &'static str,
    pub country_name: &'static str,
    pub kind: ProviderKind,
    pub url: &'static str,
    pub keywords: &'static str,
    pub default_query: Option<&'static str>,
}

pub static PROVIDER_CATALOG: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: "ligamanager-bayern",
        name: "LigaManager Bayern",
        region: "Bavaria (BSB)",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::LigaManagerBayern,
        url: "https://ligamanager.schachbund-bayern.de",
        keywords: "bayern bavaria bsb muenchen nuernberg oberliga bezirksliga kreisliga erlangen passau",
        default_query: None,
    },
    ProviderDescriptor {
        id: "dsb-ergebnisdienst",
        name: "DSB Ergebnisdienst (Bundesliga / Oberligen)",
        region: "Germany (DSB)",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::DsbErgebnisdienst,
        url: "https://ergebnisdienst.schachbund.de",
        keywords: "dsb schachbund bundesliga oberliga deutschland germany bedh",
        default_query: None,
    },
    ProviderDescriptor {
        id: "schachbundesliga",
        name: "Schachbundesliga (DSB 1./2. Bundesliga)",
        region: "Germany (1st League)",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::DsbErgebnisdienst,
        url: "https://ergebnisdienst.schachbund.de/bedh.php?liga=bl",
        keywords: "bundesliga schachbundesliga pgn baden-baden viernheim osg",
        default_query: Some("Bundesliga"),
    },
    ProviderDescriptor {
        id: "lichess-broadcasts",
        name: "Lichess Broadcasts (OTB & Tournaments)",
        region: "International",
        country_code: "",
        country_name: "International",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "lichess broadcast tournament otb tata steel candidates olympiad open world cup",
        default_query: None,
    },
    ProviderDescriptor {
        id: "chess-results",
        name: "Chess-Results.com (All Countries · PGN Directory)",
        region: "International",
        country_code: "",
        country_name: "International",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/TurnierSuche.aspx?lan=1",
        keywords: "chess-results partiesuche turniersuche swiss open fide club pgn tnr",
        default_query: Some("-:all"),
    },
    ProviderDescriptor {
        id: "chess-results-ger",
        name: "Germany — Leagues & Opens with PGNs (Chess-Results)",
        region: "Germany",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=GER",
        keywords: "germany deutschland chess-results bundesliga oberliga open pgn",
        default_query: Some("GER:all"),
    },
    ProviderDescriptor {
        id: "nrw-portal",
        name: "Schachbund NRW (Oberliga & NRW Ligen via DSB)",
        region: "North Rhine-Westphalia",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::DsbErgebnisdienst,
        url: "https://ergebnisdienst.schachbund.de",
        keywords: "nrw nordrhein westfalen koeln duesseldorf dortmund essen bochum oberliga",
        default_query: Some("NRW"),
    },
    ProviderDescriptor {
        id: "4ncl-uk",
        name: "4NCL (Four Nations Chess League)",
        region: "United Kingdom",
        country_code: "ENG",
        country_name: "United Kingdom",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.4ncl.co.uk/",
        keywords: "4ncl uk england british league london pgn",
        default_query: None,
    },
    ProviderDescriptor {
        id: "chess-results-eng",
        name: "England — Leagues & Opens with PGNs (Chess-Results)",
        region: "United Kingdom",
        country_code: "ENG",
        country_name: "United Kingdom",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=ENG",
        keywords: "england uk british varsity league open chess-results pgn",
        default_query: Some("ENG:all"),
    },
    ProviderDescriptor {
        id: "pgnmentor",
        name: "PGN Mentor Archives (.zip PGN collections)",
        region: "International",
        country_code: "",
        country_name: "International",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.pgnmentor.com/files.html",
        keywords: "pgnmentor archives openings players tournaments zip",
        default_query: None,
    },
    ProviderDescriptor {
        id: "chess-results-aut",
        name: "Austria — All Leagues & Tournaments (Chess-Results)",
        region: "Austria",
        country_code: "AUT",
        country_name: "Austria",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=AUT",
        keywords: "oesterreich austria wien graz salzburg tirol vorarlberg bundesliga landesliga staatsmeisterschaft oesb chess-results",
        default_query: Some("AUT:team"),
    },
    ProviderDescriptor {
        id: "chess-results-aut-all",
        name: "Austria — Opens & Individual Tournaments (Chess-Results)",
        region: "Austria",
        country_code: "AUT",
        country_name: "Austria",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=AUT",
        keywords: "oesterreich austria open staatsmeisterschaft turnier chess-results",
        default_query: Some("AUT:all"),
    },
    ProviderDescriptor {
        id: "top16-france",
        name: "France — Top 16 / Nationale (via Lichess)",
        region: "France",
        country_code: "FRA",
        country_name: "France",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "france top 16 top16 ffe paris lyon marseille bordeaux",
        default_query: Some("Top 16"),
    },
    ProviderDescriptor {
        id: "chess-results-fra",
        name: "France — Tournaments with PGNs (Chess-Results)",
        region: "France",
        country_code: "FRA",
        country_name: "France",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=FRA",
        keywords: "france french open chess-results pgn",
        default_query: Some("FRA:all"),
    },
    ProviderDescriptor {
        id: "meesterklasse-nl",
        name: "Netherlands — Meesterklasse (via Lichess)",
        region: "Netherlands",
        country_code: "NED",
        country_name: "Netherlands",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "netherlands holland knsb meesterklasse kennemer amsterdam",
        default_query: Some("Meesterklasse"),
    },
    ProviderDescriptor {
        id: "chess-results-ned",
        name: "Netherlands — Tournaments with PGNs (Chess-Results)",
        region: "Netherlands",
        country_code: "NED",
        country_name: "Netherlands",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=NED",
        keywords: "netherlands holland knsb meesterklasse chess-results pgn",
        default_query: Some("NED:all"),
    },
    ProviderDescriptor {
        id: "ceclub-spain",
        name: "Spain — División de Honor / CECLUB (via Lichess)",
        region: "Spain",
        country_code: "ESP",
        country_name: "Spain",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "spain espana ceclub fedo madrid barcelona",
        default_query: Some("CECLUB"),
    },
    ProviderDescriptor {
        id: "chess-results-esp",
        name: "Spain — Team Leagues with PGNs (Chess-Results)",
        region: "Spain",
        country_code: "ESP",
        country_name: "Spain",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=ESP",
        keywords: "spain espana equipos liga chess-results pgn",
        default_query: Some("ESP:team"),
    },
    ProviderDescriptor {
        id: "cis-italy",
        name: "Italy — CIS Serie Master (via Lichess)",
        region: "Italy",
        country_code: "ITA",
        country_name: "Italy",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "italy italia cis fsi serie master roma milano",
        default_query: Some("CIS"),
    },
    ProviderDescriptor {
        id: "chess-results-ita",
        name: "Italy — Leagues & Opens with PGNs (Chess-Results)",
        region: "Italy",
        country_code: "ITA",
        country_name: "Italy",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=ITA",
        keywords: "italy italia cis fsi chess-results pgn",
        default_query: Some("ITA:all"),
    },
    ProviderDescriptor {
        id: "ekstraliga-poland",
        name: "Poland — Ekstraliga (via Lichess)",
        region: "Poland",
        country_code: "POL",
        country_name: "Poland",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "poland polska ekstraliga pzszach warsaw wroclaw",
        default_query: Some("Ekstraliga"),
    },
    ProviderDescriptor {
        id: "chess-results-pol",
        name: "Poland — Tournaments with PGNs (Chess-Results)",
        region: "Poland",
        country_code: "POL",
        country_name: "Poland",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=POL",
        keywords: "poland polska chess-results pgn",
        default_query: Some("POL:all"),
    },
    ProviderDescriptor {
        id: "extraliga-czech",
        name: "Czech & Slovak Extraliga (via Lichess)",
        region: "Czechia / Slovakia",
        country_code: "CZE",
        country_name: "Czechia / Slovakia",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "czech slovakia extraliga prague novy bor",
        default_query: Some("Extraliga"),
    },
    ProviderDescriptor {
        id: "chess-results-cze",
        name: "Czechia — Team Leagues with PGNs (Chess-Results)",
        region: "Czechia / Slovakia",
        country_code: "CZE",
        country_name: "Czechia / Slovakia",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=CZE",
        keywords: "czechia czech extraliga liga prebor chess-results pgn",
        default_query: Some("CZE:team"),
    },
    ProviderDescriptor {
        id: "chess-results-svk",
        name: "Slovakia — Leagues & Opens with PGNs (Chess-Results)",
        region: "Czechia / Slovakia",
        country_code: "SVK",
        country_name: "Czechia / Slovakia",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=SVK",
        keywords: "slovakia slovak extraliga chess-results pgn",
        default_query: Some("SVK:all"),
    },
    ProviderDescriptor {
        id: "elitserien-sweden",
        name: "Sweden — Elitserien (via Lichess)",
        region: "Sweden",
        country_code: "SWE",
        country_name: "Sweden",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "sweden sverige elitserien stockholm lund",
        default_query: Some("Elitserien"),
    },
    ProviderDescriptor {
        id: "chess-results-swe",
        name: "Sweden — Tournaments with PGNs (Chess-Results)",
        region: "Sweden",
        country_code: "SWE",
        country_name: "Sweden",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=SWE",
        keywords: "sweden sverige chess-results pgn",
        default_query: Some("SWE:all"),
    },
    ProviderDescriptor {
        id: "skakligaen-denmark",
        name: "Denmark — Skakligaen (via Lichess)",
        region: "Denmark",
        country_code: "DEN",
        country_name: "Denmark",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "denmark danmark skakligaen copenhagen",
        default_query: Some("Skakligaen"),
    },
    ProviderDescriptor {
        id: "chess-results-den",
        name: "Denmark — Tournaments with PGNs (Chess-Results)",
        region: "Denmark",
        country_code: "DEN",
        country_name: "Denmark",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=DEN",
        keywords: "denmark danmark chess-results pgn",
        default_query: Some("DEN:all"),
    },
    ProviderDescriptor {
        id: "eliteserien-norway",
        name: "Norway — Eliteserien (via Lichess)",
        region: "Norway",
        country_code: "NOR",
        country_name: "Norway",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "norway norge eliteserien oslo offerspill",
        default_query: Some("Eliteserien"),
    },
    ProviderDescriptor {
        id: "chess-results-nor",
        name: "Norway — Tournaments with PGNs (Chess-Results)",
        region: "Norway",
        country_code: "NOR",
        country_name: "Norway",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=NOR",
        keywords: "norway norge chess-results pgn",
        default_query: Some("NOR:all"),
    },
    ProviderDescriptor {
        id: "superleague-hungary",
        name: "Hungary — NB I / Superleague (via Lichess)",
        region: "Hungary",
        country_code: "HUN",
        country_name: "Hungary",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "hungary magyar hungarian budapest team",
        default_query: Some("Hungarian"),
    },
    ProviderDescriptor {
        id: "chess-results-hun",
        name: "Hungary — Team Leagues with PGNs (Chess-Results)",
        region: "Hungary",
        country_code: "HUN",
        country_name: "Hungary",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=HUN",
        keywords: "hungary magyar csapatbajnoksag chess-results pgn",
        default_query: Some("HUN:team"),
    },
    ProviderDescriptor {
        id: "superliga-romania",
        name: "Romania — Superliga (via Lichess)",
        region: "Romania",
        country_code: "ROU",
        country_name: "Romania",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "romania superliga bucharest cluj",
        default_query: Some("Superliga"),
    },
    ProviderDescriptor {
        id: "chess-results-rou",
        name: "Romania — Team Leagues with PGNs (Chess-Results)",
        region: "Romania",
        country_code: "ROU",
        country_name: "Romania",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=ROU",
        keywords: "romania superliga campionatul chess-results pgn",
        default_query: Some("ROU:team"),
    },
    ProviderDescriptor {
        id: "prva-liga-serbia",
        name: "Serbia — Prva Liga (via Lichess)",
        region: "Serbia",
        country_code: "SRB",
        country_name: "Serbia",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "serbia srbija prva liga belgrade novi sad",
        default_query: Some("Serbia"),
    },
    ProviderDescriptor {
        id: "chess-results-srb",
        name: "Serbia — Team Leagues with PGNs (Chess-Results)",
        region: "Serbia",
        country_code: "SRB",
        country_name: "Serbia",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=SRB",
        keywords: "serbia srbija prva liga druga liga chess-results pgn",
        default_query: Some("SRB:team"),
    },
    ProviderDescriptor {
        id: "chess-results-cro",
        name: "Croatia — Team Leagues with PGNs (Chess-Results)",
        region: "Croatia",
        country_code: "CRO",
        country_name: "Croatia",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=CRO",
        keywords: "croatia hrvatska ekipno prvenstvo liga chess-results pgn",
        default_query: Some("CRO:team"),
    },
    ProviderDescriptor {
        id: "chess-results-slo",
        name: "Slovenia — Team Leagues with PGNs (Chess-Results)",
        region: "Slovenia",
        country_code: "SLO",
        country_name: "Slovenia",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=SLO",
        keywords: "slovenia slovenija drzavna clanska liga chess-results pgn",
        default_query: Some("SLO:team"),
    },
    ProviderDescriptor {
        id: "superleague-turkey",
        name: "Turkey — Super League / TSF (via Lichess)",
        region: "Turkey",
        country_code: "TUR",
        country_name: "Turkey",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "turkey turkiye tsf turkish super league istanbul ankara",
        default_query: Some("Turkish Super League"),
    },
    ProviderDescriptor {
        id: "chess-results-tur",
        name: "Turkey — Tournaments with PGNs (Chess-Results)",
        region: "Turkey",
        country_code: "TUR",
        country_name: "Turkey",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=TUR",
        keywords: "turkey turkiye tsf chess-results pgn",
        default_query: Some("TUR:all"),
    },
    ProviderDescriptor {
        id: "us-championship",
        name: "USA — US Championships & Open (via Lichess)",
        region: "United States",
        country_code: "USA",
        country_name: "United States",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "usa united states st louis us championship",
        default_query: Some("US Championship"),
    },
    ProviderDescriptor {
        id: "chess-results-usa",
        name: "USA — Tournaments with PGNs (Chess-Results)",
        region: "United States",
        country_code: "USA",
        country_name: "United States",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=USA",
        keywords: "usa united states chess-results pgn",
        default_query: Some("USA:all"),
    },
    ProviderDescriptor {
        id: "bsv-baden",
        name: "Badischer Schachverband (BSV)",
        region: "Baden",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.badischer-schachverband.de",
        keywords: "baden karlsruhe mannheim heidelberg freiburg bsv",
        default_query: None,
    },
    ProviderDescriptor {
        id: "svw-wuerttemberg",
        name: "Schachverband Württemberg (SVW)",
        region: "Württemberg",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.svw.info",
        keywords: "wuerttemberg stuttgart ulm tuebingen svw baden-wuerttemberg",
        default_query: None,
    },
    ProviderDescriptor {
        id: "hsv-hessen",
        name: "Hessischer Schachverband (HSV)",
        region: "Hesse",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.hessischer-schachverband.de",
        keywords: "hessen hesse frankfurt wiesbaden kassel darmstadt hsv",
        default_query: None,
    },
    ProviderDescriptor {
        id: "nsv-niedersachsen",
        name: "Niedersächsischer Schachverband (NSV)",
        region: "Lower Saxony",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.nsv-online.de",
        keywords: "niedersachsen hannover braunschweig goettingen bremen nsv",
        default_query: None,
    },
    ProviderDescriptor {
        id: "svs-sachsen",
        name: "Schachverband Sachsen (SVS)",
        region: "Saxony",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.schachverband-sachsen.de",
        keywords: "sachsen dresden leipzig chemnitz svs",
        default_query: None,
    },
    ProviderDescriptor {
        id: "bsv-berlin",
        name: "Berliner Schachverband (BMM)",
        region: "Berlin",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.berlinerschachverband.de",
        keywords: "berlin bmm berliner mannschaftsmeisterschaft",
        default_query: None,
    },
    ProviderDescriptor {
        id: "sbrp-rlp",
        name: "Schachbund Rheinland-Pfalz",
        region: "Rhineland-Palatinate",
        country_code: "GER",
        country_name: "Germany",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.sbrp.de",
        keywords: "rheinland pfalz mainz koblenz trier sbrp",
        default_query: None,
    },
    ProviderDescriptor {
        id: "smm-switzerland",
        name: "Switzerland — SMM Nationalliga A (via Lichess)",
        region: "Switzerland",
        country_code: "SUI",
        country_name: "Switzerland",
        kind: ProviderKind::LichessBroadcast,
        url: "https://lichess.org/broadcast",
        keywords: "schweiz switzerland smm nationalliga nla zurich basel genf ssb",
        default_query: Some("SMM"),
    },
    ProviderDescriptor {
        id: "chess-results-sui",
        name: "Switzerland — Tournaments with PGNs (Chess-Results)",
        region: "Switzerland",
        country_code: "SUI",
        country_name: "Switzerland",
        kind: ProviderKind::ChessResultsFederation,
        url: "https://s1.chess-results.com/fed.aspx?lan=1&fed=SUI",
        keywords: "schweiz switzerland biel davos bundesturnier chess-results pgn",
        default_query: Some("SUI:all"),
    },
    ProviderDescriptor {
        id: "ssb-switzerland",
        name: "Schweizerischer Schachbund (SSB / swisschess.ch)",
        region: "Switzerland",
        country_code: "SUI",
        country_name: "Switzerland",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.swisschess.ch",
        keywords: "schweiz switzerland zurich basel genf smm sgm ssb swisschess",
        default_query: None,
    },
    ProviderDescriptor {
        id: "ffe-france",
        name: "Fédération Française des Échecs (FFE)",
        region: "France (FFE)",
        country_code: "FRA",
        country_name: "France",
        kind: ProviderKind::RegionalHtmlPortal,
        url: "https://www.echecs.asso.fr",
        keywords: "france paris lyon top16 ffe echecs.asso.fr",
        default_query: None,
    },
    ProviderDescriptor {
        id: "custom-url",
        name: "Custom URL (.pgn, .zip, Chess-Results, or tournament page)",
        region: "Custom",
        country_code: "",
        country_name: "Custom",
        kind: ProviderKind::CustomUrl,
        url: "",
        keywords: "custom url direct pgn zip link paste html website",
        default_query: None,
    },
];

#[must_use]
pub fn filter_providers(query: &str) -> Vec<&'static ProviderDescriptor> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if terms.is_empty() {
        return PROVIDER_CATALOG.iter().collect();
    }
    PROVIDER_CATALOG
        .iter()
        .filter(|p| {
            let hay = format!(
                "{} {} {} {} {} {} {}",
                p.id, p.name, p.region, p.country_name, p.country_code, p.url, p.keywords
            )
            .to_lowercase();
            terms.iter().all(|t| hay.contains(t))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LichessBroadcastRound {
    pub id: String,
    pub name: String,
    pub ongoing: bool,
    pub finished: bool,
    pub pgn_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LichessBroadcastTour {
    pub id: String,
    pub name: String,
    pub location: Option<String>,
    pub info: Option<String>,
    pub all_pgn_url: String,
    pub rounds: Vec<LichessBroadcastRound>,
}

fn push_broadcast_value(value: &Value, out: &mut Vec<LichessBroadcastTour>) {
    let tour = value.get("tour").unwrap_or(value);
    let Some(id) = tour.get("id").and_then(Value::as_str).map(str::trim) else {
        return;
    };
    if id.is_empty() {
        return;
    }
    let name = tour
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(id)
        .to_string();
    let info_obj = tour.get("info");
    let location = info_obj
        .and_then(|v| v.get("location"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut info_parts = Vec::new();
    for key in ["format", "tc"] {
        if let Some(val) = info_obj
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            info_parts.push(val);
        }
    }
    let info = (!info_parts.is_empty()).then(|| info_parts.join(" · "));
    let rounds = value
        .get("rounds")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let rid = r.get("id").and_then(Value::as_str).map(str::trim)?;
                    if rid.is_empty() {
                        return None;
                    }
                    let rname = r
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(rid)
                        .to_string();
                    Some(LichessBroadcastRound {
                        id: rid.to_string(),
                        name: rname,
                        ongoing: r.get("ongoing").and_then(Value::as_bool).unwrap_or(false),
                        finished: r.get("finished").and_then(Value::as_bool).unwrap_or(false),
                        pgn_url: format!("https://lichess.org/api/broadcast/round/{rid}.pgn"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    out.push(LichessBroadcastTour {
        id: id.to_string(),
        name,
        location,
        info,
        all_pgn_url: format!("https://lichess.org/api/broadcast/{id}.pgn"),
        rounds,
    });
}

/// Parse Lichess `/api/broadcast` NDJSON (or `/api/broadcast/search` JSON) payloads.
#[must_use]
pub fn parse_lichess_broadcasts_ndjson(body: &str) -> Vec<LichessBroadcastTour> {
    let mut out = Vec::new();
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return out;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(arr) = value.as_array() {
            for item in arr {
                push_broadcast_value(item, &mut out);
            }
            return out;
        }
        if let Some(arr) = value.get("currentPageResults").and_then(Value::as_array) {
            for item in arr {
                push_broadcast_value(item, &mut out);
            }
            return out;
        }
        let mut grouped = false;
        for group in ["active", "upcoming", "past"] {
            if let Some(arr) = value.get(group).and_then(Value::as_array) {
                grouped = true;
                for item in arr {
                    push_broadcast_value(item, &mut out);
                }
            }
        }
        if grouped {
            return out;
        }
        push_broadcast_value(&value, &mut out);
        return out;
    }

    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            push_broadcast_value(&value, &mut out);
        }
    }
    out
}

fn encode_query_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

pub fn fetch_lichess_broadcasts(
    limit: usize,
    search_query: Option<&str>,
) -> Result<Vec<LichessBroadcastTour>> {
    let q = search_query.map(str::trim).unwrap_or("");
    let url = if q.is_empty() {
        format!(
            "https://lichess.org/api/broadcast?nb={}",
            limit.clamp(1, 100)
        )
    } else {
        format!(
            "https://lichess.org/api/broadcast/search?q={}",
            encode_query_component(q)
        )
    };
    let resp = http_agent()
        .get(&url)
        .set("Accept", "application/x-ndjson, application/json")
        .call()
        .with_context(|| format!("fetching {url}"))?;
    let mut reader = resp.into_reader();
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut raw)
        .with_context(|| format!("reading response from {url}"))?;
    let text = String::from_utf8_lossy(&raw);
    Ok(parse_lichess_broadcasts_ndjson(&text))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredPgnLink {
    pub title: String,
    pub pgn_url: String,
    pub source_label: String,
    pub initial_round: Option<u16>,
}

fn url_scheme_and_origin(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        ("https", r)
    } else {
        ("http", url.strip_prefix("http://")?)
    };
    let (authority, path_and_query) = match rest.find(['/', '?', '#']) {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return None;
    }
    Some((scheme, authority, path_and_query))
}

#[must_use]
pub fn resolve_generic_url(base_url: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty()
        || href.starts_with('#')
        || href.to_ascii_lowercase().starts_with("javascript:")
        || href.to_ascii_lowercase().starts_with("mailto:")
    {
        return None;
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_string());
    }
    let (scheme, authority, base_tail) = url_scheme_and_origin(base_url.trim())?;
    if href.starts_with("//") {
        return Some(format!("{scheme}:{href}"));
    }
    if href.starts_with('/') {
        return Some(format!("{scheme}://{authority}{href}"));
    }
    let base_path = base_tail.split(['?', '#']).next().unwrap_or("");
    let base_dir = match base_path.rfind('/') {
        Some(idx) => &base_path[..=idx],
        None => "/",
    };
    let (href_path, href_suffix) = match href.find(['?', '#']) {
        Some(idx) => (&href[..idx], &href[idx..]),
        None => (href, ""),
    };
    let mut stack: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in href_path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    let mut resolved = format!("{scheme}://{authority}/{}", stack.join("/"));
    if href_path.ends_with('/') && !resolved.ends_with('/') {
        resolved.push('/');
    }
    resolved.push_str(href_suffix);
    Some(resolved)
}

fn looks_like_pgn_href(resolved_url: &str, anchor_text: &str) -> bool {
    let lower_url = resolved_url.to_ascii_lowercase();
    let path_only = lower_url.split(['?', '#']).next().unwrap_or("");
    if path_only.ends_with(".pgn")
        || lower_url.contains(".pgn?")
        || lower_url.contains(".pgn&")
        || lower_url.contains("format=pgn")
        || lower_url.contains("output=pgn")
        || lower_url.contains("export=pgn")
    {
        return true;
    }
    let text_lower = anchor_text.to_ascii_lowercase();
    if (path_only.ends_with(".zip") || lower_url.contains(".zip?"))
        && (lower_url.contains("pgn")
            || lower_url.contains("games")
            || lower_url.contains("partien")
            || lower_url.contains("pgnmentor")
            || lower_url.contains("twic")
            || lower_url.contains("bundesliga")
            || lower_url.contains("liga")
            || lower_url.contains("archive")
            || lower_url.contains("download")
            || text_lower.contains("pgn")
            || text_lower.contains("games")
            || text_lower.contains("partien"))
    {
        return true;
    }
    text_lower.contains("pgn")
        && (lower_url.contains("download")
            || lower_url.contains("export")
            || lower_url.contains("/pgn"))
        && !path_only.ends_with(".html")
        && !path_only.ends_with(".htm")
        && !path_only.ends_with(".pdf")
}

/// Extract deduplicated `.pgn` and PGN-`.zip` download links from an arbitrary portal HTML page.
#[must_use]
pub fn discover_html_pgn_links(html: &str, page_url: &str) -> Vec<DiscoveredPgnLink> {
    let source_label = url_scheme_and_origin(page_url.trim())
        .map(|(_, host, _)| host.to_string())
        .unwrap_or_else(|| page_url.trim().to_string());
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (_, href, text) in extract_anchors(html) {
        let Some(pgn_url) = resolve_generic_url(page_url, &href) else {
            continue;
        };
        if !looks_like_pgn_href(&pgn_url, &text) {
            continue;
        }
        if !seen.insert(pgn_url.clone()) {
            continue;
        }
        let cleaned = text.trim();
        let title = if cleaned.is_empty()
            || cleaned.eq_ignore_ascii_case("pgn")
            || cleaned.eq_ignore_ascii_case("download")
        {
            pgn_url
                .split(['?', '#'])
                .next()
                .and_then(|p| p.rsplit('/').find(|s| !s.is_empty()))
                .unwrap_or("PGN Download")
                .to_string()
        } else {
            cleaned.to_string()
        };
        out.push(DiscoveredPgnLink {
            title,
            pgn_url,
            source_label: source_label.clone(),
            initial_round: None,
        });
    }
    out
}

fn rewrite_direct_portal_pgn_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let clean = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    let clean_lower = clean.to_ascii_lowercase();
    if clean_lower.ends_with(".pgn") || clean_lower.ends_with(".zip") {
        return Some(trimmed.to_string());
    }
    let (_, authority, tail) = url_scheme_and_origin(trimmed)?;
    if !authority.eq_ignore_ascii_case("lichess.org")
        && !authority.eq_ignore_ascii_case("www.lichess.org")
    {
        return None;
    }
    let path = tail.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.first().copied() == Some("study") && segs.len() >= 2 {
        return Some(format!("https://lichess.org/api/study/{}.pgn", segs[1]));
    }
    if segs.first().copied() == Some("broadcast") {
        if segs.len() >= 4 {
            return Some(format!(
                "https://lichess.org/api/broadcast/round/{}.pgn",
                segs[3]
            ));
        }
        if segs.len() == 3 {
            return Some(format!("https://lichess.org/api/broadcast/{}.pgn", segs[2]));
        }
    }
    None
}

fn is_chess_results_host(url: &str) -> bool {
    let Some((_, authority, _)) = url_scheme_and_origin(url.trim()) else {
        return false;
    };
    let host = authority
        .split(':')
        .next()
        .unwrap_or(authority)
        .to_ascii_lowercase();
    host == "chess-results.com" || host.ends_with(".chess-results.com")
}

fn normalize_chess_results_url(url: &str) -> String {
    let trimmed = url.trim();
    if !is_chess_results_host(trimmed) {
        return trimmed.to_string();
    }
    let (base, frag) = match trimmed.find('#') {
        Some(idx) => (&trimmed[..idx], &trimmed[idx..]),
        None => (trimmed, ""),
    };
    let lower = base.to_ascii_lowercase();
    if lower.contains("?lan=") || lower.contains("&lan=") {
        return trimmed.to_string();
    }
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}lan=1{frag}")
}

fn extract_chess_results_tnr(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    if let Some(idx) = lower.find("tnr=") {
        let digits: String = url[idx + 4..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            return Some(digits);
        }
    }
    if let Some(idx) = lower.find("/tnr") {
        let digits: String = url[idx + 4..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            return Some(digits);
        }
    }
    let trimmed = url.trim_start_matches(['/', '.', '\\']);
    if trimmed.to_ascii_lowercase().starts_with("tnr") {
        let digits: String = trimmed[3..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            return Some(digits);
        }
    }
    None
}

fn extract_hidden_input(html: &str, name: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find("<input") {
        let start = pos + rel;
        let Some(end_rel) = html[start..].find('>') else {
            break;
        };
        let tag_inner = &html[start + 6..start + end_rel];
        if attr_value(tag_inner, "name").is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return Some(attr_value(tag_inner, "value").unwrap_or_default());
        }
        pos = start + end_rel + 1;
    }
    None
}

fn collect_chess_results_form_inputs(html: &str) -> (Vec<(String, String)>, Option<String>) {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut pgn_button_value = None;
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find("<input") {
        let start = pos + rel;
        let Some(end_rel) = html[start..].find('>') else {
            break;
        };
        let tag_inner = &html[start + 6..start + end_rel];
        let input_type = attr_value(tag_inner, "type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let name = attr_value(tag_inner, "name").unwrap_or_default();
        if name.eq_ignore_ascii_case("ctl00$P1$cb_DownLoadPGN") {
            let val = attr_value(tag_inner, "value").unwrap_or_default();
            if !val.is_empty() {
                pgn_button_value = Some(val);
            }
        } else if !name.is_empty()
            && matches!(input_type.as_str(), "hidden" | "text" | "date" | "")
            && seen.insert(name.clone())
        {
            let val = attr_value(tag_inner, "value").unwrap_or_default();
            out.push((name, val));
        }
        pos = start + end_rel + 1;
    }
    (out, pgn_button_value)
}

fn clean_cr_move_token(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    let digit_len = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digit_len > 0 && s[digit_len..].starts_with('.') {
        s = s[digit_len..].trim_start_matches('.').trim();
    }
    let s = s.trim_end_matches(['!', '?', '‼', '⁉', '⁈', '⁇']).trim();
    if s.is_empty()
        || s == "..."
        || s == ".."
        || matches!(s, "1-0" | "0-1" | "1/2-1/2" | "½-½" | "*")
    {
        return None;
    }
    if let Some(rest) = s.strip_prefix("0-0-0") {
        return Some(format!("O-O-O{rest}"));
    }
    if let Some(rest) = s.strip_prefix("0-0") {
        return Some(format!("O-O{rest}"));
    }
    Some(s.to_string())
}

fn extract_chess_results_round(url: &str) -> Option<u16> {
    let lower = url.to_ascii_lowercase();
    for pat in ["?rd=", "&rd="] {
        if let Some(idx) = lower.find(pat) {
            let digits: String = url[idx + pat.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(r) = digits.parse::<u16>()
                && r > 0
            {
                return Some(r);
            }
        }
    }
    None
}

fn extract_chess_results_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut h_pos = 0usize;
    while let Some(rel) = lower[h_pos..].find("<h2") {
        let start = h_pos + rel;
        let Some(gt_rel) = html[start..].find('>') else {
            break;
        };
        let body_start = start + gt_rel + 1;
        let Some(close_rel) = lower[body_start..].find("</h2>") else {
            break;
        };
        let txt = decode_entities(&strip_tags(&html[body_start..body_start + close_rel]))
            .trim()
            .to_string();
        if !txt.is_empty()
            && !txt.eq_ignore_ascii_case("Game-Database of Chess Results")
            && !txt.eq_ignore_ascii_case("Board Pairings")
            && !txt.to_ascii_lowercase().starts_with("ranking")
        {
            return Some(txt);
        }
        h_pos = body_start + close_rel + 5;
    }
    if let Some(ts) = lower.find("<title")
        && let Some(gt) = html[ts..].find('>')
        && let Some(te) = lower[ts + gt + 1..].find("</title>")
    {
        let raw = decode_entities(&strip_tags(&html[ts + gt + 1..ts + gt + 1 + te]));
        let cleaned = raw
            .trim()
            .trim_start_matches("Chess-Results Server Chess-results.com - ")
            .trim_start_matches("Chess-Results.com - ")
            .trim();
        if !cleaned.is_empty()
            && !cleaned.eq_ignore_ascii_case("Partien")
            && !cleaned.eq_ignore_ascii_case("Game-Database of Chess Results")
        {
            return Some(cleaned.to_string());
        }
    }
    None
}

fn chess_results_bulk_post_url(page_url: &str, tnr: &str) -> Option<String> {
    let (scheme, auth, _) = url_scheme_and_origin(page_url.trim())?;
    Some(format!(
        "{CHESS_RESULTS_POST_PREFIX}{scheme}://{auth}/PartieSuche.aspx?lan=1&art=4&tnr={tnr}"
    ))
}

fn looks_like_cloudflare_challenge(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("cf-browser-verification")
        || lower.contains("challenge-platform")
        || lower.contains("/cdn-cgi/challenge-platform")
        || (lower.contains("just a moment...") && lower.contains("cloudflare"))
}

fn extract_form_action(html: &str, base_url: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let Some(start) = lower.find("<form")
        && let Some(end_rel) = html[start..].find('>')
        && let Some(action) = attr_value(&html[start + 5..start + end_rel], "action")
        && let Some(resolved) = resolve_generic_url(base_url, &action)
    {
        return resolved;
    }
    base_url.to_string()
}

fn german_san_to_english(san: &str) -> String {
    let mut chars = san.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let rest: String = chars.collect();
    let mut out = String::with_capacity(san.len());
    match first {
        'D' => out.push('Q'),
        'T' => out.push('R'),
        'L' => out.push('B'),
        'S' => out.push('N'),
        c => out.push(c),
    }
    if let Some(eq_idx) = rest.find('=') {
        out.push_str(&rest[..=eq_idx]);
        for c in rest[eq_idx + 1..].chars() {
            match c {
                'D' => out.push('Q'),
                'T' => out.push('R'),
                'L' => out.push('B'),
                'S' => out.push('N'),
                other => out.push(other),
            }
        }
    } else {
        out.push_str(&rest);
    }
    out
}

fn parse_cr_player_and_elo(raw: &str) -> (String, Option<String>) {
    let cleaned = decode_entities(&strip_tags(raw)).trim().to_string();
    if let Some(open) = cleaned.rfind('(')
        && cleaned.ends_with(')')
    {
        let inner = cleaned[open + 1..cleaned.len() - 1].trim();
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            let name = cleaned[..open].trim().to_string();
            return (name, Some(inner.to_string()));
        }
    }
    (cleaned, None)
}

fn format_cr_date_to_pgn(raw: &str) -> Option<String> {
    for token in raw.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let t = token.trim();
        let parts: Vec<&str> = t.split('.').collect();
        if parts.len() == 3
            && parts[0].len() <= 2
            && parts[1].len() <= 2
            && parts[2].len() == 4
            && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
        {
            return Some(format!(
                "{}.{:0>2}.{:0>2}",
                parts[2], parts[1], parts[0]
            ));
        }
    }
    None
}

fn escape_pgn_tag(val: &str) -> String {
    val.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Parse a Chess-Results single-game viewer page (`PartieSuche.aspx?art=36&id=...`)
/// where moves live in `<a class="game0" href="javascript:c(N)">SAN</a>`.
pub fn parse_chess_results_single_game_html(html: &str, url: &str) -> Result<Game> {
    let lower = html.to_ascii_lowercase();
    let mut moves_san: Vec<String> = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find("<a") {
        let start = pos + rel;
        let Some(tag_close_rel) = html[start..].find('>') else {
            break;
        };
        let tag_end = start + tag_close_rel;
        let tag_inner = &html[start + 2..tag_end];
        let is_game0 = attr_value(tag_inner, "class")
            .is_some_and(|c| c.split_whitespace().any(|tok| tok.eq_ignore_ascii_case("game0")));
        if !is_game0 {
            pos = tag_end + 1;
            continue;
        }
        let body_start = tag_end + 1;
        let tail = &lower[body_start..];
        let close_rel = tail.find("</a>");
        let next_open_rel = find_next_open_anchor(tail);
        let (body_end, next_pos) = match (close_rel, next_open_rel) {
            (Some(c), Some(o)) if o < c => (body_start + o, body_start + o),
            (Some(c), _) => (body_start + c, body_start + c + 4),
            (None, Some(o)) => (body_start + o, body_start + o),
            (None, None) => break,
        };
        let raw = decode_entities(&strip_tags(&html[body_start..body_end]));
        if let Some(token) = clean_cr_move_token(&raw) {
            moves_san.push(token);
        }
        pos = next_pos;
    }
    if moves_san.is_empty() {
        bail!("No Chess-Results game0 move elements found at {url}");
    }

    let mut white = "?".to_string();
    let mut black = "?".to_string();
    let mut white_elo: Option<String> = None;
    let mut black_elo: Option<String> = None;
    let mut event = "Chess-Results".to_string();
    let mut date_pgn: Option<String> = None;
    let mut result_str = "*".to_string();

    let mut p_pos = 0usize;
    while let Some(rel) = lower[p_pos..].find("<p") {
        let start = p_pos + rel;
        let Some(tag_close_rel) = html[start..].find('>') else {
            break;
        };
        let body_start = start + tag_close_rel + 1;
        let Some(close_rel) = lower[body_start..].find("</p>") else {
            break;
        };
        let body = &html[body_start..body_start + close_rel];
        let body_lower = body.to_ascii_lowercase();
        let stripped = decode_entities(&strip_tags(body)).trim().to_string();
        if (body_lower.contains("<b>") || white == "?")
            && let Some((w_part, rest)) = body.split_once(" - ")
            && (w_part.to_ascii_lowercase().contains("<b>") || stripped.contains(" - "))
        {
            let br_split = rest
                .split("<br>")
                .chain(rest.split("<br/>"))
                .chain(rest.split("<br />"))
                .next()
                .unwrap_or(rest);
            let (wn, we) = parse_cr_player_and_elo(w_part);
            let (bn, be) = parse_cr_player_and_elo(br_split);
            if !wn.is_empty() && !bn.is_empty() {
                white = wn;
                black = bn;
                white_elo = we;
                black_elo = be;
            }
            if let Some(br_idx) = body_lower.find("<br")
                && let Some(gt_rel) = body[br_idx..].find('>')
            {
                let meta_raw = decode_entities(&strip_tags(&body[br_idx + gt_rel + 1..]));
                let meta_trimmed = meta_raw.trim();
                if !meta_trimmed.is_empty() {
                    if let Some(d) = format_cr_date_to_pgn(meta_trimmed) {
                        date_pgn = Some(d);
                    }
                    let ev_part = meta_trimmed
                        .rsplit_once(',')
                        .map(|(e, _)| e.trim())
                        .unwrap_or(meta_trimmed);
                    if !ev_part.is_empty() {
                        event = ev_part.to_string();
                    }
                }
            }
        } else {
            match stripped.as_str() {
                "1-0" | "0-1" | "1/2-1/2" | "*" => {
                    result_str = stripped;
                }
                "½-½" => {
                    result_str = "1/2-1/2".to_string();
                }
                _ => {}
            }
        }
        p_pos = body_start + close_rel + 4;
    }

    let build_pgn = |tokens: &[String]| -> String {
        let mut s = format!(
            "[Event \"{}\"]\n[Site \"{}\"]\n[White \"{}\"]\n[Black \"{}\"]\n[Result \"{}\"]\n",
            escape_pgn_tag(&event),
            escape_pgn_tag(url),
            escape_pgn_tag(&white),
            escape_pgn_tag(&black),
            escape_pgn_tag(&result_str),
        );
        if let Some(d) = &date_pgn {
            s.push_str(&format!("[Date \"{d}\"]\n"));
        }
        if let Some(we) = &white_elo {
            s.push_str(&format!("[WhiteElo \"{we}\"]\n"));
        }
        if let Some(be) = &black_elo {
            s.push_str(&format!("[BlackElo \"{be}\"]\n"));
        }
        s.push('\n');
        for (idx, mv) in tokens.iter().enumerate() {
            if idx % 2 == 0 {
                s.push_str(&format!("{}. {mv} ", idx / 2 + 1));
            } else {
                s.push_str(&format!("{mv} "));
            }
        }
        s.push_str(&result_str);
        s.push('\n');
        s
    };

    let pgn_raw = build_pgn(&moves_san);
    if let Ok(mut games) = parse_pgn(pgn_raw.as_bytes())
        && let Some(game) = games.pop()
        && game.errors.is_empty()
        && game.nodes.len() > 1
        && game.nodes.len() > moves_san.len()
    {
        return Ok(game);
    }

    let translated: Vec<String> = moves_san.iter().map(|m| german_san_to_english(m)).collect();
    let pgn_en = build_pgn(&translated);
    let mut games = parse_pgn(pgn_en.as_bytes())
        .with_context(|| format!("parsing Chess-Results game HTML from {url}"))?;
    games
        .pop()
        .ok_or_else(|| anyhow!("No valid game parsed from Chess-Results page {url}"))
}

fn download_chess_results_pgn_post(page_url: &str) -> Result<Vec<u8>> {
    let url = normalize_chess_results_url(page_url);
    let agent = http_agent();
    let get_resp = agent
        .get(&url)
        .call()
        .with_context(|| format!("fetching Chess-Results form page {url}"))?;
    let mut cookies: Vec<String> = Vec::new();
    for sc in get_resp.all("set-cookie") {
        if let Some(pair) = sc.split(';').next().map(str::trim)
            && !pair.is_empty()
        {
            cookies.push(pair.to_string());
        }
    }
    let mut reader = get_resp.into_reader();
    let mut html_bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut html_bytes)
        .with_context(|| format!("reading Chess-Results form HTML from {url}"))?;
    let html = decode_web_text(&html_bytes);

    let (mut form_fields, pgn_btn_val) = collect_chess_results_form_inputs(&html);
    if !form_fields
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("__VIEWSTATE"))
    {
        if let Some(vs) = extract_hidden_input(&html, "__VIEWSTATE") {
            form_fields.push(("__VIEWSTATE".to_string(), vs));
        }
    }
    if form_fields.is_empty() {
        bail!("No ASP.NET __VIEWSTATE found on Chess-Results page {url}");
    }
    form_fields.push((
        "ctl00$P1$cb_DownLoadPGN".to_string(),
        pgn_btn_val.unwrap_or_else(|| "Download as PGN-File".to_string()),
    ));

    let post_url = extract_form_action(&html, &url);
    let mut req = agent.post(&post_url).set("Referer", &url);
    if !cookies.is_empty() {
        req = req.set("Cookie", &cookies.join("; "));
    }
    let pairs: Vec<(&str, &str)> = form_fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let post_resp = req
        .send_form(&pairs)
        .with_context(|| format!("posting PGN download to {post_url}"))?;
    let mut post_reader = post_resp.into_reader();
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut post_reader, &mut out)
        .with_context(|| format!("reading PGN response from {post_url}"))?;
    if !out.starts_with(b"PK\x03\x04") {
        let preview = decode_web_text(&out);
        let trimmed = preview.trim_start();
        let lower = trimmed.to_ascii_lowercase();
        if (trimmed.starts_with('<') || lower.contains("<!doctype") || lower.contains("<html"))
            && !preview.contains("[Event ")
        {
            bail!(
                "Chess-Results did not return a PGN file for {url} (no games published or download form changed)"
            );
        }
    }
    Ok(out)
}

fn collect_chess_results_page_links(
    html: &str,
    page_url: &str,
    source_label: &str,
) -> Vec<DiscoveredPgnLink> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let html_lower = html.to_ascii_lowercase();

    // 1. Single-game viewer page (`PartieSuche.aspx?art=36&id=...`): return ONLY this
    // single normalized game (ignoring unclosed `<a ...&fsize=42>` links).
    if html_lower.contains("class=\"game0\"") || html_lower.contains("class='game0'") {
        let clean_single_url = normalize_chess_results_url(page_url);
        let title = parse_chess_results_single_game_html(html, &clean_single_url)
            .map(|g| format!("{} - {} ({})", g.white(), g.black(), g.event()))
            .unwrap_or_else(|_| "Chess-Results Game".to_string());
        out.push(DiscoveredPgnLink {
            title,
            pgn_url: clean_single_url,
            source_label: source_label.to_string(),
            initial_round: None,
        });
        return out;
    }

    // 2. Direct `PartieSuche.aspx` download page (`cb_DownLoadPGN`): emit bulk POST link.
    let tour_title = extract_chess_results_title(html);
    let initial_round = extract_chess_results_round(page_url);
    if html_lower.contains("cb_downloadpgn") {
        let post_url = format!("{CHESS_RESULTS_POST_PREFIX}{}", normalize_chess_results_url(page_url));
        let title = tour_title
            .clone()
            .or_else(|| {
                extract_chess_results_tnr(page_url).map(|t| format!("Tournament {t}"))
            })
            .map(|t| format!("{t} — All Games (Chess-Results PGN)"))
            .unwrap_or_else(|| "All Games (Chess-Results PGN)".to_string());
        out.push(DiscoveredPgnLink {
            title,
            pgn_url: post_url,
            source_label: source_label.to_string(),
            initial_round,
        });
        return out;
    }

    // 3. Tournament / pairings page with `tnr=` (e.g. `tnr1410724.aspx?art=0` or `art=3&rd=5`):
    // collapse into one bulk tournament PGN via `PartieSuche.aspx?lan=1&art=4&tnr=...`
    // so Brett builds the full Players/Clubs/Rounds index cleanly without `PGN` spam rows.
    let anchor_tnr = extract_anchors(html).into_iter().find_map(|(_, href, _)| {
        let resolved = resolve_generic_url(page_url, &href)?;
        if resolved.to_ascii_lowercase().contains("partiesuche.aspx") {
            extract_chess_results_tnr(&resolved)
        } else {
            None
        }
    });
    if let Some(tnr) = extract_chess_results_tnr(page_url).or(anchor_tnr)
        && let Some(post_url) = chess_results_bulk_post_url(page_url, &tnr)
    {
        let label_base = tour_title.unwrap_or_else(|| format!("Tournament {tnr}"));
        out.push(DiscoveredPgnLink {
            title: format!("{label_base} — All Games (Chess-Results PGN)"),
            pgn_url: post_url,
            source_label: source_label.to_string(),
            initial_round,
        });
        return out;
    }

    for link in discover_html_pgn_links(html, page_url) {
        if seen.insert(link.pgn_url.clone()) {
            out.push(link);
        }
    }

    for (_, href, text) in extract_anchors(html) {
        let Some(resolved) = resolve_generic_url(page_url, &href) else {
            continue;
        };
        let r_lower = resolved.to_ascii_lowercase();
        if r_lower.contains("partiesuche.aspx")
            && r_lower.contains("art=36")
            && r_lower.contains("id=")
            && !r_lower.contains("fsize=")
        {
            let norm = normalize_chess_results_url(&resolved);
            if seen.insert(norm.clone()) {
                let title = if text.trim().is_empty() {
                    "Chess-Results Game".to_string()
                } else {
                    text.trim().to_string()
                };
                out.push(DiscoveredPgnLink {
                    title,
                    pgn_url: norm,
                    source_label: source_label.to_string(),
                    initial_round: None,
                });
            }
        }
    }

    out
}

/// Fetch a portal URL, Chess-Results page, `.zip` archive, or pasted `.pgn` URL and return all downloadable PGN links found.
pub fn fetch_portal_pgn_links(portal_url: &str) -> Result<Vec<DiscoveredPgnLink>> {
    let trimmed = portal_url.trim();
    if trimmed.is_empty() {
        bail!("Enter a portal or PGN URL first.");
    }
    let normalized = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let normalized = normalize_chess_results_url(&normalized);
    let source_label = url_scheme_and_origin(&normalized)
        .map(|(_, host, _)| host.to_string())
        .unwrap_or_else(|| normalized.clone());

    if let Some(direct) = rewrite_direct_portal_pgn_url(&normalized) {
        let title = direct
            .split(['?', '#'])
            .next()
            .and_then(|p| p.rsplit('/').find(|s| !s.is_empty()))
            .unwrap_or("Direct PGN")
            .to_string();
        return Ok(vec![DiscoveredPgnLink {
            title,
            pgn_url: direct,
            source_label,
            initial_round: None,
        }]);
    }

    let bytes = fetch_bytes(&normalized)?;
    if bytes.starts_with(b"PK\x03\x04") {
        let title = normalized
            .split(['?', '#'])
            .next()
            .and_then(|p| p.rsplit('/').find(|s| !s.is_empty()))
            .unwrap_or("PGN Zip Archive")
            .to_string();
        return Ok(vec![DiscoveredPgnLink {
            title,
            pgn_url: normalized,
            source_label,
            initial_round: None,
        }]);
    }
    let text = decode_web_text(&bytes);
    if looks_like_cloudflare_challenge(&text) {
        bail!(
            "Site blocked automated requests behind a Cloudflare challenge ({normalized}). Use “Open in browser ↗” or paste a direct PGN / Chess-Results link."
        );
    }
    let preview = text.trim_start();
    if preview.starts_with("[Event ")
        || preview.starts_with("[Site ")
        || preview.starts_with("[White ")
        || (preview.starts_with("1.") && !preview.contains("<html"))
    {
        let title = normalized
            .split(['?', '#'])
            .next()
            .and_then(|p| p.rsplit('/').find(|s| !s.is_empty()))
            .unwrap_or("Direct PGN")
            .to_string();
        return Ok(vec![DiscoveredPgnLink {
            title,
            pgn_url: normalized,
            source_label,
            initial_round: None,
        }]);
    }

    if is_chess_results_host(&normalized) {
        let mut cr_links = collect_chess_results_page_links(&text, &normalized, &source_label);
        if cr_links.is_empty() {
            let mut next_url = None;
            for (_, href, _) in extract_anchors(&text) {
                if let Some(resolved) = resolve_generic_url(&normalized, &href)
                    && resolved.to_ascii_lowercase().contains("partiesuche.aspx")
                {
                    next_url = Some(normalize_chess_results_url(&resolved));
                    break;
                }
            }
            if next_url.is_none()
                && let Some(tnr) = extract_chess_results_tnr(&normalized)
                && let Some((scheme, auth, _)) = url_scheme_and_origin(&normalized)
            {
                next_url = Some(format!(
                    "{scheme}://{auth}/PartieSuche.aspx?art=4&tnr={tnr}&lan=1"
                ));
            }
            if let Some(sub_url) = next_url
                && sub_url != normalized
                && let Ok(sub_html) = fetch_html(&sub_url)
            {
                cr_links = collect_chess_results_page_links(&sub_html, &sub_url, &source_label);
            }
        }
        return Ok(cr_links);
    }

    let mut links = discover_html_pgn_links(&text, &normalized);
    let mut seen_pgns: BTreeSet<String> = links.iter().map(|l| l.pgn_url.clone()).collect();
    let mut seen_tnr = BTreeSet::new();
    let collect_external_links = |page_html: &str,
                                  page_url: &str,
                                  links: &mut Vec<DiscoveredPgnLink>,
                                  seen_pgns: &mut BTreeSet<String>,
                                  seen_tnr: &mut BTreeSet<String>| {
        for (_, href, anchor_text) in extract_anchors(page_html) {
            let Some(resolved) = resolve_generic_url(page_url, &href) else {
                continue;
            };
            if is_chess_results_host(&resolved)
                && let Some(tnr) = extract_chess_results_tnr(&resolved)
                && seen_tnr.insert(tnr.clone())
                && let Some(bulk_url) = chess_results_bulk_post_url(&resolved, &tnr)
                && seen_pgns.insert(bulk_url.clone())
            {
                let clean_text = anchor_text.trim();
                let title = if clean_text.is_empty() {
                    format!("Chess-Results Tournament {tnr}")
                } else {
                    format!("{clean_text} [Chess-Results]")
                };
                links.push(DiscoveredPgnLink {
                    title,
                    pgn_url: bulk_url,
                    source_label: url_scheme_and_origin(&resolved)
                        .map(|(_, h, _)| h.to_string())
                        .unwrap_or_else(|| "chess-results.com".to_string()),
                    initial_round: extract_chess_results_round(&resolved),
                });
            } else if let Some(lichess_pgn) = rewrite_direct_portal_pgn_url(&resolved)
                && seen_pgns.insert(lichess_pgn.clone())
            {
                let clean_text = anchor_text.trim();
                let title = if clean_text.is_empty() {
                    lichess_pgn
                        .rsplit('/')
                        .next()
                        .unwrap_or("Lichess Broadcast PGN")
                        .to_string()
                } else {
                    clean_text.to_string()
                };
                links.push(DiscoveredPgnLink {
                    title,
                    pgn_url: lichess_pgn,
                    source_label: url_scheme_and_origin(&resolved)
                        .map(|(_, h, _)| h.to_string())
                        .unwrap_or_else(|| "lichess.org".to_string()),
                    initial_round: None,
                });
            }
        }
    };

    collect_external_links(&text, &normalized, &mut links, &mut seen_pgns, &mut seen_tnr);
    if links.is_empty() {
        let base_host = url_scheme_and_origin(&normalized).map(|(_, h, _)| h.to_ascii_lowercase());
        let mut seen_urls = BTreeSet::new();
        let mut sub_pages = Vec::new();
        for (_, href, anchor_text) in extract_anchors(&text) {
            let Some(sub_url) = resolve_generic_url(&normalized, &href) else {
                continue;
            };
            if sub_url == normalized || !seen_urls.insert(sub_url.clone()) {
                continue;
            }
            let same_host = url_scheme_and_origin(&sub_url)
                .map(|(_, h, _)| h.to_ascii_lowercase())
                == base_host;
            if !same_host {
                continue;
            }
            let hay = format!("{} {}", sub_url, anchor_text).to_ascii_lowercase();
            let path_only = hay.split(['?', '#']).next().unwrap_or("");
            if path_only.ends_with(".pdf")
                || path_only.ends_with(".png")
                || path_only.ends_with(".jpg")
            {
                continue;
            }
            if hay.contains("pgn")
                || hay.contains("partien")
                || hay.contains("download")
                || hay.contains("bundesliga")
                || hay.contains("games")
                || hay.contains("archiv")
                || hay.contains("partie")
                || hay.contains("smm")
                || hay.contains("sgm")
                || hay.contains("nationalliga")
                || hay.contains("meisterschaft")
                || hay.contains("turnier")
            {
                sub_pages.push(sub_url);
                if sub_pages.len() >= 8 {
                    break;
                }
            }
        }
        for sub_url in sub_pages {
            if let Ok(sub_html) = fetch_html(&sub_url) {
                for l in discover_html_pgn_links(&sub_html, &sub_url) {
                    if seen_pgns.insert(l.pgn_url.clone()) {
                        links.push(l);
                    }
                }
                collect_external_links(
                    &sub_html,
                    &sub_url,
                    &mut links,
                    &mut seen_pgns,
                    &mut seen_tnr,
                );
            }
        }
    }

    if links.is_empty()
        && let Some((_, host, _)) = url_scheme_and_origin(&normalized)
        && (host.eq_ignore_ascii_case("swisschess.ch")
            || host.to_ascii_lowercase().ends_with(".swisschess.ch"))
    {
        if let Ok(tours) = fetch_lichess_broadcasts(12, Some("SMM")) {
            for tour in tours {
                let lower_name = tour.name.to_ascii_lowercase();
                if !(lower_name.contains("smm")
                    || lower_name.contains("swiss")
                    || lower_name.contains("nationalliga"))
                {
                    continue;
                }
                if seen_pgns.insert(tour.all_pgn_url.clone()) {
                    links.push(DiscoveredPgnLink {
                        title: format!("{} [Lichess Broadcast]", tour.name),
                        pgn_url: tour.all_pgn_url,
                        source_label: "lichess.org (SMM)".to_string(),
                        initial_round: None,
                    });
                }
            }
        }
        if let Ok(cr_sui) =
            fetch_chess_results_tournaments("SUI", ChessResultsTournamentType::All, true, "", "0")
        {
            for item in cr_sui.into_iter().take(25) {
                if seen_pgns.insert(item.bulk_pgn_url.clone()) {
                    links.push(DiscoveredPgnLink {
                        title: format!("{} [Chess-Results SUI]", item.title),
                        pgn_url: item.bulk_pgn_url,
                        source_label: "chess-results.com (SUI)".to_string(),
                        initial_round: None,
                    });
                }
            }
        }
    }
    Ok(links)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigaManagerLeague {
    pub district: String,
    pub name: String,
    /// Path prefix without trailing `/spielplan`, e.g. `"/bsb/2026-2027/oberliga-2571"`.
    pub path_prefix: String,
    pub teams_searchable: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LigaManagerDirectory {
    pub current_season: String,
    pub seasons: Vec<String>,
    pub leagues: Vec<LigaManagerLeague>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigaManagerRoundLink {
    pub label: String,
    pub games_hint: Option<String>,
    pub pgn_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LigaManagerDownloads {
    pub title: String,
    pub season: String,
    pub all_pgn_url: Option<String>,
    pub rounds: Vec<LigaManagerRoundLink>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DsbLeagueEntry {
    pub code: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DsbSeasonEntry {
    pub label: String,
    /// Prefix relative to DSB root, e.g. `""` for current season or `"saison2025-26/"`.
    pub prefix: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DsbDirectory {
    pub seasons: Vec<DsbSeasonEntry>,
    pub leagues: Vec<DsbLeagueEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DsbLeaguePgn {
    pub league_name: String,
    pub pgn_url: String,
    pub last_modified: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedLeagueUrl {
    DirectPgn {
        portal: Option<LeaguePortal>,
        label: String,
        pgn_url: String,
    },
    LigaManagerLeague {
        path_prefix: String,
    },
    DsbLeague {
        season_prefix: String,
        league_code: String,
    },
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
}

fn fetch_html(url: &str) -> Result<String> {
    let bytes = fetch_bytes(url)?;
    Ok(decode_web_text(&bytes))
}

fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    match http_agent().get(url).call() {
        Ok(resp) => {
            let mut reader = resp.into_reader();
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut reader, &mut out)
                .with_context(|| format!("reading response from {url}"))?;
            Ok(out)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let cf_header = resp.header("cf-mitigated").is_some();
            let mut body_bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut resp.into_reader(), &mut body_bytes);
            let body_text = decode_web_text(&body_bytes);
            if cf_header
                || ((code == 403 || code == 503) && looks_like_cloudflare_challenge(&body_text))
            {
                bail!(
                    "Site blocked automated downloads behind a Cloudflare challenge (HTTP {code}) at {url}. Use “Open in browser ↗” or paste a Chess-Results / direct .pgn link."
                );
            }
            bail!("fetching {url}: HTTP status {code}");
        }
        Err(err) => Err(anyhow::Error::new(err).context(format!("fetching {url}"))),
    }
}

const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

/// Decode raw web/PGN bytes stripping any UTF-8 BOM and converting Windows-1252 / Latin-1 to UTF-8 if needed.
#[must_use]
pub fn decode_pgn_bytes_utf8(raw: &[u8]) -> Vec<u8> {
    let trimmed = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
    if std::str::from_utf8(trimmed).is_ok() {
        return trimmed.to_vec();
    }
    let mut out = String::with_capacity(trimmed.len() + trimmed.len() / 8);
    for &b in trimmed {
        match b {
            0x80..=0x9f => out.push(CP1252_HIGH[(b - 0x80) as usize]),
            _ => out.push(b as char),
        }
    }
    out.into_bytes()
}

#[must_use]
fn decode_web_text(raw: &[u8]) -> String {
    let utf8 = decode_pgn_bytes_utf8(raw);
    String::from_utf8_lossy(&utf8).into_owned()
}

fn decode_entities(text: &str) -> String {
    let basic = text
        .replace("&amp;", "&")
        .replace("&#038;", "&")
        .replace("&#38;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&ndash;", "-")
        .replace("&mdash;", "-")
        .replace("&auml;", "ä")
        .replace("&ouml;", "ö")
        .replace("&uuml;", "ü")
        .replace("&Auml;", "Ä")
        .replace("&Ouml;", "Ö")
        .replace("&Uuml;", "Ü")
        .replace("&szlig;", "ß");
    if !basic.contains("&#") {
        return basic;
    }
    let mut out = String::with_capacity(basic.len());
    let mut rest = basic.as_str();
    while let Some(idx) = rest.find("&#") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 2..];
        if let Some(semi) = after.find(';')
            && semi > 0
            && semi <= 8
        {
            let body = &after[..semi];
            let code = if let Some(hex) = body.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                body.parse::<u32>().ok()
            };
            if let Some(ch) = code.and_then(char::from_u32) {
                out.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push_str("&#");
        rest = after;
    }
    out.push_str(rest);
    out
}

fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => {
                in_tag = true;
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn attr_value(tag_inner: &str, attr: &str) -> Option<String> {
    let lower = tag_inner.to_ascii_lowercase();
    let needle = format!("{attr}=");
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find(&needle) {
        let idx = pos + rel;
        let boundary_ok = idx == 0 || lower.as_bytes()[idx - 1].is_ascii_whitespace();
        if !boundary_ok {
            pos = idx + needle.len();
            continue;
        }
        let rest = tag_inner[idx + needle.len()..].trim_start();
        if let Some(after_q) = rest.strip_prefix('"') {
            let end = after_q.find('"')?;
            return Some(decode_entities(&after_q[..end]));
        }
        if let Some(after_q) = rest.strip_prefix('\'') {
            let end = after_q.find('\'')?;
            return Some(decode_entities(&after_q[..end]));
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        return Some(decode_entities(&rest[..end]));
    }
    None
}

fn find_next_open_anchor(lower: &str) -> Option<usize> {
    let mut search = 0usize;
    while let Some(rel) = lower[search..].find("<a") {
        let at = search + rel;
        let next = lower[at + 2..].chars().next();
        if next.is_some_and(|c| c.is_whitespace() || c == '>') {
            return Some(at);
        }
        search = at + 2;
    }
    None
}

fn extract_anchors(html: &str) -> Vec<(usize, String, String)> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find("<a") {
        let start = pos + rel;
        let next_ch = html[start + 2..].chars().next();
        if !next_ch.is_some_and(|c| c.is_whitespace() || c == '>') {
            pos = start + 2;
            continue;
        }
        let Some(tag_close_rel) = html[start..].find('>') else {
            break;
        };
        let tag_end = start + tag_close_rel;
        let tag_inner = &html[start + 2..tag_end];
        let body_start = tag_end + 1;
        let tail = &lower[body_start..];
        let close_rel = tail.find("</a>");
        let next_open_rel = find_next_open_anchor(tail);
        let (body_end, next_pos) = match (close_rel, next_open_rel) {
            (Some(c), Some(o)) if o < c => (body_start + o, body_start + o),
            (Some(c), _) => (body_start + c, body_start + c + 4),
            (None, Some(o)) => (body_start + o, body_start + o),
            (None, None) => break,
        };
        if let Some(href) = attr_value(tag_inner, "href") {
            let text = strip_tags(&html[body_start..body_end]);
            out.push((start, href, text));
        }
        pos = next_pos;
    }
    out
}

fn is_season_slug(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    bytes.len() == 9
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..].iter().all(u8::is_ascii_digit)
}

#[must_use]
pub fn resolve_ligamanager_url(href: &str) -> String {
    let trimmed = href.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else if trimmed.starts_with('/') {
        format!("{LIGAMANAGER_BASE_URL}{trimmed}")
    } else {
        format!("{LIGAMANAGER_BASE_URL}/{trimmed}")
    }
}

/// Parse LigaManager Bayern `/YYYY-YYYY/gesamt` directory HTML into seasons & leagues.
#[must_use]
pub fn parse_ligamanager_directory(html: &str) -> LigaManagerDirectory {
    let lower = html.to_ascii_lowercase();
    let mut seasons_set = BTreeSet::new();
    let mut current_season = String::new();

    if let Some(canon_at) = lower.find("rel=\"canonical\"")
        && let Some(line_start) = lower[..canon_at].rfind('<')
        && let Some(line_end_rel) = html[line_start..].find('>')
        && let Some(href) = attr_value(&html[line_start..line_start + line_end_rel], "href")
    {
        for part in href.split('/') {
            if is_season_slug(part) {
                current_season = part.to_string();
                seasons_set.insert(part.to_string());
                break;
            }
        }
    }

    for (_, href, _) in extract_anchors(html) {
        if let Some(rest) = href.strip_prefix('/')
            && let Some((seg, tail)) = rest.split_once('/')
            && is_season_slug(seg)
            && tail.starts_with("gesamt")
        {
            seasons_set.insert(seg.to_string());
        }
    }

    // Collect `<h2 ...>` card headers for district/association names.
    let mut card_titles: Vec<(usize, String)> = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find("<h2") {
        let start = pos + rel;
        let Some(gt) = html[start..].find('>') else {
            break;
        };
        let body_start = start + gt + 1;
        let Some(end_rel) = lower[body_start..].find("</h2>") else {
            break;
        };
        let title = strip_tags(&html[body_start..body_start + end_rel]);
        if !title.is_empty() {
            card_titles.push((start, title));
        }
        pos = body_start + end_rel + 5;
    }

    let mut leagues = Vec::new();
    let mut seen_prefixes = BTreeSet::new();
    let mut row_pos = 0usize;
    while let Some(rel) = lower[row_pos..].find("gesamt-league-row") {
        let marker = row_pos + rel;
        let tr_start = lower[..marker].rfind("<tr").unwrap_or(marker);
        let Some(tr_tag_end_rel) = html[tr_start..].find('>') else {
            break;
        };
        let tr_tag = &html[tr_start + 3..tr_start + tr_tag_end_rel];
        let tr_close_rel = lower[tr_start..].find("</tr>").unwrap_or(1024);
        let tr_end = (tr_start + tr_close_rel + 5).min(html.len());
        let row_html = &html[tr_start..tr_end];

        let data_team = attr_value(tr_tag, "data-team").unwrap_or_default();
        let data_league = attr_value(tr_tag, "data-league").unwrap_or_default();
        let district = card_titles
            .iter()
            .rfind(|(hpos, _)| *hpos <= tr_start)
            .map(|(_, title)| title.clone())
            .unwrap_or_default();

        for (_, href, text) in extract_anchors(row_html) {
            if let Some(prefix) = href.strip_suffix("/spielplan")
                && seen_prefixes.insert(prefix.to_string())
            {
                if current_season.is_empty() {
                    for part in prefix.split('/') {
                        if is_season_slug(part) {
                            current_season = part.to_string();
                            break;
                        }
                    }
                }
                let name = if text.is_empty() {
                    data_league.clone()
                } else {
                    text
                };
                leagues.push(LigaManagerLeague {
                    district: district.clone(),
                    name,
                    path_prefix: prefix.to_string(),
                    teams_searchable: data_team.clone(),
                });
                break;
            }
        }
        row_pos = tr_end.max(marker + 17);
    }

    let seasons: Vec<String> = seasons_set.into_iter().rev().collect();
    if current_season.is_empty()
        && let Some(first) = seasons.first()
    {
        current_season = first.clone();
    }
    LigaManagerDirectory {
        current_season,
        seasons,
        leagues,
    }
}

/// Filter LigaManager leagues by space-separated search terms matching district,
/// league name, URL path, or club/team names.
#[must_use]
pub fn filter_ligamanager_leagues<'a>(
    leagues: &'a [LigaManagerLeague],
    query: &str,
) -> Vec<&'a LigaManagerLeague> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    leagues
        .iter()
        .filter(|item| {
            if terms.is_empty() {
                return true;
            }
            let hay = format!(
                "{} {} {} {}",
                item.district, item.name, item.path_prefix, item.teams_searchable
            )
            .to_lowercase();
            terms.iter().all(|term| hay.contains(term))
        })
        .collect()
}

/// Parse `/{verband}/{season}/{slug}/partien/download` HTML into `alle.pgn` and `runde-N.pgn` links.
#[must_use]
pub fn parse_ligamanager_downloads(html: &str) -> LigaManagerDownloads {
    let lower = html.to_ascii_lowercase();
    let mut title = String::new();
    if let Some(h1_at) = lower.find("<h1")
        && let Some(gt) = html[h1_at..].find('>')
        && let Some(end_rel) = lower[h1_at + gt + 1..].find("</h1>")
    {
        title = strip_tags(&html[h1_at + gt + 1..h1_at + gt + 1 + end_rel]);
        if let Some(stripped) = title.strip_suffix("- Download") {
            title = stripped.trim().to_string();
        }
    }
    let mut season = String::new();
    for part in html.split('/') {
        if is_season_slug(part) {
            season = part.to_string();
            break;
        }
    }

    let mut all_pgn_url = None;
    let mut rounds = Vec::new();
    let mut seen_rounds = BTreeSet::new();

    for (pos, href, _) in extract_anchors(html) {
        let clean = href.split('?').next().unwrap_or(&href);
        if !clean.to_ascii_lowercase().ends_with(".pgn") {
            continue;
        }
        let url = resolve_ligamanager_url(&href);
        if clean.to_ascii_lowercase().ends_with("/alle.pgn") {
            all_pgn_url = Some(url);
            continue;
        }
        if !seen_rounds.insert(url.clone()) {
            continue;
        }
        // Extract label and game count hint from surrounding `<li>...</li>`.
        let li_start = lower[..pos]
            .rfind("<li")
            .unwrap_or_else(|| pos.saturating_sub(300));
        let li_text = strip_tags(&html[li_start..pos]);
        let mut label = clean
            .rsplit('/')
            .next()
            .unwrap_or("Runde")
            .trim_end_matches(".pgn")
            .replace('-', " ");
        let mut games_hint = None;
        if let Some(at) = li_text.find("Runde ") {
            let tail = &li_text[at..];
            let tokens: Vec<&str> = tail.split_whitespace().collect();
            if tokens.len() >= 2 {
                label = format!("{} {}", tokens[0], tokens[1]);
            }
            if let Some(p_idx) = tokens.iter().position(|t| t.starts_with("Part"))
                && p_idx > 0
            {
                games_hint = Some(format!("{} Partien", tokens[p_idx - 1]));
            }
        }
        rounds.push(LigaManagerRoundLink {
            label,
            games_hint,
            pgn_url: url,
        });
    }

    LigaManagerDownloads {
        title,
        season,
        all_pgn_url,
        rounds,
    }
}

fn extract_select_options(html: &str, select_id: &str) -> Vec<(String, String)> {
    let lower = html.to_ascii_lowercase();
    let marker = format!("id=\"{select_id}\"");
    let Some(id_at) = lower
        .find(&marker)
        .or_else(|| lower.find(&format!("name=\"{select_id}\"")))
    else {
        return Vec::new();
    };
    let select_start = lower[..id_at].rfind("<select").unwrap_or(id_at);
    let Some(end_rel) = lower[select_start..].find("</select>") else {
        return Vec::new();
    };
    let block = &html[select_start..select_start + end_rel];
    let block_lower = block.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = block_lower[pos..].find("<option") {
        let start = pos + rel;
        let Some(gt) = block[start..].find('>') else {
            break;
        };
        let attrs = &block[start + 7..start + gt];
        let body_start = start + gt + 1;
        let close_rel = block_lower[body_start..].find("</option>").unwrap_or(0);
        let label = strip_tags(&block[body_start..body_start + close_rel]);
        if let Some(val) = attr_value(attrs, "value")
            && !val.trim().is_empty()
        {
            out.push((val.trim().to_string(), label));
        }
        pos = body_start + close_rel;
    }
    out
}

/// Resolve a relative DSB `href` (such as `./pgn/bl2627.pgn?v=123` or `../pgn/bl2526.pgn?v=123`)
/// against `{DSB_BASE_URL}/{season_prefix}`.
#[must_use]
pub fn resolve_dsb_relative_url(season_prefix: &str, href: &str) -> String {
    let trimmed = href.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }
    if trimmed.starts_with("//") {
        return format!("https:{trimmed}");
    }
    if trimmed.starts_with('/') {
        return format!("{DSB_BASE_URL}{trimmed}");
    }
    let mut base_segments: Vec<&str> = season_prefix
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let mut path_part = trimmed;
    let mut query_part = "";
    if let Some((p, q)) = trimmed.split_once('?') {
        path_part = p;
        query_part = q;
    }
    for seg in path_part.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                base_segments.pop();
            }
            other => base_segments.push(other),
        }
    }
    let joined = base_segments.join("/");
    if query_part.is_empty() {
        format!("{DSB_BASE_URL}/{joined}")
    } else {
        format!("{DSB_BASE_URL}/{joined}?{query_part}")
    }
}

/// Parse DSB Ergebnisdienst `bedh.php` directory HTML into seasons and leagues.
#[must_use]
pub fn parse_dsb_directory(html: &str, current_season_prefix: &str) -> DsbDirectory {
    let mut seasons = Vec::new();
    for (val, label) in extract_select_options(html, "auswahl_saison") {
        let resolved = resolve_dsb_relative_url(current_season_prefix, &val);
        let rel = resolved
            .strip_prefix(DSB_BASE_URL)
            .unwrap_or(&resolved)
            .trim_start_matches('/');
        let prefix = if let Some(idx) = rel.find("bedh.php") {
            rel[..idx].to_string()
        } else {
            String::new()
        };
        if !seasons.iter().any(|s: &DsbSeasonEntry| s.prefix == prefix) {
            seasons.push(DsbSeasonEntry { label, prefix });
        }
    }

    let mut leagues = Vec::new();
    for (val, name) in extract_select_options(html, "auswahl_liga") {
        if let Some((_, query)) = val.split_once('?') {
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=')
                    && k.eq_ignore_ascii_case("liga")
                    && !v.trim().is_empty()
                    && !leagues.iter().any(|l: &DsbLeagueEntry| l.code == v.trim())
                {
                    leagues.push(DsbLeagueEntry {
                        code: v.trim().to_string(),
                        name: name.clone(),
                    });
                }
            }
        }
    }

    DsbDirectory { seasons, leagues }
}

/// Filter DSB leagues by space-separated search terms matching league code or name.
#[must_use]
pub fn filter_dsb_leagues<'a>(
    leagues: &'a [DsbLeagueEntry],
    query: &str,
) -> Vec<&'a DsbLeagueEntry> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    leagues
        .iter()
        .filter(|item| {
            if terms.is_empty() {
                return true;
            }
            let hay = format!("{} {}", item.code, item.name).to_lowercase();
            terms.iter().all(|t| hay.contains(t))
        })
        .collect()
}

/// Parse DSB Ergebnisdienst `bedh.php?liga=...` HTML for the PGN download URL.
#[must_use]
pub fn parse_dsb_league_pgn(html: &str, season_prefix: &str) -> Option<DsbLeaguePgn> {
    let lower = html.to_ascii_lowercase();
    let mut league_name = String::new();
    if let Some(h2_at) = lower.find("<h2")
        && let Some(gt) = html[h2_at..].find('>')
        && let Some(end_rel) = lower[h2_at + gt + 1..].find("</h2>")
    {
        league_name = strip_tags(&html[h2_at + gt + 1..h2_at + gt + 1 + end_rel]);
    }
    if league_name.is_empty() {
        for (val, label) in extract_select_options(html, "auswahl_liga") {
            let idx = lower.find(&val.to_ascii_lowercase());
            let _ = idx;
            if !label.is_empty() && league_name.is_empty() {
                league_name = label;
                break;
            }
        }
    }

    for (pos, href, _) in extract_anchors(html) {
        let path_only = href.split('?').next().unwrap_or(&href);
        if path_only.to_ascii_lowercase().ends_with(".pgn") {
            let pgn_url = resolve_dsb_relative_url(season_prefix, &href);
            let tail_end = (pos + 220).min(html.len());
            let tail_text = strip_tags(&html[pos..tail_end]);
            let last_modified = tail_text
                .find("letzte Änderung")
                .map(|i| tail_text[i..].trim().to_string());
            return Some(DsbLeaguePgn {
                league_name,
                pgn_url,
                last_modified,
            });
        }
    }
    None
}

/// Resolve any user-pasted LigaManager Bayern or DSB Ergebnisdienst URL.
pub fn resolve_league_url(input: &str) -> Result<ResolvedLeagueUrl> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("Enter a LigaManager Bayern or DSB Ergebnisdienst URL.");
    }
    let normalized = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let after_scheme = normalized
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&normalized);
    let (host_raw, path_and_query) = match after_scheme.find('/') {
        Some(idx) => (&after_scheme[..idx], &after_scheme[idx..]),
        None => (after_scheme, "/"),
    };
    let host = host_raw.to_ascii_lowercase();
    let (path_only, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));

    let portal = if host.ends_with("ligamanager.schachbund-bayern.de") {
        Some(LeaguePortal::LigaManagerBayern)
    } else if host.ends_with("ergebnisdienst.schachbund.de") {
        Some(LeaguePortal::DsbErgebnisdienst)
    } else {
        None
    };

    if path_only.to_ascii_lowercase().ends_with(".pgn") {
        let label = path_only
            .rsplit('/')
            .next()
            .unwrap_or("league.pgn")
            .to_string();
        return Ok(ResolvedLeagueUrl::DirectPgn {
            portal,
            label,
            pgn_url: normalized,
        });
    }

    match portal {
        Some(LeaguePortal::LigaManagerBayern) => {
            let segs: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();
            if segs.len() >= 3 && is_season_slug(segs[1]) {
                let prefix = format!("/{}/{}/{}", segs[0], segs[1], segs[2]);
                return Ok(ResolvedLeagueUrl::LigaManagerLeague {
                    path_prefix: prefix,
                });
            }
            bail!(
                "Paste a LigaManager league URL such as https://ligamanager.schachbund-bayern.de/bsb/2026-2027/oberliga-2571/spielplan or a direct .pgn link."
            );
        }
        Some(LeaguePortal::DsbErgebnisdienst) => {
            let mut league_code = String::new();
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=')
                    && k.eq_ignore_ascii_case("liga")
                    && !v.trim().is_empty()
                {
                    league_code = v.trim().to_string();
                    break;
                }
            }
            if league_code.is_empty() {
                bail!(
                    "DSB Ergebnisdienst URL must contain `?liga=CODE` (e.g. bedh.php?liga=bl) or end in .pgn."
                );
            }
            let segs: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();
            let season_prefix = if segs.len() >= 2 && segs[0].starts_with("saison") {
                format!("{}/", segs[0])
            } else {
                String::new()
            };
            Ok(ResolvedLeagueUrl::DsbLeague {
                season_prefix,
                league_code,
            })
        }
        None => {
            bail!(
                "Unsupported league URL host '{host}'. Supported portals: ligamanager.schachbund-bayern.de and ergebnisdienst.schachbund.de (or any direct .pgn URL)."
            );
        }
    }
}

pub fn fetch_ligamanager_directory(season: Option<&str>) -> Result<LigaManagerDirectory> {
    let url = match season.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("{LIGAMANAGER_BASE_URL}/{s}/gesamt"),
        None => format!("{LIGAMANAGER_BASE_URL}/"),
    };
    let html = fetch_html(&url)?;
    let dir = parse_ligamanager_directory(&html);
    if dir.leagues.is_empty() {
        bail!("No leagues found in LigaManager directory at {url}");
    }
    Ok(dir)
}

pub fn fetch_ligamanager_downloads(path_prefix: &str) -> Result<LigaManagerDownloads> {
    let prefix = path_prefix.trim();
    let url = format!(
        "{}/{}/partien/download",
        LIGAMANAGER_BASE_URL,
        prefix.trim_start_matches('/')
    );
    let html = fetch_html(&url)?;
    let dl = parse_ligamanager_downloads(&html);
    if dl.all_pgn_url.is_none() && dl.rounds.is_empty() {
        bail!("No PGN downloads are published yet at {url}");
    }
    Ok(dl)
}

pub fn fetch_dsb_directory(season_prefix: &str) -> Result<DsbDirectory> {
    let prefix = season_prefix.trim_start_matches('/');
    let url = format!("{DSB_BASE_URL}/{prefix}bedh.php?liga=bl");
    let html = fetch_html(&url)?;
    let dir = parse_dsb_directory(&html, prefix);
    if dir.leagues.is_empty() {
        bail!("No leagues found in DSB Ergebnisdienst at {url}");
    }
    Ok(dir)
}

pub fn fetch_dsb_league_pgn(season_prefix: &str, league_code: &str) -> Result<DsbLeaguePgn> {
    let prefix = season_prefix.trim_start_matches('/');
    let url = format!("{DSB_BASE_URL}/{prefix}bedh.php?liga={league_code}");
    let html = fetch_html(&url)?;
    parse_dsb_league_pgn(&html, prefix)
        .ok_or_else(|| anyhow!("No PGN download link found on DSB Ergebnisdienst page {url}"))
}

/// Parse Chess-Results `TurnierSuche.aspx` result rows (`<tr class="CRg1|CRg2">`).
#[must_use]
pub fn parse_chess_results_search_results(
    html: &str,
    base_url: &str,
) -> Vec<ChessResultsTournament> {
    let lower = html.to_ascii_lowercase();
    let (scheme, auth) = url_scheme_and_origin(base_url.trim())
        .map(|(s, a, _)| (s.to_string(), a.to_string()))
        .unwrap_or_else(|| ("https".to_string(), "s1.chess-results.com".to_string()));
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut pos = 0usize;

    while let Some(rel) = lower[pos..].find("<tr") {
        let tr_start = pos + rel;
        let Some(gt_rel) = html[tr_start..].find('>') else {
            break;
        };
        let tr_tag = &html[tr_start + 3..tr_start + gt_rel];
        let body_start = tr_start + gt_rel + 1;
        let Some(close_rel) = lower[body_start..].find("</tr>") else {
            break;
        };
        let row_html = &html[body_start..body_start + close_rel];
        pos = body_start + close_rel + 5;

        let cls = attr_value(tr_tag, "class").unwrap_or_default();
        if !(cls.eq_ignore_ascii_case("CRg1") || cls.eq_ignore_ascii_case("CRg2")) {
            continue;
        }
        let Some((_, href, link_text)) = extract_anchors(row_html)
            .into_iter()
            .find(|(_, href, _)| extract_chess_results_tnr(href).is_some())
        else {
            continue;
        };
        let Some(tnr) = extract_chess_results_tnr(&href) else {
            continue;
        };
        if !seen.insert(tnr.clone()) {
            continue;
        }

        let mut tds = Vec::new();
        let row_lower = row_html.to_ascii_lowercase();
        let mut td_pos = 0usize;
        while let Some(td_rel) = row_lower[td_pos..].find("<td") {
            let td_start = td_pos + td_rel;
            let Some(td_gt) = row_html[td_start..].find('>') else {
                break;
            };
            let td_body = td_start + td_gt + 1;
            let Some(td_close) = row_lower[td_body..].find("</td>") else {
                break;
            };
            tds.push(strip_tags(&row_html[td_body..td_body + td_close]));
            td_pos = td_body + td_close + 5;
        }

        let title = if link_text.trim().is_empty() {
            tds.get(1).cloned().unwrap_or_else(|| format!("Tournament {tnr}"))
        } else {
            link_text
        };
        let fed = tds.get(2).cloned().unwrap_or_default();
        let last_update = tds.get(4).cloned().unwrap_or_default();
        let start_date = tds.get(5).cloned().unwrap_or_default();
        let end_date = tds.get(6).cloned().unwrap_or_default();
        let bulk_pgn_url = format!(
            "{CHESS_RESULTS_POST_PREFIX}{scheme}://{auth}/PartieSuche.aspx?lan=1&art=4&tnr={tnr}"
        );
        out.push(ChessResultsTournament {
            tnr,
            title,
            fed,
            last_update,
            start_date,
            end_date,
            bulk_pgn_url,
        });
    }
    out
}

/// Query Chess-Results `TurnierSuche.aspx` for all tournaments matching federation,
/// tournament type, optional PGN availability filter, and name substring.
pub fn fetch_chess_results_tournaments(
    fed: &str,
    tour_type: ChessResultsTournamentType,
    only_with_games: bool,
    name_filter: &str,
    max_rows_code: &str,
) -> Result<Vec<ChessResultsTournament>> {
    let url = "https://s1.chess-results.com/TurnierSuche.aspx?lan=1";
    let agent = http_agent();
    let get_resp = agent
        .get(url)
        .call()
        .with_context(|| format!("fetching Chess-Results directory {url}"))?;
    let mut cookies = Vec::new();
    for sc in get_resp.all("set-cookie") {
        if let Some(pair) = sc.split(';').next().map(str::trim)
            && !pair.is_empty()
        {
            cookies.push(pair.to_string());
        }
    }
    let mut reader = get_resp.into_reader();
    let mut html_bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut html_bytes)
        .with_context(|| format!("reading Chess-Results directory {url}"))?;
    let html = decode_web_text(&html_bytes);

    let (mut form_fields, _) = collect_chess_results_form_inputs(&html);
    if !form_fields
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("__VIEWSTATE"))
        && let Some(vs) = extract_hidden_input(&html, "__VIEWSTATE")
    {
        form_fields.push(("__VIEWSTATE".to_string(), vs));
    }
    let set_field = |fields: &mut Vec<(String, String)>, name: &str, val: &str| {
        if let Some(existing) = fields.iter_mut().find(|(k, _)| k == name) {
            existing.1 = val.to_string();
        } else {
            fields.push((name.to_string(), val.to_string()));
        }
    };
    let fed_clean = fed.trim().to_ascii_uppercase();
    let fed_val = if fed_clean.is_empty() || fed_clean == "ALL" {
        "-".to_string()
    } else {
        fed_clean
    };
    set_field(&mut form_fields, "ctl00$P1$combo_land", &fed_val);
    set_field(&mut form_fields, "ctl00$P1$combo_art", tour_type.code());
    set_field(&mut form_fields, "ctl00$P1$combo_sort", "1");
    set_field(&mut form_fields, "ctl00$P1$combo_bedenkzeit", "0");
    set_field(
        &mut form_fields,
        "ctl00$P1$combo_anzahl_zeilen",
        if max_rows_code.is_empty() {
            "1"
        } else {
            max_rows_code
        },
    );
    set_field(&mut form_fields, "ctl00$P1$txt_bez", name_filter.trim());
    if only_with_games {
        set_field(&mut form_fields, "ctl00$P1$cbox_partien_vorhanden", "on");
    }
    set_field(&mut form_fields, "ctl00$P1$cb_suchen", "Search");

    let post_url = extract_form_action(&html, url);
    let mut req = agent.post(&post_url).set("Referer", url);
    if !cookies.is_empty() {
        req = req.set("Cookie", &cookies.join("; "));
    }
    let pairs: Vec<(&str, &str)> = form_fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let post_resp = req
        .send_form(&pairs)
        .with_context(|| format!("searching Chess-Results directory at {post_url}"))?;
    let mut post_reader = post_resp.into_reader();
    let mut resp_bytes = Vec::new();
    std::io::Read::read_to_end(&mut post_reader, &mut resp_bytes)
        .with_context(|| format!("reading Chess-Results search results from {post_url}"))?;
    let resp_html = decode_web_text(&resp_bytes);
    if looks_like_cloudflare_challenge(&resp_html) {
        bail!("Chess-Results blocked automated search behind Cloudflare.");
    }
    Ok(parse_chess_results_search_results(&resp_html, &post_url))
}

/// Filter parsed league PGN games by optional comma/semicolon-separated player or
/// club/team queries (a game matches if ANY clause has all whitespace terms in
/// `White`, `Black`, `WhiteTeam`, `BlackTeam`, `Event`, or `Site`).
#[must_use]
pub fn filter_league_games(games: Vec<Game>, filter_expr: &str) -> Vec<Game> {
    let clauses: Vec<Vec<String>> = filter_expr
        .split([';', '|'])
        .map(|clause| {
            clause
                .split_whitespace()
                .map(|w| w.trim_matches(',').to_lowercase())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|terms| !terms.is_empty())
        .collect();
    if clauses.is_empty() {
        return games;
    }
    games
        .into_iter()
        .filter(|g| {
            let hay = format!(
                "{} {} {} {} {} {}",
                g.white(),
                g.black(),
                g.header("WhiteTeam").unwrap_or(""),
                g.header("BlackTeam").unwrap_or(""),
                g.event(),
                g.header("Site").unwrap_or(""),
            )
            .to_lowercase();
            clauses
                .iter()
                .any(|terms| terms.iter().all(|term| hay.contains(term)))
        })
        .collect()
}

fn unpack_pgn_or_zip(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.starts_with(b"PK\x03\x04") {
        crate::twic::extract_pgn_from_zip(raw)
    } else {
        Ok(raw.to_vec())
    }
}

pub fn parse_league_pgn_bytes(raw: &[u8], filter_expr: &str) -> Result<Vec<Game>> {
    let unpacked = unpack_pgn_or_zip(raw)?;
    let utf8 = decode_pgn_bytes_utf8(&unpacked);
    let games = parse_pgn(&utf8)?;
    Ok(filter_league_games(games, filter_expr))
}

fn download_raw_league_games(pgn_url: &str) -> Result<Vec<Game>> {
    if let Some(page_url) = pgn_url.strip_prefix(CHESS_RESULTS_POST_PREFIX) {
        let raw = download_chess_results_pgn_post(page_url)?;
        let unpacked = unpack_pgn_or_zip(&raw)?;
        let utf8 = decode_pgn_bytes_utf8(&unpacked);
        return parse_pgn(&utf8);
    }
    let normalized = normalize_chess_results_url(pgn_url);
    let raw = fetch_bytes(&normalized)?;
    if !raw.starts_with(b"PK\x03\x04") && is_chess_results_host(&normalized) {
        let text = decode_web_text(&raw);
        let trimmed = text.trim_start();
        if !trimmed.starts_with("[Event ")
            && (text.contains("class=\"game0\"") || text.contains("class='game0'"))
        {
            return Ok(vec![parse_chess_results_single_game_html(&text, &normalized)?]);
        }
    }
    let unpacked = unpack_pgn_or_zip(&raw)?;
    let utf8 = decode_pgn_bytes_utf8(&unpacked);
    parse_pgn(&utf8)
}

pub fn download_and_parse_league_pgn(
    pgn_url: &str,
    filter_expr: &str,
) -> Result<(usize, Vec<Game>)> {
    let all_games = download_raw_league_games(pgn_url)?;
    let total_downloaded = all_games.len();
    let filtered = filter_league_games(all_games, filter_expr);
    Ok((total_downloaded, filtered))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaguePlayerEntry {
    pub name: String,
    pub clubs: Vec<String>,
    pub games: usize,
    pub elo: Option<u16>,
    pub rounds: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeagueClubEntry {
    pub name: String,
    pub players: usize,
    pub games: usize,
    pub rounds: Vec<u16>,
}

#[derive(Clone, Debug, Default)]
pub struct LeagueRosterIndex {
    pub source_url: String,
    pub title: String,
    pub games: Vec<Game>,
    pub players: Vec<LeaguePlayerEntry>,
    pub clubs: Vec<LeagueClubEntry>,
    pub rounds: Vec<u16>,
}

#[must_use]
pub fn parse_game_round_number(game: &Game) -> Option<u16> {
    let raw = game.header("Round")?.trim();
    if raw.is_empty() || raw == "?" {
        return None;
    }
    let digits: String = raw
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse::<u16>().ok().filter(|&n| n > 0)
}

#[must_use]
pub fn extract_game_clubs(game: &Game) -> (Option<String>, Option<String>) {
    let clean = |s: Option<&str>| -> Option<String> {
        let t = s?.trim();
        if t.is_empty() || t == "?" || t == "-" {
            None
        } else {
            Some(t.to_string())
        }
    };
    let wt = clean(game.header("WhiteTeam"));
    let bt = clean(game.header("BlackTeam"));
    if wt.is_some() || bt.is_some() {
        return (wt, bt);
    }
    let ev = game.event().trim();
    if !ev.is_empty() && ev != "?" {
        if let Some((l, r)) = ev.split_once(" - ") {
            let l = l.trim();
            let r = r.trim();
            if !l.is_empty() && !r.is_empty() {
                return (Some(l.to_string()), Some(r.to_string()));
            }
        }
        if let Some((l, r)) = ev.split_once('-') {
            let l = l.trim();
            let r = r.trim();
            // Avoid splitting hyphenated single tokens like "Baden-Baden" unless multiple words exist.
            if !l.is_empty() && !r.is_empty() && (l.contains(' ') || r.contains(' ')) {
                return (Some(l.to_string()), Some(r.to_string()));
            }
        }
    }
    (None, None)
}

#[must_use]
pub fn parse_round_spec(spec: &str) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    for raw in spec.split([',', ';', ' ']) {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let cleaned = token
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .trim();
        if let Some((a, b)) = cleaned.split_once('-').or_else(|| cleaned.split_once("..")) {
            let start = a.trim().parse::<u16>().ok();
            let end = b.trim().parse::<u16>().ok();
            if let (Some(s), Some(e)) = (start, end) {
                let (lo, hi) = if s <= e { (s, e) } else { (e, s) };
                for r in lo..=hi.min(lo.saturating_add(100)) {
                    if r > 0 {
                        out.insert(r);
                    }
                }
            }
        } else if let Ok(r) = cleaned.parse::<u16>()
            && r > 0
        {
            out.insert(r);
        }
    }
    out
}

#[must_use]
pub fn build_league_roster_index(
    source_url: String,
    title: String,
    games: Vec<Game>,
) -> LeagueRosterIndex {
    struct PlayerAcc {
        clubs: BTreeSet<String>,
        games: usize,
        elo: Option<u16>,
        rounds: BTreeSet<u16>,
    }
    struct ClubAcc {
        players: BTreeSet<String>,
        games: usize,
        rounds: BTreeSet<u16>,
    }

    let mut player_map: BTreeMap<String, PlayerAcc> = BTreeMap::new();
    let mut club_map: BTreeMap<String, ClubAcc> = BTreeMap::new();
    let mut all_rounds = BTreeSet::new();

    for game in &games {
        let round = parse_game_round_number(game);
        if let Some(r) = round {
            all_rounds.insert(r);
        }
        let (wc, bc) = extract_game_clubs(game);
        let sides = [
            (
                game.white().trim(),
                wc.as_deref(),
                game.header("WhiteElo").and_then(|e| e.trim().parse::<u16>().ok()),
            ),
            (
                game.black().trim(),
                bc.as_deref(),
                game.header("BlackElo").and_then(|e| e.trim().parse::<u16>().ok()),
            ),
        ];
        for (pname, club_opt, elo_opt) in sides {
            if !pname.is_empty() && pname != "?" {
                let acc = player_map.entry(pname.to_string()).or_insert_with(|| PlayerAcc {
                    clubs: BTreeSet::new(),
                    games: 0,
                    elo: None,
                    rounds: BTreeSet::new(),
                });
                acc.games += 1;
                if let Some(c) = club_opt {
                    acc.clubs.insert(c.to_string());
                }
                if let Some(elo) = elo_opt {
                    acc.elo = Some(acc.elo.map_or(elo, |prev| prev.max(elo)));
                }
                if let Some(r) = round {
                    acc.rounds.insert(r);
                }
            }
            if let Some(club) = club_opt {
                let cacc = club_map.entry(club.to_string()).or_insert_with(|| ClubAcc {
                    players: BTreeSet::new(),
                    games: 0,
                    rounds: BTreeSet::new(),
                });
                if !pname.is_empty() && pname != "?" {
                    cacc.players.insert(pname.to_string());
                }
                if let Some(r) = round {
                    cacc.rounds.insert(r);
                }
            }
        }
        match (wc, bc) {
            (Some(w), Some(b)) if w.eq_ignore_ascii_case(&b) => {
                if let Some(c) = club_map.get_mut(&w) {
                    c.games += 1;
                }
            }
            (w_opt, b_opt) => {
                if let Some(w) = w_opt
                    && let Some(c) = club_map.get_mut(&w)
                {
                    c.games += 1;
                }
                if let Some(b) = b_opt
                    && let Some(c) = club_map.get_mut(&b)
                {
                    c.games += 1;
                }
            }
        }
    }

    let players = player_map
        .into_iter()
        .map(|(name, acc)| LeaguePlayerEntry {
            name,
            clubs: acc.clubs.into_iter().collect(),
            games: acc.games,
            elo: acc.elo,
            rounds: acc.rounds.into_iter().collect(),
        })
        .collect();
    let clubs = club_map
        .into_iter()
        .map(|(name, acc)| LeagueClubEntry {
            name,
            players: acc.players.len(),
            games: acc.games,
            rounds: acc.rounds.into_iter().collect(),
        })
        .collect();

    LeagueRosterIndex {
        source_url,
        title,
        games,
        players,
        clubs,
        rounds: all_rounds.into_iter().collect(),
    }
}

#[must_use]
pub fn filter_roster_game_indices(
    roster: &LeagueRosterIndex,
    selected_players: &BTreeSet<String>,
    selected_clubs: &BTreeSet<String>,
    text_filter: &str,
    round_spec: &str,
) -> Vec<usize> {
    let rounds = parse_round_spec(round_spec);
    let clauses: Vec<Vec<String>> = text_filter
        .split([';', '|'])
        .map(|clause| {
            clause
                .split_whitespace()
                .map(|w| w.trim_matches(',').to_lowercase())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|terms| !terms.is_empty())
        .collect();
    let has_entity_filter =
        !selected_players.is_empty() || !selected_clubs.is_empty() || !clauses.is_empty();

    roster
        .games
        .iter()
        .enumerate()
        .filter_map(|(idx, g)| {
            if !rounds.is_empty()
                && !parse_game_round_number(g).is_some_and(|r| rounds.contains(&r))
            {
                return None;
            }
            if !has_entity_filter {
                return Some(idx);
            }
            let w = g.white().trim();
            let b = g.black().trim();
            if selected_players.contains(w) || selected_players.contains(b) {
                return Some(idx);
            }
            if !selected_clubs.is_empty() {
                let (wc, bc) = extract_game_clubs(g);
                if wc.as_deref().is_some_and(|c| selected_clubs.contains(c))
                    || bc.as_deref().is_some_and(|c| selected_clubs.contains(c))
                {
                    return Some(idx);
                }
            }
            if !clauses.is_empty() {
                let hay = format!(
                    "{} {} {} {} {} {}",
                    w,
                    b,
                    g.header("WhiteTeam").unwrap_or(""),
                    g.header("BlackTeam").unwrap_or(""),
                    g.event(),
                    g.header("Site").unwrap_or(""),
                )
                .to_lowercase();
                if clauses
                    .iter()
                    .any(|terms| terms.iter().all(|term| hay.contains(term)))
                {
                    return Some(idx);
                }
            }
            None
        })
        .collect()
}

#[must_use]
pub fn filter_roster_games(
    roster: &LeagueRosterIndex,
    selected_players: &BTreeSet<String>,
    selected_clubs: &BTreeSet<String>,
    text_filter: &str,
    round_spec: &str,
) -> Vec<Game> {
    filter_roster_game_indices(
        roster,
        selected_players,
        selected_clubs,
        text_filter,
        round_spec,
    )
    .into_iter()
    .filter_map(|idx| roster.games.get(idx).cloned())
    .collect()
}

pub fn fetch_league_roster_index(pgn_url: &str, title: &str) -> Result<LeagueRosterIndex> {
    let games = download_raw_league_games(pgn_url)?;
    Ok(build_league_roster_index(
        pgn_url.to_string(),
        title.to_string(),
        games,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_LM_DIR: &str = r#"<!DOCTYPE html>
<html><head>
<link rel="canonical" href="https://ligamanager.schachbund-bayern.de/2026-2027/gesamt">
</head><body>
<a href="/2026-2027/gesamt">2026/2027</a>
<a href="/2025-2026/gesamt">2025/2026</a>
<div class="liga-card">
  <h2 class="card-title"><a href="/bsb/2026-2027/uebersicht">200 Bayerischer Schachbund e.V.</a></h2>
  <table>
    <tr class="gesamt-league-row" data-league="oberliga" data-team="sc erlangen 1 sk passau 1 fc bayern münchen 2">
      <td><a href="/bsb/2026-2027/oberliga-2571/spielplan">Oberliga</a></td>
    </tr>
  </table>
</div>
<div class="liga-card">
  <h2 class="card-title"><a href="/mfr/2026-2027/uebersicht">Mittelfranken</a></h2>
  <table>
    <tr class="gesamt-league-row" data-league="bezirksliga 1" data-team="sc uffenheim 1 sg fürth 3">
      <td><a href="/mfr/2026-2027/bezirksliga-1-2602/spielplan">Bezirksliga 1</a></td>
    </tr>
  </table>
</div>
</body></html>"#;

    const SAMPLE_LM_DL: &str = r#"<html><body>
<div class="liga-page-heading">
  <h1 class="h2 mb-1">Oberliga - Download</h1>
</div>
<a href="/bsb/2025-2026/oberliga-2486/partien/download/alle.pgn">Alle Partien herunterladen</a>
<ul>
  <li id="runde-1">
    <div><span class="fw-semibold">Runde 1</span><span>40 Partien</span></div>
    <a href="/bsb/2025-2026/oberliga-2486/partien/download/runde-1.pgn">PGN</a>
  </li>
  <li id="runde-2">
    <div><span class="fw-semibold">Runde 2</span><span>38 Partien</span></div>
    <a href="/bsb/2025-2026/oberliga-2486/partien/download/runde-2.pgn">PGN</a>
  </li>
</ul>
</body></html>"#;

    const SAMPLE_DSB_DIR: &str = r#"<html><body>
<h2>Bundesliga</h2>
<select name="auswahl_liga" id="auswahl_liga">
  <option value="bedh.php?liga=bl" selected="selected">Bundesliga</option>
  <option value="bedh.php?liga=olby">Oberliga Bayern</option>
</select>
<select name="auswahl_saison" id="auswahl_saison">
  <option value="./bedh.php?liga=bl" selected="selected">2026/27</option>
  <option value="./saison2025-26/bedh.php?liga=bl">2025/26</option>
</select>
<p><a href="./pgn/bl2627.pgn?v=1795114939">Download der Partien (PGN)</a> - letzte Änderung 19.11.26 20:02</p>
</body></html>"#;

    #[test]
    fn parses_ligamanager_directory_and_filters_by_club() {
        let dir = parse_ligamanager_directory(SAMPLE_LM_DIR);
        assert_eq!(dir.current_season, "2026-2027");
        assert_eq!(dir.seasons, vec!["2026-2027", "2025-2026"]);
        assert_eq!(dir.leagues.len(), 2);
        assert_eq!(dir.leagues[0].path_prefix, "/bsb/2026-2027/oberliga-2571");
        assert_eq!(dir.leagues[1].district, "Mittelfranken");

        let found = filter_ligamanager_leagues(&dir.leagues, "passau");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Oberliga");
    }

    #[test]
    fn parses_ligamanager_downloads_page() {
        let dl = parse_ligamanager_downloads(SAMPLE_LM_DL);
        assert_eq!(dl.title, "Oberliga");
        assert_eq!(dl.season, "2025-2026");
        assert_eq!(
            dl.all_pgn_url.as_deref(),
            Some(
                "https://ligamanager.schachbund-bayern.de/bsb/2025-2026/oberliga-2486/partien/download/alle.pgn"
            )
        );
        assert_eq!(dl.rounds.len(), 2);
        assert_eq!(dl.rounds[0].label, "Runde 1");
        assert_eq!(dl.rounds[0].games_hint.as_deref(), Some("40 Partien"));
    }

    #[test]
    fn parses_dsb_directory_and_relative_pgn_urls() {
        let dir = parse_dsb_directory(SAMPLE_DSB_DIR, "");
        assert_eq!(dir.seasons.len(), 2);
        assert_eq!(dir.seasons[0].prefix, "");
        assert_eq!(dir.seasons[1].prefix, "saison2025-26/");
        assert_eq!(dir.leagues.len(), 2);
        assert_eq!(dir.leagues[1].code, "olby");

        let pgn = parse_dsb_league_pgn(SAMPLE_DSB_DIR, "").unwrap();
        assert_eq!(
            pgn.pgn_url,
            "https://ergebnisdienst.schachbund.de/pgn/bl2627.pgn?v=1795114939"
        );
        assert!(pgn.last_modified.unwrap().contains("19.11.26"));

        let archive_url =
            resolve_dsb_relative_url("saison2025-26/", "../pgn/bl2526.pgn?v=1777241450");
        assert_eq!(
            archive_url,
            "https://ergebnisdienst.schachbund.de/pgn/bl2526.pgn?v=1777241450"
        );
    }

    #[test]
    fn resolves_pasted_league_urls() {
        assert_eq!(
            resolve_league_url(
                "https://ligamanager.schachbund-bayern.de/bsb/2026-2027/oberliga-2571/spielplan"
            )
            .unwrap(),
            ResolvedLeagueUrl::LigaManagerLeague {
                path_prefix: "/bsb/2026-2027/oberliga-2571".to_string()
            }
        );
        assert_eq!(
            resolve_league_url(
                "https://ergebnisdienst.schachbund.de/saison2025-26/bedh.php?liga=olby"
            )
            .unwrap(),
            ResolvedLeagueUrl::DsbLeague {
                season_prefix: "saison2025-26/".to_string(),
                league_code: "olby".to_string()
            }
        );
        assert!(matches!(
            resolve_league_url("https://ergebnisdienst.schachbund.de/pgn/bl2627.pgn").unwrap(),
            ResolvedLeagueUrl::DirectPgn {
                portal: Some(LeaguePortal::DsbErgebnisdienst),
                ..
            }
        ));
    }

    #[test]
    fn strips_bom_and_filters_by_player_or_team() {
        let mut pgn = vec![0xEF, 0xBB, 0xBF];
        pgn.extend_from_slice(
            r#"[Event "SK Passau 1-SF Augsburg 1"]
[White "Hinterreiter, Markus"]
[Black "Vuckovic, Zarko"]
[WhiteTeam "SK Passau 1"]
[BlackTeam "SF Augsburg 1"]
[Result "0-1"]

1. d4 d5 0-1

[Event "FC Bayern München 2-SC Erlangen 1"]
[White "Mustermann, Max"]
[Black "Beispiel, Erika"]
[WhiteTeam "FC Bayern München 2"]
[BlackTeam "SC Erlangen 1"]
[Result "1-0"]

1. e4 e5 1-0
"#
            .as_bytes(),
        );
        let filtered = parse_league_pgn_bytes(&pgn, "Passau").unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].white(), "Hinterreiter, Markus");

        let by_multi_name = parse_league_pgn_bytes(&pgn, "Mustermann; Vuckovic").unwrap();
        assert_eq!(by_multi_name.len(), 2);
    }
}

#[cfg(test)]
mod provider_portal_tests {
    use super::*;

    #[test]
    fn catalog_filters_regional_and_international_providers() {
        let all = filter_providers("");
        assert!(all.iter().any(|p| p.id == "ligamanager-bayern"));
        assert!(all.iter().any(|p| p.id == "dsb-ergebnisdienst"));
        assert!(all.iter().any(|p| p.id == "lichess-broadcasts"));
        assert!(all.iter().any(|p| p.id == "custom-url"));

        let bsb = filter_providers("bayern bsb");
        assert_eq!(bsb.len(), 1);
        assert_eq!(bsb[0].id, "ligamanager-bayern");

        let bundesliga = filter_providers("bundesliga");
        assert!(bundesliga.iter().any(|p| p.id == "dsb-ergebnisdienst"));
        assert!(bundesliga.iter().any(|p| p.id == "schachbundesliga"));

        let austria = filter_providers("austria");
        assert!(austria.iter().any(|p| p.id == "chess-results-aut"));
        assert!(!all.iter().any(|p| p.url.contains("chess.at")));
    }

    #[test]
    fn decodes_numeric_html_entities_and_parses_cr_directory_rows() {
        assert_eq!(
            decode_entities("P&#345;ebor &amp; Dr&#382;avna &#8211; Test &#x2013; ok"),
            "Přebor & Državna – Test – ok"
        );
        let sample = r#"
        <table>
          <tr class="CRg1">
            <td>1</td>
            <td><a href="tnr1404438.aspx?lan=1">1. BUNDESLIGA AUT 2026/2027</a></td>
            <td>AUT</td><td></td><td>41 Minutes</td><td>2026/11/25</td><td>2027/03/14</td>
          </tr>
          <tr class="CRg2">
            <td>2</td>
            <td><a href="https://s1.chess-results.com/tnr1410724.aspx?lan=1">2. BUNDESLIGA WEST AUT 2026/2027</a></td>
            <td>AUT</td><td></td><td>4 Days</td><td>2026/10/16</td><td>2027/04/11</td>
          </tr>
        </table>
        "#;
        let parsed = parse_chess_results_search_results(
            sample,
            "https://s1.chess-results.com/TurnierSuche.aspx?lan=1",
        );
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].tnr, "1404438");
        assert_eq!(parsed[0].fed, "AUT");
        assert_eq!(
            parsed[0].bulk_pgn_url,
            "brett-cr-post:https://s1.chess-results.com/PartieSuche.aspx?lan=1&art=4&tnr=1404438"
        );
        assert_eq!(parsed[1].tnr, "1410724");

        assert_eq!(
            parse_cr_default_query(Some("AUT:team")),
            ("AUT".to_string(), ChessResultsTournamentType::TeamRoundRobin)
        );
        assert_eq!(
            parse_cr_default_query(Some("-:all")),
            ("-".to_string(), ChessResultsTournamentType::All)
        );
    }

    #[test]
    fn parses_lichess_broadcasts_ndjson_and_search_results() {
        let ndjson = concat!(
            r#"{"tour":{"id":"tour1111","name":"Tata Steel 2026","info":{"location":"Wijk aan Zee","format":"14-player RR","tc":"Classical"}},"rounds":[{"id":"rnd00001","name":"Round 1","ongoing":false,"finished":true},{"id":"rnd00002","name":"Round 2","ongoing":true,"finished":false}]}"#,
            "\n\n",
            r#"{"tour":{"id":"tour2222","name":"FIDE Candidates"},"rounds":[]}"#
        );
        let tours = parse_lichess_broadcasts_ndjson(ndjson);
        assert_eq!(tours.len(), 2);
        assert_eq!(tours[0].id, "tour1111");
        assert_eq!(tours[0].name, "Tata Steel 2026");
        assert_eq!(tours[0].location.as_deref(), Some("Wijk aan Zee"));
        assert_eq!(tours[0].info.as_deref(), Some("14-player RR · Classical"));
        assert_eq!(
            tours[0].all_pgn_url,
            "https://lichess.org/api/broadcast/tour1111.pgn"
        );
        assert_eq!(tours[0].rounds.len(), 2);
        assert_eq!(
            tours[0].rounds[0].pgn_url,
            "https://lichess.org/api/broadcast/round/rnd00001.pgn"
        );
        assert!(tours[0].rounds[0].finished);
        assert!(!tours[0].rounds[0].ongoing);
        assert!(tours[0].rounds[1].ongoing);
        assert_eq!(tours[1].name, "FIDE Candidates");

        let search_json = r#"{"currentPageResults":[{"tour":{"id":"srch1","name":"Bundesliga"},"rounds":[{"id":"srchr1","name":"Runde 1"}]}]}"#;
        let search_tours = parse_lichess_broadcasts_ndjson(search_json);
        assert_eq!(search_tours.len(), 1);
        assert_eq!(search_tours[0].rounds[0].name, "Runde 1");
    }

    #[test]
    fn resolves_urls_and_discovers_html_pgn_links() {
        assert_eq!(
            resolve_generic_url(
                "https://www.schachbund-nrw.de/liga/index.html",
                "pgn/r1.pgn"
            )
            .as_deref(),
            Some("https://www.schachbund-nrw.de/liga/pgn/r1.pgn")
        );
        assert_eq!(
            resolve_generic_url(
                "https://www.chess.at/turniere/open/",
                "../files/open.pgn?v=2"
            )
            .as_deref(),
            Some("https://www.chess.at/turniere/files/open.pgn?v=2")
        );
        assert_eq!(resolve_generic_url("https://example.org/", "#top"), None);
        assert_eq!(
            resolve_generic_url("https://example.org/", "javascript:void(0)"),
            None
        );

        let html = r#"
            <html><body>
                <a href="news.html">News</a>
                <a href="/downloads/oberliga_r1.pgn">Oberliga Round 1</a>
                <a href="/downloads/oberliga_r1.pgn">Duplicate link</a>
                <a href="files/all_games.pgn?dl=1">PGN</a>
                <a href="/export.php?format=pgn&id=7">Export Tournament</a>
                <a href="bulletin.pdf">Bulletin PDF</a>
            </body></html>
        "#;
        let links = discover_html_pgn_links(html, "https://www.example-chess.de/2026/table.html");
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].title, "Oberliga Round 1");
        assert_eq!(
            links[0].pgn_url,
            "https://www.example-chess.de/downloads/oberliga_r1.pgn"
        );
        assert_eq!(links[0].source_label, "www.example-chess.de");
        assert_eq!(links[1].title, "all_games.pgn");
        assert_eq!(
            links[1].pgn_url,
            "https://www.example-chess.de/2026/files/all_games.pgn?dl=1"
        );
        assert_eq!(links[2].title, "Export Tournament");
        assert_eq!(
            links[2].pgn_url,
            "https://www.example-chess.de/export.php?format=pgn&id=7"
        );
    }

    #[test]
    fn parses_chess_results_single_game_html_english_and_german() {
        let html_en = r#"
            <html><body>
            <p><b>Carlsen, Magnus</b> (2830) - <b>Caruana, Fabiano</b> (2805)<br>Candidates 2026, 15.04.2026</p>
            <a class="game0" href="javascript:c(0)">e4</a>
            <a class="game0" href="javascript:c(1)">e5</a>
            <a class="game0" href="javascript:c(2)">Nf3</a>
            <a class="game0" href="javascript:c(3)">Nc6</a>
            <p>1/2-1/2</p>
            </body></html>
        "#;
        let g_en = parse_chess_results_single_game_html(
            html_en,
            "https://s1.chess-results.com/PartieSuche.aspx?art=36&id=6716323",
        )
        .unwrap();
        assert_eq!(g_en.white(), "Carlsen, Magnus");
        assert_eq!(g_en.black(), "Caruana, Fabiano");
        assert_eq!(g_en.header("WhiteElo"), Some("2830"));
        assert_eq!(g_en.header("Date"), Some("2026.04.15"));
        assert_eq!(g_en.nodes.len(), 5);

        let html_de = r#"
            <html><body>
            <p><b>Mueller, Hans</b> (2150) - <b>Schmidt, Peter</b> (2090)<br>Oberliga Bayern, 01.03.2026</p>
            <a class="game0" href="javascript:c(0)">d4</a>
            <a class="game0" href="javascript:c(1)">d5</a>
            <a class="game0" href="javascript:c(2)">Lf4</a>
            <a class="game0" href="javascript:c(3)">Sf6</a>
            <p>1-0</p>
            </body></html>
        "#;
        let g_de = parse_chess_results_single_game_html(
            html_de,
            "https://s1.chess-results.com/PartieSuche.aspx?art=36&id=1",
        )
        .unwrap();
        assert_eq!(g_de.nodes.len(), 5);
        assert_eq!(g_de.result(), "1-0");
    }

    #[test]
    fn collects_chess_results_links_and_hidden_inputs() {
        let html = r#"
            <title>Chess-Results Server Chess-results.com - 2. BUNDESLIGA WEST AUT 2026/2027</title>
            <h2>2. BUNDESLIGA WEST AUT 2026/2027</h2>
            <input type="hidden" name="__VIEWSTATE" value="abc123&#038;xyz" />
            <input name="ctl00$P1$txt_dbkey" type="text" value="1410724" />
            <input type="submit" name="ctl00$P1$cb_DownLoadPGN" value="Download as PGN-File" />
            <a href="PartieSuche.aspx?art=36&id=6716323">Board 1</a>
        "#;
        assert_eq!(
            extract_hidden_input(html, "__VIEWSTATE").as_deref(),
            Some("abc123&xyz")
        );
        let (fields, btn_val) = collect_chess_results_form_inputs(html);
        assert!(fields
            .iter()
            .any(|(k, v)| k == "ctl00$P1$txt_dbkey" && v == "1410724"));
        assert_eq!(btn_val.as_deref(), Some("Download as PGN-File"));

        let links = collect_chess_results_page_links(
            html,
            "https://s1.chess-results.com/PartieSuche.aspx?art=4&tnr=1410724&lan=1",
            "s1.chess-results.com",
        );
        assert_eq!(links.len(), 1);
        assert!(links[0].pgn_url.starts_with(CHESS_RESULTS_POST_PREFIX));
        assert!(links[0].title.contains("2. BUNDESLIGA WEST AUT 2026/2027"));
    }

    #[test]
    fn parses_real_chess_results_6716323_fixture_with_moves_and_unclosed_fsize_tags() {
        let html = r#"<!DOCTYPE html><html><head>
        <title>Chess-Results.com - Partien</title></head><body>
        <a  href=partiesuche.aspx?lan=1&amp;art=36&amp;id=6716323&fsize=42>42<a>
        <a  href=partiesuche.aspx?lan=1&amp;art=36&amp;id=6716323&fsize=52>52<a>
        <p><b>Fedorovsky, Michael</b> (2347) - <b>Hinterreiter, Markus</b> (2205)<br>2. BUNDESLIGA WEST AUT 2026/2027, 22.11.2026</p>
        <p>
        <a class="game0" href="javascript:c(1)" id="l1">1.d4</a>
        <a class="game0" href="javascript:c(2)" id="l2">Nf6</a>
        <a class="game0" href="javascript:c(3)" id="l3">2.c4</a>
        <a class="game0" href="javascript:c(4)" id="l4">g6</a>
        <a class="game0" href="javascript:c(5)" id="l5">3.Nc3</a>
        <a class="game0" href="javascript:c(6)" id="l6">d5</a>
        <a class="game0" href="javascript:c(7)" id="l7">4.Nf3</a>
        <a class="game0" href="javascript:c(8)" id="l8">Bg7</a>
        <a class="game0" href="javascript:c(9)" id="l9">5.e3</a>
        <a class="game0" href="javascript:c(10)" id="l10">O-O</a>
        <a class="game0" href="javascript:c(11)" id="l11">6.Be2</a>
        <a class="game0" href="javascript:c(12)" id="l12">c5</a>
        </p>
        <p>1-0</p>
        </body></html>"#;

        let links = collect_chess_results_page_links(
            html,
            "https://s1.chess-results.com/PartieSuche.aspx?art=36&id=6716323&lan=1",
            "s1.chess-results.com",
        );
        assert_eq!(links.len(), 1, "expected 1 deduped game link, got {links:?}");
        assert!(links[0].title.contains("Fedorovsky, Michael"));
        assert!(links[0].title.contains("Hinterreiter, Markus"));
        assert!(!links[0].pgn_url.contains("fsize="));

        let g = parse_chess_results_single_game_html(html, &links[0].pgn_url).unwrap();
        assert_eq!(g.white(), "Fedorovsky, Michael");
        assert_eq!(g.black(), "Hinterreiter, Markus");
        assert_eq!(g.result(), "1-0");
        assert_eq!(g.header("Date"), Some("2026.11.22"));
        assert_eq!(g.mainline_len(), 12);
    }

    #[test]
    fn collects_single_bulk_link_from_tnr_round_and_overview_pages() {
        let html_rd5 = r#"
            <h2>2. BUNDESLIGA WEST AUT 2026/2027</h2>
            <h2>Board Pairings</h2>
            <a href="PartieSuche.aspx?lan=1&amp;id=50023&amp;tnr=1410724&amp;art=3">179 games available</a>
            <a href="PartieSuche.aspx?art=36&amp;id=6716308">PGN</a>
            <a href="PartieSuche.aspx?art=36&amp;id=6716309">PGN</a>
        "#;
        let rd5_links = collect_chess_results_page_links(
            html_rd5,
            "https://s1.chess-results.com/tnr1410724.aspx?lan=1&art=3&rd=5&SNode=S0",
            "s1.chess-results.com",
        );
        assert_eq!(rd5_links.len(), 1);
        assert_eq!(rd5_links[0].initial_round, Some(5));
        assert_eq!(
            rd5_links[0].pgn_url,
            "brett-cr-post:https://s1.chess-results.com/PartieSuche.aspx?lan=1&art=4&tnr=1410724"
        );
        assert!(rd5_links[0].title.contains("2. BUNDESLIGA WEST AUT 2026/2027"));

        let html_art0 = r#"
            <h2>2. BUNDESLIGA WEST AUT 2026/2027</h2>
            <a href="PartieSuche.aspx?lan=1&amp;id=50023&amp;tnr=1410724&amp;art=3">179 games</a>
        "#;
        let art0_links = collect_chess_results_page_links(
            html_art0,
            "https://s3.chess-results.com/tnr1410724.aspx?lan=1&art=0&SNode=S0",
            "s3.chess-results.com",
        );
        assert_eq!(art0_links.len(), 1);
        assert_eq!(art0_links[0].initial_round, None);
        assert_eq!(
            art0_links[0].pgn_url,
            "brett-cr-post:https://s3.chess-results.com/PartieSuche.aspx?lan=1&art=4&tnr=1410724"
        );
    }

    #[test]
    fn roster_index_round_spec_and_filters_work() {
        let pgn = r#"[Event "SK Passau 1 - FC Bayern 2"]
[Round "1.1"]
[White "Doe, Jane"]
[Black "Miller, Max"]
[WhiteElo "2310"]
[BlackElo "2250"]
[WhiteTeam "SK Passau 1"]
[BlackTeam "FC Bayern 2"]
[Result "1-0"]

1. d4 d5 1-0

[Event "SK Passau 1 - SC Erlangen 1"]
[Round "3"]
[White "Doe, Jane"]
[Black "Weber, Paul"]
[WhiteElo "2320"]
[BlackElo "2400"]
[WhiteTeam "SK Passau 1"]
[BlackTeam "SC Erlangen 1"]
[Result "0-1"]

1. e4 c5 0-1

[Event "SC Erlangen 1 - FC Bayern 2"]
[Round "5.2"]
[White "Weber, Paul"]
[Black "Miller, Max"]
[WhiteTeam "SC Erlangen 1"]
[BlackTeam "FC Bayern 2"]
[Result "1/2-1/2"]

1. c4 e5 1/2-1/2
"#;
        let games = parse_pgn(pgn.as_bytes()).unwrap();
        let roster = build_league_roster_index(
            "https://example.org/alle.pgn".to_string(),
            "Oberliga".to_string(),
            games,
        );
        assert_eq!(roster.rounds, vec![1, 3, 5]);
        assert_eq!(roster.players.len(), 3);
        assert_eq!(roster.clubs.len(), 3);
        let doe = roster.players.iter().find(|p| p.name == "Doe, Jane").unwrap();
        assert_eq!(doe.elo, Some(2320));
        assert_eq!(doe.rounds, vec![1, 3]);

        assert_eq!(
            parse_round_spec("1-3, 5, 6").into_iter().collect::<Vec<_>>(),
            vec![1, 2, 3, 5, 6]
        );

        let mut sel_players = BTreeSet::new();
        sel_players.insert("Doe, Jane".to_string());
        let kept = filter_roster_games(&roster, &sel_players, &BTreeSet::new(), "", "1-2, 5");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].black(), "Miller, Max");

        let mut sel_clubs = BTreeSet::new();
        sel_clubs.insert("FC Bayern 2".to_string());
        let club_kept = filter_roster_games(&roster, &BTreeSet::new(), &sel_clubs, "", "5");
        assert_eq!(club_kept.len(), 1);
        assert_eq!(club_kept[0].white(), "Weber, Paul");

        let indices = filter_roster_game_indices(&roster, &BTreeSet::new(), &sel_clubs, "", "5");
        assert_eq!(indices, vec![2]);
    }

    #[test]
    fn international_league_catalog_entries_present() {
        let all = filter_providers("");
        for expected in [
            "4ncl-uk",
            "pgnmentor",
            "chess-results-aut",
            "chess-results-sui",
            "smm-switzerland",
            "top16-france",
            "meesterklasse-nl",
            "ceclub-spain",
            "cis-italy",
            "ekstraliga-poland",
            "extraliga-czech",
            "elitserien-sweden",
            "skakligaen-denmark",
            "eliteserien-norway",
            "chess-results-cro",
            "chess-results-slo",
        ] {
            let p = all
                .iter()
                .find(|p| p.id == expected)
                .unwrap_or_else(|| panic!("missing provider {expected}"));
            assert!(!p.country_name.is_empty(), "empty country_name on {expected}");
        }
    }

    #[test]
    #[ignore = "live network smoke test"]
    fn live_chess_results_single_game_6716323() {
        let links =
            fetch_portal_pgn_links("https://s1.chess-results.com/PartieSuche.aspx?art=36&id=6716323")
                .expect("fetch id=6716323");
        assert_eq!(links.len(), 1, "expected 1 deduped link, got {links:?}");
        let (total, games) = download_and_parse_league_pgn(&links[0].pgn_url, "").expect("parse game");
        assert_eq!(total, 1);
        assert_eq!(games.len(), 1);
        let g = &games[0];
        println!(
            "live 6716323: {} vs {} · {} · {} plies",
            g.white(),
            g.black(),
            g.result(),
            g.mainline_len()
        );
        assert!(g.white().contains("Fedorovsky"));
        assert!(g.black().contains("Hinterreiter"));
        assert!(
            g.mainline_len() >= 30,
            "expected >=30 plies, got {}",
            g.mainline_len()
        );
    }

    #[test]
    #[ignore = "live network smoke test"]
    fn live_chess_results_tournament_1410724_rd5_and_art0() {
        let rd5 = fetch_portal_pgn_links(
            "https://s1.chess-results.com/tnr1410724.aspx?lan=1&art=3&rd=5&SNode=S0",
        )
        .expect("fetch rd=5");
        assert_eq!(rd5.len(), 1, "expected single bulk link on rd=5, got {rd5:?}");
        assert_eq!(rd5[0].initial_round, Some(5));

        let art0 = fetch_portal_pgn_links(
            "https://s3.chess-results.com/tnr1410724.aspx?lan=1&art=0&SNode=S0",
        )
        .expect("fetch art=0");
        assert_eq!(art0.len(), 1, "expected single bulk link on art=0, got {art0:?}");
        assert_eq!(art0[0].initial_round, None);

        let roster = fetch_league_roster_index(&art0[0].pgn_url, &art0[0].title)
            .expect("compile tnr1410724 roster");
        println!(
            "live tnr1410724: {} games, {} players, {} clubs, rounds {:?}",
            roster.games.len(),
            roster.players.len(),
            roster.clubs.len(),
            roster.rounds
        );
        assert!(roster.games.len() >= 100, "got {} games", roster.games.len());
        assert!(roster.players.len() >= 50, "got {} players", roster.players.len());
        assert!(roster.clubs.len() >= 8, "got {} clubs", roster.clubs.len());
        assert!(
            roster
                .players
                .iter()
                .any(|p| p.name.to_lowercase().contains("hinterreiter")),
            "expected Hinterreiter in tnr1410724 roster"
        );
        for g in &roster.games {
            let cat = crate::players::classify_game_category(g);
            assert_ne!(
                cat,
                crate::players::GameCategory::Correspondence,
                "unexpected Correspondence classification on OTB game {} - {} ({} / {})",
                g.white(),
                g.black(),
                g.event(),
                g.header("Site").unwrap_or("")
            );
        }
    }

    #[test]
    #[ignore = "live network smoke test"]
    fn live_providers_smoke_check() {
        let lm_dir = fetch_ligamanager_directory(None).expect("LigaManager directory");
        println!("LigaManager Bayern leagues: {}", lm_dir.leagues.len());
        assert!(!lm_dir.leagues.is_empty());
        let first_lm = &lm_dir.leagues[0];
        let dl = fetch_ligamanager_downloads(&first_lm.path_prefix).expect("LM downloads");
        println!("LM first league: {} (all={:?}, rounds={})", dl.title, dl.all_pgn_url, dl.rounds.len());
        let lm_url = dl
            .all_pgn_url
            .clone()
            .or_else(|| dl.rounds.first().map(|r| r.pgn_url.clone()))
            .expect("LM download URL");
        let lm_roster = fetch_league_roster_index(&lm_url, &dl.title).expect("LM roster");
        println!("LM roster: {} games, {} players", lm_roster.games.len(), lm_roster.players.len());
        assert!(!lm_roster.games.is_empty());

        let dsb_dir = fetch_dsb_directory("").expect("DSB directory");
        println!("DSB leagues: {}", dsb_dir.leagues.len());
        assert!(!dsb_dir.leagues.is_empty());
        let dsb_pgn = fetch_dsb_league_pgn("", "bl").expect("DSB Bundesliga PGN");
        println!("DSB bl PGN: {}", dsb_pgn.pgn_url);
        assert!(dsb_pgn.pgn_url.contains(".pgn"));
        let dsb_roster = fetch_league_roster_index(&dsb_pgn.pgn_url, &dsb_pgn.league_name).expect("DSB bl roster");
        println!("DSB bl roster: {} games, {} clubs", dsb_roster.games.len(), dsb_roster.clubs.len());
        assert!(!dsb_roster.games.is_empty());

        let broadcasts = fetch_lichess_broadcasts(10, None).expect("Lichess broadcasts");
        println!("Lichess broadcasts loaded: {}", broadcasts.len());
        assert!(!broadcasts.is_empty());
        let first_tour = &broadcasts[0];
        let tour_url = first_tour
            .rounds
            .iter()
            .find(|r| r.finished || r.ongoing)
            .map(|r| r.pgn_url.as_str())
            .unwrap_or(first_tour.all_pgn_url.as_str());
        let (bc_total, bc_games) = download_and_parse_league_pgn(tour_url, "").expect("Lichess broadcast PGN");
        println!("Lichess broadcast '{}': {} games parsed", first_tour.name, bc_games.len());
        assert!(bc_total > 0);

        let aut_leagues = fetch_chess_results_tournaments(
            "AUT",
            ChessResultsTournamentType::TeamRoundRobin,
            true,
            "",
            "0",
        )
        .expect("AUT Chess-Results team leagues");
        println!("Chess-Results AUT team leagues with PGN: {}", aut_leagues.len());
        assert!(!aut_leagues.is_empty());
        if let Some(first_aut) = aut_leagues.first() {
            let aut_roster =
                fetch_league_roster_index(&first_aut.bulk_pgn_url, &first_aut.title)
                    .expect("AUT Chess-Results roster");
            println!(
                "AUT '{}': {} games",
                first_aut.title,
                aut_roster.games.len()
            );
            assert!(!aut_roster.games.is_empty());
        }

        let ch_links = fetch_portal_pgn_links("https://www.swisschess.ch").expect("SwissChess");
        println!("SwissChess links: {}", ch_links.len());
        assert!(!ch_links.is_empty());

        let uk = fetch_portal_pgn_links("https://www.4ncl.co.uk/").expect("4NCL UK");
        println!("4NCL UK links: {} (first: {})", uk.len(), uk[0].pgn_url);
        assert!(!uk.is_empty());

        let mentor = fetch_portal_pgn_links("https://www.pgnmentor.com/files.html").expect("PGN Mentor");
        println!("PGN Mentor links: {}", mentor.len());
        assert!(mentor.len() >= 100);
        if let Some(sample_zip) = mentor.iter().find(|l| l.pgn_url.ends_with(".zip")) {
            let (m_total, _) = download_and_parse_league_pgn(&sample_zip.pgn_url, "").expect("PGN Mentor zip");
            println!("PGN Mentor sample '{}': {} games", sample_zip.title, m_total);
            assert!(m_total > 0);
        }
    }
}
