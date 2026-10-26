//! Shared native interaction and geometry primitives.
use crate::{Icon, Theme, animated_state, icon, motion::MotionProfile};
use egui::{
    Id, Key, Modifiers, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};

pub const CONTROL: f32 = 28.0;
pub const ICON: f32 = 16.0;
pub const INSET: f32 = 6.0;
pub const GAP: f32 = 4.0;
pub const POPUP_HEADER: f32 = 38.0;
pub fn menu_row_height(t: &Theme) -> f32 {
    CONTROL * (t.font_size / 14.0).max(1.0)
}

/// A quiet overflow hint, shared by searchable result menus. `height` and the
/// parent child's max rect must describe the settled viewport; animate its clip.
pub fn menu_scroll_area(
    ui: &mut Ui,
    t: &Theme,
    salt: impl egui::AsIdSalt,
    height: f32,
) -> egui::ScrollArea {
    let scale = t.metrics().scale;
    // egui queues even a zero-duration scroll animation, and constructs the
    // next pass's content geometry before applying that target. Disable the
    // animation itself for reduced/direct motion so a reveal is visible on the
    // next pass, without requiring another otherwise-idle frame to settle it.
    let animated = !crate::motion::direct_layout(ui.ctx()) && t.duration() > 0.0;
    let style = &mut ui.spacing_mut().scroll;
    style.floating = true;
    style.bar_width = 8.0 * scale;
    style.floating_width = 2.0 * scale;
    style.floating_allocated_width = 4.0 * scale;
    style.dormant_handle_opacity = 0.45;
    style.active_handle_opacity = 0.7;
    style.foreground_color = true;
    egui::ScrollArea::vertical()
        .id_salt(salt)
        .animated(animated)
        .auto_shrink([false, false])
        .min_scrolled_height(1.0)
        .max_height(height.max(1.0))
}

/// Reserve the scrollbar slot on the first frame exactly when real content
/// exceeds the settled viewport. egui's automatic policy uses last-frame
/// overflow to reserve width, which otherwise shifts rows during opening and
/// filtering even when the viewport itself is already stable.
pub fn menu_scroll_area_for_content(
    ui: &mut Ui,
    t: &Theme,
    salt: impl egui::AsIdSalt,
    height: f32,
    content_height: f32,
) -> egui::ScrollArea {
    menu_scroll_area(ui, t, salt, height).scroll_bar_visibility(
        if content_height > height.max(1.0).ceil() {
            egui::scroll_area::ScrollBarVisibility::AlwaysVisible
        } else {
            egui::scroll_area::ScrollBarVisibility::AlwaysHidden
        },
    )
}

/// Reveal a virtual row using the actual scroll viewport, including short or
/// still-opening menus. In-view selection changes leave the scroll position alone.
pub fn reveal_menu_row(ui: &Ui, t: &Theme, row: usize, first: usize, row_height: f32) {
    let top = ui.max_rect().top() + (row as f32 - first as f32) * row_height;
    let rect = Rect::from_min_size(pos2(ui.max_rect().left(), top), vec2(1.0, row_height));
    let animation = if crate::motion::direct_layout(ui.ctx()) {
        egui::style::ScrollAnimation::none()
    } else {
        egui::style::ScrollAnimation::duration(t.duration())
    };
    ui.scroll_to_rect_animation(rect, None, animation);
}

pub fn set_overlay_active(ctx: &egui::Context, active: bool) {
    let (time, pointer_busy) = ctx.input(|input| {
        (
            input.time,
            input.pointer.any_down()
                || input.pointer.any_pressed()
                || input.pointer.any_released()
                || input.smooth_scroll_delta != egui::Vec2::ZERO,
        )
    });
    let resizing = crate::motion::interactive_resize(ctx);
    let remaining = ctx.data_mut(|data| {
        let key = Id::new("foldex-overlay-active");
        let previous = data.get_temp::<bool>(key).unwrap_or(false);
        data.insert_temp(key, active);
        let deadline = Id::new("foldex-tooltip-after");
        let mut after = data.get_temp::<f64>(deadline).unwrap_or(0.0);
        if previous && !active || pointer_busy || resizing {
            after = time + 0.4;
            data.insert_temp(deadline, after);
        }
        data.insert_temp(
            Id::new("foldex-tooltips-blocked"),
            active || pointer_busy || resizing || time < after,
        );
        if !active { after - time } else { 0.0 }
    });
    if remaining > 0.0 {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(remaining));
    }
}
pub fn quiet_tooltips(ctx: &egui::Context) {
    let after = ctx.input(|input| input.time) + 0.4;
    ctx.data_mut(|data| data.insert_temp(Id::new("foldex-tooltip-after"), after));
    ctx.request_repaint_after(std::time::Duration::from_millis(400));
}
pub fn tooltips_blocked(ctx: &egui::Context) -> bool {
    let time = ctx.input(|input| input.time);
    crate::motion::direct_layout(ctx)
        || ctx.data(|data| {
            data.get_temp::<bool>(Id::new("foldex-tooltips-blocked"))
                .unwrap_or(false)
                || time
                    < data
                        .get_temp::<f64>(Id::new("foldex-tooltip-after"))
                        .unwrap_or(0.0)
        })
}
pub fn overlay_active(ctx: &egui::Context) -> bool {
    ctx.data(|data| {
        data.get_temp::<bool>(Id::new("foldex-overlay-active"))
            .unwrap_or(false)
    })
}

/// Block a covered surface without dimming it. Restore the alpha policy so
/// controls that are unavailable in their own right still retain that state.
pub fn disable_interaction(ui: &mut Ui) {
    let alpha = ui.visuals().disabled_alpha;
    ui.visuals_mut().disabled_alpha = 1.0;
    ui.disable();
    ui.visuals_mut().disabled_alpha = alpha;
}

pub fn popup_surface<R>(
    ctx: &egui::Context,
    area: egui::Area,
    interactive: bool,
    content: impl FnOnce(&mut Ui) -> R,
) -> R {
    popup_surface_with_backdrop(ctx, area, interactive, true, content)
}
pub fn popup_surface_with_backdrop<R>(
    ctx: &egui::Context,
    area: egui::Area,
    interactive: bool,
    backdrop: bool,
    content: impl FnOnce(&mut Ui) -> R,
) -> R {
    let area = area
        .kind(egui::UiKind::Modal)
        .sense(Sense::hover())
        .movable(false)
        .constrain(false);
    if interactive && backdrop {
        egui::Modal {
            area,
            backdrop_color: egui::Color32::TRANSPARENT,
            frame: Some(egui::Frame::NONE),
        }
        .show(ctx, content)
        .inner
    } else {
        area.show(ctx, |ui| {
            if !interactive {
                disable_interaction(ui);
            }
            content(ui)
        })
        .inner
    }
}

pub fn can_focus(ui: &Ui) -> bool {
    ui.is_enabled() && ui.memory(|memory| memory.allows_interaction(ui.layer_id()))
}

/// Consume an exact chord. egui's `consume_key(NONE, ...)` also matches Alt and
/// Shift, which would steal access keys and editor shortcuts from their owner.
pub fn consume_key(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> bool {
    ctx.input_mut(|input| {
        let mut consumed = false;
        input.events.retain(|event| {
            let matched = matches!(event, egui::Event::Key {
                key: event_key, pressed: true, modifiers: event_modifiers, ..
            } if *event_key == key && *event_modifiers == modifiers);
            consumed |= matched;
            !matched
        });
        consumed
    })
}

/// Shared button interaction, including consumption of a keyboard click before
/// it can activate a newly opened popup or a focus-return target in this pass.
/// Pointer and AccessKit clicks retain egui's normal behavior. Callers that need
/// a composite row should use `Sense::CLICK` instead and let its owner handle keys.
pub fn button_response(ui: &Ui, rect: Rect, id: Id) -> Response {
    let mut response = ui.interact(rect, id, Sense::click());
    if response.enabled() && response.has_focus() {
        // egui 0.36 synthesizes clicks for *all* Enter/Space chords and does not
        // consume them (context.rs::get_response). Correct only that flag; keep
        // genuine pointer clicks and assistive-technology activation intact.
        let (key_pressed, accessible_click) = ui.input(|input| {
            (
                input.key_pressed(Key::Enter) || input.key_pressed(Key::Space),
                input.has_accesskit_action_request(id, egui::accesskit::Action::Click),
            )
        });
        if key_pressed {
            let enter = consume_key(ui.ctx(), Modifiers::NONE, Key::Enter);
            let space = consume_key(ui.ctx(), Modifiers::NONE, Key::Space);
            response.flags.set(
                egui::response::Flags::FAKE_PRIMARY_CLICKED,
                enter || space || accessible_click,
            );
            if space {
                // Some integrations report the printable space as well.
                ui.input_mut(|input| {
                    input
                        .events
                        .retain(|event| !matches!(event, egui::Event::Text(text) if text == " "));
                });
            }
        }
    }
    if response.clicked() && !response.has_focus() {
        response.request_focus();
    }
    register_focus_target(ui, rect, id);
    response
}

/// Give an existing native/custom widget a contextual accessible name without
/// replacing its native role, numeric value, range, or supported actions.
pub fn label_control(response: &Response, label: &str) {
    response
        .ctx
        .accesskit_node_builder(response.id, |node| node.set_label(label));
}

/// A native slider with Foldex's focus outline and exact Home/End endpoints.
/// The native editable value keeps its own Tab stop and text/IME handling.
pub fn slider<T: egui::emath::Numeric>(
    ui: &mut Ui,
    t: &Theme,
    label: &str,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    step: f64,
) -> Response {
    let start = *range.start();
    let end = *range.end();
    let mut response = ui.add(egui::Slider::new(value, range).step_by(step));
    label_control(&response, label);
    if response.has_focus() && ui.ctx().output(|output| output.ime.is_none()) {
        let endpoint = if consume_key(ui.ctx(), Modifiers::NONE, Key::Home) {
            Some(start)
        } else if consume_key(ui.ctx(), Modifiers::NONE, Key::End) {
            Some(end)
        } else {
            None
        };
        if let Some(endpoint) = endpoint {
            if value.to_f64() != endpoint.to_f64() {
                *value = endpoint;
                response.mark_changed();
            }
            response
                .widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), value.to_f64(), label));
        }
        // The native slider reads arrow presses without consuming them.
        consume_key(ui.ctx(), Modifiers::NONE, Key::ArrowLeft);
        consume_key(ui.ctx(), Modifiers::NONE, Key::ArrowRight);
    }
    focus_ring(ui, response.rect, response.id, t, response.has_focus());
    response
}

#[derive(Clone, Default)]
struct FocusTargets {
    pass: u64,
    ids: egui::IdSet,
}

/// Register an existing control/composite as a valid popup return target for
/// this pass. Call for native widgets too; registration adds no tab stop.
pub fn register_focus_target(ui: &Ui, rect: Rect, id: Id) {
    if !rect.is_positive() || !rect.intersects(ui.clip_rect()) {
        return;
    }
    let ctx = ui.ctx();
    let pass = ctx.cumulative_pass_nr();
    let key = Id::new(("focus-return-targets", ctx.viewport_id()));
    ctx.data_mut(|data| {
        let targets = data.get_temp_mut_or_default::<FocusTargets>(key);
        if targets.pass != pass {
            targets.pass = pass;
            targets.ids.clear();
        }
        if targets.ids.len() < 2048 {
            targets.ids.insert(id);
        }
    });
}

fn focus_target_present(ctx: &egui::Context, id: Id) -> bool {
    let pass = ctx.cumulative_pass_nr();
    let key = Id::new(("focus-return-targets", ctx.viewport_id()));
    ctx.data_mut(|data| {
        let targets = data.get_temp_mut_or_default::<FocusTargets>(key);
        targets.pass == pass && targets.ids.contains(&id)
    })
}

#[derive(Clone)]
struct FocusReturn {
    target: Id,
    through_frame: u64,
}

/// Restore a visible invoking control across egui's closing modal-layer grace
/// pass. A newly focused control cancels the deferred restoration.
pub fn return_focus(ctx: &egui::Context, target: Id) {
    let install = ctx.data_mut(|data| {
        let key = Id::new("foldex-focus-return-hook");
        let installed = data.get_temp::<bool>(key).unwrap_or(false);
        data.insert_temp(key, true);
        !installed
    });
    if install {
        ctx.on_end_pass(
            "foldex-focus-return",
            std::sync::Arc::new(|ui| {
                let ctx = ui.ctx();
                let key = Id::new(("foldex-focus-return", ctx.viewport_id()));
                let pending = ctx.data(|data| data.get_temp::<FocusReturn>(key));
                if let Some(pending) = pending {
                    let focused = ctx.memory(|memory| memory.focused());
                    let present = focus_target_present(ctx, pending.target);
                    if present && (focused.is_none() || focused == Some(pending.target)) {
                        if focused.is_none() {
                            ctx.memory_mut(|memory| memory.request_focus(pending.target));
                        }
                    } else {
                        ctx.data_mut(|data| data.remove::<FocusReturn>(key));
                        return;
                    }
                    if ctx.cumulative_frame_nr() >= pending.through_frame {
                        ctx.data_mut(|data| data.remove::<FocusReturn>(key));
                    }
                }
            }),
        );
    }
    // egui retains a modal layer for one pass after it disappears, and creating
    // a covered native control surrenders its focus. Reapply at the end of this
    // pass and the next frame, only if no new control has taken focus. This also
    // supports callers that draw the return target after the popup.
    let key = Id::new(("foldex-focus-return", ctx.viewport_id()));
    let through_frame = ctx.cumulative_frame_nr() + 1;
    ctx.data_mut(|data| {
        data.insert_temp(
            key,
            FocusReturn {
                target,
                through_frame,
            },
        )
    });
    consume_key(ctx, Modifiers::NONE, Key::Enter);
    consume_key(ctx, Modifiers::NONE, Key::Space);
    ctx.memory_mut(|memory| memory.request_focus(target));
    ctx.request_repaint();
}

/// A popup's single keyboard owner. Call `update(true)` before handling input,
/// draw `surface` in the popup, and call `update(false)` whenever it closes,
/// including its final frame. Route keys only when `owns_keyboard` is true.
/// Nested popups capture and return to their parent owner automatically. Tab is
/// locked so the owner can close the popup and restore its trigger explicitly.
#[derive(Default)]
pub struct PopupFocus {
    owns: bool,
    previous: Option<Id>,
}
impl PopupFocus {
    pub fn owns_keyboard(&self, ctx: &egui::Context, id: Id) -> bool {
        self.owns
            && ctx.memory(|memory| {
                memory.has_focus(id)
                    || (memory.focused().is_none() && memory.had_focus_last_frame(id))
            })
    }

