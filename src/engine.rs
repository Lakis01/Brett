//! UCI engine process management.
//!
//! The engine runs in its own threads so starting Stockfish, waiting for
//! `uciok`, and reading analysis output never blocks the egui frame. The UI
//! talks to [`EngineHandle`] through a command channel and receives
//! [`EngineEvent`]s from a single receiver.
//!
//! UCI scores are reported **from the side to move's point of view**. The UI
//! is responsible for converting them to White's perspective.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_MULTIPV: usize = 8;

/// Default Stockfish location, overridable with `BRETT_ENGINE`.
pub fn default_engine_path() -> PathBuf {
    std::env::var_os("BRETT_ENGINE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/stockfish"))
}

/// A score reported by UCI. Values are from the side to move's perspective.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Score {
    /// Centipawns.
    Cp(i32),
    /// Mate in this many moves. Negative means the side to move is mated.
    Mate(i32),
}

/// One `info` line from the engine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineInfo {
    pub depth: u32,
    pub seldepth: Option<u32>,
    /// 1-based MultiPV index.
    pub multipv: usize,
    pub score: Option<Score>,
    /// UCI moves, best first.
    pub pv: Vec<String>,
    pub nodes: Option<u64>,
    pub nps: Option<u64>,
    pub time_ms: Option<u64>,
    pub hashfull: Option<u32>,
}

/// Search termination condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchLimit {
    Depth(u32),
    MoveTime(Duration),
}

impl SearchLimit {
    fn uci(self) -> String {
        match self {
            Self::Depth(depth) => format!("depth {}", depth.max(1)),
            Self::MoveTime(duration) => {
                format!("movetime {}", duration.as_millis().max(1))
            }
        }
    }
}

/// Events produced by the engine worker.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// Engine identification, normally the first event.
    Name(String),
    /// Handshake completed and the engine accepts searches.
    Ready,
    /// A partial search result.
    Info { request: u64, info: EngineInfo },
    /// The search finished. `best` is a UCI move such as `e2e4`.
    BestMove {
        request: u64,
        best: String,
        ponder: Option<String>,
    },
    /// A protocol or process error.
    Error(String),
    /// The engine process ended.
    Exited,
}

enum EngineCommand {
    Analyze {
        request: u64,
        fen: String,
        multipv: usize,
        limit: SearchLimit,
    },
    Stop,
    Quit,
}

/// A running UCI engine.
///
/// Dropping the handle asks the worker to quit the engine, so the child
/// process does not outlive the application.
pub struct EngineHandle {
    path: PathBuf,
    commands: Sender<EngineCommand>,
    events: Receiver<EngineEvent>,
    next_request: u64,
}

impl EngineHandle {
    /// Spawn an engine and begin the UCI handshake asynchronously.
    pub fn spawn(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let mut child = ProcessCommand::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting UCI engine {}", path.display()))?;
        let stdin = child.stdin.take().context("engine has no stdin pipe")?;
        let stdout = child.stdout.take().context("engine has no stdout pipe")?;

        let (commands_tx, commands_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let (lines_tx, lines_rx) = mpsc::channel();

        thread::Builder::new()
            .name("brett-engine-reader".to_string())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            if lines_tx.send(line).is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            let _ = lines_tx.send(format!("__READ_ERROR__ {err}"));
                            break;
                        }
                    }
                }
            })
            .context("spawning engine reader thread")?;

        thread::Builder::new()
            .name("brett-engine-worker".to_string())
            .spawn(move || run_worker(child, stdin, commands_rx, lines_rx, events_tx))
            .context("spawning engine worker thread")?;

        Ok(Self {
            path,
            commands: commands_tx,
            events: events_rx,
            next_request: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Queue a search. Any search currently running is stopped first.
    ///
    /// The returned request id is echoed on every `Info` and `BestMove` event,
    /// so the UI can discard results from an older position.
    pub fn analyze(&mut self, fen: impl Into<String>, multipv: usize, limit: SearchLimit) -> u64 {
        self.next_request = self.next_request.wrapping_add(1).max(1);
        let request = self.next_request;
        let _ = self.commands.send(EngineCommand::Analyze {
            request,
            fen: fen.into(),
            multipv: multipv.clamp(1, MAX_MULTIPV),
            limit,
        });
        request
    }

    /// Stop the current search without starting another one.
    pub fn stop(&self) {
        let _ = self.commands.send(EngineCommand::Stop);
    }

    /// Non-blocking event poll.
    pub fn try_recv(&self) -> Option<EngineEvent> {
        self.events.try_recv().ok()
    }

    /// Blocking event poll with a timeout. Intended for tests and tools.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<EngineEvent> {
        self.events.recv_timeout(timeout).ok()
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(EngineCommand::Quit);
    }
}

