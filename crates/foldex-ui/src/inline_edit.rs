//! The text box that renames an item where its name is drawn, as File Explorer
//! does. It owns only editing: the caller decides what Enter, Escape, Tab and a
//! lost focus mean, and draws the box over the item's name every frame while it
//! is open (egui drops the focus of a widget that skips a frame).
use crate::Theme;
use egui::{
    Align, Color32, EventFilter, Id, Key, LayerId, Modifiers, Order, Rect, Response, Stroke,
    StrokeKind, Ui, UiBuilder, pos2, text::CCursor, text::CCursorRange, vec2,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineLayout {
    /// Details, List and Columns: one line that scrolls sideways.
    SingleLine,
    /// Icon views: centred, word-wrapped text that grows downward.
    Wrapping { max_lines: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineOutcome {
    Editing,
    /// Enter.
    Commit,
    /// Escape.
    Cancel,
    /// Tab or Shift+Tab: commit and edit the next or previous item.
    Move {
        backwards: bool,
    },
    /// The editor had focus and lost it: a click elsewhere, another control.
    FocusLost,
}

/// Per-editor state the caller keeps while the editor is open.
#[derive(Clone, Debug, Default)]
pub struct InlineEditState {
    /// Character range selected once the editor has focus.
    pub select: Option<(usize, usize)>,
    /// Keep requesting focus until the editor owns it.
    pub focus: bool,
    pub had_focus: bool,
    /// A context menu action is running; its click must not end the edit.
    menu_open: bool,
}

impl InlineEditState {
    pub fn opening(select: (usize, usize)) -> Self {
        Self {
            select: Some(select),
            focus: true,
            had_focus: false,
            menu_open: false,
        }
    }
}

pub struct InlineEditOutput {
    pub outcome: InlineOutcome,
    /// The painted box, which may extend below `anchor` in icon views.
    pub rect: Rect,
    pub response: Response,
    /// True when typing or pasting supplied a character `reject` refused.
    pub rejected: bool,
}

fn take(ctx: &egui::Context, key: Key, modifiers: Modifiers) -> bool {
    crate::components::consume_key(ctx, modifiers, key)
}

/// Draws the editor over `anchor`, the rectangle where the name is painted.
/// `reject` removes characters as they are typed or pasted; `max_units` caps the
/// text in UTF-16 code units. Enter, Escape and Tab are taken before the text
/// field sees them, while it has or awaits focus and no IME is composing.
#[allow(clippy::too_many_arguments)]
pub fn inline_name_editor(
    ui: &mut Ui,
    anchor: Rect,
    id: Id,
    t: &Theme,
    text: &mut String,
    layout: InlineLayout,
    state: &mut InlineEditState,
    composing: bool,
    reject: &dyn Fn(char) -> bool,
    max_units: usize,
) -> InlineEditOutput {
    let ctx = ui.ctx().clone();
    let scale = t.metrics().scale;
    let focused = ctx.memory(|memory| memory.has_focus(id));
    // egui drops a just-requested focus when Escape arrives before the editor
    // could claim it, so keys also belong to an editor that is still opening.
    let opening = state.focus && !state.had_focus;
    let mut outcome = InlineOutcome::Editing;
    if (focused || opening) && !composing {
        if take(&ctx, Key::Enter, Modifiers::NONE) {
            outcome = InlineOutcome::Commit;
        } else if take(&ctx, Key::Escape, Modifiers::NONE) {
            outcome = InlineOutcome::Cancel;
        } else if take(&ctx, Key::Tab, Modifiers::SHIFT) {
            outcome = InlineOutcome::Move { backwards: true };
        } else if take(&ctx, Key::Tab, Modifiers::NONE) {
            outcome = InlineOutcome::Move { backwards: false };
        }
    }
    // egui collapses an unfocused editor's selection, so apply it each frame
    // until focus arrives, and again right after asking for focus.
    let apply_selection = |state: &InlineEditState| {
        if !state.had_focus
            && let Some((start, end)) = state.select
        {
            let mut edit = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
            edit.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(start),
                CCursor::new(end),
            )));
            edit.store(&ctx, id);
        }
    };
    apply_selection(state);

    let font = t.font();
    let pad = vec2(4.0 * scale, 1.0 * scale);
    let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
    let rect = match layout {
        InlineLayout::SingleLine => anchor,
        InlineLayout::Wrapping { max_lines } => {
            let wrap = (anchor.width() - 2.0 * pad.x).max(1.0);
            let rows = ui
                .fonts_mut(|fonts| fonts.layout(text.clone(), font.clone(), Color32::WHITE, wrap))
                .rows
                .len()
                .clamp(1, max_lines.max(1));
            Rect::from_min_size(
                anchor.min,
                vec2(
                    anchor.width(),
                    (rows as f32 * row_height + 2.0 * pad.y).max(anchor.height()),
                ),
            )
        }
    };
    let clip = ui.clip_rect();
    let painter = ui.painter().with_clip_rect(clip);
    painter.rect_filled(rect, t.radius, t.bg());
    painter.rect_stroke(
        rect,
        t.radius,
        Stroke::new(1.0, t.accent()),
        StrokeKind::Inside,
    );
    let inner = rect.shrink2(pad);
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(id.with("inline-editor"))
            .max_rect(inner)
            .layout(egui::Layout::top_down(Align::Min)),
    );
    child.set_clip_rect(inner.intersect(clip));
    crate::fonts::request_text(&ctx, text);
    let before = text.clone();
    fn edit(text: &mut String, layout: InlineLayout) -> egui::TextEdit<'_> {
        match layout {
            InlineLayout::SingleLine => egui::TextEdit::singleline(text)
                .vertical_align(Align::Center)
                .clip_text(true),
            InlineLayout::Wrapping { .. } => egui::TextEdit::multiline(text)
                .desired_rows(1)
                .horizontal_align(Align::Center),
        }
    }
    let show = |ui: &mut Ui, text: &mut String| {
        edit(text, layout)
            .id(id)
            .font(font.clone())
            .text_color(t.fg())
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .min_size(inner.size())
            .desired_width(inner.width())
            .return_key(None)
            .show(ui)
    };
    let output = match layout {
        InlineLayout::SingleLine => show(&mut child, text),
        InlineLayout::Wrapping { max_lines } => {
            egui::ScrollArea::vertical()
                .id_salt(id.with("scroll"))
                .max_height(max_lines.max(1) as f32 * row_height)
                .auto_shrink([false, true])
                .show(&mut child, |ui| show(ui, text))
                .inner
        }
    };
    let response = output.response.response.clone();
    crate::components::register_focus_target(ui, rect, id);

    let mut rejected = false;
    if *text != before {
        let cursor = output
            .cursor_range
            .map_or(text.chars().count(), |range| range.primary.index.0);
        let removed_before = text
            .chars()
            .take(cursor)
            .filter(|character| reject(*character))
            .count();
        let mut kept = String::with_capacity(text.len());
        let mut units = 0;
        let mut truncated_before = 0;
        for (index, character) in text.chars().enumerate() {
            if reject(character) {
                rejected = true;
                continue;
            }
            if units + character.len_utf16() > max_units {
                if index < cursor {
                    truncated_before += 1;
                }
                continue;
            }
            units += character.len_utf16();
            kept.push(character);
        }
        if kept != *text {
            *text = kept;
            let at = cursor.saturating_sub(removed_before + truncated_before);
            let mut edit = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
            edit.cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(at))));
            edit.store(&ctx, id);
        }
    }

    let has_focus = response.has_focus();
    if has_focus {
        state.had_focus = true;
        state.select = None;
        // Arrows move the caret, never the list's focus; Escape and Tab reach
        // this editor instead of egui's focus navigation.
        ctx.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });
    }
    let menu = edit_menu(&response, &ctx, id, text);
    let menu_open = menu || response.context_menu_opened();
    if state.focus && !has_focus && crate::components::can_focus(ui) {
        response.request_focus();
        apply_selection(state);
    } else if has_focus {
        state.focus = false;
    }
    if outcome == InlineOutcome::Editing && state.had_focus && !has_focus {
        if menu_open || state.menu_open {
            // A menu click took the focus; give it back for the menu's action.
            state.focus = true;
            response.request_focus();
        } else {
            outcome = InlineOutcome::FocusLost;
        }
    }
    state.menu_open = menu_open;
    InlineEditOutput {
        outcome,
        rect,
        response,
        rejected,
    }
}

