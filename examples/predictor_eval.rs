//! Chronological held-out evaluation for the preparation predictor.
//!
//! Usage:
//! `cargo run --release --example predictor_eval -- <database.pgn> [player] [holdout-games]`
//!
//! The last N games by the selected player are held out. Background statistics
//! are built only from games before that date. The report compares the full
//! model with a frequency-only baseline and an Elo-filtered frequency baseline.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use brett::chess::{Color, Position, UciMove};
use brett::domain::Game;
use brett::evaluation::{Evaluator, Metrics};
use brett::library::PgnLibrary;
use brett::players::{
    self, PlayerBook, PlayerDirectory, PlayerSummary, PredictionConfig, PriorSource,
};
use brett::position::{self, MoveStat};

type MoveAggregate = HashMap<u64, BTreeMap<u32, MoveStat>>;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .context("usage: predictor_eval <database.pgn> [player] [holdout-games]")?;
    let player_query = args.next();
    let holdout = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("holdout-games must be a positive integer")?;

    let library = PgnLibrary::build(path.clone())?;
    let directory = PlayerDirectory::build(&library);
    let player = select_player(&directory, player_query.as_deref())?;
    let player = player.clone();

    let mut player_games: Vec<(usize, u32)> = player
        .game_indices
        .iter()
        .map(|&index| {
            let date = library
                .get(index)
                .and_then(|game| players::parse_date(game.date()))
                .unwrap_or(0);
            (index, date)
        })
        .collect();
    if player_games.len() < 2 {
        bail!("{} has fewer than two games", player.name);
    }
    player_games.sort_unstable_by_key(|&(index, date)| (date, index));

    let test_count = holdout
        .unwrap_or_else(|| (player_games.len() / 5).max(1))
        .clamp(1, player_games.len() - 1);
    let split = player_games.len() - test_count;
    let train_games = &player_games[..split];
    let test_games = &player_games[split..];
    let cutoff = test_games.first().map_or(0, |&(_, date)| date);
    let as_of = if cutoff > 0 {
        cutoff.saturating_sub(1)
    } else {
        players::today()
    };
    let config = PredictionConfig::default();

    let mut training = Vec::new();
    for &(index, _) in train_games {
        training.push(
            library
                .load(index)
                .with_context(|| format!("loading game {index}"))?,
        );
    }
    let book = PlayerBook::from_games(
        player.key.clone(),
        player.name.clone(),
        &training,
        config,
        as_of,
    );
    let frequency_book =
        PlayerBook::from_games("frequency", "Frequency baseline", &[], config, as_of);

    let target_elo = average_player_elo(&training, &player.key);
    let elo_band = target_elo.map(|elo| (elo.saturating_sub(200), elo.saturating_add(200)));
    let test_indices: HashSet<usize> = test_games.iter().map(|&(index, _)| index).collect();

    let mut global = MoveAggregate::new();
    let mut elo_global = MoveAggregate::new();
    for index in 0..library.len() {
        if test_indices.contains(&index) {
            continue;
        }
        let game = match library.load(index) {
            Ok(game) => game,
            Err(err) => {
                eprintln!("skipping game {index}: {err:#}");
                continue;
            }
        };
        let date = players::parse_date(game.date()).unwrap_or(0);
        if cutoff > 0 && date > 0 && date >= cutoff {
            continue;
        }
        add_game(&game, &mut global);
        if let (Some((min, max)), Some(elo)) = (elo_band, average_game_elo(&game))
            && (min..=max).contains(&elo)
        {
            add_game(&game, &mut elo_global);
        }
    }

    let mut model = Evaluator::default();
    let mut frequency = Evaluator::default();
    let mut elo_only = Evaluator::default();
    let mut test_moves = 0usize;
    for &(index, _) in test_games {
        let game = match library.load(index) {
            Ok(game) => game,
            Err(err) => {
                eprintln!("skipping test game {index}: {err:#}");
                continue;
            }
        };
        let color = match players::player_key(game.white()).as_deref() {
            Some(key) if key == player.key => Color::White,
            _ => match players::player_key(game.black()).as_deref() {
                Some(key) if key == player.key => Color::Black,
                _ => continue,
            },
        };
        let mut node = 0usize;
        while let Some(&child) = game.nodes[node].children.first() {
            let parent = &game.nodes[node];
            let Some(ply) = game.nodes[child].mv.as_ref() else {
                break;
            };
            if ply.uci == "0000" {
                break;
            }
            if ply.color == color {
                let Ok(uci) = ply.uci.parse::<UciMove>() else {
                    break;
                };
                let Ok(mv) = uci.to_move(&parent.pos) else {
                    break;
                };
                let Some(actual) = position::encode_move(mv) else {
                    break;
                };
                let key = position::position_key(&parent.pos);
                let legal_moves = parent.pos.legal_moves().len();
                let prior = moves_at(&global, key);
                let prediction = players::predict(
                    &book,
                    key,
                    &prior,
                    PriorSource::CurrentDatabase,
                    legal_moves,
                );
                model.record(&prediction, actual);
                test_moves += 1;

                let baseline = players::predict(
                    &frequency_book,
                    key,
                    &prior,
                    PriorSource::Reference,
                    legal_moves,
                );
                frequency.record(&baseline, actual);

                if elo_band.is_some() {
                    let prior = moves_at(&elo_global, key);
                    let baseline = players::predict(
                        &frequency_book,
                        key,
                        &prior,
                        PriorSource::Reference,
                        legal_moves,
                    );
                    elo_only.record(&baseline, actual);
                }
            }
            node = child;
        }
    }

    println!(
        "Player: {} · {} training games · {} held-out games · {} held-out moves",
        player.name,
        train_games.len(),
        test_games.len(),
        test_moves
    );
    if cutoff == 0 {
        println!(
            "Warning: the held-out split has no usable dates; background filtering is not chronological."
        );
    }
    if let Some(target) = target_elo {
        println!(
            "Elo-only baseline window: {} ±200 (training average)",
            target
        );
    } else {
        println!("Elo-only baseline: unavailable (no ratings in training games)");
    }
    println!();
    print_metrics("model", model.metrics());
    print_metrics("frequency", frequency.metrics());
    if elo_band.is_some() {
        print_metrics("elo-only", elo_only.metrics());
    }

    println!("\nTop-1 calibration:");
    report_calibration("model", &model);
    report_calibration("frequency", &frequency);
    if elo_band.is_some() {
        report_calibration("elo-only", &elo_only);
    }
    Ok(())
}