    /// Override the return target for a popup invoked through an app shortcut.
    /// The target must register itself in the pass in which restoration occurs.
    pub fn set_return_focus(&mut self, id: Id) {
        self.previous = Some(id);
    }

    pub fn update(&mut self, ctx: &egui::Context, id: Id, active: bool) {
        if active {
            if !self.owns {
                self.previous = self
                    .previous
                    .or_else(|| ctx.memory(|memory| memory.focused()));
                ctx.memory_mut(|memory| memory.request_focus(id));
                self.owns = true;
                ctx.request_repaint();
            }
            if ctx.memory(|memory| memory.focused().is_none()) {
                // Area's first sizing pass is invisible and disabled, so its
                // surface registration surrenders focus. The second update,
                // after rendering, must retain ownership for the visible pass.
                ctx.memory_mut(|memory| memory.request_focus(id));
                ctx.request_repaint();
            }
            ctx.memory_mut(|memory| {
                if memory.has_focus(id) {
                    // The first focused pass cannot yet install an egui lock
                    // filter. Clear its queued spatial/Tab navigation too.
                    memory.move_focus(egui::FocusDirection::None);
                }
                memory.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        } else if self.owns {
            // read_response also finds last-pass widgets. Restore only a target
            // drawn in this pass: a resized-away trigger has no AccessKit node.
            let previous = self
                .previous
                .filter(|previous| focus_target_present(ctx, *previous));
            if ctx.memory(|memory| memory.focused().is_none() || memory.focused() == Some(id)) {
                if let Some(previous) = previous {
                    return_focus(ctx, previous);
                } else {
                    ctx.memory_mut(|memory| memory.surrender_focus(id));
                }
            }
            self.owns = false;
            self.previous = None;
        }
    }
    pub fn surface(ui: &Ui, rect: Rect, id: Id, label: &str) -> Response {
        let response = ui.interact(rect, id, Sense::focusable_noninteractive());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, ui.is_enabled(), label)
        });
        register_focus_target(ui, rect, id);
        response
    }
}

#[derive(Clone, Default)]
struct DialogFocus {
    previous: Option<Id>,
    active: bool,
}

impl DialogFocus {
    fn begin(&mut self, ctx: &egui::Context) {
        if !self.active {
            self.previous = ctx.memory(|memory| memory.focused());
            self.active = true;
        }
    }

    fn contain(&self, ctx: &egui::Context, layer: egui::LayerId, fallback: Id) {
        let focused = ctx.memory(|memory| memory.focused());
        // A text editor or a nested popup can choose its own initial focus.
        // Do not request it again: request_focus interrupts IME composition.
        let inside = focused
            .and_then(|id| ctx.read_response(id))
            .is_some_and(|response| {
                response.layer_id == layer
                    || (Some(response.id) != self.previous
                        && ctx.memory(|memory| memory.top_modal_layer() == Some(response.layer_id)))
            });
        if !inside {
            ctx.memory_mut(|memory| {
                memory.move_focus(egui::FocusDirection::None);
                memory.interested_in_focus(fallback, layer);
                memory.request_focus(fallback);
            });
        }
    }

    fn restore(&mut self, ctx: &egui::Context, layer: egui::LayerId) {
        let focused = ctx.memory(|memory| memory.focused());
        let owns = focused.is_none()
            || focused
                .and_then(|id| ctx.read_response(id))
                .is_some_and(|r| r.layer_id == layer);
        if self.active && owns {
            let previous = self.previous.filter(|id| focus_target_present(ctx, *id));
            // A closing key must not click the restored trigger later this pass.
            consume_key(ctx, Modifiers::NONE, Key::Enter);
            consume_key(ctx, Modifiers::NONE, Key::Space);
            if let Some(previous) = previous {
                return_focus(ctx, previous);
            } else if let Some(focused) = focused {
                ctx.memory_mut(|memory| memory.surrender_focus(focused));
            }
        }
        self.active = false;
        self.previous = None;
    }
}

#[derive(Clone, Default)]
struct DialogMotion {
    frame: u64,
    opacity: f32,
    height: f32,
    measured: f32,
    closing: bool,
}

pub fn close_dialog(ctx: &egui::Context, id: Id) {
    ctx.data_mut(|data| {
        if let Some(mut motion) = data.get_temp::<DialogMotion>(id.with("entry-motion")) {
            motion.closing = true;
            data.insert_temp(id.with("entry-motion"), motion);
        }
    });
    ctx.request_repaint();
}

/// A caller can release native resources as soon as dismissal starts while the
/// modal layer continues to shield covered controls through its closing motion.
pub fn dialog_is_closing(ctx: &egui::Context, id: Id) -> bool {
    ctx.data(|data| {
        data.get_temp::<DialogMotion>(id.with("entry-motion"))
            .is_some_and(|motion| motion.closing)
    })
}

/// Shared modal geometry. Measurement uses the available screen, independently
/// of the animated clip, so asynchronous content can grow beyond its first frame.
pub fn dialog<R>(
    ctx: &egui::Context,
    id: Id,
    t: &Theme,
    title: &str,
    desired_width: f32,
    open: &mut bool,
    content: impl FnOnce(&mut Ui) -> R,
) -> R {
    dialog_interactive(ctx, id, t, title, desired_width, open, true, content)
}

