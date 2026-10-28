use std::io::Cursor;

use brett::chess::{San, SanPlus};
use brett::pgn::reader::{Reader, Visitor};
use std::ops::ControlFlow;

#[derive(Default)]
struct Recorder {
    tags: Vec<(String, String)>,
    events: Vec<Event>,
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Tag(String, String),
    San(String),
    Null,
    Nag(u8),
    Comment(String),
    Open,
    Close,
}

type RecordedGame = (Vec<(String, String)>, Vec<Event>);

impl Visitor for Recorder {
    type Tags = Vec<(String, String)>;
    type Movetext = Vec<Event>;
    type Output = RecordedGame;

    fn begin_tags(&mut self) -> ControlFlow<Self::Output, Self::Tags> {
        ControlFlow::Continue(Vec::new())
    }

    fn tag(&mut self, tags: &mut Self::Tags, name: &str, value: &str) -> ControlFlow<Self::Output> {
        tags.push((name.to_string(), value.to_string()));
        self.events
            .push(Event::Tag(name.to_string(), value.to_string()));
        ControlFlow::Continue(())
    }

    fn begin_movetext(&mut self, tags: Self::Tags) -> ControlFlow<Self::Output, Self::Movetext> {
        self.tags = tags;
        ControlFlow::Continue(Vec::new())
    }

    fn san(&mut self, mt: &mut Self::Movetext, san: SanPlus) -> ControlFlow<Self::Output> {
        match san.san {
            San::Null => mt.push(Event::Null),
            _ => mt.push(Event::San(san.text)),
        }
        ControlFlow::Continue(())
    }

    fn nag(&mut self, mt: &mut Self::Movetext, nag: u8) -> ControlFlow<Self::Output> {
        mt.push(Event::Nag(nag));
        ControlFlow::Continue(())
    }

    fn comment(&mut self, mt: &mut Self::Movetext, comment: &str) -> ControlFlow<Self::Output> {
        mt.push(Event::Comment(comment.to_string()));
        ControlFlow::Continue(())
    }

    fn begin_variation(&mut self, mt: &mut Self::Movetext) -> ControlFlow<Self::Output> {
        mt.push(Event::Open);
        ControlFlow::Continue(())
    }

    fn end_variation(&mut self, mt: &mut Self::Movetext) -> ControlFlow<Self::Output> {
        mt.push(Event::Close);
        ControlFlow::Continue(())
    }

    fn end_game(&mut self, mt: Self::Movetext) -> Self::Output {
        (std::mem::take(&mut self.tags), mt)
    }
}

fn read_all(pgn: &[u8]) -> Vec<RecordedGame> {
    let mut reader = Reader::new(Cursor::new(pgn));
    let mut visitor = Recorder::default();
    let mut games = Vec::new();
    while let Some(game) = reader.read_game(&mut visitor).unwrap() {
        games.push(game);
    }
    games
}

#[test]
fn reads_tags_moves_nags_comments_and_variations() {
    let pgn = br#"[Event "Test"]
[White "A"]
[Black "B"]
[Result "*"]

1. e4 e5 2. Nf3 (2. Bc4 Nc6) Nc6 3. Bb5 a6 $1 {main line} 1-0
"#;
    let games = read_all(pgn);
    assert_eq!(games.len(), 1);
    let (_, events) = &games[0];
    assert!(events.contains(&Event::San("e4".into())));
    assert!(events.contains(&Event::San("Nf3".into())));
    assert!(events.contains(&Event::Open));
    assert!(events.contains(&Event::Close));
    assert!(events.contains(&Event::Nag(1)));
    assert!(events.contains(&Event::Comment("main line".into())));
}

#[test]
fn tags_support_escaped_quotes_and_lossy_utf8() {
    let pgn = b"[Event \"A \\\"quote\\\" and \\\\ slash\"]\n[White \"Jos\xe9\"]\n\n1. e4 *\n";
    let games = read_all(pgn);
    assert_eq!(
        games[0].0[0],
        ("Event".to_string(), "A \"quote\" and \\ slash".to_string())
    );
    assert_eq!(games[0].0[1].1, "Jos\u{fffd}");
}

#[test]
fn bracket_inside_comment_is_not_a_new_game() {
    let pgn = br#"[Event "One"]
[Result "*"]

{ Comment with [Event "Not a game"] and a blank line.

Still the same comment. } 1. e4 *

[Event "Two"]
[Result "*"]

1. d4 *
"#;
    let games = read_all(pgn);
    assert_eq!(games.len(), 2);
}

