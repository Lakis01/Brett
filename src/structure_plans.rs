//! Original educational structure plans and animated piece maneuvers for
//! middlegame pawn formations in Brett.
//!
//! All prose descriptions and instructive setups in this module use standard,
//! original chess concepts and terminology so they illustrate classic plans
//! without copying external books or game scores.

use crate::chess::{Color, Piece, Role, Square};
use crate::search::{StructureFamily, StructurePreset};

/// Animated multi-step piece trajectory rendered on the illustrative preview board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimatedManeuver {
    pub piece: Piece,
    pub waypoints: &'static [Square],
}

/// One typical strategic plan belonging to a [`StructureFamily`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StructurePlan {
    pub family: StructureFamily,
    pub side: Color,
    pub plan_number: u8,
    pub title: &'static str,
    pub summary: &'static str,
    pub setup_fen: &'static str,
    pub search_preset: Option<StructurePreset>,
    pub green_squares: &'static [Square],
    pub gold_squares: &'static [Square],
    pub arrows: &'static [(Square, Square)],
    pub animated_maneuver: Option<AnimatedManeuver>,
}

const WK: Piece = Piece::new(Color::White, Role::King);
const WQ: Piece = Piece::new(Color::White, Role::Queen);
const WR: Piece = Piece::new(Color::White, Role::Rook);
const WB: Piece = Piece::new(Color::White, Role::Bishop);
const WN: Piece = Piece::new(Color::White, Role::Knight);
const WP: Piece = Piece::new(Color::White, Role::Pawn);
const BK: Piece = Piece::new(Color::Black, Role::King);
const BQ: Piece = Piece::new(Color::Black, Role::Queen);
const BR: Piece = Piece::new(Color::Black, Role::Rook);
const BB: Piece = Piece::new(Color::Black, Role::Bishop);
const BN: Piece = Piece::new(Color::Black, Role::Knight);
const BP: Piece = Piece::new(Color::Black, Role::Pawn);

