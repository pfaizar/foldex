//! Input modality controls focus decoration, never focus ownership or selection.
use egui::{Context, Event, Id, Pos2};

#[derive(Clone, Copy, Default)]
struct InputMode {
    pass: Option<u64>,
    keyboard: bool,
    pointer: Option<Pos2>,
}

impl InputMode {
    fn observe(&mut self, events: &[Event], pixels_per_point: f32) {
        let minimum_movement = 1.0 / pixels_per_point.max(0.1);
        for event in events {
            match event {
                Event::Key { pressed: true, .. } | Event::AccessKitActionRequest(_) => {
                    self.keyboard = true;
                }
                Event::PointerButton { pos, .. } => {
                    self.pointer = Some(*pos);
                    self.keyboard = false;
                }
                Event::PointerMoved(pos) => {
                    if self
                        .pointer
                        .is_none_or(|previous| previous.distance(*pos) >= minimum_movement)
                    {
                        self.keyboard = false;
                        self.pointer = Some(*pos);
                    }
                }
                Event::Touch { .. } | Event::MouseWheel { .. } => self.keyboard = false,
                // Key releases, stationary pointer reports and PointerGone do
                // not change the user's active navigation method.
                _ => {}
            }
        }
    }
}

/// Sample every viewport before widgets consume shortcuts or defer popup focus.
pub(crate) fn install(ctx: &Context) {
    let first = ctx.data_mut(|data| {
        let key = Id::new("foldex-input-mode-hook");
        let installed = data.get_temp::<bool>(key).unwrap_or(false);
        data.insert_temp(key, true);
        !installed
    });
    if first {
        ctx.on_begin_pass(
            "foldex-input-mode",
            std::sync::Arc::new(|ui| {
                keyboard_visible(ui.ctx());
            }),
        );
    }
}

