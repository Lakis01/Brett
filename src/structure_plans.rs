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
    /// Optional sequence of legal SAN moves played out on `setup_fen` in the mini-board preview.
    pub animated_moves: &'static [&'static str],
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
    // --- STONEWALL (6 plans: 3 White, 3 Black) ---
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::White,
        plan_number: 1,
        title: "Bd2-e1-h4 bishop transfer vs Nf6",
        summary: "Transfer the dark-squared bishop via d2 and e1 to h4 to pin or pressure the f6-knight and kingside.",
        setup_fen: "r2q1rk1/pb2bppp/1pn1pn2/2pp4/3P1P2/2PBPN2/PP1BQ1PP/RN3RK1 w - - 2 10",
        search_preset: Some(StructurePreset::StonewallBd2e1h4),
        green_squares: &[Square::H4],
        gold_squares: &[Square::D2, Square::E1, Square::F6],
        arrows: &[(Square::D2, Square::E1), (Square::E1, Square::H4)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WB,
            waypoints: &[Square::D2, Square::E1, Square::H4],
        }),
        animated_moves: &[],
    },
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::White,
        plan_number: 2,
        title: "Establish outpost knight on e5",
        summary: "Plant a knight on e5 supported by d4 and f4 before mobilizing a kingside attack.",
        setup_fen: "r1bq1rk1/p3bppp/1pn1pn2/2pp4/3P1P2/2PBPN2/PP1N2PP/R1BQ1RK1 w - - 0 9",
        search_preset: Some(StructurePreset::StonewallClassic),
        green_squares: &[Square::E5],
        gold_squares: &[Square::D4, Square::F4],
        arrows: &[(Square::F3, Square::E5)],
        animated_maneuver: None,
        animated_moves: &["Ne5"],
    },
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::White,
        plan_number: 3,
        title: "Kingside rook lift (Rf3-h3) & attack",
        summary: "Swing the rook via f3 to h3 against the castled king once the e5 outpost is secured.",
        setup_fen: "r2q1rk1/pb2bppp/1pn1pn2/2ppN3/3P1P2/2PBP3/PP1N2PP/R1BQ1RK1 w - - 2 10",
        search_preset: Some(StructurePreset::StonewallAny),
        green_squares: &[Square::F3, Square::H3, Square::H7],
        gold_squares: &[Square::E5, Square::F4],
        arrows: &[(Square::F1, Square::F3), (Square::F3, Square::H3)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WR,
            waypoints: &[Square::F1, Square::F3, Square::H3],
        }),
        animated_moves: &[],
    },
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::Black,
        plan_number: 1,
        title: "Trade off bad bishop with ...b6 & ...Ba6",
        summary: "Reroute the c8-bishop via a6 to swap off White's strong light-squared bishop on d3.",
        setup_fen: "rn1q1rk1/p1p1b1pp/bp2pn2/3p1p2/3P4/1PPBPN2/P2NQPPP/R1B2RK1 b - - 3 9",
        search_preset: Some(StructurePreset::StonewallBadBishopTrade),
        green_squares: &[Square::A6, Square::D3],
        gold_squares: &[Square::B6],
        arrows: &[(Square::A6, Square::D3)],
        animated_maneuver: None,
        animated_moves: &["Bxd3", "Qxd3"],
    },
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::Black,
        plan_number: 2,
        title: "Central ...Ne4 outpost & ...c5 break",
        summary: "Challenge the center with ...c5 and anchor a knight on the strong e4 outpost.",
        setup_fen: "r1bq1rk1/pp2b1pp/2n1pn2/2pp1p2/2PP4/1P2PNP1/PB3PBP/RN1Q1RK1 b - - 0 9",
        search_preset: Some(StructurePreset::StonewallC5Break),
        green_squares: &[Square::E4, Square::C5],
        gold_squares: &[Square::D5, Square::F5],
        arrows: &[(Square::F6, Square::E4), (Square::C5, Square::D4)],
        animated_maneuver: None,
        animated_moves: &["Ne4"],
    },
    StructurePlan {
        family: StructureFamily::Stonewall,
        side: Color::Black,
        plan_number: 3,
        title: "Queen transfer ...Qe8-h5 & ...g5 counterattack",
        summary: "Transfer the queen via e8 to h5 and launch ...g5 to generate kingside counterplay.",
        setup_fen: "r1b1qrk1/pp2b1pp/2n1pn2/3p1p2/2PP4/2N1PNP1/PP3PBP/R1BQ1RK1 b - - 4 10",
        search_preset: Some(StructurePreset::StonewallNc6),
        green_squares: &[Square::H5, Square::G5],
        gold_squares: &[Square::E8, Square::F5],
        arrows: &[(Square::E8, Square::H5), (Square::G7, Square::G5)],
        animated_maneuver: None,
        animated_moves: &["Qh5", "b3", "g5"],
    },
    // --- BADEWANNE (4 plans: 2 White, 2 Black) ---
    StructurePlan {
        family: StructureFamily::Badewanne,
        side: Color::White,
        plan_number: 1,
        title: "Central e3-e4 push from the bathtub",
        summary: "Prepare a central e3-e4 advance from the c4-d3-e3-f4 shell to seize central space.",
        setup_fen: "r3r1k1/pppq1ppp/2np1n2/4p3/1bP2Pb1/2NPP1P1/PP2N1BP/R1BQ1RK1 w - - 1 10",
        search_preset: Some(StructurePreset::BadewanneCentral),
        green_squares: &[Square::E4],
        gold_squares: &[Square::C4, Square::D3, Square::E3, Square::F4],
        arrows: &[(Square::E3, Square::E4)],
        animated_maneuver: None,
        animated_moves: &["e4"],
    },
    StructurePlan {
        family: StructureFamily::Badewanne,
        side: Color::White,
        plan_number: 2,
        title: "Kingside f4-f5 flank expansion",
        summary: "Keep the center flexible and push f4-f5 to open the f-file and undermine Black's kingside.",
        setup_fen: "r1bq1rk1/pp2ppbp/2np1np1/8/2P2P2/2NPP1P1/PP2N1BP/R1BQ1RK1 w - - 2 9",
        search_preset: Some(StructurePreset::BadewanneCentral),
        green_squares: &[Square::F5, Square::G6],
        gold_squares: &[Square::C4, Square::D3, Square::E3, Square::F4],
        arrows: &[(Square::F4, Square::F5)],
        animated_maneuver: None,
        animated_moves: &["f5"],
    },
    StructurePlan {
        family: StructureFamily::Badewanne,
        side: Color::Black,
        plan_number: 1,
        title: "Flexible rim bathtub & ...f4 counterplay",
        summary: "Keep d6-e6 compact behind c5 and f5 rim pawns, striking with ...f4 or challenging the queenside.",
        setup_fen: "r1bq1rk1/pp2b1pp/2npp3/2p2p1n/1PP5/P1NP1NP1/4PPBP/1RBQ1RK1 b - - 1 11",
        search_preset: Some(StructurePreset::BadewanneFlexibleRim),
        green_squares: &[Square::F4, Square::C5],
        gold_squares: &[Square::D6, Square::E6],
        arrows: &[(Square::F5, Square::F4), (Square::C5, Square::B4)],
        animated_maneuver: None,
        animated_moves: &["f4"],
    },
    StructurePlan {
        family: StructureFamily::Badewanne,
        side: Color::Black,
        plan_number: 2,
        title: "Central ...d5 break against the d6-e6 shell",
        summary: "Prepare ...d6-d5 once pieces coordinate behind the compact c5-d6-e6-f5 formation.",
        setup_fen: "r1bq1rk1/pp2b1pp/2nppn2/2p2p2/2P5/2NP1NP1/PP2PPBP/R1BQ1RK1 b - - 2 9",
        search_preset: Some(StructurePreset::BadewanneAny),
        green_squares: &[Square::D5, Square::E4],
        gold_squares: &[Square::C5, Square::D6, Square::E6, Square::F5],
        arrows: &[(Square::D6, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["d5"],
    },
    // --- CARLSBAD (7 plans: 4 White, 3 Black) ---
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::White,
        plan_number: 1,
        title: "Minority attack (b4-b5)",
        summary: "Advance b2-b4-b5 to create a backward c6-pawn or weak queenside targets after ...bxc6.",
        setup_fen: "r1bqrnk1/1p3p1p/2pb1np1/3p2B1/1P1P4/2NBPN2/2Q2PPP/1R3RK1 w - - 1 15",
        search_preset: Some(StructurePreset::CarlsbadMinorityAttack),
        green_squares: &[Square::B5, Square::C6],
        gold_squares: &[Square::B4, Square::D4],
        arrows: &[(Square::B4, Square::B5), (Square::B5, Square::C6)],
        animated_maneuver: None,
        animated_moves: &["b5"],
    },
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::White,
        plan_number: 2,
        title: "Central f3 + e4 break",
        summary: "Consolidate with f2-f3 and break with e3-e4 to seize central space and attack the kingside.",
        setup_fen: "2rqrnk1/pp2bppp/2p1bn2/3p2B1/3P4/2NBPP2/PPQ1N1PP/3R1RK1 w - - 3 13",
        search_preset: Some(StructurePreset::CarlsbadF3E4),
        green_squares: &[Square::E4],
        gold_squares: &[Square::D4, Square::F3],
        arrows: &[(Square::E3, Square::E4)],
        animated_maneuver: None,
        animated_moves: &["e4"],
    },
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::White,
        plan_number: 3,
        title: "Half-open e-file control & Ne5 outpost",
        summary: "Use the half-open e-file with Re1 and plant a knight on e5 to support kingside play.",
        setup_fen: "r1bqrnk1/pp2bppp/2p2n2/3p4/3P1B2/2NBPN2/PPQ2PPP/1R2R1K1 w - - 6 12",
        search_preset: Some(StructurePreset::CarlsbadAny),
        green_squares: &[Square::E5, Square::E1],
        gold_squares: &[Square::D4, Square::E3],
        arrows: &[(Square::F3, Square::E5)],
        animated_maneuver: None,
        animated_moves: &["Ne5"],
    },
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::White,
        plan_number: 4,
        title: "Restrain ...b5 and occupy c5 outpost",
        summary: "After a3-a4/b4-b5 queenside probes or ...b5, route a knight to the c5 hole.",
        setup_fen: "r2qrnk1/p3bppp/2pb1n2/1p1p2B1/PP1P4/2NBPN2/2Q2PPP/1R3RK1 w - - 0 15",
        search_preset: Some(StructurePreset::CarlsbadMinorityAttack),
        green_squares: &[Square::C5],
        gold_squares: &[Square::B4, Square::C6],
        arrows: &[
            (Square::A4, Square::B5),
            (Square::C3, Square::A4),
            (Square::A4, Square::C5),
        ],
        animated_maneuver: None,
        animated_moves: &["axb5", "cxb5", "Ne2"],
    },
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::Black,
        plan_number: 1,
        title: "Plant a knight on e4 & kingside counterplay",
        summary: "Use the half-open e-file and e4-outpost to neutralize queenside pressure and counterattack.",
        setup_fen: "r1bqrnk1/pp2bppp/2p2n2/3p4/3P1B2/2NBPN2/PPQ2PPP/1R3RK1 b - - 8 11",
        search_preset: Some(StructurePreset::CarlsbadAny),
        green_squares: &[Square::E4],
        gold_squares: &[Square::C6, Square::D5],
        arrows: &[(Square::F6, Square::E4)],
        animated_maneuver: None,
        animated_moves: &["Ne4"],
    },
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::Black,
        plan_number: 2,
        title: "Queenside ...b5 counter-clamp & ...Nc4",
        summary: "Meet White's b4 advance with ...a6 and ...b5 to deny b4-b5 and fix a c4 outpost.",
        setup_fen: "r1bqrnk1/5ppp/p1pb1n2/1p1p2B1/1P1P4/2NBPN2/P1Q2PPP/1R3RK1 b - - 1 13",
        search_preset: Some(StructurePreset::CarlsbadMinorityAttack),
        green_squares: &[Square::B5, Square::C4],
        gold_squares: &[Square::C6, Square::D5],
        arrows: &[
            (Square::F8, Square::D7),
            (Square::D7, Square::B6),
            (Square::B6, Square::C4),
        ],
        animated_maneuver: None,
        animated_moves: &["Ng6", "Rfe1", "Bg4"],
    },
    StructurePlan {
        family: StructureFamily::Carlsbad,
        side: Color::Black,
        plan_number: 3,
        title: "Exchange light-squared bishops with ...Bf5",
        summary: "Trade off White's strong d3-bishop via ...Bf5 before reorganizing around the c-file.",
        setup_fen: "r2qrnk1/pp2bppp/2p2n2/3p1b2/3P4/2NBPN2/PPQ2PPP/R1B2RK1 b - - 5 11",
        search_preset: Some(StructurePreset::CarlsbadAny),
        green_squares: &[Square::F5, Square::D3],
        gold_squares: &[Square::C6, Square::D5],
        arrows: &[(Square::F5, Square::D3)],
        animated_maneuver: None,
        animated_moves: &["Bxd3", "Qxd3"],
    },
    // --- IQP & PANOV (9 plans: 5 White, 4 Black) ---
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::White,
        plan_number: 1,
        title: "Outpost knight on e5 vs e-pawn (no fianchetto)",
        summary: "Occupy e5 with a knight supported by Re1 and d4 before Black finishes consolidating.",
        setup_fen: "r1bq1rk1/1p2bppp/p1n1pn2/8/3P4/1BN2N2/PP3PPP/R1BQR1K1 w - - 5 11",
        search_preset: Some(StructurePreset::IqpVsEPawnNoFianchetto),
        green_squares: &[Square::E5],
        gold_squares: &[Square::D4, Square::E6],
        arrows: &[(Square::F3, Square::E5)],
        animated_maneuver: None,
        animated_moves: &["Ne5"],
    },
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::White,
        plan_number: 2,
        title: "Central d4-d5 break to liquidate the IQP",
        summary: "Push d4-d5 when major and minor pieces back the square, trading off the isolated pawn for initiative.",
        setup_fen: "r2q1rk1/1p2bppp/p1n1pn2/8/3P4/1BN2N2/PP1Q1PPP/R3R1K1 w - - 4 13",
        search_preset: Some(StructurePreset::IqpVsEPawnNoFianchetto),
        green_squares: &[Square::D5],
        gold_squares: &[Square::D4, Square::E6],
        arrows: &[(Square::D4, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["d5", "exd5", "Nxd5"],
    },
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::White,
        plan_number: 3,
        title: "Kingside rook lift (Re1-e3-g3/h3)",
        summary: "Use the open third rank to swing the e1-rook via e3 toward the kingside in dynamic IQP play.",
        setup_fen: "r2q1rk1/1p1bbppp/p1n1pn2/4N3/3P4/1BN5/PP2QPPP/R1B1R1K1 w - - 4 13",
        search_preset: Some(StructurePreset::IqpDynamic),
        green_squares: &[Square::E3, Square::G3, Square::H3],
        gold_squares: &[Square::D4, Square::E5],
        arrows: &[(Square::E1, Square::E3), (Square::E3, Square::G3)],
        animated_maneuver: Some(AnimatedManeuver {
            piece: WR,
            waypoints: &[Square::E1, Square::E3, Square::G3],
        }),
        animated_moves: &[],
    },
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::White,
        plan_number: 4,
        title: "Neutralize K-side fianchetto with Bg5 / Qd2-Bh6",
        summary: "Challenge Black's g7-bishop by aligning queen and dark-squared bishop to h6 or trading on f6.",
        setup_fen: "r1bq1rk1/1p3pbp/p1n1pnp1/6B1/3P4/1BN2N2/PP1Q1PPP/R3R1K1 w - - 2 12",
        search_preset: Some(StructurePreset::IqpVsEPawnFianchetto),
        green_squares: &[Square::H6, Square::G7],
        gold_squares: &[Square::D4, Square::G5],
        arrows: &[(Square::G5, Square::H6)],
        animated_maneuver: None,
        animated_moves: &["Bh6"],
    },
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::White,
        plan_number: 5,
        title: "IQP vs c-pawn: seize open e-file & e5",
        summary: "When Black holds a c-pawn instead of e-pawn, White controls the open e-file with Re1 and Ne5.",
        setup_fen: "r2q1rk1/pp1nbppp/2p1bn2/8/3P4/2NB1N2/PP3PPP/R1BQR1K1 w - - 4 11",
        search_preset: Some(StructurePreset::IqpVsCPawnNoFianchetto),
        green_squares: &[Square::E5, Square::E1],
        gold_squares: &[Square::D4, Square::C6],
        arrows: &[(Square::F3, Square::E5), (Square::E1, Square::E5)],
        animated_maneuver: None,
        animated_moves: &["Ne5"],
    },
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::Black,
        plan_number: 1,
        title: "Blockade d5 & pressure d4 with ...Bf6",
        summary: "Anchor a minor piece firmly on d5 and bring ...Bf6 to tie White down to defending d4.",
        setup_fen: "r1bq1rk1/pp2bppp/2n1p3/3n4/3P4/2NB1N2/PP3PPP/R1BQR1K1 b - - 5 10",
        search_preset: Some(StructurePreset::IqpBlockaded),
        green_squares: &[Square::D5, Square::F6],
        gold_squares: &[Square::D4],
        arrows: &[(Square::E7, Square::F6), (Square::F6, Square::D4)],
        animated_maneuver: None,
        animated_moves: &["Bf6"],
    },
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::Black,
        plan_number: 2,
        title: "Trade minor pieces toward an IQP endgame",
        summary: "Simplify minor pieces so the d4-pawn loses dynamic backing and becomes an endgame target.",
        setup_fen: "r2q1rk1/1p2bppp/p1n1pn2/3b4/3P4/2NB1N2/PP3PPP/R1BQR1K1 b - - 2 12",
        search_preset: Some(StructurePreset::IqpVsEPawnNoFianchetto),
        green_squares: &[Square::D5, Square::D4],
        gold_squares: &[Square::C3, Square::F3],
        arrows: &[(Square::D5, Square::F3)],
        animated_maneuver: None,
        animated_moves: &["Bxf3", "Qxf3"],
    },
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::Black,
        plan_number: 3,
        title: "Fianchetto ...Bg7 pressure along the dark diagonal",
        summary: "Aim the g7-bishop directly at d4 while Knights maneuver via b4/a5 to control d5.",
        setup_fen: "r1bq1rk1/1p3pbp/p1n1pnp1/8/3P4/1BN2N2/PP3PPP/R1BQR1K1 b - - 2 11",
        search_preset: Some(StructurePreset::IqpVsEPawnFianchetto),
        green_squares: &[Square::D4, Square::D5],
        gold_squares: &[Square::G7, Square::G6],
        arrows: &[(Square::G7, Square::D4), (Square::C6, Square::A5)],
        animated_maneuver: None,
        animated_moves: &["Na5", "Bc2", "b5"],
    },
    StructurePlan {
        family: StructureFamily::Iqp,
        side: Color::Black,
        plan_number: 4,
        title: "IQP vs c-pawn: ...Rc8 & ...Nd5 c-file pressure",
        summary: "With an intact c6-pawn and open e-file, place a knight on d5 and target the queenside.",
        setup_fen: "2rq1rk1/pp1nbppp/2p1bn2/8/3P4/2NB1N2/PP3PPP/R1BQR1K1 b - - 2 12",
        search_preset: Some(StructurePreset::IqpVsCPawnNoFianchetto),
        green_squares: &[Square::D5, Square::C8],
        gold_squares: &[Square::C6, Square::D4],
        arrows: &[(Square::F6, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["Nd5"],
    },
    // --- HANGING PAWNS (7 plans: 4 White, 3 Black) ---
    StructurePlan {
        family: StructureFamily::HangingPawns,
        side: Color::White,
        plan_number: 1,
        title: "Central d4-d5 push with minor-piece backing",
        summary: "Advance d4-d5 when rooks and bishops back the square, creating a passed d-pawn or open lines.",
        setup_fen: "r3r1k1/pbqnbppp/1p2pn2/8/2PP4/2NB1N2/PB2QPPP/R2R2K1 w - - 3 14",
        search_preset: Some(StructurePreset::HangingPawnsVsNoFianchetto),
        green_squares: &[Square::D5],
        gold_squares: &[Square::C4, Square::D4],
        arrows: &[(Square::D4, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["d5"],
    },
    StructurePlan {
        family: StructureFamily::HangingPawns,
        side: Color::White,
        plan_number: 2,
        title: "Kingside attack behind stable c4+d4 pawns",
        summary: "Use the central space gained by c4+d4 to plant Ne5 and swing pieces toward the kingside.",
        setup_fen: "r2qr1k1/pb1nbppp/1p2pn2/8/2PP4/2NB1N2/PB2QPPP/R2R2K1 w - - 2 13",
        search_preset: Some(StructurePreset::HangingPawnsClassic),
        green_squares: &[Square::E5, Square::H7],
        gold_squares: &[Square::C4, Square::D4],
        arrows: &[(Square::F3, Square::E5)],
        animated_maneuver: None,
        animated_moves: &["Ne5"],
    },
    StructurePlan {
        family: StructureFamily::HangingPawns,
        side: Color::White,
        plan_number: 3,
        title: "Advance c4-c5 to fix a queenside wedge",
        summary: "Push c4-c5 when Black leaves b6 loose or c5 uncontested, claiming queenside space.",
        setup_fen: "2rq1rk1/pb1nbppp/1p2pn2/8/2PP4/2NB1N2/PB2QPPP/R2R2K1 w - - 4 14",
        search_preset: Some(StructurePreset::HangingPawnsVsNoFianchetto),
        green_squares: &[Square::C5, Square::B6],
        gold_squares: &[Square::C4, Square::D4],
        arrows: &[(Square::C4, Square::C5)],
        animated_maneuver: None,
        animated_moves: &["c5"],
    },
    StructurePlan {
        family: StructureFamily::HangingPawns,
        side: Color::White,
        plan_number: 4,
        title: "Neutralize K-side fianchetto before pushing d5",
        summary: "Against ...g6+Bg7, guard d4 with Rac1/Qd2 and trade the dark-squared bishop via Bh6 before advancing.",
        setup_fen: "r2qr1k1/pb1n1pbp/1p2pnp1/8/2PP4/2NB1N2/PB1Q1PPP/R3R1K1 w - - 2 13",
        search_preset: Some(StructurePreset::HangingPawnsVsFianchetto),
        green_squares: &[Square::E4, Square::D5],
        gold_squares: &[Square::C4, Square::D4, Square::G7],
        arrows: &[(Square::A1, Square::D1), (Square::D4, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["Rad1", "Rc8", "d5"],
    },
    StructurePlan {
        family: StructureFamily::HangingPawns,
        side: Color::Black,
        plan_number: 1,
        title: "Target c4 with ...Na5 & ...Rc8 to split the pair",
        summary: "Attack the hanging c4-pawn with ...Na5 and ...Rc8 so White must advance or concede a target.",
        setup_fen: "2rq1rk1/pb2bppp/1pn1pn2/8/2PP4/P1NB1N2/1B2QPPP/R2R2K1 b - - 2 14",
        search_preset: Some(StructurePreset::HangingPawnsClassic),
        green_squares: &[Square::A5, Square::C4],
        gold_squares: &[Square::D4],
        arrows: &[(Square::C6, Square::A5), (Square::A5, Square::C4)],
        animated_maneuver: None,
        animated_moves: &["Na5"],
    },
    StructurePlan {
        family: StructureFamily::HangingPawns,
        side: Color::Black,
        plan_number: 2,
        title: "Flank break ...b5 or ...e5 to force a decision",
        summary: "Strike the base of the hanging pair with ...b5 so White is left with an isolated pawn or weak squares.",
        setup_fen: "2rqr1k1/pb2bppp/1pn1pn2/8/2PP4/2NB1N2/PB2QPPP/R2R2K1 b - - 2 14",
        search_preset: Some(StructurePreset::HangingPawnsVsNoFianchetto),
        green_squares: &[Square::B5, Square::C4],
        gold_squares: &[Square::B6, Square::C4, Square::D4],
        arrows: &[(Square::B6, Square::B5)],
        animated_maneuver: None,
        animated_moves: &["b5", "cxb5"],
    },
    StructurePlan {
        family: StructureFamily::HangingPawns,
        side: Color::Black,
        plan_number: 3,
        title: "K-side fianchetto: pile ...Bg7 + ...Rfd8 on d4",
        summary: "Combine ...Bg7, ...Nc6, and ...Rfd8 along the dark diagonal and d-file against d4.",
        setup_fen: "2rq1rk1/pb3pbp/1pn1pnp1/8/2PP4/P1NB1N2/1B2QPPP/2RR2K1 b - - 2 14",
        search_preset: Some(StructurePreset::HangingPawnsVsFianchetto),
        green_squares: &[Square::D4, Square::A5],
        gold_squares: &[Square::G7, Square::C4],
        arrows: &[(Square::G7, Square::D4), (Square::C6, Square::A5)],
        animated_maneuver: None,
        animated_moves: &["Na5"],
    },
    // --- MARÓCZY & HEDGEHOG (7 plans: 3 White, 4 Black) ---
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::White,
        plan_number: 1,
        title: "Maróczy clamp (no K-side fianchetto): restrain d5 & b5",
        summary: "Develop Be2/Be3 and f3 without g3, squeeze Black with c4+e4, and plant a piece on d5.",
        setup_fen: "r2q1rk1/1p2ppbp/p1bp1np1/8/2PBP3/2N2P2/PP1QB1PP/R4RK1 w - - 0 13",
        search_preset: Some(StructurePreset::MaroczyBindNoFianchetto),
        green_squares: &[Square::D5, Square::B5],
        gold_squares: &[Square::C4, Square::E4],
        arrows: &[(Square::C3, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["Nd5"],
    },
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::White,
        plan_number: 2,
        title: "Maróczy with K-side fianchetto (g3+Bg2): control d5",
        summary: "Fianchetto Bg2 and station a dominant knight on the d5 outpost.",
        setup_fen: "r2q1rk1/pp2ppbp/2bp1np1/8/2PBP3/2N3P1/PP3PBP/R2Q1RK1 w - - 1 12",
        search_preset: Some(StructurePreset::MaroczyBindFianchetto),
        green_squares: &[Square::D5],
        gold_squares: &[Square::C4, Square::E4, Square::G2],
        arrows: &[(Square::C3, Square::D5), (Square::G2, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["Nd5"],
    },
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::White,
        plan_number: 3,
        title: "Anti-Hedgehog space grab with f4-f5",
        summary: "Against Black's passive Hedgehog shell, expand on the kingside with f4-f5 to probe e6.",
        setup_fen: "r4rk1/1bqnbppp/pp1ppn2/8/2PQP3/1PN3P1/PB2NPBP/2RR2K1 w - - 4 13",
        search_preset: Some(StructurePreset::MaroczyBindAny),
        green_squares: &[Square::F5, Square::E6],
        gold_squares: &[Square::C4, Square::E4],
        arrows: &[(Square::F2, Square::F4), (Square::F4, Square::F5)],
        animated_maneuver: None,
        animated_moves: &["f4", "Rac8", "f5"],
    },
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::Black,
        plan_number: 1,
        title: "Maróczy queenside break with ...a6 & ...b5",
        summary: "Challenge the c4-clamp directly with ...a6, ...Rb8, and ...b5 to gain queenside counterplay.",
        setup_fen: "1r1q1rk1/1p2ppbp/p1bp1np1/8/2P1P3/2N1BP2/PP1QB1PP/R4RK1 b - - 2 13",
        search_preset: Some(StructurePreset::MaroczyBindNoFianchetto),
        green_squares: &[Square::B5, Square::C4],
        gold_squares: &[Square::A6, Square::D6],
        arrows: &[(Square::B7, Square::B5)],
        animated_maneuver: None,
        animated_moves: &["b5"],
    },
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::Black,
        plan_number: 2,
        title: "Maróczy kingside ...f5 strike",
        summary: "When White locks the center with Bg2, prepare ...Ne8/...Nd7 and strike with ...f7-f5.",
        setup_fen: "r2qnrk1/pp2ppbp/2bp2p1/8/2PBP3/2N3P1/PP3PBP/R2Q1RK1 b - - 2 12",
        search_preset: Some(StructurePreset::MaroczyBindFianchetto),
        green_squares: &[Square::F5, Square::E4],
        gold_squares: &[Square::D6, Square::G7],
        arrows: &[(Square::F7, Square::F5)],
        animated_maneuver: None,
        animated_moves: &["f5"],
    },
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::Black,
        plan_number: 3,
        title: "Trade pieces to ease Maróczy cramp",
        summary: "Swap dark-squared bishops or knights via ...Nxd4 / ...Bxd4 so White's space advantage shrinks.",
        setup_fen: "r2q1rk1/1p2ppbp/p1np1np1/8/2P1P3/2N1BP2/PP1QB1PP/R4RK1 b - - 2 12",
        search_preset: Some(StructurePreset::MaroczyBindNoFianchetto),
        green_squares: &[Square::D4],
        gold_squares: &[Square::C6, Square::G7],
        arrows: &[(Square::C6, Square::D4)],
        animated_maneuver: None,
        animated_moves: &["Nxd4", "Bxd4"],
    },
    StructurePlan {
        family: StructureFamily::MaroczyAndHedgehog,
        side: Color::Black,
        plan_number: 4,
        title: "Hedgehog counterbreaks (...b5 or ...d5)",
        summary: "Coil minor pieces behind a6-b6-d6-e6 and strike dynamically with ...b5 or ...d5.",
        setup_fen: "r4rk1/1bqnbppp/pp1ppn2/8/2PQP3/1PN2NP1/PB3PBP/2RR2K1 b - - 6 13",
        search_preset: Some(StructurePreset::Hedgehog),
        green_squares: &[Square::B5, Square::D5],
        gold_squares: &[Square::A6, Square::B6, Square::D6, Square::E6],
        arrows: &[(Square::B6, Square::B5), (Square::D6, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["b5"],
    },
    // --- SICILIAN STRUCTURES (8 plans: 4 White, 4 Black) ---
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::White,
        plan_number: 1,
        title: "Boleslavsky d5 outpost: Bxf6 Bxf6 Nd5",
        summary: "Eliminate Black's f6-knight guarding d5 with Bxf6 Bxf6, then plant a dominant knight on d5.",
        setup_fen: "r1bq1rk1/3nbppp/pp1p1n2/4p1B1/P3P3/1NN5/1PP1BPPP/R2Q1RK1 w - - 0 11",
        search_preset: Some(StructurePreset::Boleslavsky),
        green_squares: &[Square::D5],
        gold_squares: &[Square::F6, Square::D6],
        arrows: &[(Square::G5, Square::F6), (Square::C3, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["Bxf6", "Bxf6", "Nd5"],
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::White,
        plan_number: 2,
        title: "Najdorf vs ...e5: f4-f5 & d5 pressure",
        summary: "Exploit the backward d5 square and kingside f4-f5 lever before Black stabilizes with ...d5.",
        setup_fen: "2rq1rk1/1b1nbppp/p2p1n2/1p2p3/4PP2/PNN1BB2/1PP3PP/R2Q1RK1 w - - 3 13",
        search_preset: Some(StructurePreset::NajdorfTypeIII),
        green_squares: &[Square::F5, Square::D5],
        gold_squares: &[Square::F4, Square::E5],
        arrows: &[(Square::F4, Square::F5), (Square::C3, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["f5"],
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::White,
        plan_number: 3,
        title: "Scheveningen kingside pawn storm (g4-g5)",
        summary: "Launch g4-g5 against Black's compact d6-e6 small center to drive away the f6-defender.",
        setup_fen: "r1bq1rk1/1p1nbppp/p2ppn2/8/3NP1P1/2N1BP2/PPPQ3P/2KR1B1R w - - 2 11",
        search_preset: Some(StructurePreset::Scheveningen),
        green_squares: &[Square::G5, Square::F6],
        gold_squares: &[Square::G4, Square::D6, Square::E6],
        arrows: &[(Square::G4, Square::G5)],
        animated_maneuver: None,
        animated_moves: &["g5"],
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::White,
        plan_number: 4,
        title: "Dragon Yugoslav attack (h4-h5 & Bh6)",
        summary: "Storm the h-file with h4-h5 and trade off Black's g7-bishop via Bh6.",
        setup_fen: "r1bq1rk1/pp2ppbp/2np1np1/8/3NP2P/2N1BP2/PPPQ2P1/2KR1B1R w - - 1 10",
        search_preset: Some(StructurePreset::Dragon),
        green_squares: &[Square::H5, Square::H6],
        gold_squares: &[Square::G6, Square::G7],
        arrows: &[(Square::H4, Square::H5), (Square::E3, Square::H6)],
        animated_maneuver: None,
        animated_moves: &["h5"],
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::Black,
        plan_number: 1,
        title: "Najdorf classical ...b5 expansion & ...d5 break",
        summary: "Challenge White's central space with queenside ...b5 expansion or a timely ...d6-d5 break.",
        setup_fen: "rnb2rk1/1pq1bppp/p2ppn2/8/P2NPP2/2N1B3/1PP1B1PP/R2Q1RK1 b - - 0 10",
        search_preset: Some(StructurePreset::NajdorfStructure),
        green_squares: &[Square::B5, Square::D5],
        gold_squares: &[Square::A6, Square::D6, Square::E6],
        arrows: &[(Square::B7, Square::B5), (Square::D6, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["b5"],
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::Black,
        plan_number: 2,
        title: "Najdorf queenside ...b5 + ...Bb7 counterplay",
        summary: "Press the c-file and e4 pawn with ...b5 and ...Bb7 in queenside-expanded Najdorf setups.",
        setup_fen: "r1bqkb1r/3n1pp1/p2ppn1p/1p6/3NP1P1/2N1BP2/PPPQ3P/2KR1B1R b kq - 1 10",
        search_preset: Some(StructurePreset::NajdorfTypeII),
        green_squares: &[Square::B7, Square::E4],
        gold_squares: &[Square::B5, Square::A6],
        arrows: &[(Square::C8, Square::B7), (Square::B7, Square::E4)],
        animated_maneuver: None,
        animated_moves: &["Bb7"],
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::Black,
        plan_number: 3,
        title: "Scheveningen central ...d5 liberation",
        summary: "Once the c-file and kingside are stabilized, break in the center with ...d6-d5.",
        setup_fen: "2rq1rk1/1p1bbppp/p1nppn2/8/3NP3/2N1B3/PPP1BPPP/R2Q1RK1 b - - 4 11",
        search_preset: Some(StructurePreset::Scheveningen),
        green_squares: &[Square::D5, Square::E4],
        gold_squares: &[Square::D6, Square::E6],
        arrows: &[(Square::D6, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["d5"],
    },
    StructurePlan {
        family: StructureFamily::Sicilian,
        side: Color::Black,
        plan_number: 4,
        title: "Dragon c-file ...Rc8 & ...Nc4 counterattack",
        summary: "Place a rook on c8 and leap ...Ne5-c4 to target b2 and e3 opposite White's castled king.",
        setup_fen: "2rq1rk1/pp1bppbp/3p1np1/4n3/3NP3/2N1BP2/PPPQ2PP/2KR1B1R b - - 3 11",
        search_preset: Some(StructurePreset::Dragon),
        green_squares: &[Square::C4, Square::C2],
        gold_squares: &[Square::G7, Square::C8],
        arrows: &[(Square::E5, Square::C4)],
        animated_maneuver: None,
        animated_moves: &["Nc4", "Bxc4", "Rxc4"],
    },
    // --- SLAV, TRIANGLE & GRÜNFELD (8 plans: 4 White, 4 Black) ---
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::White,
        plan_number: 1,
        title: "Central e3-e4 break against the Semi-Slav triangle",
        summary: "Consolidate with Bd3/O-O/Re1 and push e3-e4 to open lines against Black's cramped setup.",
        setup_fen: "r1bq1rk1/pp1nbppp/2p1pn2/3p4/2PP4/2NBPN2/PP3PPP/R1BQR1K1 w - - 4 9",
        search_preset: Some(StructurePreset::SlavFormation),
        green_squares: &[Square::E4],
        gold_squares: &[Square::D4, Square::E3],
        arrows: &[(Square::E3, Square::E4)],
        animated_maneuver: None,
        animated_moves: &["e4"],
    },
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::White,
        plan_number: 2,
        title: "Slav / Caro-Kann space gain & minority clamp",
        summary: "Develop actively around d4+c4 while restraining ...c5 and probing the queenside.",
        setup_fen: "r2q1rk1/pp1nbppp/2p1bn2/3p4/2PP4/2N1PN2/PP2BPPP/R1BQ1RK1 w - - 0 9",
        search_preset: Some(StructurePreset::SlavCaroKann),
        green_squares: &[Square::D5, Square::C5],
        gold_squares: &[Square::C4, Square::D4],
        arrows: &[(Square::C4, Square::D5), (Square::D4, Square::C5)],
        animated_maneuver: None,
        animated_moves: &["cxd5", "cxd5"],
    },
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::White,
        plan_number: 3,
        title: "Grünfeld d4-d5 passed-pawn advance",
        summary: "Use the broad d4+e4 pawn center to push d4-d5, seizing space and driving back Black's knight.",
        setup_fen: "r1bq1rk1/pp2ppbp/2n3p1/8/3PP3/2P1BN2/P3BPPP/R2Q1RK1 w - - 4 10",
        search_preset: Some(StructurePreset::GrunfeldCenter),
        green_squares: &[Square::D5],
        gold_squares: &[Square::D4, Square::E4],
        arrows: &[(Square::D4, Square::D5)],
        animated_maneuver: None,
        animated_moves: &["d5"],
    },
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::White,
        plan_number: 4,
        title: "Botvinnik System: Ne2 + kingside f4 break",
        summary: "Guard d4 with c4-d3-e4 while developing Nc3/Ne2/Bg2 and preparing a timed f2-f4 storm.",
        setup_fen: "r2q1rk1/ppp2pbp/2npbnp1/4p3/2P1P3/2NP2P1/PP2NPBP/R1BQ1RK1 w - - 5 9",
        search_preset: Some(StructurePreset::Botvinnik),
        green_squares: &[Square::F4, Square::D5],
        gold_squares: &[Square::C4, Square::D3, Square::E4, Square::E2],
        arrows: &[(Square::F2, Square::F4), (Square::F4, Square::F5)],
        animated_maneuver: None,
        animated_moves: &["f4"],
    },
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::Black,
        plan_number: 1,
        title: "Semi-Slav ...dxc4 and ...b5/...c5 liberation",
        summary: "Hold the d5-c6-e6 triangle until pieces develop, then capture on c4 and expand with ...b5.",
        setup_fen: "r1bqkb1r/pp1n1ppp/2p1pn2/3p4/2PP4/2NBPN2/PP3PPP/R1BQK2R b KQkq - 2 6",
        search_preset: Some(StructurePreset::SlavFormation),
        green_squares: &[Square::C4, Square::B5],
        gold_squares: &[Square::C6, Square::D5, Square::E6],
        arrows: &[(Square::D5, Square::C4), (Square::B7, Square::B5)],
        animated_maneuver: None,
        animated_moves: &["dxc4", "Bxc4", "b5"],
    },
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::Black,
        plan_number: 2,
        title: "Slav / Caro-Kann ...c5 counterstrike",
        summary: "After solving the light-squared bishop via ...Bf5, strike the center with ...c6-c5.",
        setup_fen: "r2q1rk1/pp1nbppp/2p2n2/3p1b2/2PP4/2N1PN2/PP2BPPP/R1BQ1RK1 b - - 4 9",
        search_preset: Some(StructurePreset::SlavCaroKann),
        green_squares: &[Square::C5, Square::D4],
        gold_squares: &[Square::C6, Square::D5],
        arrows: &[(Square::C6, Square::C5)],
        animated_maneuver: None,
        animated_moves: &["dxc4", "Bxc4", "Nb6"],
    },
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::Black,
        plan_number: 3,
        title: "Grünfeld pressure on d4 with ...c5 and ...Bg7",
        summary: "Target the broad e4-d4 pawn center along the dark diagonal while striking d4 with ...cxd4.",
        setup_fen: "r1bq1rk1/pp2ppbp/2n3p1/2p5/3PP3/2P1BN2/P3BPPP/R2Q1RK1 b - - 5 10",
        search_preset: Some(StructurePreset::GrunfeldCenter),
        green_squares: &[Square::D4],
        gold_squares: &[Square::C5, Square::G7],
        arrows: &[(Square::C5, Square::D4), (Square::G7, Square::D4)],
        animated_maneuver: None,
        animated_moves: &["cxd4", "cxd4"],
    },
    StructurePlan {
        family: StructureFamily::SlavAndGrunfeld,
        side: Color::Black,
        plan_number: 4,
        title: "Anti-Botvinnik ...Nd4 outpost & ...c6-b5 play",
        summary: "Exploit the d4 hole left by White's c4-d3-e4 structure with ...Nd4 or queenside ...b5 expansion.",
        setup_fen: "r2q1rk1/pp1bnpbp/2np2p1/4p3/2P1P3/2NP2P1/PP2NPBP/R1BQ1RK1 b - - 2 9",
        search_preset: Some(StructurePreset::Botvinnik),
        green_squares: &[Square::D4, Square::B5],
        gold_squares: &[Square::C6, Square::E5],
        arrows: &[(Square::C6, Square::D4)],
        animated_maneuver: None,
        animated_moves: &["Nd4"],
    },
    // --- CLOSED CHAINS, FRENCH & KID (8 plans: 4 White, 4 Black) ---
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::White,
        plan_number: 1,
        title: "French Chain: kingside space & f4-f5 storm",
        summary: "Defend d4 solidly and launch f2-f4-f5 against Black's castled king along the dark squares.",
        setup_fen: "r2q1rk1/pp1bbppp/2n1p1n1/2ppP3/3P1P2/2PB1N2/PP1N2PP/R1BQR1K1 w - - 2 11",
        search_preset: Some(StructurePreset::FrenchChain),
        green_squares: &[Square::F5, Square::H7],
        gold_squares: &[Square::D4, Square::E5, Square::F4],
        arrows: &[(Square::F4, Square::F5)],
        animated_maneuver: None,
        animated_moves: &["f5"],
    },
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::White,
        plan_number: 2,
        title: "King's Indian Chain: queenside c4-c5 storm",
        summary: "Expand where White's pawns point with c4-c5 to open the c-file and attack d6.",
        setup_fen: "r1bq1rk1/ppp1n1bp/3p1np1/3Pp3/2P1Pp2/2NN1P2/PP1BB1PP/R2Q1RK1 w - - 0 13",
        search_preset: Some(StructurePreset::KingsIndianChain),
        green_squares: &[Square::C5, Square::D6],
        gold_squares: &[Square::C4, Square::D5, Square::E4],
        arrows: &[(Square::C4, Square::C5), (Square::C5, Square::D6)],
        animated_maneuver: None,
        animated_moves: &["c5"],
    },
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::White,
        plan_number: 3,
        title: "Benoni central breakthrough with e4-e5",
        summary: "Coordinate f4, Nf3, and Bd3/Re1 to push e4-e5 and shatter Black's d6-pawn.",
        setup_fen: "r1bq1rk1/1p1nbppp/p2p1n2/2pP4/4PP2/2NB1N2/PP4PP/R1BQR1K1 w - - 2 13",
        search_preset: Some(StructurePreset::BenoniStructure),
        green_squares: &[Square::E5, Square::D6],
        gold_squares: &[Square::D5, Square::E4],
        arrows: &[(Square::E4, Square::E5)],
        animated_maneuver: None,
        animated_moves: &["e5"],
    },
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::White,
        plan_number: 4,
        title: "Nimzo doubled c-pawns: f3 + e4 central roller",
        summary: "Turn the bishop pair and extra central pawn into a decisive e4+f3 kingside and central steamroller.",
        setup_fen: "r1bq1rk1/pp1n1ppp/2p1pn2/3p4/2PP4/2PBPN2/P4PPP/R1BQ1RK1 w - - 0 10",
        search_preset: Some(StructurePreset::NimzoDoubledCPawns),
        green_squares: &[Square::E4, Square::F3],
        gold_squares: &[Square::C3, Square::C4, Square::D4],
        arrows: &[(Square::F2, Square::F3), (Square::E3, Square::E4)],
        animated_maneuver: None,
        animated_moves: &["e4"],
    },
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::Black,
        plan_number: 1,
        title: "French Chain: undermine e5 with ...f6",
        summary: "Strike the head of White's pawn chain with ...f6 (with f6 clear!) alongside ...c5 pressure on d4.",
        setup_fen: "r2q1rk1/pp1bbppp/2n1p1n1/2ppP3/3P4/2PB1N2/PP1N1PPP/R1BQR1K1 b - - 12 10",
        search_preset: Some(StructurePreset::FrenchChain),
        green_squares: &[Square::F6, Square::D4],
        gold_squares: &[Square::C5, Square::E5],
        arrows: &[(Square::F7, Square::F6), (Square::C5, Square::D4)],
        animated_maneuver: None,
        animated_moves: &["f6", "exf6", "Rxf6"],
    },
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::Black,
        plan_number: 2,
        title: "King's Indian Chain: kingside ...f5 attack",
        summary: "Launch ...f7-f5 after ...Ne8/Nd7 to undermine e4 and build a kingside pawn storm.",
        setup_fen: "r1bq1rk1/pppnnpbp/3p2p1/3Pp3/2P1P3/2N1B3/PP2BPPP/R2QNRK1 b - - 4 10",
        search_preset: Some(StructurePreset::KingsIndianChain),
        green_squares: &[Square::F5, Square::F4],
        gold_squares: &[Square::D6, Square::E5, Square::G6],
        arrows: &[(Square::F7, Square::F5), (Square::F5, Square::F4)],
        animated_maneuver: None,
        animated_moves: &["f5"],
    },
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::Black,
        plan_number: 3,
        title: "Benoni queenside ...a6 + ...b5 majority push",
        summary: "Mobilize the 3-vs-2 queenside majority with ...a6 and ...b5, aided by the g7-bishop.",
        setup_fen: "r1bq1rk1/1p1n1pbp/p2p1np1/2pP4/4P3/2N2N2/PP2BPPP/R1BQ1RK1 b - - 0 11",
        search_preset: Some(StructurePreset::BenoniStructure),
        green_squares: &[Square::B5, Square::C4],
        gold_squares: &[Square::A6, Square::C5, Square::D6],
        arrows: &[(Square::B7, Square::B5)],
        animated_maneuver: None,
        animated_moves: &["b5"],
    },
    StructurePlan {
        family: StructureFamily::ClosedChains,
        side: Color::Black,
        plan_number: 4,
        title: "Blockade doubled c-pawns with ...Ba6 & ...Na5",
        summary: "Fix White's doubled c4-pawn and pile ...Ba6, ...Rc8, and ...Na5 onto the static target.",
        setup_fen: "r1bq1rk1/p2n1ppp/1pp1pn2/3p4/2PP4/2PBPN2/P4PPP/R1BQ1RK1 b - - 0 10",
        search_preset: Some(StructurePreset::NimzoDoubledCPawns),
        green_squares: &[Square::A6, Square::C4],
        gold_squares: &[Square::C3, Square::C4],
        arrows: &[(Square::C8, Square::A6), (Square::A6, Square::C4)],
        animated_maneuver: None,
        animated_moves: &["Ba6"],
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
    use crate::chess::{Chess, Fen, Position, San};
    use crate::search::PositionQuery;

    fn material_value(pos: &Chess, color: Color) -> i32 {
        let b = pos.board();
        let count = |role| b.colored_pieces_mask(color, role).count_ones() as i32;
        count(Role::Pawn)
            + count(Role::Knight) * 3
            + count(Role::Bishop) * 3
            + count(Role::Rook) * 5
            + count(Role::Queen) * 9
    }

    #[test]
    fn every_structure_plan_has_valid_balanced_fen_legal_moves_and_matches_its_preset() {
        for fam in StructureFamily::ALL {
            let plans = plans_for_family(fam);
            let white_count = plans.iter().filter(|p| p.side == Color::White).count();
            let black_count = plans.iter().filter(|p| p.side == Color::Black).count();
            assert!(
                white_count >= 2 && black_count >= 2,
                "family {fam:?} must have at least 2 White and 2 Black plans (got {white_count}W/{black_count}B)"
            );
            for plan in plans {
                let fen: Fen = plan
                    .setup_fen
                    .parse()
                    .unwrap_or_else(|e| panic!("invalid FEN for {}: {e}", plan.title));
                let pos: Chess = fen
                    .into_position(crate::chess::CastlingMode::Standard)
                    .unwrap_or_else(|e| panic!("illegal position for {}: {e}", plan.title));

                let w_mat = material_value(&pos, Color::White);
                let b_mat = material_value(&pos, Color::Black);
                assert!(
                    (w_mat - b_mat).abs() <= 1,
                    "unbalanced material ({w_mat} vs {b_mat}) in {}",
                    plan.title
                );

                if let Some(preset) = plan.search_preset {
                    assert!(
                        preset.to_query().compile().matches_position(&pos),
                        "setup_fen for '{}' does not match preset {preset:?}",
                        plan.title
                    );
                    let (_, sqs, clauses) = preset.to_pattern_components();
                    let exported = PositionQuery {
                        squares: sqs,
                        extra_clauses: clauses,
                        ..PositionQuery::default()
                    };
                    assert!(
                        exported.compile().matches_position(&pos),
                        "setup_fen for '{}' does not match exported pattern of {preset:?}",
                        plan.title
                    );
                }

                if !plan.animated_moves.is_empty() {
                    assert_eq!(
                        pos.turn(),
                        plan.side,
                        "setup_fen turn must match plan.side for '{}'",
                        plan.title
                    );
                    let mut cur = pos.clone();
                    for san_str in plan.animated_moves {
                        let san = San::Raw((*san_str).to_string());
                        let mv = san.to_move(&cur).unwrap_or_else(|e| {
                            panic!("illegal move '{san_str}' in '{}': {e}", plan.title)
                        });
                        cur = cur.play_unchecked(mv);
                    }
                }

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
                    for &wp in &m.waypoints[1..] {
                        assert_eq!(
                            pos.board().piece_at(wp),
                            None,
                            "waypoint {wp} is unexpectedly occupied in {}",
                            plan.title
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn boleslavsky_d5_plan_has_bxf6_bxf6_nd5_and_french_f6_is_clear() {
        let sicilian = plans_for_family(StructureFamily::Sicilian);
        let d5_plan = sicilian
            .iter()
            .find(|p| p.search_preset == Some(StructurePreset::Boleslavsky))
            .expect("Boleslavsky d5 plan present");
        assert_eq!(d5_plan.animated_moves, &["Bxf6", "Bxf6", "Nd5"]);

        let closed = plans_for_family(StructureFamily::ClosedChains);
        let french = closed
            .iter()
            .find(|p| {
                p.side == Color::Black && p.search_preset == Some(StructurePreset::FrenchChain)
            })
            .expect("French Black plan present");
        let pos: Chess = french
            .setup_fen
            .parse::<Fen>()
            .unwrap()
            .into_position(crate::chess::CastlingMode::Standard)
            .unwrap();
        assert_eq!(pos.board().piece_at(Square::F6), None);
        assert_eq!(french.animated_moves, &["f6", "exf6", "Rxf6"]);
    }
}