/// The standard edit menu for the name box. Returns true when an item ran.
fn edit_menu(response: &Response, ctx: &egui::Context, id: Id, text: &mut String) -> bool {
    let mut ran = false;
    response.context_menu(|ui| {
        let mut edit = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let range = edit.cursor.char_range();
        let selected = range.filter(|range| !range.is_empty()).map(|range| {
            let [start, end] = range.sorted_cursors();
            (start.index.0, end.index.0)
        });
        let slice = |text: &str, (start, end): (usize, usize)| -> String {
            text.chars().skip(start).take(end - start).collect()
        };
        let mut undoer = edit.undoer();
        let can_undo = range.is_some_and(|range| undoer.has_undo(&(range, text.clone())));
        let mut close = false;
        if ui
            .add_enabled(can_undo, egui::Button::new("Undo"))
            .clicked()
        {
            if let Some(range) = range
                && let Some((restored_range, restored)) =
                    undoer.undo(&(range, text.clone())).cloned()
            {
                *text = restored;
                edit.cursor.set_char_range(Some(restored_range));
                edit.set_undoer(undoer);
                edit.clone().store(ctx, id);
            }
            close = true;
        }
        ui.separator();
        if ui
            .add_enabled(selected.is_some(), egui::Button::new("Cut"))
            .clicked()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut);
            close = true;
        }
        if ui
            .add_enabled(selected.is_some(), egui::Button::new("Copy"))
            .clicked()
        {
            if let Some(selected) = selected {
                ctx.copy_text(slice(text, selected));
            }
            close = true;
        }
        if ui.button("Paste").clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            close = true;
        }
        if ui
            .add_enabled(selected.is_some(), egui::Button::new("Delete"))
            .clicked()
        {
            if let Some((start, end)) = selected {
                *text = text
                    .chars()
                    .take(start)
                    .chain(text.chars().skip(end))
                    .collect();
                edit.cursor
                    .set_char_range(Some(CCursorRange::one(CCursor::new(start))));
                edit.clone().store(ctx, id);
            }
            close = true;
        }
        ui.separator();
        if ui.button("Select all").clicked() {
            edit.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(0),
                CCursor::new(text.chars().count()),
            )));
            edit.clone().store(ctx, id);
            close = true;
        }
        if close {
            ran = true;
            ui.close();
        }
    });
    ran
}