fn run_worker(
    mut child: Child,
    mut stdin: ChildStdin,
    commands: Receiver<EngineCommand>,
    lines: Receiver<String>,
    events: Sender<EngineEvent>,
) {
    let mut ready = false;
    let mut searching = false;
    let mut pending: Option<EngineCommand> = None;
    let mut current_request = 0u64;
    let mut handshake_deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut quit = false;

    if !write_line(&mut stdin, "uci") {
        let _ = events.send(EngineEvent::Error(
            "could not send the UCI handshake".to_string(),
        ));
        quit = true;
    }

    loop {
        let mut activity = false;

        loop {
            match lines.try_recv() {
                Ok(line) => {
                    activity = true;
                    if line.starts_with("__READ_ERROR__") {
                        let _ = events.send(EngineEvent::Error(line));
                        quit = true;
                        break;
                    }
                    handle_line(
                        &line,
                        &mut stdin,
                        &mut ready,
                        &mut searching,
                        &mut current_request,
                        &mut pending,
                        &events,
                    );
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    quit = true;
                    break;
                }
            }
        }

        loop {
            match commands.try_recv() {
                Ok(EngineCommand::Quit) => {
                    quit = true;
                    break;
                }
                Ok(EngineCommand::Stop) => {
                    pending = None;
                    if searching {
                        let _ = write_line(&mut stdin, "stop");
                    }
                }
                Ok(command @ EngineCommand::Analyze { .. }) => {
                    if !ready {
                        pending = Some(command);
                    } else if searching {
                        let _ = write_line(&mut stdin, "stop");
                        pending = Some(command);
                    } else {
                        start_command(&mut stdin, &command, &mut searching, &mut current_request);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    quit = true;
                    break;
                }
            }
        }

        if ready
            && !searching
            && let Some(command) = pending.take()
        {
            start_command(&mut stdin, &command, &mut searching, &mut current_request);
        }

        if quit {
            break;
        }
        if !ready && Instant::now() >= handshake_deadline {
            let _ = events.send(EngineEvent::Error(format!(
                "{} did not answer the UCI handshake within {} seconds",
                "engine",
                HANDSHAKE_TIMEOUT.as_secs()
            )));
            break;
        }
        if !activity {
            thread::sleep(Duration::from_millis(4));
        } else {
            handshake_deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        }
    }

    let _ = write_line(&mut stdin, "quit");
    thread::sleep(Duration::from_millis(30));
    let _ = child.kill();
    let _ = child.wait();
    let _ = events.send(EngineEvent::Exited);
}

fn handle_line(
    line: &str,
    stdin: &mut ChildStdin,
    ready: &mut bool,
    searching: &mut bool,
    current_request: &mut u64,
    pending: &mut Option<EngineCommand>,
    events: &Sender<EngineEvent>,
) {
    if line == "uciok" {
        // `uciok` only means the option list is complete. UCI engines must
        // also answer `isready` before a search is queued.
        let _ = write_line(stdin, "isready");
        return;
    }
    if line == "readyok" {
        *ready = true;
        let _ = events.send(EngineEvent::Ready);
        return;
    }
    if let Some(name) = line.strip_prefix("id name ") {
        let _ = events.send(EngineEvent::Name(name.trim().to_string()));
        return;
    }
    if line.starts_with("info ") {
        if *searching && let Some(info) = parse_info(line) {
            let _ = events.send(EngineEvent::Info {
                request: *current_request,
                info,
            });
        }
        return;
    }
    if let Some(rest) = line.strip_prefix("bestmove ") {
        let mut fields = rest.split_whitespace();
        let best = fields.next().unwrap_or("(none)").to_string();
        let ponder = if fields.next() == Some("ponder") {
            fields.next().map(ToOwned::to_owned)
        } else {
            None
        };
        if *searching {
            let _ = events.send(EngineEvent::BestMove {
                request: *current_request,
                best,
                ponder,
            });
            *searching = false;
        }
        if let Some(command @ EngineCommand::Analyze { .. }) = pending.take() {
            start_command(stdin, &command, searching, current_request);
        }
        return;
    }
    if line.starts_with("No such option")
        || line.contains("error")
        || line.starts_with("Unknown command")
    {
        let _ = events.send(EngineEvent::Error(line.to_string()));
    }
}

fn start_command(
    stdin: &mut ChildStdin,
    command: &EngineCommand,
    searching: &mut bool,
    current_request: &mut u64,
) {
    let EngineCommand::Analyze {
        request,
        fen,
        multipv,
        limit,
    } = command
    else {
        return;
    };
    if !write_line(
        stdin,
        &format!(
            "setoption name MultiPV value {}",
            (*multipv).clamp(1, MAX_MULTIPV)
        ),
    ) || !write_line(stdin, &format!("position fen {fen}"))
        || !write_line(stdin, &format!("go {}", limit.uci()))
    {
        *searching = false;
        return;
    }
    *searching = true;
    *current_request = *request;
}

fn write_line(stdin: &mut ChildStdin, line: &str) -> bool {
    writeln!(stdin, "{line}").is_ok() && stdin.flush().is_ok()
}

/// Parse a UCI `info` line. Returns `None` for informational/irrelevant lines.
pub fn parse_info(line: &str) -> Option<EngineInfo> {
    let mut fields = line.split_whitespace();
    if fields.next() != Some("info") {
        return None;
    }
    let mut info = EngineInfo {
        multipv: 1,
        ..EngineInfo::default()
    };
    let mut in_pv = false;
    while let Some(field) = fields.next() {
        if in_pv {
            info.pv.push(field.to_string());
            continue;
        }
        match field {
            "depth" => info.depth = parse_u32(fields.next())?,
            "seldepth" => info.seldepth = parse_u32(fields.next()),
            "multipv" => info.multipv = parse_u32(fields.next())? as usize,
            "nodes" => info.nodes = parse_u64(fields.next()),
            "nps" => info.nps = parse_u64(fields.next()),
            "time" => info.time_ms = parse_u64(fields.next()),
            "hashfull" => info.hashfull = parse_u32(fields.next()),
            "score" => {
                let kind = fields.next()?;
                let value = fields.next()?.parse::<i32>().ok()?;
                info.score = match kind {
                    "cp" => Some(Score::Cp(value)),
                    "mate" => Some(Score::Mate(value)),
                    _ => None,
                };
                // Some engines append `lowerbound`/`upperbound` here. It does
                // not change the numeric value shown to the user.
            }
            "pv" => in_pv = true,
            _ => {}
        }
    }
    if info.pv.is_empty() && info.score.is_none() {
        return None;
    }
    Some(info)
}

fn parse_u32(value: Option<&str>) -> Option<u32> {
    value?.parse().ok()
}

fn parse_u64(value: Option<&str>) -> Option<u64> {
    value?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn parses_centipawn_and_mate_info() {
        let cp = parse_info(
            "info depth 18 seldepth 25 multipv 2 score cp -34 nodes 123456 nps 2000000 time 61 hashfull 12 pv e2e4 e7e5 g1f3",
        )
        .unwrap();
        assert_eq!(cp.depth, 18);
        assert_eq!(cp.seldepth, Some(25));
        assert_eq!(cp.multipv, 2);
        assert_eq!(cp.score, Some(Score::Cp(-34)));
        assert_eq!(cp.pv, ["e2e4", "e7e5", "g1f3"]);
        assert_eq!(cp.nodes, Some(123456));

        let mate = parse_info("info depth 9 score mate 3 pv h5f7").unwrap();
        assert_eq!(mate.score, Some(Score::Mate(3)));
    }

    #[test]
    fn ignores_informational_lines() {
        assert!(parse_info("info string NNUE evaluation using nn.bin").is_none());
        assert!(parse_info("info currmove e2e4 currmovenumber 1").is_none());
    }

    #[test]
    fn search_limits_are_formatted() {
        assert_eq!(SearchLimit::Depth(14).uci(), "depth 14");
        assert_eq!(
            SearchLimit::MoveTime(Duration::from_millis(250)).uci(),
            "movetime 250"
        );
    }

    #[test]
    fn fake_engine_handshake_and_search() {
        let dir = std::env::temp_dir().join(format!(
            "brett-engine-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-engine.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    uci) echo "id name Fake Engine"; echo "uciok" ;;
    isready) echo "readyok" ;;
    go*) echo "info depth 5 seldepth 7 multipv 1 score cp 42 nodes 1000 nps 200000 time 5 pv e2e4 e7e5"; echo "bestmove e2e4 ponder e7e5" ;;
    stop) echo "bestmove e2e4" ;;
    quit) exit 0 ;;
  esac
done
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let mut engine = EngineHandle::spawn(&script).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut ready = false;
        let mut requested = false;
        let mut info = None;
        let mut best = None;
        while Instant::now() < deadline && best.is_none() {
            match engine.recv_timeout(Duration::from_millis(50)) {
                Some(EngineEvent::Ready) => ready = true,
                Some(EngineEvent::Info { info: value, .. }) => info = Some(value),
                Some(EngineEvent::BestMove { best: value, .. }) => best = Some(value),
                Some(EngineEvent::Error(err)) => panic!("engine error: {err}"),
                Some(EngineEvent::Exited) => panic!("fake engine exited early"),
                Some(EngineEvent::Name(_)) => {}
                None => {}
            }
            if ready && !requested {
                engine.analyze(
                    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
                    1,
                    SearchLimit::Depth(5),
                );
                requested = true;
            }
        }
        assert!(ready);
        assert_eq!(info.unwrap().score, Some(Score::Cp(42)));
        assert_eq!(best.as_deref(), Some("e2e4"));
        fs::remove_dir_all(dir).unwrap();
    }
}