/// A parent stays visually stable while its nested surface owns interaction.
pub fn dialog_interactive<R>(
    ctx: &egui::Context,
    id: Id,
    t: &Theme,
    title: &str,
    desired_width: f32,
    open: &mut bool,
    interactive: bool,
    content: impl FnOnce(&mut Ui) -> R,
) -> R {
    let m = t.metrics();
    let screen = ctx.content_rect();
    let width = desired_width.min((screen.width() - 32.0 * m.scale).max(120.0));
    let max_height =
        (screen.height() - 64.0 * m.scale).max(m.popup_header + m.control + 24.0 * m.scale);
    let max_body = (max_height - m.popup_header - 24.0 * m.scale).max(m.control);
    let frame = ctx.cumulative_frame_nr();
    let key = id.with("entry-motion");
    let mut motion = ctx
        .data(|data| data.get_temp::<DialogMotion>(key))
        .unwrap_or_default();
    let new_open = motion.frame == 0
        || motion.frame + 1 < frame
        || (motion.closing && motion.height <= 0.25 && motion.opacity <= 0.01);
    if new_open {
        motion.opacity = 0.0;
        motion.height = 0.0;
        motion.closing = false;
    }
    let focus_key = id.with("dialog-focus");
    let mut focus = ctx
        .data(|data| data.get_temp::<DialogFocus>(focus_key))
        .unwrap_or_default();
    if motion.frame + 1 < frame {
        focus = DialogFocus::default();
    }
    if !focus.active {
        focus.begin(ctx);
        // The invocation key belongs to the trigger, even if it was a native
        // egui control rather than one of Foldex's shared buttons.
        consume_key(ctx, Modifiers::NONE, Key::Enter);
        consume_key(ctx, Modifiers::NONE, Key::Space);
    }
    if motion.frame != frame {
        let gains = crate::motion::popup_gains(ctx, t.duration());
        let fade = gains.step_normalized(
            &mut motion.opacity,
            if motion.closing { 0.0 } else { 1.0 },
            MotionProfile::Standard,
        );
        let resize = gains.step_pixels(
            &mut motion.height,
            if motion.closing {
                0.0
            } else {
                motion.measured.min(max_height)
            },
            MotionProfile::Standard,
        );
        if fade || resize {
            ctx.request_repaint();
        }
        motion.frame = frame;
    }
    let outer = Rect::from_center_size(
        screen.center() - vec2(0.0, 4.0 * m.scale * (1.0 - motion.opacity)),
        vec2(width, motion.height),
    );
    let mut dismiss = false;
    let area = egui::Area::new(id)
        .kind(egui::UiKind::Modal)
        .order(egui::Order::Foreground)
        .sense(Sense::hover())
        .movable(false)
        .fixed_pos(outer.min)
        .fade_in(false)
        .constrain(false);
    let layer = area.layer();
    let render = |ui: &mut Ui| {
        ui.ctx().accesskit_node_builder(ui.id(), |node| {
            node.set_role(egui::accesskit::Role::Dialog);
            node.set_label(title);
            node.set_modal();
        });
        ui.set_min_size(outer.size());
        ui.set_max_size(outer.size());
        ui.set_clip_rect(outer.intersect(screen));
        ui.set_opacity(motion.opacity);
        if motion.closing || !interactive {
            disable_interaction(ui);
        }
        ui.painter()
            .rect_filled(outer, t.radius.saturating_add(3), Theme::color(t.surface));
        ui.painter().rect_stroke(
            outer,
            t.radius.saturating_add(3),
            Stroke::new(1.0, t.line()),
            StrokeKind::Inside,
        );
        let header = Rect::from_min_size(outer.min, vec2(width, m.popup_header));
        let close = popup_action_rect(header, 0, 1);
        elided_label(
            ui.painter(),
            Rect::from_min_max(
                header.min + vec2(12.0 * m.scale, 0.0),
                pos2(close.left() - m.gap, header.bottom()),
            ),
            t,
            title,
            t.fg(),
            false,
        );
        dismiss = icon_button_at(ui, close, id.with("close"), t, Icon::Close, false)
            .on_hover_text("Close")
            .clicked();
        ui.painter().hline(
            header.x_range(),
            header.bottom(),
            Stroke::new(1.0, t.line()),
        );
        let origin = pos2(
            outer.left() + 12.0 * m.scale,
            header.bottom() + 12.0 * m.scale,
        );
        let mut body = ui.new_child(UiBuilder::new().id_salt(id.with("content")).max_rect(
            Rect::from_min_size(origin, vec2((width - 24.0 * m.scale).max(1.0), max_body)),
        ));
        let clip = Rect::from_min_max(
            origin,
            pos2(
                outer.right() - 12.0 * m.scale,
                (outer.bottom() - 12.0 * m.scale).max(origin.y),
            ),
        );
        body.set_clip_rect(clip.intersect(outer).intersect(screen));
        body.spacing_mut().item_spacing = vec2(m.gap, m.gap);
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(id.with("body"))
            .max_height(max_body)
            .auto_shrink([false, true]);
        if new_open {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        let output = scroll.show(&mut body, content);
        let measured = m.popup_header + 24.0 * m.scale + output.content_size.y.min(max_body);
        if (measured - motion.measured).abs() > 0.25 {
            ctx.request_repaint();
        }
        motion.measured = measured;
        output.inner
    };
    // Keep the modal layer through its closing animation. A disabled Area alone
    // would expose the covered controls to Tab and activation during the fade.
    let response = egui::Modal::new(id)
        .area(area)
        .backdrop_color(egui::Color32::TRANSPARENT)
        .frame(egui::Frame::NONE)
        .show(ctx, render);
    let close = interactive && response.should_close();
    let inner = response.inner;
    if !motion.closing && (dismiss || close) {
        motion.closing = true;
        ctx.request_repaint();
    }
    if motion.closing && motion.height <= 0.25 && motion.opacity <= 0.01 {
        *open = false;
        focus.restore(ctx, layer);
    } else if interactive && !motion.closing {
        focus.contain(ctx, layer, id.with("close"));
    }
    ctx.data_mut(|data| {
        data.insert_temp(key, motion);
        data.insert_temp(focus_key, focus);
    });
    inner
}

#[derive(Clone, Default)]
struct RevealMotion {
    height: f32,
    measured: f32,
    frame: Option<u64>,
}

pub fn reveal<R>(
    ui: &mut Ui,
    id: Id,
    t: &Theme,
    open: bool,
    content: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let key = id.with("reveal-motion");
    let mut state = ui
        .ctx()
        .data(|data| data.get_temp::<RevealMotion>(key))
        .unwrap_or_default();
    let frame = ui.ctx().cumulative_frame_nr();
    if state
        .frame
        .is_some_and(|previous| frame.wrapping_sub(previous) > 1)
    {
        state.height = 0.0;
        state.measured = 0.0;
    }
    // A sizing/discard pass shares the frame delta. Applying it again changes
    // the visible clip between passes and accelerates expansion under relayout.
    let gains = crate::motion::layout_gains(ui.ctx(), t.duration());
    let moving = if state.frame != Some(frame) || gains.is_immediate() {
        state.frame = Some(frame);
        gains.step_pixels(
            &mut state.height,
            if open { state.measured } else { 0.0 },
            MotionProfile::Standard,
        )
    } else {
        (state.height - if open { state.measured } else { 0.0 }).abs() > 0.25
    };
    if !open && state.height <= 0.25 {
        ui.ctx().data_mut(|data| data.insert_temp(key, state));
        return None;
    }
    let origin = ui.cursor().min;
    let width = ui.available_width();
    let rect = Rect::from_min_size(origin, vec2(width, state.height));
    let mut body = ui.new_child(
        UiBuilder::new()
            .id_salt(id)
            .max_rect(Rect::from_min_size(origin, vec2(width, 10000.0))),
    );
    body.set_clip_rect(rect.intersect(ui.clip_rect()));
    body.set_opacity(if state.measured > 0.0 {
        (state.height / state.measured).clamp(0.0, 1.0)
    } else {
        0.0
    });
    if !open {
        body.disable();
    }
    let inner = content(&mut body);
    let measured = body.min_rect().height();
    if moving || open && (measured - state.height).abs() > 0.25 {
        ui.ctx().request_repaint();
    }
    state.measured = measured;
    ui.allocate_exact_size(vec2(width, state.height), Sense::hover());
    ui.ctx().data_mut(|data| data.insert_temp(key, state));
    Some(inner)
}

pub struct PopupLayout {
    pub header: Rect,
    pub body: Rect,
    pub footer: Rect,
}
impl PopupLayout {
    /// Lay out against the full target body, independently of the animated
    /// surface height. Keep the parent and body clipped to the visible surface.
    pub fn with_body_height(outer: Rect, t: &Theme, body_height: f32) -> Self {
        let m = t.metrics();
        Self::new(
            Rect::from_min_size(
                outer.min,
                vec2(
                    outer.width(),
                    m.popup_header + body_height.max(0.0) + 29.0 * m.scale,
                ),
            ),
            t,
        )
    }

    pub fn new(outer: Rect, t: &Theme) -> Self {
        let m = t.metrics();
        let header = Rect::from_min_size(outer.min, vec2(outer.width(), m.popup_header));
        let footer_height = (outer.height() - m.popup_header).clamp(0.0, 28.0 * m.scale);
        let footer = Rect::from_min_size(
            pos2(outer.left(), outer.bottom() - footer_height),
            vec2(outer.width(), footer_height),
        );
        let top = header.bottom() + m.scale;
        let body = Rect::from_min_size(
            pos2(outer.left() + m.inset, top),
            vec2(
                (outer.width() - 2.0 * m.inset).max(1.0),
                (footer.top() - top).max(0.0),
            ),
        );
        Self {
            header,
            body,
            footer,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Metrics {
    pub scale: f32,
    pub control: f32,
    pub icon: f32,
    pub inset: f32,
    pub gap: f32,
    pub toolbar: f32,
    pub footer: f32,
    pub columns: f32,
    pub popup_header: f32,
}
impl Metrics {
    pub fn new(font_size: f32) -> Self {
        let scale = (font_size / 14.0).max(1.0);
        Self {
            scale,
            control: CONTROL * scale,
            icon: ICON * scale,
            inset: INSET * scale,
            gap: GAP * scale,
            toolbar: 33.0 * scale,
            footer: 31.0 * scale,
            columns: 25.0 * scale,
            popup_header: POPUP_HEADER * scale,
        }
    }
}

pub fn popup_action_rect(header: Rect, index: usize, count: usize) -> Rect {
    let scale = header.height() / POPUP_HEADER;
    let control = CONTROL * scale;
    let right = header.right()
        - INSET * scale
        - (count.saturating_sub(index + 1) as f32) * (CONTROL + GAP) * scale;
    Rect::from_center_size(
        pos2(right - control * 0.5, header.center().y),
        vec2(control, control),
    )
}

pub fn popup_input_rect(header: Rect, actions: usize) -> Rect {
    let scale = header.height() / POPUP_HEADER;
    let right = if actions == 0 {
        header.right() - INSET * scale
    } else {
        popup_action_rect(header, 0, actions).left() - GAP * 2.0 * scale
    };
    Rect::from_min_max(
        pos2(
            header.left() + 34.0 * scale,
            header.center().y - CONTROL * scale * 0.5,
        ),
        pos2(
            right.max(header.left() + 35.0 * scale),
            header.center().y + CONTROL * scale * 0.5,
        ),
    )
}

pub fn popup_input_icon(ui: &Ui, header: Rect, t: &Theme, glyph: Icon) {
    let m = t.metrics();
    icon(
        ui.painter(),
        pos2(header.left() + 20.0 * m.scale, header.center().y),
        glyph,
        t.dim(),
        m.icon,
    );
}

/// Reserve a modest parent-path tail while giving the filename priority.
pub fn file_name_width(width: f32, scale: f32, has_parent: bool) -> f32 {
    if has_parent && width >= 128.0 * scale {
        (width * 0.62).max(88.0 * scale).min(width - 44.0 * scale)
    } else {
        width
    }
}

/// Shared inline location treatment for search results and Home's Recent rows.
pub fn file_parent_label(
    p: &egui::Painter,
    rect: Rect,
    t: &Theme,
    name_width: f32,
    parent: &str,
    color: egui::Color32,
) {
    let scale = t.metrics().scale;
    let left = rect.left() + name_width + 6.0 * scale;
    if parent.is_empty() || rect.right() - left < 40.0 * scale {
        return;
    }
    let p = p.with_clip_rect(rect);
    crate::chevron(
        &p,
        pos2(left + 5.0 * scale, rect.center().y),
        0.0,
        color,
        11.0 * scale,
    );
    elided_label(
        &p,
        Rect::from_min_max(pos2(left + 16.0 * scale, rect.top()), rect.max),
        t,
        parent,
        color,
        false,
    );
}

pub fn elided_label(
    p: &egui::Painter,
    rect: Rect,
    t: &Theme,
    label: &str,
    color: egui::Color32,
    right: bool,
) {
    if rect.width() <= 0.0
        || rect.height() <= 0.0
        || !rect.intersects(p.clip_rect())
        || !p.is_visible()
        || p.opacity() == 0.0
        || color == egui::Color32::TRANSPARENT
    {
        return;
    }
    let galley = crate::fonts::layout(
        p.ctx(),
        label,
        t.font(),
        egui::Color32::PLACEHOLDER,
        rect.width(),
        1,
    );
    let x = if right {
        rect.right() - galley.size().x
    } else {
        rect.left()
    };
    p.with_clip_rect(rect).galley(
        pos2(x, rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
}

pub fn message_galley(
    ctx: &egui::Context,
    t: &Theme,
    text: &str,
    width: f32,
    height: f32,
    color: egui::Color32,
) -> std::sync::Arc<egui::Galley> {
    let line = ctx.fonts_mut(|fonts| fonts.row_height(&t.font())).max(1.0);
    crate::fonts::layout(
        ctx,
        text,
        t.font(),
        color,
        width.max(1.0),
        (height / line).floor().max(1.0) as usize,
    )
}
pub fn message_at(ui: &Ui, rect: Rect, id: Id, t: &Theme, text: &str, color: egui::Color32) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let m = t.metrics();
    let inner = rect.shrink2(vec2(12.0 * m.scale, 4.0 * m.scale));
    let galley = message_galley(ui.ctx(), t, text, inner.width(), inner.height(), color);
    let elided = galley.elided;
    ui.painter()
        .with_clip_rect(rect)
        .galley(rect.center() - galley.size() * 0.5, galley, color);
    let response = ui.interact(rect, id, Sense::hover());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), text));
    if elided {
        response.on_hover_text(text);
    }
}

/// One compact keyboard instruction. Space-separated keys and `+` combinations
/// are separate keycaps; the explanation stays ordinary text.
#[derive(Clone, Copy)]
pub struct PopupHint<'a> {
    pub keys: &'a str,
    pub label: &'a str,
}
impl<'a> PopupHint<'a> {
    pub const fn new(keys: &'a str, label: &'a str) -> Self {
        Self { keys, label }
    }
}

struct HintSpan {
    galley: std::sync::Arc<egui::Galley>,
    keycap: bool,
    left: f32,
    width: f32,
}
struct HintLayout {
    spans: Vec<HintSpan>,
    width: f32,
}

fn hint_layout(p: &egui::Painter, t: &Theme, hints: &[PopupHint<'_>]) -> HintLayout {
    let scale = t.metrics().scale;
    let font = egui::FontId::proportional((t.font_size - 1.0).max(10.0));
    let mut layout = HintLayout {
        spans: Vec::new(),
        width: 0.0,
    };
    for hint in hints.iter().take(16) {
        if hint.keys.is_empty() && hint.label.is_empty() {
            continue;
        }
        if !layout.spans.is_empty() {
            layout.width += 12.0 * scale;
        }
        let mut push = |text: &str, keycap: bool, gap: f32| {
            layout.width += gap;
            let galley = crate::fonts::no_wrap(p.ctx(), text, font.clone());
            let width = galley.size().x + if keycap { 8.0 * scale } else { 0.0 };
            layout.spans.push(HintSpan {
                galley,
                keycap,
                left: layout.width,
                width,
            });
            layout.width += width;
        };
        let mut first = true;
        for key in hint
            .keys
            .split([' ', '+'])
            .filter(|key| !key.is_empty())
            .take(8)
        {
            // Legacy helpers also use the compact adjacent-arrow spelling.
            if key.chars().all(|c| matches!(c, '↑' | '↓' | '←' | '→')) {
                for (offset, character) in key.char_indices().take(8) {
                    push(
                        &key[offset..offset + character.len_utf8()],
                        true,
                        if first { 0.0 } else { 2.0 * scale },
                    );
                    first = false;
                }
            } else {
                push(key, true, if first { 0.0 } else { 2.0 * scale });
                first = false;
            }
        }
        if !hint.label.is_empty() {
            push(hint.label, false, if first { 0.0 } else { 4.0 * scale });
        }
    }
    layout
}

fn fit_hint_layout(
    p: &egui::Painter,
    t: &Theme,
    full: &[PopupHint<'_>],
    compact: &[PopupHint<'_>],
    width: f32,
) -> HintLayout {
    for hints in [
        full,
        compact,
        &[PopupHint::new("Esc", "close")],
        &[PopupHint::new("Esc", "")],
    ] {
        let layout = hint_layout(p, t, hints);
        if layout.width <= width {
            return layout;
        }
    }
    HintLayout {
        spans: Vec::new(),
        width: 0.0,
    }
}

/// A single, left-aligned line of small native keycaps inspired by shadcn Kbd.
/// This paints inside the existing footer and never changes its height/font.
pub fn popup_hints(
    p: &egui::Painter,
    rect: Rect,
    t: &Theme,
    full: &[PopupHint<'_>],
    compact: &[PopupHint<'_>],
) {
    if rect.height() < t.font_size
        || !rect.intersects(p.clip_rect())
        || !p.is_visible()
        || p.opacity() == 0.0
    {
        return;
    }
    let scale = t.metrics().scale;
    let content = rect.shrink2(vec2(14.0 * scale, 0.0));
    let layout = fit_hint_layout(p, t, full, compact, content.width().max(0.0));
    let painter = p.with_clip_rect(rect.intersect(p.clip_rect()));
    for span in layout.spans {
        let left = content.left() + span.left;
        if span.keycap {
            let height =
                (span.galley.size().y + 3.0 * scale).min((rect.height() - 6.0 * scale).max(1.0));
            let cap = Rect::from_min_size(
                pos2(left, rect.center().y - height * 0.5),
                vec2(span.width, height),
            );
            painter.rect_filled(cap, t.radius.min(3), Theme::color(t.chrome));
            painter.rect_stroke(
                cap,
                t.radius.min(3),
                Stroke::new(1.0, t.line()),
                StrokeKind::Inside,
            );
            painter.galley(
                cap.center() - span.galley.size() * 0.5,
                span.galley,
                t.dim(),
            );
        } else {
            painter.galley(
                pos2(left, rect.center().y - span.galley.size().y * 0.5),
                span.galley,
                t.dim(),
            );
        }
    }
}

fn legacy_popup_hints(text: &str) -> Vec<PopupHint<'_>> {
    text.split("   ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .take(16)
        .map(|part| {
            let (keys, label) = part
                .split_once("  ")
                .or_else(|| part.split_once(' '))
                .unwrap_or((part, ""));
            let first = keys.split([' ', '+']).next().unwrap_or("");
            if matches!(
                first,
                "Esc"
                    | "Escape"
                    | "Enter"
                    | "Tab"
                    | "Ctrl"
                    | "Alt"
                    | "Shift"
                    | "Home"
                    | "End"
                    | "PgUp"
                    | "PgDn"
                    | "Space"
            ) || !first.is_empty() && first.chars().all(|c| matches!(c, '↑' | '↓' | '←' | '→'))
            {
                PopupHint::new(keys.trim(), label.trim())
            } else {
                PopupHint::new("", part)
            }
        })
        .collect()
}

/// Backwards-compatible adapter for existing popup instructions. New callers
/// can pass explicit key/label pairs to [`popup_hints`].
pub fn popup_hint(p: &egui::Painter, rect: Rect, t: &Theme, full: &str, compact: &str) {
    popup_hints(
        p,
        rect,
        t,
        &legacy_popup_hints(full),
        &legacy_popup_hints(compact),
    );
}

pub fn icon_button_at(
    ui: &Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    glyph: Icon,
    active: bool,
) -> Response {
    icon_button_at_size(ui, rect, id, t, glyph, active, t.metrics().icon)
}
pub fn icon_button_at_enabled(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    glyph: Icon,
    active: bool,
    enabled: bool,
) -> Response {
    let mut child = ui.new_child(UiBuilder::new().id_salt(id.with("enabled")).max_rect(rect));
    if !enabled {
        child.disable();
    }
    icon_button_at(&child, rect, id, t, glyph, active)
}

pub fn icon_button_at_size(
    ui: &Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    glyph: Icon,
    active: bool,
    size: f32,
) -> Response {
    let response = button_response(ui, rect, id).on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            active,
            icon_button_label(glyph),
        )
    });
    let k = animated_state(
        ui.ctx(),
        id.with("hover"),
        response.hovered(),
        t,
        MotionProfile::Fast,
    );
    let selection = animated_state(
        ui.ctx(),
        id.with("selection"),
        active,
        t,
        MotionProfile::Standard,
    );
    if k > 0.0 {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            t.radius,
            Theme::color(t.hover).gamma_multiply(k * (1.0 - selection)),
        );
    }
    if selection > 0.0 {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            t.radius,
            Theme::color(t.selection).gamma_multiply(selection),
        );
    }
    icon(
        ui.painter(),
        rect.center(),
        glyph,
        t.dim()
            .lerp_to_gamma(t.fg(), k)
            .lerp_to_gamma(t.accent(), selection),
        size.min(rect.width() - 4.0)
            .min(rect.height() - 4.0)
            .max(1.0),
    );
    focus_ring(ui, rect, id, t, response.has_focus());
    response
}

pub fn focus_ring(ui: &Ui, rect: Rect, id: Id, t: &Theme, focused: bool) {
    focus_ring_with_radius(ui, rect, id, t, focused, t.radius);
}

pub fn focus_ring_with_radius(ui: &Ui, rect: Rect, id: Id, t: &Theme, focused: bool, radius: u8) {
    register_focus_target(ui, rect, id);
    if focused && can_focus(ui) && crate::focus::keyboard_visible(ui.ctx()) {
        // Replace a control's border at the same bounds instead of stacking an
        // inset ring and a contrasting separator around its selection/hover.
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, focus_color(t)),
            StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;

    fn keys(key: Key, modifiers: Modifiers) -> Vec<egui::Event> {
        [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            })
            .collect()
    }

    fn frame(
        ctx: &egui::Context,
        number: u32,
        events: Vec<egui::Event>,
        draw: impl FnMut(&mut Ui),
    ) {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, 320.0))),
                time: Some(number as f64 / 60.0),
                events,
                ..Default::default()
            },
            draw,
        )
        .drop_without_applying_deltas();
    }

    fn theme() -> Theme {
        Theme {
            reduced_motion: true,
            ..Theme::default()
        }
    }
    fn trigger() -> Id {
        Id::new("keyboard-test-trigger")
    }
    fn anchor() -> Rect {
        Rect::from_min_size(pos2(350.0, 40.0), vec2(100.0, 28.0))
    }
    fn menu_id() -> Id {
        Id::new("keyboard-test-menu")
    }

    fn items() -> Vec<MenuItem<'static>> {
        [
            ("Unavailable", false),
            ("", true),
            ("Alpha", true),
            ("Beta", true),
            ("Bravo", true),
        ]
        .into_iter()
        .enumerate()
        .map(|(id, (label, enabled))| MenuItem {
            id: id as u32,
            label,
            enabled,
            glyph: Icon::Empty,
            checked: None,
            shortcut: "",
        })
        .collect()
    }

    fn menu_frame(
        ctx: &egui::Context,
        number: u32,
        menu: &mut MenuState,
        items: &[MenuItem<'_>],
        events: Vec<egui::Event>,
    ) -> (Option<u32>, bool) {
        let mut picked = None;
        let mut background_clicked = false;
        frame(ctx, number, events, |ui| {
            // Register before showing the popup, but draw the trigger after it.
            // A leaked activation will therefore click the restored trigger in
            // the same pass, which a pure state-transition test would miss.
            register_focus_target(ui, anchor(), trigger());
            picked = menu.show(ui.ctx(), menu_id(), &theme(), anchor(), items);
            background_clicked =
                text_button_at(ui, anchor(), trigger(), &theme(), "Menu", false).clicked();
        });
        (picked, background_clicked)
    }

    fn opened_menu(ctx: &egui::Context, items: &[MenuItem<'_>]) -> MenuState {
        let mut menu = MenuState::default();
        menu_frame(ctx, 0, &mut menu, items, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(trigger()));
        menu.toggle(ctx);
        menu_frame(ctx, 1, &mut menu, items, vec![]);
        menu
    }

    #[test]
    fn opening_enter_or_space_does_not_pick_the_first_command() {
        for key in [Key::Enter, Key::Space] {
            let ctx = egui::Context::default();
            let mut menu = MenuState::default();
            let items = items();
            menu_frame(&ctx, 0, &mut menu, &items, vec![]);
            ctx.memory_mut(|memory| memory.request_focus(trigger()));
            let mut picked = None;
            frame(&ctx, 1, keys(key, Modifiers::NONE), |ui| {
                if text_button_at(ui, anchor(), trigger(), &theme(), "Menu", false).clicked() {
                    menu.toggle(ui.ctx());
                }
                picked = menu.show(ui.ctx(), menu_id(), &theme(), anchor(), &items);
            });
            assert!(menu.open);
            assert_eq!(picked, None);
            assert_eq!(
                ctx.memory(|memory| memory.focused()),
                Some(menu_id().with("keyboard"))
            );
        }
    }

    #[test]
    fn menu_activation_consumes_enter_and_space_before_restoring_focus() {
        for key in [Key::Enter, Key::Space] {
            let ctx = egui::Context::default();
            let items = items();
            let mut menu = opened_menu(&ctx, &items);
            let mut events = keys(key, Modifiers::NONE);
            if key == Key::Space {
                events.push(egui::Event::Text(" ".into()));
            }
            let (picked, background_clicked) = menu_frame(&ctx, 2, &mut menu, &items, events);
            assert_eq!(picked, Some(2));
            assert!(!menu.open);
            assert!(
                !background_clicked,
                "closing {key:?} clicked the return target"
            );
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(trigger()));
        }
    }

    #[test]
    fn menu_navigation_skips_disabled_rows_and_separators_in_event_order() {
        let ctx = egui::Context::default();
        let items = items();
        let mut menu = opened_menu(&ctx, &items);
        assert_eq!(menu.cursor, 2);
        let events = [Key::ArrowDown, Key::ArrowDown, Key::ArrowUp]
            .into_iter()
            .flat_map(|key| keys(key, Modifiers::NONE))
            .collect();
        menu_frame(&ctx, 2, &mut menu, &items, events);
        assert_eq!(menu.cursor, 3);
        menu_frame(&ctx, 3, &mut menu, &items, keys(Key::Home, Modifiers::NONE));
        assert_eq!(menu.cursor, 2);
        menu_frame(
            &ctx,
            4,
            &mut menu,
            &items,
            keys(Key::ArrowUp, Modifiers::NONE),
        );
        assert_eq!(menu.cursor, 4);
        let mut events = keys(Key::End, Modifiers::NONE);
        events.extend(keys(Key::Enter, Modifiers::NONE));
        assert_eq!(menu_frame(&ctx, 5, &mut menu, &items, events).0, Some(4));
    }

    #[test]
    fn typeahead_cycles_and_modified_navigation_is_left_to_its_owner() {
        let ctx = egui::Context::default();
        let items = items();
        let mut menu = opened_menu(&ctx, &items);
        menu_frame(
            &ctx,
            2,
            &mut menu,
            &items,
            vec![egui::Event::Text("b".into())],
        );
        assert_eq!(menu.cursor, 3);
        menu_frame(
            &ctx,
            3,
            &mut menu,
            &items,
            vec![egui::Event::Text("b".into())],
        );
        assert_eq!(menu.cursor, 4);
        let mut alt_down_survived = false;
        frame(&ctx, 4, keys(Key::ArrowDown, Modifiers::ALT), |ui| {
            menu.show(ui.ctx(), menu_id(), &theme(), anchor(), &items);
            alt_down_survived = consume_key(ui.ctx(), Modifiers::ALT, Key::ArrowDown);
        });
        assert!(alt_down_survived);
        assert_eq!(menu.cursor, 4);
    }

    #[test]
    fn tab_shift_tab_and_escape_close_the_menu_and_return_to_its_trigger() {
        for (key, modifiers) in [
            (Key::Tab, Modifiers::NONE),
            (Key::Tab, Modifiers::SHIFT),
            (Key::Escape, Modifiers::NONE),
        ] {
            let ctx = egui::Context::default();
            let items = items();
            let mut menu = opened_menu(&ctx, &items);
            let (picked, clicked) = menu_frame(&ctx, 2, &mut menu, &items, keys(key, modifiers));
            assert!(!menu.open);
            assert_eq!(picked, None);
            assert!(!clicked);
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(trigger()));
        }
    }

    #[test]
    fn keyboard_selection_is_revealed_in_a_long_menu() {
        let ctx = egui::Context::default();
        let items: Vec<_> = (0..100)
            .map(|id| MenuItem {
                id,
                label: "Command",
                enabled: true,
                glyph: Icon::Empty,
                checked: None,
                shortcut: "",
            })
            .collect();
        let mut menu = opened_menu(&ctx, &items);
        menu_frame(&ctx, 2, &mut menu, &items, keys(Key::End, Modifiers::NONE));
        assert_eq!(
            menu.cursor, 99,
            "End must select the final command before revealing it"
        );
        let mut visible = None;
        frame(&ctx, 3, vec![], |ui| {
            register_focus_target(ui, anchor(), trigger());
            menu.show(ui.ctx(), menu_id(), &theme(), anchor(), &items);
            // Snapshot during the active pass. Context::end_pass swaps its
            // current/previous widget maps; read_response after run_ui can
            // otherwise combine an old offscreen hit rect with the new rect.
            visible = Some((
                ui.ctx()
                    .read_response(menu_id().with("keyboard"))
                    .unwrap()
                    .rect,
                ui.ctx().read_response(menu_id().with(99_u32)).unwrap(),
                ui.memory(|memory| memory.focused()),
            ));
            text_button_at(ui, anchor(), trigger(), &theme(), "Menu", false);
        });
        let (surface, last, focused) = visible.unwrap();
        assert_eq!(focused, Some(menu_id().with("keyboard")));
        assert_eq!(ctx.memory(|memory| memory.focused()), focused);
        assert!(
            last.interact_rect.is_positive(),
            "cursor={}, surface={surface:?}, last={:?}, interact={:?}, height={}, focus={:?}",
            menu.cursor,
            last.rect,
            last.interact_rect,
            menu.height,
            ctx.memory(|memory| memory.focused())
        );
        assert!(
            surface.contains_rect(last.rect),
            "selected row must fit: surface={surface:?}, last={:?}, interact={:?}",
            last.rect,
            last.interact_rect
        );
    }

    #[test]
    fn modified_enter_is_not_a_button_click_or_a_consumed_shortcut() {
        let ctx = egui::Context::default();
        frame(&ctx, 0, vec![], |ui| {
            text_button_at(ui, anchor(), trigger(), &theme(), "Button", false);
            ui.memory_mut(|memory| memory.request_focus(trigger()));
        });
        let mut clicked = false;
        let mut shortcut_survived = false;
        frame(&ctx, 1, keys(Key::Enter, Modifiers::CTRL), |ui| {
            clicked = text_button_at(ui, anchor(), trigger(), &theme(), "Button", false).clicked();
            shortcut_survived = consume_key(ui.ctx(), Modifiers::CTRL, Key::Enter);
        });
        assert!(!clicked);
        assert!(shortcut_survived);
    }

    #[test]
    fn slider_home_and_end_use_the_exact_range_endpoints() {
        let ctx = egui::Context::default();
        let mut value = 5.0;
        let mut rail = Id::NULL;
        frame(&ctx, 0, vec![], |ui| {
            rail = slider(ui, &theme(), "Spacing", &mut value, 0.0..=10.0, 1.0).id;
        });
        ctx.memory_mut(|memory| memory.request_focus(rail));
        frame(&ctx, 1, keys(Key::End, Modifiers::NONE), |ui| {
            slider(ui, &theme(), "Spacing", &mut value, 0.0..=10.0, 1.0);
        });
        assert_eq!(value, 10.0);
        frame(&ctx, 2, keys(Key::Home, Modifiers::NONE), |ui| {
            slider(ui, &theme(), "Spacing", &mut value, 0.0..=10.0, 1.0);
        });
        assert_eq!(value, 0.0);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(rail));
    }

    #[test]
    fn dialog_tabs_stay_inside_and_escape_restores_the_invoker() {
        let ctx = egui::Context::default();
        let id = Id::new("keyboard-test-dialog");
        let first = id.with("first");
        let last = id.with("last");
        let mut open = false;
        let mut value = String::new();
        let mut background_clicked = false;
        let mut draw = |ui: &mut Ui| {
            register_focus_target(ui, anchor(), trigger());
            if open {
                dialog(
                    ui.ctx(),
                    id,
                    &theme(),
                    "Test dialog",
                    320.0,
                    &mut open,
                    |ui| {
                        form_input(ui, first, &theme(), "Name", &mut value, 64);
                        let (rect, _) = ui.allocate_exact_size(vec2(80.0, 28.0), Sense::hover());
                        text_button_at(ui, rect, last, &theme(), "Done", false);
                    },
                );
            }
            background_clicked |=
                text_button_at(ui, anchor(), trigger(), &theme(), "Invoker", false).clicked();
        };
        frame(&ctx, 0, vec![], &mut draw);
        drop(draw);
        ctx.memory_mut(|memory| memory.request_focus(trigger()));
        open = true;
        for (number, events) in [
            (1, vec![]),
            (2, keys(Key::Tab, Modifiers::NONE)),
            (3, keys(Key::Tab, Modifiers::NONE)),
            (4, keys(Key::Tab, Modifiers::NONE)),
            (5, vec![]),
        ] {
            frame(&ctx, number, events, |ui| {
                register_focus_target(ui, anchor(), trigger());
                if open {
                    dialog(
                        ui.ctx(),
                        id,
                        &theme(),
                        "Test dialog",
                        320.0,
                        &mut open,
                        |ui| {
                            form_input(ui, first, &theme(), "Name", &mut value, 64);
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(80.0, 28.0), Sense::hover());
                            text_button_at(ui, rect, last, &theme(), "Done", false);
                        },
                    );
                }
                background_clicked |=
                    text_button_at(ui, anchor(), trigger(), &theme(), "Invoker", false).clicked();
            });
            let focused = ctx.memory(|memory| memory.focused());
            assert!(
                matches!(focused, Some(focus) if focus == id.with("close") || focus == first || focus == last)
            );
        }
        for number in 6..9 {
            frame(
                &ctx,
                number,
                if number == 6 {
                    keys(Key::Escape, Modifiers::NONE)
                } else {
                    vec![]
                },
                |ui| {
                    register_focus_target(ui, anchor(), trigger());
                    if open {
                        dialog(
                            ui.ctx(),
                            id,
                            &theme(),
                            "Test dialog",
                            320.0,
                            &mut open,
                            |_| {},
                        );
                    }
                    background_clicked |=
                        text_button_at(ui, anchor(), trigger(), &theme(), "Invoker", false)
                            .clicked();
                },
            );
        }
        assert!(!open);
        assert!(!background_clicked);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(trigger()));
    }
}