#[test]
fn stream_positions_cover_each_game() {
    let pgn = b"[Event \"One\"]\n\n1. e4 *\n\n[Event \"Two\"]\n\n1. d4 *\n";
    let mut reader = Reader::new(Cursor::new(pgn));
    let mut visitor = Recorder::default();
    let first_start = reader.stream_position();
    assert!(reader.read_game(&mut visitor).unwrap().is_some());
    let first_end = reader.stream_position();
    assert!(reader.read_game(&mut visitor).unwrap().is_some());
    let second_end = reader.stream_position();
    assert!(reader.read_game(&mut visitor).unwrap().is_none());
    assert_eq!(first_start, 0);
    assert!(first_end > first_start);
    assert!(second_end > first_end);
    let first = &pgn[first_start as usize..first_end as usize];
    let second = &pgn[first_end as usize..second_end as usize];
    assert!(first.windows(4).any(|w| w == b"One\""));
    assert!(second.windows(4).any(|w| w == b"Two\""));
}

#[test]
fn header_only_visitor_advances_to_next_game() {
    #[derive(Default)]
    struct Headers;
    impl Visitor for Headers {
        type Tags = Vec<(String, String)>;
        type Movetext = Vec<(String, String)>;
        type Output = Vec<(String, String)>;
        fn begin_tags(&mut self) -> ControlFlow<Self::Output, Self::Tags> {
            ControlFlow::Continue(Vec::new())
        }
        fn tag(
            &mut self,
            tags: &mut Self::Tags,
            name: &str,
            value: &str,
        ) -> ControlFlow<Self::Output> {
            tags.push((name.to_string(), value.to_string()));
            ControlFlow::Continue(())
        }
        fn begin_movetext(
            &mut self,
            tags: Self::Tags,
        ) -> ControlFlow<Self::Output, Self::Movetext> {
            ControlFlow::Continue(tags)
        }
        fn wants_movetext(&self) -> bool {
            false
        }
        fn end_game(&mut self, tags: Self::Movetext) -> Self::Output {
            tags
        }
    }

    let pgn = b"[Event \"One\"]\n\n1. e4 {a} (1. d4) e5 *\n\n[Event \"Two\"]\n\n1. d4 *\n";
    let mut reader = Reader::new(Cursor::new(pgn));
    let mut visitor = Headers;
    let first = reader.read_game(&mut visitor).unwrap().unwrap();
    assert_eq!(first[0].1, "One");
    let second = reader.read_game(&mut visitor).unwrap().unwrap();
    assert_eq!(second[0].1, "Two");
    assert!(reader.read_game(&mut visitor).unwrap().is_none());
}

#[test]
fn null_moves_results_and_glued_move_numbers() {
    let pgn = b"[Event \"Null\"]\n\n1.e4 e5 2. Nf3 -- 3. Nxe5 1-0\n";
    let games = read_all(pgn);
    let (_, events) = &games[0];
    assert_eq!(events.iter().filter(|e| **e == Event::Null).count(), 1);
    assert!(events.contains(&Event::San("e4".into())));
    assert!(events.contains(&Event::San("Nxe5".into())));
}

#[test]
fn unclosed_variation_is_closed_at_eof() {
    let pgn = b"[Event \"Trunc\"]\n\n1. e4 (1. d4 d5\n";
    let games = read_all(pgn);
    let (_, events) = &games[0];
    assert!(events.contains(&Event::Open));
    assert!(events.contains(&Event::Close));
}

#[test]
fn castling_zeroes_glyph_nags_and_ep_markers() {
    let pgn = b"[Event \"Glyphs\"]\n\n1. e4! e5? 2. Nf3!! Nc6?? 3. d4!? exd4?! 4. Nxd4 Bc5 5. Be3 Qf6 6. c3 Nge7 7. Bc4 0-0 8. 0-0-0+ d5 9. exd5 e.p. *\n";
    let games = read_all(pgn);
    let (_, events) = &games[0];
    assert!(events.contains(&Event::Nag(1)));
    assert!(events.contains(&Event::Nag(2)));
    assert!(events.contains(&Event::Nag(3)));
    assert!(events.contains(&Event::Nag(4)));
    assert!(events.contains(&Event::Nag(5)));
    assert!(events.contains(&Event::Nag(6)));
    assert!(events.contains(&Event::San("O-O".into())));
    assert!(events.contains(&Event::San("O-O-O+".into())));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::San(s) if s.contains("e.p.")))
    );
}