/// Keyboard cues disappear on pointer use without surrendering logical focus.
/// Read the retained raw events: shortcuts may already have been consumed from
/// `InputState::events` by the time a custom widget paints its focus indicator.
pub fn keyboard_visible(ctx: &Context) -> bool {
    let key = Id::new(("foldex-input-mode", ctx.viewport_id()));
    let pass = ctx.cumulative_pass_nr();
    let mut state = ctx
        .data(|data| data.get_temp::<InputMode>(key))
        .unwrap_or_default();
    if state.pass != Some(pass) {
        let pixels_per_point = ctx.pixels_per_point();
        ctx.input(|input| state.observe(&input.raw.events, pixels_per_point));
        state.pass = Some(pass);
        ctx.data_mut(|data| data.insert_temp(key, state));
    }
    state.keyboard && ctx.input(|input| input.raw.focused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Icon, Theme, components};
    use egui::{Key, Modifiers, PointerButton, RawInput, Rect, Shape, pos2, vec2};

    fn key(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn pointer(pressed: bool) -> Event {
        Event::PointerButton {
            pos: pos2(40.0, 24.0),
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    fn context() -> Context {
        let ctx = Context::default();
        Theme {
            reduced_motion: true,
            ..Default::default()
        }
        .apply(&ctx);
        ctx
    }

    struct Frame {
        focused: bool,
        keyboard: bool,
        outlines: Vec<(Rect, f32)>,
        enter_consumed: bool,
    }

    fn button_frame(ctx: &Context, frame: u64, events: Vec<Event>) -> Frame {
        let id = Id::new("modality-button");
        let mut result = Frame {
            focused: false,
            keyboard: false,
            outlines: Vec::new(),
            enter_consumed: false,
        };
        let output = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(320.0, 160.0))),
                time: Some(frame as f64 / 60.0),
                events,
                ..Default::default()
            },
            |ui| {
                components::icon_button_at(
                    ui,
                    Rect::from_min_size(pos2(12.0, 10.0), vec2(100.0, 28.0)),
                    id,
                    &Theme::default(),
                    Icon::More,
                    false,
                );
                result.focused = ctx.memory(|memory| memory.has_focus(id));
                result.keyboard = keyboard_visible(ctx);
                result.enter_consumed = !ctx.input(|input| input.key_pressed(Key::Enter));
            },
        );
        for shape in &output.shapes {
            if let Shape::Rect(rect) = &shape.shape
                && rect.stroke.width > 0.0
            {
                result.outlines.push((rect.rect, rect.stroke.width));
            }
        }
        output.drop_without_applying_deltas();
        result
    }

    #[test]
    fn mouse_click_focus_has_no_keyboard_outline_and_keyboard_activation_has_one() {
        let ctx = context();
        button_frame(&ctx, 0, vec![]);
        button_frame(
            &ctx,
            1,
            vec![Event::PointerMoved(pos2(40.0, 24.0)), pointer(true)],
        );
        let clicked = button_frame(&ctx, 2, vec![pointer(false)]);
        assert!(clicked.focused);
        assert!(!clicked.keyboard && clicked.outlines.is_empty());

        let keyboard = button_frame(&ctx, 3, vec![key(Key::Enter)]);
        assert!(keyboard.focused && keyboard.keyboard && keyboard.enter_consumed);
        assert_eq!(keyboard.outlines.len(), 1);
        assert_eq!(keyboard.outlines[0].1, 1.0);
    }

    #[test]
    fn moving_the_pointer_hides_the_cue_without_dropping_focus_or_keyboard_access() {
        let ctx = context();
        button_frame(&ctx, 0, vec![]);
        button_frame(
            &ctx,
            1,
            vec![Event::PointerMoved(pos2(40.0, 24.0)), pointer(true)],
        );
        button_frame(&ctx, 2, vec![pointer(false)]);
        assert!(button_frame(&ctx, 3, vec![key(Key::Enter)]).keyboard);

        let hovered = button_frame(&ctx, 4, vec![Event::PointerMoved(pos2(42.0, 24.0))]);
        assert!(hovered.focused);
        assert!(!hovered.keyboard && hovered.outlines.is_empty());
        let resumed = button_frame(&ctx, 5, vec![key(Key::Enter)]);
        assert!(resumed.focused && resumed.keyboard);
        assert_eq!(resumed.outlines.len(), 1);
    }

    #[test]
    fn stationary_pointer_reports_and_key_releases_do_not_erase_keyboard_cues() {
        let mut mode = InputMode::default();
        mode.observe(&[Event::PointerMoved(pos2(20.0, 20.0)), key(Key::Tab)], 1.5);
        assert!(mode.keyboard);
        let mut released = key(Key::Tab);
        if let Event::Key { pressed, .. } = &mut released {
            *pressed = false;
        }
        mode.observe(
            &[
                Event::PointerMoved(pos2(20.1, 20.1)),
                Event::PointerGone,
                released,
            ],
            1.5,
        );
        assert!(mode.keyboard);
        mode.observe(&[Event::PointerMoved(pos2(22.0, 20.0))], 1.5);
        assert!(!mode.keyboard);
    }

    #[test]
    fn the_last_real_input_in_a_batch_selects_the_visual_modality() {
        let mut mode = InputMode::default();
        mode.observe(&[key(Key::Tab), pointer(true)], 1.0);
        assert!(!mode.keyboard);
        mode.observe(&[pointer(false), key(Key::Tab)], 1.0);
        assert!(mode.keyboard);
    }

    #[test]
    fn a_search_with_a_containing_border_does_not_paint_an_inner_focus_ring() {
        let ctx = context();
        let id = Id::new("modality-search");
        let mut value = String::new();
        let outer = Rect::from_min_size(pos2(12.0, 10.0), vec2(250.0, 28.0));
        let output = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(320.0, 160.0))),
                events: vec![key(Key::Tab)],
                ..Default::default()
            },
            |ui| {
                ui.memory_mut(|memory| memory.request_focus(id));
                components::search_input_unframed(
                    ui,
                    outer,
                    id,
                    &Theme::default(),
                    &mut value,
                    "Filter files…",
                    128,
                );
                ui.painter().rect_stroke(
                    outer,
                    8,
                    egui::Stroke::new(1.0, Theme::default().accent()),
                    egui::StrokeKind::Inside,
                );
                assert!(keyboard_visible(&ctx));
            },
        );
        let outlines = output
            .shapes
            .iter()
            .filter(|shape| matches!(&shape.shape, Shape::Rect(rect) if rect.stroke.width > 0.0))
            .count();
        assert_eq!(outlines, 1);
        output.drop_without_applying_deltas();
    }
}