fn luminance(rgb: [u8; 3]) -> f32 {
    let linear = rgb.map(|byte| {
        let value = byte as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
}

fn focus_color(t: &Theme) -> egui::Color32 {
    let backgrounds = [t.background, t.chrome, t.surface, t.hover, t.selection];
    let contrast = |rgb| {
        let light = luminance(rgb);
        backgrounds
            .into_iter()
            .map(|background| {
                let other = luminance(background);
                (light.max(other) + 0.05) / (light.min(other) + 0.05)
            })
            .fold(f32::INFINITY, f32::min)
    };
    if contrast(t.accent) >= 3.0 {
        return t.accent();
    }
    let mut best = t.text;
    for candidate in [[0, 0, 0], [255, 255, 255]] {
        if contrast(candidate) > contrast(best) {
            best = candidate;
        }
    }
    Theme::color(best)
}

fn icon_button_label(glyph: Icon) -> &'static str {
    match glyph {
        Icon::Back => "Back",
        Icon::Forward => "Forward",
        Icon::Up => "Up",
        Icon::Down => "Down",
        Icon::MoveDown => "Move down",
        Icon::ChevronUp => "Collapse",
        Icon::Close => "Close",
        Icon::Plus => "Add",
        Icon::Minus => "Remove",
        Icon::Menu | Icon::More | Icon::Ellipsis => "More actions",
        Icon::Search => "Search",
        Icon::Split => "Split panel",
        Icon::SplitBelow => "Split panel below",
        Icon::Commands => "Commands",
        Icon::Keyboard => "Keyboard shortcuts",
        Icon::Help => "Help",
        Icon::Inspect => "Inspect",
        Icon::Settings => "Settings",
        Icon::Refresh => "Refresh",
        Icon::Bookmark | Icon::BookmarkFilled => "Bookmark",
        Icon::Clock => "History",
        Icon::Drive => "Drives",
        Icon::Home => "Home",
        Icon::Copy => "Copy",
        Icon::Cut => "Cut",
        Icon::Paste => "Paste",
        Icon::Save => "Save",
        Icon::Star | Icon::StarOutline => "Favorite",
        Icon::Rename => "Rename",
        Icon::Trash => "Delete",
        Icon::Check => "Confirm",
        Icon::SelectAll => "Select all",
        Icon::SelectNone => "Select none",
        Icon::InvertSelection => "Invert selection",
        Icon::Expand => "Expand",
        Icon::Recursive => "Include subfolders",
        Icon::List => "List view",
        Icon::Grid => "Grid view",
        Icon::Columns => "Columns view",
        Icon::Picture => "Preview",
        Icon::Window => "Open window",
        Icon::Calculator => "Calculator",
        Icon::Filter => "Search conditions",
        Icon::Folder => "Open folder",
        Icon::File => "Open file",
        Icon::Image => "Open image",
        Icon::Code => "Open source",
        Icon::Archive => "Open archive",
        Icon::Music => "Open audio",
        Icon::Video => "Open video",
        Icon::Empty => "Action",
    }
}

pub fn resize_divider(ui: &Ui, rect: Rect, response: &Response, t: &Theme, vertical: bool) {
    let hover = animated_state(
        ui.ctx(),
        response.id.with("divider-hover"),
        response.hovered() || response.dragged(),
        t,
        MotionProfile::Fast,
    );
    let line = Stroke::new(1.0, t.line().lerp_to_gamma(t.accent(), hover * 0.75));
    if vertical {
        ui.painter().vline(rect.center().x, rect.y_range(), line);
    } else {
        ui.painter().hline(rect.x_range(), rect.center().y, line);
    }
}

/// Preserve quick press/move/release gestures, including an entire drag in one
/// input frame. egui's `dragged` flag can already be false on that release frame.
pub fn divider_delta(ui: &Ui, rect: Rect, response: &Response) -> egui::Vec2 {
    let press = primary_press_in(ui, rect);
    let delta = if let Some(press) = press {
        ui.input(|input| {
            input
                .pointer
                .interact_pos()
                .map_or(egui::Vec2::ZERO, |point| point - press)
        })
    } else if response.dragged() || response.drag_stopped() {
        ui.input(|input| input.pointer.delta())
    } else {
        egui::Vec2::ZERO
    };
    if delta != egui::Vec2::ZERO && delta.is_finite() {
        let until = ui.input(|input| input.time) + 0.12;
        ui.ctx()
            .data_mut(|data| data.insert_temp(Id::new("foldex-divider-until"), until));
        crate::motion::set_direct_layout(ui.ctx(), true);
        crate::motion::set_interactive_resize(ui.ctx(), true);
        ui.ctx().request_repaint();
        delta
    } else {
        egui::Vec2::ZERO
    }
}
fn primary_press_in(ui: &Ui, rect: Rect) -> Option<egui::Pos2> {
    ui.input(|input| {
        input.events.iter().find_map(|event| match event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                ..
            } => Some(*pos),
            _ => None,
        })
    })
    .filter(|point| {
        can_focus(ui)
            && rect.contains(*point)
            && ui.ctx().layer_id_at(*point) == Some(ui.layer_id())
    })
}
pub fn drag_position(ui: &Ui, rect: Rect, response: &Response) -> Option<egui::Pos2> {
    if response.clicked()
        || response.dragged()
        || response.drag_stopped()
        || primary_press_in(ui, rect).is_some()
    {
        ui.input(|input| input.pointer.interact_pos())
    } else {
        None
    }
}
pub fn divider_dragging(ctx: &egui::Context) -> bool {
    let until = ctx.data(|data| {
        data.get_temp::<f64>(Id::new("foldex-divider-until"))
            .unwrap_or(0.0)
    });
    ctx.input(|input| input.time) < until
}