static PLANS: &[StructurePlan] = &[
    // --- STONEWALL ---
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::White,
        plan_number: 1,
        title: "Bd2-e1-h4 bishop transfer vs Nf6",
        summary: "Transfer the dark-squared bishop via d2 and e1 to h4 to pin or pressure the f6-knight and kingside.",
        // Pawns: a2,b2,c3,d4,e3,f4,g2,h2; White: Ra1,Nb1,Bc1,Qd1,Bd3,Nf3,Kg1,Rf1; Black: reasonable setup w/ Nf6
        setup_fen: "rnbq1rk1/pp2bppp/2p1pn2/3p4/3P1P2/2PBPN2/PP4PP/RNBQ1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::StonewallBd2e1h4),
        green_squares: &[Square::H4],
        gold_squares: &[Square::D2, Square::E1, Square::F6],
        arrows: &[
            (Square::C1, Square::D2),
            (Square::D2, Square::E1),
            (Square::E1, Square::H4),
        ],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WB,
            waypoints: &[Square::C1, Square::D2, Square::E1, Square::H4],
        }),
    },
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::White,
        plan_number: 2,
        title: "Establish outpost knight on e5",
        summary: "Plant a knight on e5 supported by f4/d4 and mobilize rook lift + g2-g4 kingside storm.",
        setup_fen: "r1bq1rk1/pp2bppp/2n1pn2/3p4/3P1P2/2PBPN2/PP4PP/RNBQ1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::StonewallClassic),
        green_squares: &[Square::E5],
        gold_squares: &[Square::D4, Square::F4],
        arrows: &[(Square::F3, Square::E5), (Square::G2, Square::G4)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WN,
            waypoints: &[Square::F3, Square::E5],
        }),
    },
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::White,
        plan_number: 3,
        title: "Kingside pawn storm (g4-g5)",
        summary: "Launch g2-g4 and f4-f5 or Rf3-h3 against castled king once center is locked.",
        setup_fen: "r1bq1rk1/pp2bppp/2n1pn2/3p4/3P1PP1/2PBPN2/PP5P/RNBQ1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::StonewallAny),
        green_squares: &[Square::G4, Square::G5, Square::H7],
        gold_squares: &[Square::F4],
        arrows: &[
            (Square::G2, Square::G4),
            (Square::F1, Square::F3),
            (Square::F3, Square::H3),
        ],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WR,
            waypoints: &[Square::F1, Square::F3, Square::H3],
        }),
    },
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::Black,
        plan_number: 1,
        title: "Trade off bad bishop with b6 & Ba6",
        summary: "Reroute the c8-bishop via a6 to swap off the strong light-squared bishop.",
        setup_fen: "rnbq1rk1/p3bppp/1p2pn2/3p4/2PP1P2/4PN2/PP2B1PP/RNBQ1RK1 b - - 0 1",
        search_preset: Some(StructurePreset::StonewallBadBishopTrade),
        green_squares: &[Square::A6],
        gold_squares: &[Square::D3, Square::B6],
        arrows: &[(Square::C8, Square::A6), (Square::B7, Square::B6)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: BB,
            waypoints: &[Square::C8, Square::A6],
        }),
    },
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::Black,
        plan_number: 2,
        title: "Central e4 outpost & c5 break",
        summary: "Challenge the pyramid with ...c5 or anchor a knight on e4.",
        setup_fen: "r1bq1rk1/pp3ppp/2n1pn2/2pp4/2PP1P2/3BP3/PP2N1PP/R1BQ1RK1 b - - 0 1",
        search_preset: Some(StructurePreset::StonewallC5Break),
        green_squares: &[Square::E4, Square::C5],
        gold_squares: &[Square::D4],
        arrows: &[(Square::F6, Square::E4), (Square::C7, Square::C5)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: BN,
            waypoints: &[Square::F6, Square::E4],
        }),
    },
    // --- BADEWANNE ---
    StructurePlan {
        family: StructureFamily::Badewanne,
        side: Color::White,
        plan_number: 1,
        title: "Central d4/e4 advance against backward shell",
        summary: "Prepare a central e3-e4 push or place knights on the open central squares.",
        setup_fen: "r1bq1rk1/ppp2ppp/2n2n2/3pp3/2PP4/4PN2/PP3PPP/RNBQKB1R w KQ - 0 1",
        search_preset: Some(StructurePreset::BadewanneCentral),
        green_squares: &[Square::D4, Square::E4],
        gold_squares: &[Square::D5, Square::E5],
        arrows: &[(Square::E3, Square::E4)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WP,
            waypoints: &[Square::E3, Square::E4],
        }),
    },
    StructurePlan {
        family: StructureFamily::Badewanne,
        side: Color::Black,
        plan_number: 1,
        title: "Hold central tension & target c4/d4",
        summary: "Keep flexible rim pawns while pressuring White's central pawns with pieces.",
        setup_fen: "r2q1rk1/pp3ppp/2p2n2/2bpp3/2PP4/2N1PN2/PP3PPP/R1BQ1RK1 b - - 0 1",
        search_preset: Some(StructurePreset::BadewanneFlexibleRim),
        green_squares: &[Square::D4, Square::E5],
        gold_squares: &[Square::D6],
        arrows: &[(Square::C5, Square::D4)],
        animated_maneuver: None,
    },
    // --- CARLSBAD ---
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::White,
        plan_number: 1,
        title: "Minority attack (b4-b5)",
        summary: "Advance a2-a4 and b2-b4-b5 to create a backward c6-pawn or weak b5/c6 target.",
        setup_fen: "r1bq1rk1/pp3ppp/2n1p3/3p4/2P5/2N1P3/PP3PPP/R1BQ1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::CarlsbadMinorityAttack),
        green_squares: &[Square::B5, Square::C6],
        gold_squares: &[Square::B4],
        arrows: &[(Square::B4, Square::B5), (Square::B5, Square::C6)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WP,
            waypoints: &[Square::B2, Square::B4, Square::B5],
        }),
    },
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::White,
        plan_number: 2,
        title: "Central f3 + e4 break",
        summary: "Keep the king safe and prepare f2-f3 followed by e3-e4 to seize central space.",
        setup_fen: "r2q1rk1/pp2bppp/2p1pn2/3p4/3P4/2NBP3/PP3PPP/R2Q1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::CarlsbadF3E4),
        green_squares: &[Square::E4, Square::F3],
        gold_squares: &[Square::D4],
        arrows: &[(Square::F2, Square::F3), (Square::E3, Square::E4)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WP,
            waypoints: &[Square::E3, Square::E4],
        }),
    },
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::Black,
        plan_number: 1,
        title: "Plant a knight on e4 & attack kingside",
        summary: "Use the semi-open e-file and e4-outpost to counterattack on the kingside.",
        setup_fen: "r2q1rk1/pp3ppp/2p1pn2/3p4/1P6/2N1P3/P3BPPP/R1BQ1RK1 b - - 0 1",
        search_preset: Some(StructurePreset::CarlsbadAny),
        green_squares: &[Square::E4],
        gold_squares: &[Square::C6],
        arrows: &[(Square::F6, Square::E4), (Square::D8, Square::H4)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: BN,
            waypoints: &[Square::F6, Square::E4],
        }),
    },
    // --- IQP ---
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::White,
        plan_number: 1,
        title: "Establish knight on e5 & kingside build-up",
        summary: "Place minor pieces actively on e5, c4/b3, and e4/f5 squares before Black consolidates.",
        setup_fen: "r2q1rk1/pp3ppp/2n1pn2/8/3P4/2NBPN2/PP3PPP/R2Q1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::IqpDynamic),
        green_squares: &[Square::E5, Square::D5],
        gold_squares: &[Square::D4],
        arrows: &[(Square::F3, Square::E5), (Square::D4, Square::D5)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WN,
            waypoints: &[Square::F3, Square::E5],
        }),
    },
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::Black,
        plan_number: 1,
        title: "Blockade d5 & trade minor pieces",
        summary: "Anchor a knight or bishop on d5, trade attackers, and target the static d4-pawn.",
        setup_fen: "r2q1rk1/pp3ppp/4pn2/8/3P4/3B1N2/PP3PPP/R2Q1RK1 b - - 0 1",
        search_preset: Some(StructurePreset::IqpBlockaded),
        green_squares: &[Square::D5],
        gold_squares: &[Square::D4],
        arrows: &[(Square::F6, Square::D5), (Square::D5, Square::D4)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: BN,
            waypoints: &[Square::F6, Square::D5],
        }),
    },
    // --- HANGING PAWNS ---
    StructurePlan {
        family: StructureFamily::HangingPawns,
        side: Color::White,
        plan_number: 1,
        title: "Central d4-d5 push with minor-piece backing",
        summary: "Advance d4-d5 when pieces control d5 to open lines against the enemy king.",
        setup_fen: "r2q1rk1/pp2bppp/4pn2/8/2PP4/2N2N2/PP2BPPP/R1BQ1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::HangingPawnsClassic),
        green_squares: &[Square::D5],
        gold_squares: &[Square::C4, Square::D4],
        arrows: &[(Square::D4, Square::D5)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WP,
            waypoints: &[Square::D4, Square::D5],
        }),
    },
    StructurePlan {
        family: StructureFamily::HangingPawns,
        side: Color::Black,
        plan_number: 1,
        title: "Provoke ...b6/...e5 to split the pair",
        summary: "Attack the hanging pawns with ...Nc6, ...Rd8, and ...Na5/Ba6 so one must advance weakly.",
        setup_fen: "r2q1rk1/p3bppp/1p2pn2/2n5/2PP4/2N2N2/PP2BPPP/R1BQ1RK1 b - - 0 1",
        search_preset: Some(StructurePreset::HangingPawnsDynamic),
        green_squares: &[Square::C4, Square::D4],
        gold_squares: &[Square::C5],
        arrows: &[(Square::C5, Square::D4), (Square::C8, Square::A6)],
        animated_maneuver: None,
    },
    // --- MARÓCZY & HEDGEHOG ---
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::White,
        plan_number: 1,
        title: "Maróczy clamp (no K-side fianchetto): restrain d5 & b5",
        summary: "Develop Be2/Be3 and Nc3 without g3, squeeze Black with c4+e4, and press along c- and d-files.",
        setup_fen: "r1bq1rk1/pp2ppbp/2np1np1/8/2P1P3/2N1BN2/PP2BPPP/R2Q1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::MaroczyBindNoFianchetto),
        green_squares: &[Square::D5, Square::B5],
        gold_squares: &[Square::C4, Square::E4],
        arrows: &[
            (Square::C4, Square::D5),
            (Square::E4, Square::D5),
            (Square::C3, Square::D5),
        ],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WN,
            waypoints: &[Square::C3, Square::D5],
        }),
    },
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::White,
        plan_number: 2,
        title: "Establish a strong knight on d5 (with K-side fianchetto)",
        summary: "Fianchetto Bg2 and station a dominant knight on d5 (highlighted green).",
        setup_fen: "r2q1rk1/1p1bppbp/p2p1np1/3N4/2P1P3/4B1P1/PP2NPBP/R2Q1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::MaroczyBindFianchetto),
        green_squares: &[Square::D5],
        gold_squares: &[Square::C4, Square::E4, Square::G2],
        arrows: &[(Square::C3, Square::D5), (Square::G2, Square::D5)],
        animated_maneuver: None,
    },
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::Black,
        plan_number: 1,
        title: "Hedgehog counterbreaks (...b5 or ...d5)",
        summary: "Coil minor pieces behind a6-b6-d6-e6 and strike dynamically with ...b5 or ...d5.",
        setup_fen: "r2q1rk1/1b1nbppp/pp1pp3/8/2P1P3/2N1B3/PP2BPPP/R2Q1RK1 b - - 0 1",
        search_preset: Some(StructurePreset::Hedgehog),
        green_squares: &[Square::B5, Square::D5],
        gold_squares: &[Square::A6, Square::B6, Square::D6, Square::E6],
        arrows: &[(Square::B6, Square::B5), (Square::D6, Square::D5)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: BP,
            waypoints: &[Square::D6, Square::D5],
        }),
    },
    // --- SICILIAN STRUCTURES ---
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::White,
        plan_number: 1,
        title: "Establish a strong knight on d5",
        summary: "Dominate the backward d5 square (e.g. Boleslavsky/Najdorf structure) by planting a knight firmly on d5 (highlighted green).",
        setup_fen: "r2q1rk1/1p2bppp/p2p1n2/3Np3/4P3/4B3/PPPQBPPP/R4RK1 w - - 0 1",
        search_preset: Some(StructurePreset::Boleslavsky),
        green_squares: &[Square::D5],
        gold_squares: &[Square::D6, Square::E4],
        arrows: &[(Square::C3, Square::D5), (Square::E3, Square::B6)],
        animated_maneuver: None,
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::Black,
        plan_number: 1,
        title: "Najdorf Type I: ...b5 expansion & ...d5 break",
        summary: "Challenge White's central space with queenside ...b5-b4 expansion or a timely ...d6-d5 central break.",
        setup_fen: "r2q1rk1/1p2bppp/p2ppn2/8/3NP3/2N1B3/PPP1BPPP/R2Q1RK1 b - - 0 1",
        search_preset: Some(StructurePreset::NajdorfStructure),
        green_squares: &[Square::B5, Square::D5],
        gold_squares: &[Square::D6, Square::E6],
        arrows: &[(Square::B7, Square::B5), (Square::D6, Square::D5)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: BP,
            waypoints: &[Square::B7, Square::B5, Square::B4],
        }),
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::Black,
        plan_number: 2,
        title: "Najdorf Type II: ...b5 + ...Bb7 counterplay",
        summary: "Press the c-file and e4 pawn with ...b5, ...Bb7, and ...Nc5/...b4 in queenside-expanded Najdorf structures.",
        setup_fen: "r2q1rk1/3nbppp/p2pp3/1p6/4P3/1NN1BP2/PPPQ2PP/2KR1B1R b - - 0 1",
        search_preset: Some(StructurePreset::NajdorfTypeII),
        green_squares: &[Square::B5, Square::E4],
        gold_squares: &[Square::C5, Square::B7],
        arrows: &[(Square::C8, Square::B7), (Square::D7, Square::C5)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: BN,
            waypoints: &[Square::D7, Square::C5],
        }),
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::White,
        plan_number: 3,
        title: "Najdorf Type III: f4-f5 / d5 pressure vs ...e5",
        summary: "Exploit the backward d5 square and kingside f4-f5 lever before Black stabilizes the center with ...d5.",
        setup_fen: "r2q1rk1/1p2bppp/p2p1n2/4p3/4PP2/2N1B3/PPPQ2PP/R4RK1 b - - 0 1",
        search_preset: Some(StructurePreset::NajdorfTypeIII),
        green_squares: &[Square::D5, Square::F5],
        gold_squares: &[Square::E5, Square::D6],
        arrows: &[(Square::F4, Square::F5), (Square::C3, Square::D5)],
        animated_maneuver: None,
    },
    // --- SLAV, TRIANGLE & GRÜNFELD ---
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::Black,
        plan_number: 1,
        title: "Semi-Slav ...dxc4 and ...c5/...e5 break",
        summary: "Hold the d5-c6-e6 triangle until pieces develop, then capture on c4 and liberate with ...b5 and ...c5.",
        setup_fen: "r1bq1rk1/pp3ppp/2p1pn2/3p4/2PP4/2N1PN2/PP3PPP/R1BQ1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::SlavFormation),
        green_squares: &[Square::C4, Square::C5],
        gold_squares: &[Square::D5, Square::E6],
        arrows: &[(Square::D5, Square::C4), (Square::C6, Square::C5)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: BP,
            waypoints: &[Square::D5, Square::C4],
        }),
    },
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::Black,
        plan_number: 2,
        title: "Grünfeld pressure on d4 with ...c5 and ...Bg7",
        summary: "Target the broad e4-d4 pawn center along the dark diagonal and c-file while challenging d4 with ...Nc6/...cxd4.",
        setup_fen: "r1bq1rk1/pp2ppbp/6p1/2p5/3PP3/2N1B3/PP3PPP/R2QKB1R w KQ - 0 1",
        search_preset: Some(StructurePreset::GrunfeldCenter),
        green_squares: &[Square::D4, Square::C5],
        gold_squares: &[Square::E4, Square::G7],
        arrows: &[(Square::C5, Square::D4), (Square::G7, Square::D4)],
        animated_maneuver: None,
    },
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::White,
        plan_number: 1,
        title: "Botvinnik Triangle: kingside f4 push",
        summary: "Guard d4 with e4-c4-d3 while maneuvering Ne2 and preparing a timed f2-f4 kingside pawn storm.",
        setup_fen: "r1bq1rk1/ppp2ppp/2np1n2/4p3/2P1P3/2NP2P1/PP3PBP/R1BQ1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::BotvinnikTriangle),
        green_squares: &[Square::F4, Square::D5],
        gold_squares: &[Square::C4, Square::E4],
        arrows: &[(Square::F2, Square::F4), (Square::F4, Square::F5)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WP,
            waypoints: &[Square::F2, Square::F4],
        }),
    },
    // --- CLOSED CHAINS ---
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::Black,
        plan_number: 1,
        title: "Attack the base of the pawn chain (...c5 / ...f5)",
        summary: "Undermine d4 in the French chain with ...c5 and ...Qb6, or launch ...f5 on the kingside in KID.",
        setup_fen: "r1bq1rk1/pp3ppp/2n1pn2/2ppP3/3P4/2P2N2/PP2BPPP/R1BQ1RK1 b - - 0 1",
        search_preset: Some(StructurePreset::FrenchChain),
        green_squares: &[Square::D4, Square::C5],
        gold_squares: &[Square::E5],
        arrows: &[(Square::C5, Square::D4), (Square::F7, Square::F6)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: BP,
            waypoints: &[Square::F7, Square::F6],
        }),
    },
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::White,
        plan_number: 1,
        title: "Expand where pawns point (f4 / kingside space)",
        summary: "Gain kingside space with Nf3-g5 or f2-f4 while keeping the base d4 firmly defended.",
        setup_fen: "r1bq1rk1/pp3ppp/2n1pn2/2ppP3/3P1P2/2P5/PP2B1PP/RNBQ1RK1 w - - 0 1",
        search_preset: Some(StructurePreset::KingsIndianChain),
        green_squares: &[Square::F5, Square::G4],
        gold_squares: &[Square::D4, Square::E5],
        arrows: &[(Square::F4, Square::F5), (Square::E2, Square::D3)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WB,
            waypoints: &[Square::E2, Square::D3, Square::B1],
        }),
    },
];

