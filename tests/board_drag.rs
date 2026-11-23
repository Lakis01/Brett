//! Headless egui tests for click and drag-and-drop board input.

use std::cell::RefCell;

use brett::chess::{Chess, Color, Square};
use brett::domain::ShapeColor;
use brett::ui::board::{self, BoardEvent, BoardState};
use brett::ui::pieces::PieceTextures;
use egui::{Context, Event, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};

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
    press_with(pos, PointerButton::Primary, Modifiers::default())
}

fn release(pos: Pos2) -> Event {
    release_with(pos, PointerButton::Primary, Modifiers::default())
}

fn press_with(pos: Pos2, button: PointerButton, modifiers: Modifiers) -> Event {
    Event::PointerButton {
        pos,
        button,
        pressed: true,
        modifiers,
    }
}

fn release_with(pos: Pos2, button: PointerButton, modifiers: Modifiers) -> Event {
    Event::PointerButton {
        pos,
        button,
        pressed: false,
        modifiers,
    }
}

fn modifiers_changed(modifiers: Modifiers) -> Event {
    Event::ModifiersChanged(modifiers)
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
                shapes: &[],
                drag_from: *drag.borrow(),
                shape_drag_from: None,
                draw_mode: None,
                touch_swipe: false,
            },
            SIDE,
        );
        if let Some(event) = event {
            events.borrow_mut().push(event);
            match event {
                BoardEvent::DragStart(from) => *drag.borrow_mut() = Some(from),
                BoardEvent::Drop { .. }
                | BoardEvent::DragCancel
                | BoardEvent::Click(_)
                | BoardEvent::ShapeSquare { .. }
                | BoardEvent::ShapeArrow { .. }
                | BoardEvent::ShapeDragStart(_)
                | BoardEvent::SwipePrev
                | BoardEvent::SwipeNext => {
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
fn right_click_and_shift_click_emit_coloured_shapes() {
    let (ctx, textures, pos) = start();
    let drag = RefCell::new(None);
    let events = RefCell::new(Vec::new());
    let e4 = Pos2::new(450.0, 450.0);

    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.0, vec![Event::PointerMoved(e4)]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(
            0.1,
            vec![
                Event::PointerMoved(e4),
                press_with(e4, PointerButton::Secondary, Modifiers::default()),
            ],
        ),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(
            0.2,
            vec![
                Event::PointerMoved(e4),
                release_with(e4, PointerButton::Secondary, Modifiers::default()),
            ],
        ),
    );

    let d4 = Pos2::new(350.0, 450.0);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(3.0, vec![Event::PointerMoved(d4)]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(
            3.1,
            vec![
                Event::PointerMoved(d4),
                modifiers_changed(shift),
                press_with(d4, PointerButton::Primary, shift),
            ],
        ),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(
            3.2,
            vec![
                Event::PointerMoved(d4),
                release_with(d4, PointerButton::Primary, shift),
                modifiers_changed(shift),
            ],
        ),
    );

    assert_eq!(
        events.borrow().as_slice(),
        [
            BoardEvent::ShapeSquare {
                square: Square::E4,
                color: ShapeColor::Green,
            },
            BoardEvent::ShapeSquare {
                square: Square::D4,
                color: ShapeColor::Red,
            },
        ]
    );
}

#[test]
fn secondary_drag_draws_an_arrow_instead_of_moving() {
    let (ctx, textures, pos) = start();
    let drag = RefCell::new(None);
    let events = RefCell::new(Vec::new());
    let g1 = Pos2::new(650.0, 750.0);
    let f3 = Pos2::new(550.0, 550.0);

    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.0, vec![Event::PointerMoved(g1)]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(
            0.1,
            vec![
                Event::PointerMoved(g1),
                press_with(g1, PointerButton::Secondary, Modifiers::default()),
            ],
        ),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(0.2, vec![Event::PointerMoved(f3)]),
    );
    run_frame(
        &ctx,
        &textures,
        &pos,
        &drag,
        &events,
        raw(
            0.3,
            vec![
                Event::PointerMoved(f3),
                release_with(f3, PointerButton::Secondary, Modifiers::default()),
            ],
        ),
    );

    assert_eq!(
        events.borrow().as_slice(),
        [BoardEvent::ShapeArrow {
            from: Square::G1,
            to: Square::F3,
            color: ShapeColor::Green,
        }]
    );
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