pub fn horizontal_edge_fade(ui: &Ui, rect: Rect, t: &Theme, left: bool, right: bool) {
    let solid = Theme::color(t.chrome);
    let clear = solid.gamma_multiply(0.0);
    for (enabled, start, end, a, b) in [
        (left, rect.left(), rect.left() + 12.0, solid, clear),
        (right, rect.right() - 12.0, rect.right(), clear, solid),
    ] {
        if !enabled {
            continue;
        }
        let edge = Rect::from_min_max(pos2(start, rect.top()), pos2(end, rect.bottom()));
        let mut mesh = egui::Mesh::default();
        for (pos, color) in [
            (edge.left_top(), a),
            (edge.right_top(), b),
            (edge.right_bottom(), b),
            (edge.left_bottom(), a),
        ] {
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv: egui::epaint::WHITE_UV,
                color,
            });
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        ui.painter().add(egui::Shape::mesh(mesh));
    }
}

pub fn selection_checkbox(ui: &Ui, rect: Rect, id: Id, t: &Theme, state: i8) -> Response {
    let response = button_response(ui, rect, id);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            ui.is_enabled(),
            state != 0,
            "Select all files",
        )
    });
    if state < 0 {
        ui.ctx()
            .accesskit_node_builder(id, |node| node.set_toggled(egui::accesskit::Toggled::Mixed));
    }
    let glyph = Rect::from_center_size(rect.center(), vec2(16.0, 16.0) * t.metrics().scale);
    checkbox_mark(ui, glyph, &response, t, state);
    focus_ring(ui, rect, id, t, response.has_focus());
    response
}

pub fn checkbox(ui: &mut Ui, t: &Theme, value: &mut bool, label: &str) -> Response {
    let m = t.metrics();
    let label_width = if label.is_empty() {
        0.0
    } else {
        crate::fonts::no_wrap(ui.ctx(), label, t.font()).size().x
    };
    let width = (m.control + label_width).min(ui.max_rect().width().max(m.control));
    let (id, rect) = ui.allocate_space(vec2(width, m.control));
    let mut response = button_response(ui, rect, id);
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
        response.request_focus();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *value, label)
    });
    let glyph = Rect::from_center_size(
        pos2(rect.left() + m.control * 0.5, rect.center().y),
        vec2(m.icon, m.icon),
    );
    checkbox_mark(ui, glyph, &response, t, i8::from(*value));
    if !label.is_empty() {
        elided_label(
            ui.painter(),
            Rect::from_min_max(pos2(rect.left() + m.control, rect.top()), rect.max),
            t,
            label,
            t.fg(),
            false,
        );
    }
    focus_ring(ui, rect, id, t, response.has_focus());
    response
}

fn checkbox_mark(ui: &Ui, glyph: Rect, response: &Response, t: &Theme, state: i8) {
    let id = response.id;
    let hover = animated_state(
        ui.ctx(),
        id.with("hover"),
        response.hovered(),
        t,
        MotionProfile::Fast,
    );
    let selected = animated_state(
        ui.ctx(),
        id.with("selected"),
        state != 0,
        t,
        MotionProfile::Standard,
    );
    let mixed = animated_state(
        ui.ctx(),
        id.with("mixed"),
        state < 0,
        t,
        MotionProfile::Fast,
    );
    let checked = animated_state(
        ui.ctx(),
        id.with("checked"),
        state > 0,
        t,
        MotionProfile::Fast,
    );
    let scale = t.metrics().scale;
    ui.painter()
        .rect_filled(glyph, t.radius.min(4), t.accent().gamma_multiply(selected));
    ui.painter().rect_stroke(
        glyph,
        t.radius.min(4),
        Stroke::new(1.0, t.line().lerp_to_gamma(t.accent(), hover.max(selected))),
        StrokeKind::Inside,
    );
    if mixed > 0.0 {
        ui.painter().hline(
            (glyph.center().x - 4.0 * scale * mixed)..=(glyph.center().x + 4.0 * scale * mixed),
            glyph.center().y,
            Stroke::new(1.5, egui::Color32::WHITE.gamma_multiply(mixed)),
        );
    }
    if checked > 0.0 {
        icon(
            ui.painter(),
            glyph.center(),
            Icon::Check,
            egui::Color32::WHITE.gamma_multiply(checked),
            12.0 * scale * (0.8 + 0.2 * checked),
        );
    }
}

pub fn text_button(ui: &mut Ui, t: &Theme, label: &str, active: bool) -> Response {
    text_button_enabled(ui, t, label, active, true)
}

/// Keep an editable label and its removal action together when a row wraps.
pub fn removable_button(
    ui: &mut Ui,
    t: &Theme,
    label: &str,
    active: bool,
    remove_label: &str,
) -> (Response, Response) {
    let m = t.metrics();
    let label_width = crate::fonts::no_wrap(ui.ctx(), label, t.font()).size().x + 16.0 * m.scale;
    let width = (label_width.max(m.control) + m.gap + m.control)
        .min(ui.max_rect().width().max(2.0 * m.control + m.gap));
    let (id, rect) = ui.allocate_space(vec2(width, m.control));
    let button_rect = Rect::from_min_max(
        rect.min,
        pos2(rect.right() - m.control - m.gap, rect.bottom()),
    );
    let mut child = ui.new_child(UiBuilder::new().id_salt(id).max_rect(button_rect));
    let button = text_button(&mut child, t, label, active);
    let remove = icon_button_at(
        ui,
        Rect::from_min_size(
            pos2(rect.right() - m.control, rect.top()),
            vec2(m.control, m.control),
        ),
        id.with("remove"),
        t,
        Icon::Close,
        false,
    )
    .on_hover_text(remove_label);
    (button, remove)
}

pub fn text_button_enabled(
    ui: &mut Ui,
    t: &Theme,
    label: &str,
    active: bool,
    enabled: bool,
) -> Response {
    let m = t.metrics();
    let galley = crate::fonts::no_wrap(ui.ctx(), label, t.font());
    let width = (galley.size().x + 16.0 * m.scale)
        .max(m.control)
        .min(ui.max_rect().width().max(m.control));
    // Reserve the complete control before making its disabled child. A scope
    // created first only sees the remainder of a wrapped row and clips labels.
    let (id, rect) = ui.allocate_space(vec2(width, m.control));
    let mut button = ui.new_child(UiBuilder::new().id_salt(id).max_rect(rect));
    if !enabled {
        button.disable();
    }
    text_button_at(&button, rect, id, t, label, active)
}

pub fn text_button_at(
    ui: &Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    label: &str,
    active: bool,
) -> Response {
    let m = t.metrics();
    let response = button_response(ui, rect, id);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), active, label)
    });
    let hover = animated_state(
        ui.ctx(),
        response.id.with("hover"),
        ui.is_enabled() && response.hovered(),
        t,
        MotionProfile::Fast,
    );
    let selection = animated_state(
        ui.ctx(),
        response.id.with("selection"),
        active,
        t,
        MotionProfile::Standard,
    );
    let bg = Theme::color(t.chrome)
        .lerp_to_gamma(Theme::color(t.hover), hover)
        .lerp_to_gamma(Theme::color(t.selection), selection);
    ui.painter().rect_filled(rect, t.radius, bg);
    focus_ring(ui, rect, response.id, t, response.has_focus());
    elided_label(
        ui.painter(),
        rect.shrink2(vec2(8.0 * m.scale, 0.0)),
        t,
        label,
        t.fg().lerp_to_gamma(t.accent(), selection),
        false,
    );
    response
}

pub fn select_button(ui: &mut Ui, t: &Theme, label: &str, width: f32, open: bool) -> Response {
    let m = t.metrics();
    let (id, rect) = ui.allocate_space(vec2(
        width.min(ui.max_rect().width()).max(m.control),
        m.control,
    ));
    let response = button_response(ui, rect, id);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), label)
    });
    let hover = animated_state(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered() || open,
        t,
        MotionProfile::Fast,
    );
    ui.painter().rect_filled(
        rect,
        t.radius,
        Theme::color(t.chrome).lerp_to_gamma(Theme::color(t.hover), hover),
    );
    ui.painter().rect_stroke(
        rect,
        t.radius,
        Stroke::new(1.0, t.line().lerp_to_gamma(t.accent(), hover)),
        StrokeKind::Inside,
    );
    let text = Rect::from_min_max(
        rect.min + vec2(8.0 * m.scale, 0.0),
        pos2(
            (rect.right() - m.control).max(rect.left() + 8.0 * m.scale),
            rect.bottom(),
        ),
    );
    elided_label(ui.painter(), text, t, label, t.fg(), false);
    let turn = animated_state(
        ui.ctx(),
        response.id.with("expanded"),
        open,
        t,
        MotionProfile::Standard,
    );
    crate::chevron(
        ui.painter(),
        pos2(rect.right() - m.control * 0.5, rect.center().y),
        1.0 - 2.0 * turn,
        t.dim().lerp_to_gamma(t.fg(), hover),
        m.icon,
    );
    focus_ring(ui, rect, response.id, t, response.has_focus());
    if response.clicked() {
        response.request_focus();
    }
    response
}

pub fn text_input_at(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    value: &mut String,
    hint: &str,
    limit: usize,
) -> egui::text_edit::TextEditOutput {
    crate::fonts::request_text(ui.ctx(), value);
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    let output = egui::TextEdit::singleline(value)
        .id(id)
        .font(t.font())
        .text_color(t.fg())
        .frame(egui::Frame::NONE)
        .margin(egui::Margin::ZERO)
        .vertical_align(egui::Align::Center)
        .min_size(rect.size())
        .desired_width(rect.width())
        .clip_text(true)
        .hint_text(egui::RichText::new(hint).font(t.font()))
        .char_limit(limit)
        .show(&mut child);
    register_focus_target(ui, output.response.rect, id);
    output
}

pub fn form_input(
    ui: &mut Ui,
    id: Id,
    t: &Theme,
    label: &str,
    value: &mut String,
    limit: usize,
) -> egui::text_edit::TextEditOutput {
    let m = t.metrics();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), m.control), Sense::hover());
    let label_width = (100.0 * m.scale).min(rect.width() * 0.32);
    let field = Rect::from_min_max(pos2(rect.left() + label_width, rect.top()), rect.max);
    elided_label(
        ui.painter(),
        Rect::from_min_max(rect.min, pos2(field.left() - m.gap, rect.bottom())),
        t,
        label,
        t.fg(),
        false,
    );
    let output = input_box(ui, field, id, t, value, "", limit);
    label_control(&output.response, label);
    output
}