/// Return all White or Black plans belonging to a [`StructureFamily`].
#[must_use]
pub fn plans_for_family(family: StructureFamily) -> Vec<&'static StructurePlan> {
    PLANS.iter().filter(|p| p.family == family).collect()
}

/// Find the closest plan matching a specific [`StructurePreset`], if one exists.
#[must_use]
pub fn plan_for_preset(preset: StructurePreset) -> Option<&'static StructurePlan> {
    PLANS
        .iter()
        .find(|p| p.search_preset == Some(preset))
        .or_else(|| PLANS.iter().find(|p| p.family == preset.family()))
}

#[allow(dead_code)]
fn _piece_palette_used() -> [Piece; 12] {
    [WK, WQ, WR, WB, WN, WP, BK, BQ, BR, BB, BN, BP]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chess::{Chess, Fen, Position};

    #[test]
    fn every_structure_family_has_at_least_one_valid_plan_fen() {
        for fam in StructureFamily::ALL {
            let plans = plans_for_family(fam);
            assert!(!plans.is_empty(), "missing plans for {fam:?}");
            for plan in plans {
                let fen: Fen = plan
                    .setup_fen
                    .parse()
                    .unwrap_or_else(|e| panic!("invalid FEN for {}: {e}", plan.title));
                let pos: Chess = fen
                    .into_position(crate::chess::CastlingMode::Standard)
                    .unwrap_or_else(|e| panic!("illegal position for {}: {e}", plan.title));
                if let Some(m) = plan.animated_maneuver {
                    assert!(
                        !m.waypoints.is_empty(),
                        "empty waypoints for {}",
                        plan.title
                    );
                    assert_eq!(
                        pos.board().piece_at(m.waypoints[0]),
                        Some(m.piece),
                        "starting square {:?} missing {:?} in {}",
                        m.waypoints[0],
                        m.piece,
                        plan.title
                    );
                }
            }
        }
    }

    #[test]
    fn stonewall_bd2e1h4_plan_has_exact_setup_and_black_nf6() {
        let plans = plans_for_family(StructureFamily::Stonewall);
        let p = plans
            .iter()
            .find(|p| p.search_preset == Some(StructurePreset::StonewallBd2e1h4))
            .expect("Bd2-e1-h4 plan present");
        let fen: Fen = p.setup_fen.parse().unwrap();
        let pos: Chess = fen
            .into_position(crate::chess::CastlingMode::Standard)
            .unwrap();
        let b = pos.board();
        for sq in [
            Square::A2,
            Square::B2,
            Square::C3,
            Square::D4,
            Square::E3,
            Square::F4,
            Square::G2,
            Square::H2,
        ] {
            assert_eq!(b.piece_at(sq), Some(WP), "expected White pawn at {sq}");
        }
        assert_eq!(b.piece_at(Square::F6), Some(BN));
        assert_eq!(b.piece_at(Square::C1), Some(WB));
        assert_eq!(b.piece_at(Square::D3), Some(WB));
        assert_eq!(b.piece_at(Square::F3), Some(WN));
    }
}
