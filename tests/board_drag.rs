//! Headless egui tests for click and drag-and-drop board input.

use std::cell::RefCell;

use brett::ui::board::{self, BoardEvent, BoardState};
use brett::ui::pieces::PieceTextures;
use egui::{Context, Event, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use shakmaty::{Chess, Color, Square};

const SIDE: f32 = 800.0;

fn raw(time: f64, events: Vec<Event>) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(SIDE, SIDE))),
        time: Some(time),
        events,
        ..Default::default()
    }
}

fn press(pos: Pos2) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::default(),
    }
}

fn release(pos: Pos2) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::default(),
    }
}

fn run_frame(
    ctx: &Context,
    textures: &PieceTextures,
    pos: &Chess,
    drag: &RefCell<Option<Square>>,
    events: &RefCell<Vec<BoardEvent>>,
    input: RawInput,
) {
    let output = ctx.run_ui(input, |ui| {
        let event = board::draw(
            ui,
            textures,
            BoardState {
                pos,
                orientation: Color::White,
                last_move: None,
                selected: *drag.borrow(),
                targets: &[],
                drag_from: *drag.borrow(),
            },
            SIDE,
        );
        if let Some(event) = event {
            events.borrow_mut().push(event);
            match event {
                BoardEvent::DragStart(from) => *drag.borrow_mut() = Some(from),
                BoardEvent::Drop { .. } | BoardEvent::DragCancel | BoardEvent::Click(_) => {
                    *drag.borrow_mut() = None;
                }
            }
        }
    });
    output.drop_without_applying_deltas();
}

fn start() -> (Context, PieceTextures, Chess) {
    let ctx = Context::default();
    let textures = PieceTextures::load(&ctx);
    (ctx, textures, Chess::default())
}

#[test]
fn dragging_e2_to_e4_emits_drag_start_and_drop() {
    let (ctx, textures, pos) = start();
    let drag = RefCell::new(None);
    let events = RefCell::new(Vec::new());

    // e2 and e4 in a white-oriented 800 px board.
    let e2 = Pos2::new(450.0, 650.0);
    let e4 = Pos2::new(450.0, 450.0);

    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.0, vec![Event::PointerMoved(e2)]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.1, vec![Event::PointerMoved(e2), press(e2)]),
    );
    assert!(events.borrow().is_empty());

    // Cross egui's click/drag threshold. The press origin, not the current
    // pointer, must decide which piece is being dragged.
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.2, vec![Event::PointerMoved(Pos2::new(450.0, 550.0))]),
    );
    assert_eq!(
        events.borrow().as_slice(),
        [BoardEvent::DragStart(Square::E2)]
    );

    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.3, vec![Event::PointerMoved(e4)]),
    );
    assert_eq!(events.borrow().len(), 1);

    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.4, vec![Event::PointerMoved(e4), release(e4)]),
    );
    assert_eq!(
        events.borrow().as_slice(),
        [
            BoardEvent::DragStart(Square::E2),
            BoardEvent::Drop {
                from: Square::E2,
                to: Square::E4,
            }
        ]
    );
    assert_eq!(*drag.borrow(), None);
}

#[test]
fn releasing_outside_the_board_cancels_the_drag() {
    let (ctx, textures, pos) = start();
    let drag = RefCell::new(None);
    let events = RefCell::new(Vec::new());

    let e2 = Pos2::new(450.0, 650.0);
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.0, vec![Event::PointerMoved(e2)]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.1, vec![Event::PointerMoved(e2), press(e2)]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.2, vec![Event::PointerMoved(Pos2::new(450.0, 550.0))]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.3, vec![Event::PointerMoved(Pos2::new(900.0, 900.0))]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(
            0.4,
            vec![
                Event::PointerMoved(Pos2::new(900.0, 900.0)),
                release(Pos2::new(900.0, 900.0)),
            ],
        ),
    );

    assert_eq!(
        events.borrow().as_slice(),
        [BoardEvent::DragStart(Square::E2), BoardEvent::DragCancel]
    );
    assert_eq!(*drag.borrow(), None);
}

#[test]
fn a_plain_click_still_selects_a_square() {
    let (ctx, textures, pos) = start();
    let drag = RefCell::new(None);
    let events = RefCell::new(Vec::new());
    let e2 = Pos2::new(450.0, 650.0);

    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.0, vec![Event::PointerMoved(e2)]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.1, vec![Event::PointerMoved(e2), press(e2)]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.2, vec![Event::PointerMoved(e2), release(e2)]),
    );

    assert_eq!(events.borrow().as_slice(), [BoardEvent::Click(Square::E2)]);
}