pub fn input_box(
    ui: &mut Ui,
    field: Rect,
    id: Id,
    t: &Theme,
    value: &mut String,
    hint: &str,
    limit: usize,
) -> egui::text_edit::TextEditOutput {
    ui.painter()
        .rect_filled(field, t.radius, Theme::color(t.chrome));
    let input = field.shrink2(vec2(8.0 * t.metrics().scale, 0.0));
    let left = ui
        .interact(
            Rect::from_min_max(field.min, pos2(input.left(), field.bottom())),
            id.with("left-inset"),
            Sense::CLICK,
        )
        .on_hover_cursor(egui::CursorIcon::Text);
    let right = ui
        .interact(
            Rect::from_min_max(pos2(input.right(), field.top()), field.max),
            id.with("right-inset"),
            Sense::CLICK,
        )
        .on_hover_cursor(egui::CursorIcon::Text);
    let mut output = text_input_at(ui, input, id, t, value, hint, limit);
    if left.clicked() || right.clicked() {
        output.response.request_focus();
        let range = egui::text::CCursorRange::one(egui::text::CCursor::new(if left.clicked() {
            0
        } else {
            value.chars().count()
        }));
        output.state.cursor.set_char_range(Some(range));
        output.state.clone().store(ui.ctx(), id);
        output.cursor_range = Some(range);
    }
    let focus = animated_state(
        ui.ctx(),
        id.with("outline"),
        output.response.has_focus(),
        t,
        MotionProfile::Fast,
    );
    ui.painter().rect_stroke(
        field,
        t.radius,
        Stroke::new(1.0, t.line().lerp_to_gamma(t.accent(), focus)),
        StrokeKind::Inside,
    );
    focus_ring(ui, field, id, t, output.response.has_focus());
    output
}

pub fn integer_field<T: egui::emath::Numeric + std::fmt::Display>(
    ui: &mut Ui,
    t: &Theme,
    label: &str,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
) -> Response {
    let m = t.metrics();
    let field_width = 64.0 * m.scale;
    let label_width = crate::fonts::no_wrap(ui.ctx(), label, t.font()).size().x;
    let (_, rect) = ui.allocate_space(vec2(label_width + m.gap + field_width, m.control));
    let mut group = ui.new_child(
        UiBuilder::new()
            .id_salt(label)
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    group.spacing_mut().item_spacing.x = m.gap;
    let name = group.label(label);
    let response = group
        .add_sized(
            [field_width, m.control],
            egui::DragValue::new(value)
                .range(range)
                .custom_formatter(|value, _| {
                    if value.abs() >= 1_000_000.0 {
                        format!("{value:.1e}")
                    } else {
                        format!("{value:.0}")
                    }
                }),
        )
        .labelled_by(name.id);
    focus_ring(&group, response.rect, response.id, t, response.has_focus());
    if value.to_f64().abs() >= 1_000_000.0 {
        response.on_hover_ui(|ui| {
            ui.label(value.to_string());
        })
    } else {
        response
    }
}

/// Filters share a contained clear action and the same animated space reservation.
pub fn search_input_clearable(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    value: &mut String,
    hint: &str,
    limit: usize,
) -> egui::text_edit::TextEditOutput {
    let m = t.metrics();
    // Keep Escape owned by this field across frames: egui's focus pass otherwise
    // drops the focused widget before application widgets see the key.
    let escaped = ui.memory(|memory| memory.has_focus(id))
        && consume_key(ui.ctx(), Modifiers::NONE, Key::Escape);
    let cleared = escaped && !value.is_empty();
    if escaped {
        value.clear();
        ui.memory_mut(|memory| memory.surrender_focus(id));
        ui.ctx().request_repaint();
    }
    let visible = !value.is_empty() && rect.width() >= 63.0 * m.scale;
    let reveal = animated_state(
        ui.ctx(),
        id.with("clear-reveal"),
        visible,
        t,
        MotionProfile::Fast,
    );
    let reserve = m.control * reveal;
    let field = Rect::from_min_max(
        rect.min,
        pos2((rect.right() - reserve).max(rect.left()), rect.bottom()),
    );
    let mut output = search_input(ui, field, id, t, value, hint, limit);
    if cleared {
        output.response.mark_changed();
    }
    if reserve > 0.5 {
        let button = Rect::from_center_size(
            pos2(rect.right() - m.control * 0.5, rect.center().y),
            vec2(m.control, m.control),
        );
        let clip =
            Rect::from_min_max(pos2(field.right(), rect.top()), rect.max).intersect(ui.clip_rect());
        let mut action_ui = ui.new_child(
            UiBuilder::new()
                .id_salt(id.with("clear-surface"))
                .max_rect(button),
        );
        action_ui.set_clip_rect(clip);
        action_ui.multiply_opacity(reveal);
        if !visible {
            disable_interaction(&mut action_ui);
        }
        let action = icon_button_at(&action_ui, button, id.with("clear"), t, Icon::Close, false)
            .on_hover_text("Clear filter");
        let enabled = can_focus(&action_ui);
        action.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "Clear filter")
        });
        if action.clicked() {
            value.clear();
            output.response.mark_changed();
            if can_focus(ui) {
                ui.memory_mut(|memory| memory.request_focus(id));
            }
            ui.ctx().request_repaint();
        }
    }
    if ui.memory(|memory| memory.has_focus(id)) {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
    if output.response.changed() {
        ui.ctx().request_repaint();
    }
    output
}

pub fn search_input(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    value: &mut String,
    hint: &str,
    limit: usize,
) -> egui::text_edit::TextEditOutput {
    let output = search_input_unframed(ui, rect, id, t, value, hint, limit);
    focus_ring(ui, rect, id, t, output.response.has_focus());
    output
}

/// A search editor whose containing surface paints its one focus border.
pub fn search_input_unframed(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    value: &mut String,
    hint: &str,
    limit: usize,
) -> egui::text_edit::TextEditOutput {
    // The containing surface owns decoration, but the whole editor remains a
    // popup return target even during clipped or covered TextEdit passes.
    register_focus_target(ui, rect, id);
    let m = t.metrics();
    let split = (rect.left() + 28.0 * m.scale).min(rect.right());
    let leading = ui
        .interact(
            Rect::from_min_max(rect.min, pos2(split, rect.bottom())),
            id.with("leading"),
            Sense::CLICK,
        )
        .on_hover_cursor(egui::CursorIcon::Text);
    let output = text_input_at(
        ui,
        Rect::from_min_max(pos2(split, rect.top()), rect.max),
        id,
        t,
        value,
        hint,
        limit,
    );
    if leading.clicked() {
        output.response.request_focus();
    }
    let hover = animated_state(
        ui.ctx(),
        id.with("search-hover"),
        leading.hovered() || output.response.hovered(),
        t,
        MotionProfile::Fast,
    );
    let focus = animated_state(
        ui.ctx(),
        id.with("search-focus"),
        output.response.has_focus(),
        t,
        MotionProfile::Standard,
    );
    icon(
        &ui.painter().with_clip_rect(rect),
        pos2(rect.left() + 14.0 * m.scale, rect.center().y),
        Icon::Search,
        t.dim()
            .lerp_to_gamma(t.fg(), hover)
            .lerp_to_gamma(t.accent(), focus),
        m.icon,
    );
    output
}

pub struct MenuItem<'a> {
    pub id: u32,
    pub label: &'a str,
    pub shortcut: &'a str,
    pub glyph: Icon,
    pub checked: Option<bool>,
    pub enabled: bool,
}
impl MenuItem<'_> {
    /// An empty label denotes a compact non-interactive separator, skipped by
    /// keyboard navigation and type-ahead.
    pub fn selectable(&self) -> bool {
        self.enabled && !self.label.trim().is_empty()
    }
}

/// Presentation-only grouping before an existing command ID. Groups add no
/// menu items or Tab stops, so command indices and keyboard navigation stay put.
#[derive(Clone, Copy)]
pub struct MenuSection<'a> {
    pub before: u32,
    pub label: &'a str,
}
impl<'a> MenuSection<'a> {
    pub const fn new(before: u32, label: &'a str) -> Self {
        Self { before, label }
    }
    pub const fn separator(before: u32) -> Self {
        Self::new(before, "")
    }
    fn height(&self, t: &Theme, first: bool) -> f32 {
        let height = if self.label.is_empty() {
            if first { 0.0 } else { 8.0 }
        } else if first {
            20.0
        } else {
            26.0
        };
        height * t.metrics().scale
    }
}

fn menu_item_height(t: &Theme, item: &MenuItem<'_>) -> f32 {
    if item.label.trim().is_empty() {
        8.0 * t.metrics().scale
    } else {
        menu_row_height(t)
    }
}

fn menu_section_before<'a>(
    sections: &'a [MenuSection<'a>],
    item: &MenuItem<'_>,
) -> Option<&'a MenuSection<'a>> {
    sections.iter().find(|section| section.before == item.id)
}

/// Return total content height and the largest viewport ending at a complete
/// command. A heading stays with its first command when the viewport is short.
fn menu_content_heights(
    t: &Theme,
    items: &[MenuItem<'_>],
    sections: &[MenuSection<'_>],
    available: f32,
) -> (f32, f32) {
    let mut content = 0.0;
    let mut complete = 0.0;
    for (row, item) in items.iter().enumerate() {
        content +=
            menu_section_before(sections, item).map_or(0.0, |section| section.height(t, row == 0));
        content += menu_item_height(t, item);
        if content <= available && !item.label.trim().is_empty() {
            complete = content;
        }
    }
    let viewport = if content <= available {
        content
    } else if complete > 0.0 {
        complete
    } else {
        available
    };
    (content, viewport.max(1.0))
}

fn paint_menu_section(ui: &Ui, rect: Rect, t: &Theme, section: &MenuSection<'_>, first: bool) {
    let m = t.metrics();
    let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    if !first {
        p.hline(
            (rect.left() + m.inset)..=(rect.right() - m.inset),
            rect.top() + 4.0 * m.scale,
            Stroke::new(1.0, t.line()),
        );
    }
    if !section.label.is_empty() {
        let label = crate::fonts::no_wrap(
            ui.ctx(),
            section.label,
            egui::FontId::proportional((t.font_size - 1.0).max(10.0)),
        );
        let label_area = rect.shrink2(vec2(10.0 * m.scale, 0.0));
        p.with_clip_rect(label_area.intersect(ui.clip_rect()))
            .galley(
                pos2(
                    label_area.left(),
                    rect.top()
                        + if first { 0.0 } else { 6.0 * m.scale }
                        + (20.0 * m.scale - label.size().y) * 0.5,
                ),
                label,
                t.dim(),
            );
    }
}

struct MenuRowLayout {
    glyph: Pos2,
    label: Rect,
    shortcut: Rect,
    check: Option<Pos2>,
}
impl MenuRowLayout {
    fn new(rect: Rect, t: &Theme, shortcut: egui::Vec2, checked: bool) -> Self {
        let scale = t.metrics().scale;
        let left = (rect.left() + 30.0 * scale).min(rect.right());
        let right = (rect.right() - (10.0 + if checked { 26.0 } else { 0.0 }) * scale)
            .max(left)
            .min(rect.right());
        let shortcut_width = shortcut.x.min((right - left).max(0.0));
        let shortcut_rect = Rect::from_min_size(
            pos2(right - shortcut_width, rect.center().y - shortcut.y * 0.5),
            vec2(shortcut_width, shortcut.y),
        );
        let label_right = if shortcut.x > 0.0 {
            (shortcut_rect.left() - 12.0 * scale).max(left)
        } else {
            right
        };
        Self {
            glyph: pos2(rect.left() + 16.0 * scale, rect.center().y),
            label: Rect::from_min_max(pos2(left, rect.top()), pos2(label_right, rect.bottom())),
            shortcut: shortcut_rect,
            check: checked.then(|| pos2(rect.right() - 14.0 * scale, rect.center().y)),
        }
    }
}
pub fn choice_row(
    ui: &Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    label: &str,
    glyph: Icon,
    selected: bool,
) -> Response {
    choice_row_trailing(ui, rect, id, t, label, glyph, selected, None)
}
pub fn choice_row_trailing(
    ui: &Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    label: &str,
    glyph: Icon,
    selected: bool,
    trailing: Option<Icon>,
) -> Response {
    // Result lists are composites: their owner handles arrows and activation.
    let response = ui.interact(rect.intersect(ui.clip_rect()), id, Sense::CLICK);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), selected, label)
    });
    let hover = animated_state(
        ui.ctx(),
        id.with("hover"),
        response.hovered(),
        t,
        MotionProfile::Fast,
    );
    let selection = animated_state(
        ui.ctx(),
        id.with("selected"),
        selected,
        t,
        MotionProfile::Standard,
    );
    let m = t.metrics();
    ui.painter().rect_filled(
        rect,
        t.radius,
        Theme::color(t.hover).gamma_multiply(hover * (1.0 - selection)),
    );
    ui.painter().rect_filled(
        rect,
        t.radius,
        Theme::color(t.selection)
            .lerp_to_gamma(t.accent(), 0.5)
            .gamma_multiply(selection),
    );
    icon(
        ui.painter(),
        pos2(rect.left() + 14.0 * m.scale, rect.center().y),
        glyph,
        if matches!(glyph, Icon::Folder) {
            Theme::color(t.folder)
        } else {
            t.fg()
        },
        m.icon,
    );
    elided_label(
        ui.painter(),
        Rect::from_min_max(
            rect.min + vec2(m.control, 0.0),
            rect.max
                - vec2(
                    if trailing.is_some() {
                        m.control
                    } else {
                        m.inset
                    },
                    0.0,
                ),
        ),
        t,
        label,
        t.fg(),
        false,
    );
    if let Some(glyph) = trailing {
        icon(
            ui.painter(),
            pos2(rect.right() - 14.0 * m.scale, rect.center().y),
            glyph,
            t.dim(),
            m.icon,
        );
    }
    focus_ring(ui, rect, id, t, response.has_focus());
    response
}
pub fn menu_row(
    ui: &Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    item: &MenuItem<'_>,
    selected: bool,
) -> Response {
    let m = t.metrics();
    let hit = rect.intersect(ui.clip_rect());
    let response = ui.interact(
        hit,
        id,
        if item.selectable() {
            Sense::CLICK
        } else {
            Sense::hover()
        },
    );
    if item.label.trim().is_empty() {
        ui.painter().hline(
            (rect.left() + m.inset)..=(rect.right() - m.inset),
            rect.center().y,
            Stroke::new(1.0, t.line()),
        );
        return response;
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            if item.checked.is_some() {
                egui::WidgetType::Checkbox
            } else {
                egui::WidgetType::Button
            },
            ui.is_enabled() && item.enabled,
            item.checked.unwrap_or(false),
            item.label,
        )
    });
    ui.ctx().accesskit_node_builder(id, |node| {
        node.set_role(if item.checked.is_some() {
            egui::accesskit::Role::MenuItemCheckBox
        } else {
            egui::accesskit::Role::MenuItem
        });
    });
    let k = animated_state(
        ui.ctx(),
        id.with("hover"),
        item.enabled && (response.hovered() || selected),
        t,
        MotionProfile::Fast,
    );
    let p = ui.painter().with_clip_rect(hit);
    p.rect_filled(rect, t.radius, Theme::color(t.hover).gamma_multiply(k));
    let color = if item.enabled {
        t.fg()
    } else {
        t.dim().gamma_multiply(0.45)
    };
    let shortcut_color = if item.enabled {
        t.dim()
    } else {
        t.dim().gamma_multiply(0.45)
    };
    let shortcut = crate::fonts::no_wrap(
        p.ctx(),
        item.shortcut,
        egui::FontId::proportional((t.font_size - 1.0).max(10.0)),
    );
    let layout = MenuRowLayout::new(rect, t, shortcut.size(), item.checked.is_some());
    icon(&p, layout.glyph, item.glyph, color, m.icon);
    if item.checked == Some(true) {
        icon(&p, layout.check.unwrap(), Icon::Check, color, m.icon);
    }
    elided_label(&p, layout.label, t, item.label, color, false);
    p.with_clip_rect(layout.shortcut.intersect(hit)).galley(
        pos2(
            layout.shortcut.right() - shortcut.size().x,
            layout.shortcut.top(),
        ),
        shortcut,
        shortcut_color,
    );
    response
}