fn report_calibration(label: &str, evaluator: &Evaluator) {
    println!("  {label}:");
    let bins = evaluator.calibration(10);
    if bins.is_empty() {
        println!("    no cases");
        return;
    }
    println!("    bin       cases  predicted  observed");
    for bin in bins {
        println!(
            "    {:>3.0}-{:<3.0}%  {:>5}  {:>8.1}%  {:>7.1}%",
            bin.lower * 100.0,
            bin.upper * 100.0,
            bin.cases,
            bin.mean_predicted * 100.0,
            bin.observed * 100.0
        );
    }
}

fn select_player<'a>(
    directory: &'a PlayerDirectory,
    query: Option<&str>,
) -> Result<&'a PlayerSummary> {
    let Some(query) = query else {
        return directory
            .players()
            .first()
            .context("the database contains no players");
    };
    let key = players::player_key(query);
    if let Some(key) = key.as_deref()
        && let Some(index) = directory.find_key(key)
        && let Some(player) = directory.get(index)
    {
        return Ok(player);
    }
    let needle = query.to_lowercase();
    directory
        .players()
        .iter()
        .find(|player| player.name.to_lowercase().contains(&needle))
        .context("no player matched the query")
}

fn add_game(game: &Game, stats: &mut MoveAggregate) {
    let mut seen = HashSet::new();
    let mut node = 0usize;
    while let Some(&child) = game.nodes[node].children.first() {
        let parent = &game.nodes[node];
        let Some(ply) = game.nodes[child].mv.as_ref() else {
            break;
        };
        if ply.uci == "0000" {
            break;
        }
        let Ok(uci) = ply.uci.parse::<UciMove>() else {
            break;
        };
        let Ok(mv) = uci.to_move(&parent.pos) else {
            break;
        };
        let Some(code) = position::encode_move(mv) else {
            break;
        };
        let key = position::position_key(&parent.pos);
        if !seen.insert(key) {
            break;
        }
        let entry = stats
            .entry(key)
            .or_default()
            .entry(code)
            .or_insert(MoveStat {
                key,
                mv: code,
                ..MoveStat::default()
            });
        entry.games += 1;
        node = child;
    }
}

fn moves_at(stats: &MoveAggregate, key: u64) -> Vec<MoveStat> {
    stats
        .get(&key)
        .map(|moves| moves.values().copied().collect())
        .unwrap_or_default()
}

fn average_game_elo(game: &Game) -> Option<u32> {
    let white = game.header("WhiteElo").and_then(parse_elo);
    let black = game.header("BlackElo").and_then(parse_elo);
    match (white, black) {
        (Some(white), Some(black)) => Some(((u64::from(white) + u64::from(black)) / 2) as u32),
        (Some(elo), None) | (None, Some(elo)) => Some(elo),
        (None, None) => None,
    }
}

fn average_player_elo(games: &[Game], key: &str) -> Option<u32> {
    let mut sum = 0u64;
    let mut count = 0u32;
    for game in games {
        let tag = if players::player_key(game.white()).as_deref() == Some(key) {
            "WhiteElo"
        } else if players::player_key(game.black()).as_deref() == Some(key) {
            "BlackElo"
        } else {
            continue;
        };
        if let Some(elo) = game.header(tag).and_then(parse_elo) {
            sum += u64::from(elo);
            count += 1;
        }
    }
    (count > 0).then(|| (sum / u64::from(count)) as u32)
}

fn parse_elo(value: &str) -> Option<u32> {
    value.trim().parse().ok().filter(|elo| *elo > 0)
}

fn print_metrics(label: &str, metrics: Metrics) {
    println!(
        "{label:<10} cases {:>4} · log loss {:.4} · Brier {:.4} · top-1 {:.1}% · top-3 {:.1}%",
        metrics.cases,
        metrics.log_loss,
        metrics.brier,
        metrics.top1 * 100.0,
        metrics.top3 * 100.0
    );
}