/// The explanation shown under the editor, like Explorer's balloon tip.
pub fn name_balloon(ctx: &egui::Context, below: Rect, id: Id, t: &Theme, message: &str) {
    let scale = t.metrics().scale;
    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, id.with("balloon")));
    let font = egui::FontId::proportional((t.font_size - 1.0).max(10.0));
    let width = (300.0 * scale)
        .min(ctx.content_rect().width() - 16.0 * scale)
        .max(80.0);
    let galley = painter.layout(message.to_owned(), font, t.fg(), width - 20.0 * scale);
    let size = galley.size() + vec2(20.0 * scale, 14.0 * scale);
    let screen = ctx.content_rect();
    let mut min = pos2(below.left(), below.bottom() + 4.0 * scale);
    if min.y + size.y > screen.bottom() {
        min.y = below.top() - 4.0 * scale - size.y;
    }
    min.x = min
        .x
        .min(screen.right() - size.x - 4.0)
        .max(screen.left() + 4.0);
    let rect = Rect::from_min_size(min, size);
    painter.rect_filled(rect, t.radius, Theme::color(t.chrome));
    painter.rect_stroke(
        rect,
        t.radius,
        Stroke::new(1.0, Theme::color(t.danger)),
        StrokeKind::Inside,
    );
    // Painted only, never hit-tested, so it can't block the list below.
    painter.galley(rect.min + vec2(10.0 * scale, 7.0 * scale), galley, t.fg());
}