#[derive(Default)]
pub struct MenuState {
    pub open: bool,
    height: f32,
    opened: u64,
    cursor: usize,
    pointer_navigation: bool,
    focus: PopupFocus,
    reset_scroll: bool,
    typeahead: String,
    typeahead_until: f64,
    reveal_cursor: bool,
}
impl MenuState {
    pub fn active(&self) -> bool {
        self.open || self.height > 0.25
    }
    pub fn toggle(&mut self, ctx: &egui::Context) {
        self.open = !self.open;
        self.opened = ctx.cumulative_frame_nr();
        self.cursor = 0;
        self.pointer_navigation = false;
        self.reset_scroll = self.open;
        self.typeahead.clear();
        ctx.request_repaint();
    }
    pub fn close(&mut self) {
        self.open = false;
    }
    pub fn select(&mut self, index: usize) {
        self.cursor = index;
        self.reveal_cursor = true;
    }
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        t: &Theme,
        anchor: Rect,
        items: &[MenuItem<'_>],
    ) -> Option<u32> {
        self.show_width(ctx, id, t, anchor, items, 248.0 * t.metrics().scale)
    }
    pub fn show_width(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        t: &Theme,
        anchor: Rect,
        items: &[MenuItem<'_>],
        desired_width: f32,
    ) -> Option<u32> {
        self.show_sections_width(ctx, id, t, anchor, items, &[], desired_width)
    }
    pub fn show_sections(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        t: &Theme,
        anchor: Rect,
        items: &[MenuItem<'_>],
        sections: &[MenuSection<'_>],
    ) -> Option<u32> {
        self.show_sections_width(
            ctx,
            id,
            t,
            anchor,
            items,
            sections,
            248.0 * t.metrics().scale,
        )
    }
    pub fn show_sections_width(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        t: &Theme,
        anchor: Rect,
        items: &[MenuItem<'_>],
        sections: &[MenuSection<'_>],
        desired_width: f32,
    ) -> Option<u32> {
        let focus_id = id.with("keyboard");
        let opening = self.open && !self.focus.owns;
        self.focus.update(ctx, focus_id, self.open);
        let screen = ctx.content_rect();
        let m = t.metrics();
        let safe_top = screen.top() + 36.0;
        let below = (anchor.bottom() + m.gap).max(safe_top);
        let above = (anchor.top() - m.gap).min(screen.bottom() - m.inset);
        let below_room = (screen.bottom() - m.inset - below).max(0.0);
        let above_room = (above - safe_top).max(0.0);
        let (content_height, _) = menu_content_heights(t, items, sections, f32::MAX);
        let desired = content_height + 8.0 * m.scale;
        let open_below = desired <= below_room || below_room >= above_room;
        let available = (if open_below { below_room } else { above_room }).max(1.0);
        let (_, viewport_height) =
            menu_content_heights(t, items, sections, (available - 8.0 * m.scale).max(1.0));
        let target = (viewport_height + 8.0 * m.scale).min(available);
        let moving = crate::motion::popup_gains(ctx, t.duration()).step_pixels(
            &mut self.height,
            if self.open { target } else { 0.0 },
            MotionProfile::Standard,
        );
        if moving {
            ctx.request_repaint();
        }
        if self.height <= 0.25 {
            return None;
        }
        let width = desired_width.min((screen.width() - 12.0 * m.scale).max(1.0));
        let x = (anchor.right() - width).clamp(
            screen.left() + m.inset,
            (screen.right() - width - m.inset).max(screen.left() + m.inset),
        );
        let y = if open_below {
            below
        } else {
            above - self.height
        };
        let outer = Rect::from_min_size(pos2(x, y), vec2(width, self.height));
        let mut picked = None;
        let mut follow_cursor = opening || self.reveal_cursor;
        if !items.get(self.cursor).is_some_and(MenuItem::selectable) {
            self.cursor = items.iter().position(MenuItem::selectable).unwrap_or(0);
            follow_cursor = true;
        }
        let pointer_moved = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::PointerMoved(_) | egui::Event::MouseWheel { .. }
                )
            })
        });
        if pointer_moved {
            self.pointer_navigation = true;
        }
        if self.open && self.focus.owns_keyboard(ctx, focus_id) {
            let (now, events) = ctx.input_mut(|input| {
                let text_allowed = !input.modifiers.ctrl
                    && !input.modifiers.alt
                    && !input.modifiers.command
                    && !input.modifiers.mac_cmd;
                let mut events = Vec::new();
                input.events.retain(|event| {
                    let owned = match event {
                        egui::Event::Key {
                            key,
                            pressed: true,
                            modifiers,
                            ..
                        } => {
                            (*modifiers == Modifiers::NONE
                                && matches!(
                                    key,
                                    Key::ArrowDown
                                        | Key::ArrowUp
                                        | Key::Home
                                        | Key::End
                                        | Key::Enter
                                        | Key::Space
                                        | Key::Escape
                                        | Key::Tab
                                ))
                                || (*key == Key::Tab && *modifiers == Modifiers::SHIFT)
                        }
                        egui::Event::Text(_) => text_allowed,
                        _ => false,
                    };
                    if owned {
                        events.push(event.clone());
                    }
                    !owned
                });
                (input.time, events)
            });
            // Process the actual event order, including several arrow presses
            // before Enter in one frame, rather than collapsing them to booleans.
            for event in events {
                match event {
                    egui::Event::Key {
                        key: Key::Tab | Key::Escape,
                        ..
                    } => {
                        self.open = false;
                    }
                    egui::Event::Key {
                        key: Key::Enter | Key::Space,
                        ..
                    } => {
                        if !opening && items.get(self.cursor).is_some_and(MenuItem::selectable) {
                            picked = Some(items[self.cursor].id);
                            self.open = false;
                        }
                    }
                    egui::Event::Key { key, .. } => {
                        self.typeahead.clear();
                        match key {
                            Key::Home => {
                                self.cursor =
                                    items.iter().position(MenuItem::selectable).unwrap_or(0)
                            }
                            Key::End => {
                                self.cursor =
                                    items.iter().rposition(MenuItem::selectable).unwrap_or(0)
                            }
                            Key::ArrowDown | Key::ArrowUp if !items.is_empty() => {
                                for _ in 0..items.len() {
                                    self.cursor = if key == Key::ArrowDown {
                                        (self.cursor + 1) % items.len()
                                    } else {
                                        (self.cursor + items.len() - 1) % items.len()
                                    };
                                    if items[self.cursor].selectable() {
                                        break;
                                    }
                                }
                            }
                            _ => {}
                        }
                        follow_cursor = true;
                        self.pointer_navigation = false;
                    }
                    egui::Event::Text(input) => {
                        if now > self.typeahead_until {
                            self.typeahead.clear();
                        }
                        let remaining = 32 - self.typeahead.chars().count();
                        self.typeahead.extend(
                            input
                                .chars()
                                .filter(|ch| !ch.is_control())
                                .flat_map(char::to_lowercase)
                                .take(remaining),
                        );
                        self.typeahead_until = now + 0.7;
                        let repeated = self
                            .typeahead
                            .chars()
                            .next()
                            .filter(|first| self.typeahead.chars().all(|ch| ch == *first));
                        let prefix =
                            repeated.map_or_else(|| self.typeahead.clone(), |ch| ch.to_string());
                        let start = if prefix.chars().count() == 1 {
                            self.cursor.saturating_add(1)
                        } else {
                            self.cursor
                        };
                        if !prefix.is_empty()
                            && let Some(row) = (0..items.len())
                                .map(|offset| (start + offset) % items.len())
                                .find(|&row| {
                                    items[row].selectable()
                                        && items[row].label.to_lowercase().starts_with(&prefix)
                                })
                        {
                            self.cursor = row;
                            follow_cursor = true;
                            self.pointer_navigation = false;
                        }
                    }
                    _ => {}
                }
                if !self.open {
                    break;
                }
            }
            if !self.open {
                ctx.request_repaint();
            }
        }
        let keyboard_focus = self.open && self.focus.owns_keyboard(ctx, focus_id);
        popup_surface(
            ctx,
            egui::Area::new(id)
                .order(egui::Order::Foreground)
                .fixed_pos(outer.min)
                .fade_in(false),
            self.open,
            |ui| {
                if self.open {
                    PopupFocus::surface(ui, outer, focus_id, "Menu");
                    ui.ctx().accesskit_node_builder(focus_id, |node| {
                        node.set_role(egui::accesskit::Role::Menu);
                        if let Some(item) = items.get(self.cursor).filter(|item| item.selectable())
                        {
                            node.set_active_descendant(id.with(item.id).accesskit_id());
                        }
                    });
                }
                ui.set_min_size(outer.size());
                ui.set_max_size(outer.size());
                ui.set_clip_rect(outer.intersect(screen));
                ui.painter()
                    .rect_filled(outer, t.radius, Theme::color(t.surface));
                ui.painter().rect_stroke(
                    outer,
                    t.radius,
                    Stroke::new(1.0, t.line()),
                    StrokeKind::Inside,
                );
                let inner = Rect::from_min_size(
                    outer.min + vec2(4.0, 4.0) * m.scale,
                    vec2(
                        (width - 8.0 * m.scale).max(1.0),
                        (target - 8.0 * m.scale).max(1.0),
                    ),
                );
                if inner.height() > 0.0 {
                    ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
                        ui.set_clip_rect(inner.intersect(outer).intersect(screen));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let mut scroll = menu_scroll_area_for_content(
                            ui,
                            t,
                            id.with("rows"),
                            inner.height(),
                            content_height,
                        );
                        if self.reset_scroll {
                            scroll = scroll.vertical_scroll_offset(0.0);
                        }
                        scroll.show(ui, |ui| {
                            for (row, item) in items.iter().enumerate() {
                                if let Some(section) = menu_section_before(sections, item) {
                                    let (rect, _) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), section.height(t, row == 0)),
                                        Sense::hover(),
                                    );
                                    paint_menu_section(ui, rect, t, section, row == 0);
                                }
                                let (rect, _) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), menu_item_height(t, item)),
                                    Sense::hover(),
                                );
                                let r = menu_row(
                                    ui,
                                    rect,
                                    id.with(item.id),
                                    t,
                                    item,
                                    !self.pointer_navigation && self.cursor == row,
                                );
                                if self.pointer_navigation && r.hovered() && item.selectable() {
                                    self.cursor = row;
                                }
                                if self.open && r.clicked() && item.selectable() {
                                    picked = Some(item.id);
                                }
                                focus_ring(
                                    ui,
                                    rect,
                                    id.with(item.id),
                                    t,
                                    keyboard_focus && self.cursor == row && item.selectable(),
                                );
                                if follow_cursor && self.cursor == row {
                                    let animation = if crate::motion::direct_layout(ui.ctx()) {
                                        egui::style::ScrollAnimation::none()
                                    } else {
                                        egui::style::ScrollAnimation::duration(t.duration())
                                    };
                                    ui.scroll_to_rect_animation(rect, None, animation);
                                }
                            }
                        });
                        if !ui.is_sizing_pass() {
                            self.reset_scroll = false;
                            self.reveal_cursor = false;
                        }
                    });
                }
            },
        );
        if self.open
            && ctx.cumulative_frame_nr() > self.opened
            && ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer.interact_pos().is_some_and(|p| !outer.contains(p))
            })
        {
            self.open = false;
            ctx.request_repaint();
        }
        if picked.is_some() {
            self.open = false;
            ctx.request_repaint();
        }
        self.focus.update(ctx, focus_id, self.open);
        picked
    }
}

#[cfg(test)]
mod menu_design_tests {
    use super::*;

    fn item(id: u32) -> MenuItem<'static> {
        MenuItem {
            id,
            label: "Command",
            shortcut: "Ctrl+S",
            glyph: Icon::Split,
            checked: Some(true),
            enabled: true,
        }
    }

    #[test]
    fn section_geometry_keeps_a_heading_with_its_first_command() {
        let items = [item(0), item(1), item(2)];
        let sections = [
            MenuSection::new(0, "Section"),
            MenuSection::separator(1),
            MenuSection::new(2, "Visible places"),
            MenuSection::new(99, "Not present"),
        ];
        for font_size in [12.0, 14.0, 24.0] {
            let t = Theme {
                font_size,
                ..Theme::default()
            };
            let scale = t.metrics().scale;
            // The next heading would fit by itself at 110, but its command
            // would not. End the viewport at the previous complete command.
            let (content, viewport) = menu_content_heights(&t, &items, &sections, 110.0 * scale);
            assert!((content - 138.0 * scale).abs() < 0.01);
            assert!((viewport - 84.0 * scale).abs() < 0.01);
            let (_, fitted) = menu_content_heights(&t, &items, &sections, 200.0 * scale);
            assert_eq!(fitted, content);
        }
    }

    #[test]
    fn menu_label_shortcut_and_check_slots_do_not_overlap() {
        for font_size in [12.0, 24.0] {
            let t = Theme {
                font_size,
                ..Theme::default()
            };
            let scale = t.metrics().scale;
            for width in [160.0, 248.0, 288.0] {
                let rect =
                    Rect::from_min_size(pos2(20.0, 30.0), vec2(width * scale, menu_row_height(&t)));
                for shortcut in [0.0, 56.0, 160.0] {
                    let layout =
                        MenuRowLayout::new(rect, &t, vec2(shortcut * scale, 14.0 * scale), true);
                    let check = Rect::from_center_size(
                        layout.check.unwrap(),
                        vec2(t.metrics().icon, t.metrics().icon),
                    );
                    assert!(rect.contains_rect(layout.label));
                    assert!(rect.contains_rect(check));
                    assert!(layout.label.right() <= check.left());
                    assert!(layout.shortcut.right() < check.left());
                    assert!(layout.glyph.x + t.metrics().icon * 0.5 < layout.label.left());
                    if shortcut > 0.0 {
                        assert!(layout.label.right() <= layout.shortcut.left());
                    }
                }
            }
        }
    }

    #[test]
    fn checked_rows_render_the_action_glyph_and_a_separate_trailing_check() {
        let ctx = egui::Context::default();
        let t = Theme::default();
        let rect = Rect::from_min_size(pos2(20.0, 30.0), vec2(248.0, 28.0));
        let output = ctx.run_ui(Default::default(), |ui| {
            menu_row(ui, rect, Id::new("checked-menu-glyph"), &t, &item(0), false);
        });
        let lines: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::LineSegment { points, .. } = &shape.shape {
                    Some(*points)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            lines.iter().any(|points| {
                (points[0].x - (rect.left() + 16.0)).abs() < 0.01
                    && (points[1].x - points[0].x).abs() < 0.01
                    && (points[1].y - points[0].y).abs() > 10.0
            }),
            "checked state must not replace the split glyph"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|points| { points.iter().all(|point| point.x > rect.right() - 30.0) })
                .count(),
            2,
            "the two check strokes belong in the trailing slot"
        );
        output.drop_without_applying_deltas();
    }

    #[test]
    fn grouped_menu_reveals_and_activates_the_last_command_using_its_actual_rect() {
        let ctx = egui::Context::default();
        let t = Theme {
            reduced_motion: true,
            ..Theme::default()
        };
        let id = Id::new("grouped-menu-reveal");
        let trigger = id.with("trigger");
        let anchor = Rect::from_min_size(pos2(350.0, 40.0), vec2(100.0, 28.0));
        let items: Vec<_> = (0..40).map(item).collect();
        let sections = [
            MenuSection::new(0, "Section"),
            MenuSection::separator(9),
            MenuSection::new(19, "Visible places"),
            MenuSection::separator(29),
        ];
        let mut menu = MenuState::default();
        for number in 0..5 {
            if number == 1 {
                ctx.memory_mut(|memory| memory.request_focus(trigger));
                menu.toggle(&ctx);
            }
            let key = match number {
                2 => Some(Key::End),
                4 => Some(Key::Enter),
                _ => None,
            };
            let events = key
                .into_iter()
                .flat_map(|key| {
                    [true, false].map(|pressed| egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    })
                })
                .collect();
            let mut picked = None;
            let mut visible = None;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 320.0))),
                    time: Some(number as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    register_focus_target(ui, anchor, trigger);
                    picked = menu.show_sections(ui.ctx(), id, &t, anchor, &items, &sections);
                    if number == 3 {
                        visible = Some((
                            ui.ctx().read_response(id.with("keyboard")).unwrap().rect,
                            ui.ctx().read_response(id.with(39_u32)).unwrap(),
                        ));
                    }
                    text_button_at(ui, anchor, trigger, &t, "Menu", false);
                },
            )
            .drop_without_applying_deltas();
            if let Some((surface, last)) = visible {
                assert!(last.interact_rect.is_positive());
                assert!(surface.contains_rect(last.rect));
                assert_eq!(menu.cursor, 39);
            }
            if number == 4 {
                assert_eq!(picked, Some(39));
                assert!(!menu.open);
                assert_eq!(ctx.memory(|memory| memory.focused()), Some(trigger));
            }
        }
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    #[test]
    fn elided_labels_reuse_geometry_with_independent_colors_and_skip_clipped_text() {
        let ctx = egui::Context::default();
        let t = Theme::default();
        let output = ctx.run_ui(Default::default(), |ui| {
            let painter = ui
                .painter()
                .with_clip_rect(Rect::from_min_size(pos2(10.0, 10.0), vec2(240.0, 100.0)));
            for (top, color) in [(20.0, egui::Color32::RED), (50.0, egui::Color32::GREEN)] {
                elided_label(
                    &painter,
                    Rect::from_min_size(pos2(20.0, top), vec2(200.0, 28.0)),
                    &t,
                    "Same label",
                    color,
                    false,
                );
            }
            let before = ui.fonts(|fonts| fonts.num_galleys_in_cache());
            elided_label(
                &painter,
                Rect::from_min_size(pos2(20.0, 200.0), vec2(200.0, 28.0)),
                &t,
                "Clipped work must not reach layout",
                t.fg(),
                false,
            );
            let mut invisible = painter.clone();
            invisible.set_opacity(0.0);
            elided_label(
                &invisible,
                Rect::from_min_size(pos2(20.0, 20.0), vec2(200.0, 28.0)),
                &t,
                "Transparent work must not reach layout",
                t.fg(),
                false,
            );
            assert_eq!(before, ui.fonts(|fonts| fonts.num_galleys_in_cache()));
        });
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some(text)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(texts.len(), 2);
        assert!(std::sync::Arc::ptr_eq(&texts[0].galley, &texts[1].galley));
        assert_eq!(texts[0].fallback_color, egui::Color32::RED);
        assert_eq!(texts[1].fallback_color, egui::Color32::GREEN);
        output.drop_without_applying_deltas();
    }

    #[test]
    fn cached_labels_preserve_tessellated_colors_at_disabled_opacity() {
        let t = Theme::default();
        for opacity in [1.0, 0.45] {
            let ctx = egui::Context::default();
            let output = ctx.run_ui(Default::default(), |ui| {
                let rect = Rect::from_min_size(pos2(20.0, 20.0), vec2(90.0, 28.0));
                let mut painter = ui.painter().clone();
                painter.set_opacity(opacity);
                let color = t.dim().gamma_multiply(0.45);
                let mut job = egui::text::LayoutJob::simple(
                    "A long disabled label".into(),
                    t.font(),
                    color,
                    rect.width(),
                );
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                let expected = painter.layout_job(job);
                painter.with_clip_rect(rect).galley(
                    pos2(rect.left(), rect.center().y - expected.size().y * 0.5),
                    expected,
                    color,
                );
                elided_label(&painter, rect, &t, "A long disabled label", color, false);
            });
            let shapes: Vec<_> = output
                .shapes
                .iter()
                .filter(|shape| matches!(shape.shape, egui::Shape::Text(_)))
                .cloned()
                .collect();
            assert_eq!(shapes.len(), 2);
            let expected = ctx.tessellate(vec![shapes[0].clone()], output.pixels_per_point);
            let actual = ctx.tessellate(vec![shapes[1].clone()], output.pixels_per_point);
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(&expected) {
                let (
                    egui::epaint::Primitive::Mesh(actual),
                    egui::epaint::Primitive::Mesh(expected),
                ) = (&actual.primitive, &expected.primitive)
                else {
                    panic!("labels must tessellate to text meshes")
                };
                assert_eq!(actual.vertices, expected.vertices);
                assert_eq!(actual.indices, expected.indices);
            }
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn reveal_advances_once_per_frame_and_reduced_motion_still_snaps_in_sizing_passes() {
        for reduced_motion in [false, true] {
            let ctx = egui::Context::default();
            let id = Id::new("multipass-reveal");
            let t = Theme {
                reduced_motion,
                ..Default::default()
            };
            let mut heights = Vec::new();
            ctx.run_ui(
                egui::RawInput {
                    time: Some(0.0),
                    ..Default::default()
                },
                |ui| {
                    reveal(ui, id, &t, true, |ui| ui.set_min_size(vec2(120.0, 80.0)));
                    heights.push(ui.ctx().data(|data| {
                        data.get_temp::<RevealMotion>(id.with("reveal-motion"))
                            .unwrap()
                            .height
                    }));
                    if ui.ctx().current_pass_index() == 0 {
                        ui.ctx().request_discard("exercise a sizing pass");
                    }
                },
            )
            .drop_without_applying_deltas();
            assert!(heights.len() >= 2, "the test must run the discarded pass");
            if reduced_motion {
                assert_eq!(heights[1], 80.0);
            } else {
                assert_eq!(
                    heights[1], heights[0],
                    "one frame must consume its gain only once"
                );
            }
            for frame in 1..40 {
                ctx.run_ui(
                    egui::RawInput {
                        time: Some(frame as f64 / 60.0),
                        ..Default::default()
                    },
                    |ui| {
                        reveal(ui, id, &t, true, |ui| ui.set_min_size(vec2(120.0, 80.0)));
                    },
                )
                .drop_without_applying_deltas();
            }
            assert_eq!(
                ctx.data(|data| {
                    data.get_temp::<RevealMotion>(id.with("reveal-motion"))
                        .unwrap()
                        .height
                }),
                80.0
            );
            assert!(
                !ctx.has_requested_repaint(),
                "a settled reveal must let the UI sleep"
            );
        }
    }

    #[test]
    fn settled_scroll_viewport_and_reservation_ignore_the_opening_clip() {
        let ctx = egui::Context::default();
        let t = Theme::default();
        let full = Rect::from_min_size(pos2(20.0, 20.0), vec2(240.0, 84.0));
        // Reuse the same scroll id across fitting, overflowing and filtered
        // content. Reservation must be correct immediately in either direction.
        for (frame, (rows, visible)) in [(2, 4.0), (2, 84.0), (20, 8.0), (20, 84.0), (2, 12.0)]
            .into_iter()
            .enumerate()
        {
            let mut measured = None;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, 480.0))),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| {
                    ui.scope_builder(UiBuilder::new().id_salt("viewport").max_rect(full), |ui| {
                        ui.set_clip_rect(Rect::from_min_size(
                            full.min,
                            vec2(full.width(), visible),
                        ));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let output = menu_scroll_area_for_content(
                            ui,
                            &t,
                            "rows",
                            full.height(),
                            rows as f32 * CONTROL,
                        )
                        .vertical_scroll_offset(56.0)
                        .show_rows(ui, CONTROL, rows, |ui, range| {
                            for _ in range {
                                ui.allocate_exact_size(
                                    vec2(ui.available_width(), CONTROL),
                                    Sense::hover(),
                                );
                            }
                        });
                        measured = Some((output.inner_rect, output.state.offset.y));
                    });
                },
            )
            .drop_without_applying_deltas();
            let (viewport, offset) = measured.unwrap();
            let overflow = rows > 3;
            assert_eq!(viewport.height(), full.height());
            assert_eq!(
                viewport.width(),
                full.width() - if overflow { 4.0 } else { 0.0 }
            );
            assert_eq!(offset, if overflow { 56.0 } else { 0.0 });
        }
    }

    #[test]
    fn fitting_menu_rows_keep_their_width_through_opening_animation() {
        let ctx = egui::Context::default();
        let t = Theme::default();
        let id = Id::new("opening-menu");
        let mut menu = MenuState {
            open: true,
            ..Default::default()
        };
        let items: Vec<_> = (0..4)
            .map(|id| MenuItem {
                id,
                label: "A fitting command",
                shortcut: "",
                glyph: Icon::Empty,
                checked: None,
                enabled: true,
            })
            .collect();
        let anchor = Rect::from_min_size(pos2(350.0, 50.0), vec2(28.0, 28.0));
        for frame in 0..24 {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, 480.0))),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| {
                    menu.show(ui.ctx(), id, &t, anchor, &items);
                },
            )
            .drop_without_applying_deltas();
            assert_eq!(
                ctx.read_response(id.with(0_u32)).unwrap().rect.width(),
                240.0
            );
        }
    }

    #[test]
    fn popup_layout_uses_the_full_body_while_its_surface_is_clipped() {
        let t = Theme::default();
        for visible in [1.0, 22.0, 70.0, 235.0] {
            let outer = Rect::from_min_size(pos2(40.0, 50.0), vec2(400.0, visible));
            let layout = PopupLayout::with_body_height(outer, &t, 168.0);
            assert_eq!(layout.body.height(), 168.0);
            assert_eq!(layout.body.top(), 89.0);
            assert_eq!(layout.footer.top(), 257.0);
        }
    }

    #[test]
    fn keycap_adapter_keeps_counts_plain_and_splits_shortcut_keys() {
        let hints = legacy_popup_hints(
            "12 results     ↑ ↓  navigate     Ctrl+Enter  open in panel     Esc  close",
        );
        assert_eq!((hints[0].keys, hints[0].label), ("", "12 results"));
        assert_eq!((hints[1].keys, hints[1].label), ("↑ ↓", "navigate"));
        assert_eq!(
            (hints[2].keys, hints[2].label),
            ("Ctrl+Enter", "open in panel")
        );
        let compact = legacy_popup_hints("↑↓ Navigate    Enter Copy    Esc Close");
        assert_eq!((compact[0].keys, compact[0].label), ("↑↓", "Navigate"));
        assert_eq!((compact[2].keys, compact[2].label), ("Esc", "Close"));
    }

    #[test]
    fn keycap_footers_stay_left_aligned_and_inside_the_existing_height() {
        let ctx = egui::Context::default();
        let t = Theme::default();
        for width in [72.0, 180.0, 720.0] {
            let footer = Rect::from_min_size(pos2(20.0, 50.0), vec2(width, 28.0));
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 480.0))),
                    ..Default::default()
                },
                |ui| {
                    popup_hint(
                        ui.painter(),
                        footer,
                        &t,
                        "↑ ↓  navigate     Enter  use     Esc  close",
                        "Enter  use     Esc  close",
                    );
                },
            );
            let caps: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| {
                    if let egui::Shape::Rect(shape) = &shape.shape
                        && shape.fill == Theme::color(t.chrome)
                    {
                        Some(shape.rect)
                    } else {
                        None
                    }
                })
                .collect();
            assert!(!caps.is_empty());
            assert_eq!(caps[0].left(), footer.left() + 14.0);
            assert!(caps.iter().all(|cap| footer.contains_rect(*cap)));
            output.drop_without_applying_deltas();
        }
    }
}
