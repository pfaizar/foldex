use super::*;
use egui::{Align2, FontId, Stroke, StrokeKind};

const PALETTE_HEADER: f32 = foldex_ui::components::POPUP_HEADER;
const PALETTE_FOOTER: f32 = 28.0;
const RESULT_HEIGHT: f32 = 36.0;
const SECTION_HEIGHT: f32 = 28.0;
/// The calculator's heading plus its single answer row.
const CALCULATOR_HEIGHT: f32 = SECTION_HEIGHT + 56.0 + 8.0;
const DISCOVERY_APPS: usize = 6;

fn results_focus_id() -> Id {
    Id::new("launcher-results-focus")
}

fn calculator_copy_id() -> Id {
    Id::new("launcher-calculator-copy")
}

#[derive(Default)]
pub(super) struct CalculatorPaint {
    answer_rect: Option<Rect>,
    copy_rect: Option<Rect>,
    rolling: bool,
}

/// Read-only evidence for frame/native probes. Bounds describe the most recent
/// paint; reading this never activates the result or schedules clipboard work.
#[derive(Clone, Debug)]
pub(crate) struct CalculatorPresentation {
    pub answer: String,
    pub displayed_query: String,
    pub pending: bool,
    pub copy_enabled: bool,
    pub rolling: bool,
    pub answer_rect: Option<Rect>,
    pub copy_rect: Option<Rect>,
}

/// Keep scientific mantissas and exponents as separate numeric runs. The
/// shared reels deliberately render an intact `1e10` label statically, while
/// the calculator can roll each run around a stationary exponent marker.
fn scientific_parts(text: &str) -> Option<(&str, &str)> {
    let number = &text[..text.find(char::is_whitespace).unwrap_or(text.len())];
    let at = number.find(['e', 'E'])?;
    let (mantissa, exponent) = (&number[..at], &number[at + 1..]);
    if !mantissa.parse::<f64>().ok()?.is_finite() || exponent.parse::<i32>().is_err() {
        return None;
    }
    Some((&text[..at], &text[at + 1..]))
}

// egui's consume_key deliberately ignores extra Shift/Alt modifiers. Navigation
// here must leave modified text editing and platform shortcuts untouched.
fn consume_plain_key(ctx: &egui::Context, key: Key) -> bool {
    ctx.input_mut(|input| {
        let mut consumed = false;
        input.events.retain(|event| {
            let take = matches!(event, egui::Event::Key {
                key: pressed, pressed: true, modifiers, ..
            } if *pressed == key && modifiers.is_none());
            consumed |= take;
            !take
        });
        consumed
    })
}

#[derive(Clone, Default)]
struct QueryComposition {
    active: bool,
    frame: Option<u64>,
    blocked: bool,
}

fn query_composing(ctx: &egui::Context, query: Id) -> bool {
    let frame = ctx.cumulative_frame_nr();
    let id = query.with("ime");
    let mut state = ctx
        .data(|data| data.get_temp::<QueryComposition>(id))
        .unwrap_or_default();
    if state.frame != Some(frame) {
        state.blocked = state.active;
        ctx.input(|input| {
            for event in &input.events {
                if let egui::Event::Ime(event) = event {
                    state.blocked = true;
                    match event {
                        egui::ImeEvent::Preedit { text, .. } => state.active = !text.is_empty(),
                        egui::ImeEvent::Commit(_) => state.active = false,
                        _ => {}
                    }
                }
            }
        });
        state.frame = Some(frame);
        ctx.data_mut(|data| data.insert_temp(id, state.clone()));
    }
    state.blocked
}

#[derive(Clone, Copy)]
enum LineKind {
    Heading(&'static str, &'static str),
    Empty(&'static str),
    Result(usize),
    Calculator(usize),
}
struct ResultLine {
    top: f32,
    height: f32,
    kind: LineKind,
}

/// Headings and guidance never become result indices. Discovery previews a
/// small application set; typing searches the complete backend catalog.
fn result_lines(
    rows: &[Arc<ResultRow>],
    discovery: bool,
    label: &'static str,
    row_height: f32,
    group_height: f32,
    calculator: Option<usize>,
) -> Vec<ResultLine> {
    let mut lines = Vec::with_capacity(rows.len() + 6);
    let mut top = 0.0;
    let mut push = |kind, height| {
        lines.push(ResultLine { top, height, kind });
        top += height;
    };
    if let Some(answer) = calculator {
        push(
            LineKind::Calculator(answer),
            CALCULATOR_HEIGHT * row_height / RESULT_HEIGHT,
        );
        if rows.len() > 1 {
            push(LineKind::Heading("Other results", ""), group_height);
            for at in 0..rows.len() {
                if at != answer {
                    push(LineKind::Result(at), row_height);
                }
            }
        }
    } else if discovery {
        for (group, empty) in [
            (
                ResultGroup::Favorites,
                "Pin a folder in Foldex to keep it here.",
            ),
            (
                ResultGroup::Recents,
                "Recently visited folders appear here.",
            ),
            (ResultGroup::Apps, ""),
        ] {
            let count = rows.iter().filter(|row| row.group == group).count();
            if count == 0 && group == ResultGroup::Apps {
                continue;
            }
            let label = if group == ResultGroup::Apps {
                "Applications"
            } else {
                group.label()
            };
            let hint = if group == ResultGroup::Apps && count > DISCOVERY_APPS {
                "Type to find more"
            } else {
                ""
            };
            push(LineKind::Heading(label, hint), group_height);
            if count == 0 {
                push(LineKind::Empty(empty), row_height * 0.8);
            } else {
                let mut displayed = 0;
                for (at, row) in rows.iter().enumerate() {
                    if row.group == group {
                        if group == ResultGroup::Apps && displayed == DISCOVERY_APPS {
                            break;
                        }
                        push(LineKind::Result(at), row_height);
                        displayed += 1;
                    }
                }
            }
        }
        let other: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                !matches!(
                    row.group,
                    ResultGroup::Favorites | ResultGroup::Recents | ResultGroup::Apps
                )
            })
            .map(|(at, _)| at)
            .collect();
        if !other.is_empty() {
            push(LineKind::Heading("Suggestions", ""), group_height);
            for at in other {
                push(LineKind::Result(at), row_height);
            }
        }
    } else {
        push(LineKind::Heading(label, ""), group_height);
        for at in 0..rows.len() {
            push(LineKind::Result(at), row_height);
        }
    }
    lines
}

fn presented_indices(lines: &[ResultLine]) -> Vec<usize> {
    lines
        .iter()
        .filter_map(|line| match line.kind {
            LineKind::Result(at) | LineKind::Calculator(at) => Some(at),
            _ => None,
        })
        .collect()
}

// Keep the natural leading edge while scrolling, but never paint a sliver of
// an unreadable result immediately above the fixed footer.
fn readable_at_bottom(rect: Rect, clip: Rect, minimum: f32) -> bool {
    rect.intersects(clip)
        && (rect.top() < clip.top() || clip.bottom().min(rect.bottom()) - rect.top() >= minimum)
}

fn heading_has_companion(rect: Rect, clip: Rect, companion_minimum: f32) -> bool {
    readable_at_bottom(rect, clip, rect.height())
        && rect.bottom() + companion_minimum <= clip.bottom()
}

/// Display-only context: at most two parent names, never a full absolute path.
fn parent_context(path: &std::path::Path) -> String {
    let Some(parent) = path.parent() else {
        return String::new();
    };
    let leaf = parent
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            parent
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_owned()
        });
    let grand = parent
        .parent()
        .and_then(std::path::Path::file_name)
        .map(|name| name.to_string_lossy());
    match grand {
        Some(grand) if matches!(grand.as_ref(), "Users" | "home") => "Home".into(),
        Some(grand)
            if !matches!(grand.as_ref(), "Users" | "home")
                && grand.chars().count() + leaf.chars().count() <= 26 =>
        {
            format!("{grand} / {leaf}")
        }
        _ => leaf,
    }
}
fn result_context(row: &ResultRow, discovery: bool) -> String {
    if row.group == ResultGroup::Apps {
        return if discovery { "" } else { "App" }.into();
    }
    match &row.action {
        ResultAction::Open(path) | ResultAction::Browse(path) | ResultAction::Reveal(path) => {
            parent_context(path)
        }
        ResultAction::Application(_) => "Application".into(),
        ResultAction::Window(_) => "Window".into(),
        ResultAction::Copy(_) => if row.group == ResultGroup::Calculator {
            "Result"
        } else {
            "Text"
        }
        .into(),
        ResultAction::Builtin(_) => "Action".into(),
    }
}

#[derive(Clone, Copy)]
struct PaletteSize {
    height: f32,
    from: f32,
    target: f32,
    candidate: f32,
    changed: f64,
    started: f64,
    epoch: u64,
    scale: f32,
    opened: bool,
}

fn result_scroll_area(
    ui: &mut Ui,
    t: &Theme,
    outer: Rect,
    body: Rect,
    content_height: f32,
) -> egui::ScrollArea {
    let settled_height = ui
        .ctx()
        .data(|data| data.get_temp::<PaletteSize>(Id::new("launcher-palette-size")))
        .map_or(outer.height(), |size| size.candidate);
    let settled = foldex_ui::components::PopupLayout::new(
        Rect::from_min_size(outer.min, vec2(outer.width(), settled_height)),
        t,
    );
    let settled_body_height = (settled.body.height() - 12.0 * t.metrics().scale).max(1.0);
    let visibility = if content_height > settled_body_height.ceil() {
        egui::scroll_area::ScrollBarVisibility::AlwaysVisible
    } else {
        egui::scroll_area::ScrollBarVisibility::AlwaysHidden
    };
    // The current native frame bounds scrolling, while the settled fit decides
    // the scrollbar slot. Short results must not flash a bar or change width
    // just because a growing native window has not reached its requested size.
    foldex_ui::components::menu_scroll_area(ui, t, "launcher-results", body.height())
        .scroll_bar_visibility(visibility)
}

fn row_icon_key(row: &ResultRow, size: u16) -> Option<foldex_assets::AssetKey> {
    use foldex_assets::AssetKey;
    match &row.action {
        ResultAction::Open(path) | ResultAction::Browse(path) | ResultAction::Reveal(path)
            if path.is_absolute() =>
        {
            Some(AssetKey {
                path: path.clone(),
                size,
                thumbnail: false,
            })
        }
        ResultAction::Application(identifier) => AssetKey::application_icon(identifier, size),
        ResultAction::Window(window) => AssetKey::window_icon(*window, size),
        _ => None,
    }
}

fn row_has_native_icon(row: &ResultRow) -> bool {
    match &row.action {
        ResultAction::Open(path) | ResultAction::Browse(path) | ResultAction::Reveal(path) => {
            path.is_absolute()
        }
        ResultAction::Application(identifier) => {
            identifier.len() <= foldex_assets::MAX_SHELL_ICON_ID_BYTES
                && applications::valid_target(identifier)
        }
        ResultAction::Window(window) => *window > 0,
        _ => false,
    }
}

fn visible_icon_indices(lines: &[ResultLine], content: Rect, clip: Rect) -> Vec<usize> {
    lines
        .iter()
        .filter_map(|line| {
            let LineKind::Result(at) = line.kind else {
                return None;
            };
            let rect = Rect::from_min_size(
                content.min + vec2(0.0, line.top),
                vec2(content.width(), line.height),
            );
            rect.intersects(clip).then_some(at)
        })
        .take(foldex_assets::MAX_REQUESTS_PER_VIEW)
        .collect()
}

fn visible_icons(
    manager: &mut crate::assets::AssetManager,
    launcher: &Launcher,
    lines: &[ResultLine],
    content: Rect,
    clip: Rect,
    logical_size: f32,
    ppp: f32,
) -> Vec<(usize, crate::assets::AssetImage)> {
    use crate::assets::{AssetStamp, ViewportSignature, VisibleAsset};
    use std::hash::{Hash, Hasher};
    let indices: Vec<_> = visible_icon_indices(lines, content, clip)
        .into_iter()
        .filter(|at| row_has_native_icon(&launcher.rows[*at]))
        .collect();
    let size = (logical_size * ppp)
        .ceil()
        .clamp(1.0, foldex_assets::MAX_IMAGE_EDGE as f32) as u16;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for at in &indices {
        launcher.rows[*at].id.hash(&mut hash);
    }
    let epoch = launcher.worker.epoch.load(Ordering::Acquire);
    let signature = ViewportSignature {
        epoch,
        revision: hash.finish(),
        first: 0,
        end: indices.len(),
        size,
        thumbnails: false,
    };
    if manager
        .set_requests(Launcher::icon_view_id(), signature, || {
            indices
                .iter()
                .filter_map(|at| row_icon_key(&launcher.rows[*at], size))
                .map(|key| VisibleAsset {
                    key,
                    stamp: AssetStamp::UNOBSERVED,
                })
                .collect()
        })
        .is_err()
    {
        return Vec::new();
    }
    indices
        .iter()
        .enumerate()
        .filter_map(|(slot, at)| {
            manager
                .image(Launcher::icon_view_id(), epoch, slot)
                .map(|image| (*at, image))
        })
        .collect()
}

impl Launcher {
    /// The native viewport owner must use this before hiding on Escape, so an
    /// IME candidate's cancellation never dismisses the launcher underneath it.
    pub fn take_dismiss_key(&self, ctx: &egui::Context) -> bool {
        if self.focus {
            ctx.data_mut(|data| {
                data.remove::<QueryComposition>(Id::new("launcher-query").with("ime"))
            });
        }
        !query_composing(ctx, Id::new("launcher-query")) && consume_plain_key(ctx, Key::Escape)
    }

    /// Root routing should yield while this native launcher is shown. Its
    /// query, header actions and result composite all own local navigation.
    pub fn owns_navigation(&self, ctx: &egui::Context) -> bool {
        self.native.state.opened.load(Ordering::Acquire)
            || ctx.memory(|memory| memory.focused()).is_some_and(|id| {
                [
                    Id::new("launcher-query"),
                    Id::new("launcher-all"),
                    Id::new("launcher-close"),
                    results_focus_id(),
                ]
                .contains(&id)
            })
    }
    /// Painting is independent of action readiness. A valid pending edit keeps
    /// this scene and its geometry; invalid/provider changes remove it in request.
    pub fn calculator_scene(&self) -> Option<usize> {
        if !matches!(self.mode, Mode::All | Mode::Calculator) {
            return None;
        }
        self.rows
            .iter()
            .position(|row| row.group == ResultGroup::Calculator)
    }

    pub(crate) fn calculator_presentation(&self) -> Option<CalculatorPresentation> {
        let at = self.calculator_scene()?;
        let row = &self.rows[at];
        Some(CalculatorPresentation {
            answer: row.title.clone(),
            displayed_query: row.detail.clone(),
            pending: self.calculator.pending,
            copy_enabled: self.row_action_ready(at),
            rolling: self.calculator_paint.rolling,
            answer_rect: self.calculator_paint.answer_rect,
            copy_rect: self.calculator_paint.copy_rect,
        })
    }

    pub fn set_launcher_flags(&mut self, background: bool, clipboard: bool) {
        self.settings.background = background;
        self.settings.clipboard_history = clipboard;
        self.hotkey_edit = self.settings.hotkey.clone();
        self.settings_error = None;
        self.native
            .configure(background, clipboard, &self.settings.hotkey);
        self.settings_changed = true;
        if !clipboard {
            self.clear_text_history();
        }
    }

    pub fn set_open_folder(&mut self, open: super::OpenFolder) {
        self.settings.open_folder = open;
        self.settings_changed = true;
    }

    pub fn clear_text_history(&mut self) {
        self.worker.clear_history.store(true, Ordering::Release);
        let _ = self.worker.signal.try_send(());
        if self.native.state.opened.load(Ordering::Acquire) && self.mode == Mode::Clipboard {
            self.request();
        }
    }

    pub fn apply_launcher_shortcut(&mut self, shortcut: &str) -> Result<(), String> {
        if shortcut.len() > 64 || (!shortcut.is_empty() && native::parse_hotkey(shortcut).is_none())
        {
            return Err("Record a shortcut with a modifier and one key.".into());
        }
        self.hotkey_edit = shortcut.to_owned();
        self.settings_error = None;
        self.native.configure(
            self.settings.background,
            self.settings.clipboard_history,
            shortcut,
        );
        self.settle_launcher_shortcut();
        Ok(())
    }

    /// Commit persisted settings only after Windows accepts the replacement.
    /// The native registration keeps the old binding on any conflict.
    pub fn settle_launcher_shortcut(&mut self) {
        if self.hotkey_edit == self.settings.hotkey {
            return;
        }
        let feedback = self.native.hotkey_feedback();
        if feedback.pending || feedback.requested != self.hotkey_edit {
            return;
        }
        if let Some(error) = feedback.error {
            self.settings_error = Some(error);
        } else if self.hotkey_edit != self.settings.hotkey {
            self.settings.hotkey = self.hotkey_edit.clone();
            self.settings_changed = true;
            self.settings_error = None;
        }
    }

    pub fn launcher_settings_error(&self) -> Option<String> {
        self.launcher_shortcut_error()
            .or_else(|| self.status_error())
    }

    pub fn launcher_shortcut_error(&self) -> Option<String> {
        self.native
            .hotkey_feedback()
            .error
            .or_else(|| self.settings_error.clone())
    }

    pub fn apply_launcher_roots(&mut self, text: &str) -> Result<(), String> {
        if text.len() > 4096 {
            return Err("Search folders must fit within 4096 characters.".into());
        }
        let roots: Vec<_> = text
            .split(';')
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(PathBuf::from)
            .collect();
        if roots.len() > 2 {
            return Err("Choose up to two search folders.".into());
        }
        if roots.iter().any(|path| !path.is_absolute()) {
            return Err("Use an absolute path for each search folder.".into());
        }
        self.roots_edit = text.to_owned();
        self.settings.file_roots = roots;
        self.settings_changed = true;
        self.update_roots();
        if self.native.state.opened.load(Ordering::Acquire) {
            self.request();
        }
        Ok(())
    }

    /// Maximum 640 x 420 at base text size. Settled short searches and calculator
    /// answers fit their content. Typing gets a 140 ms quiet period followed by
    /// a bounded 110 ms resize; hidden windows schedule no animation or polling.
    pub fn render_size(&self, ctx: &egui::Context, t: &Theme, compact: bool) -> [f32; 2] {
        let scale = t.metrics().scale;
        let opened = self.native.state.opened.load(Ordering::Acquire);
        let key = Id::new("launcher-palette-size");
        if compact {
            ctx.data_mut(|data| data.remove::<PaletteSize>(key));
            return [380.0, 320.0];
        }
        if !opened {
            // The retained hidden viewport only needs its last native size.
            // Explorer repaints must not rebuild launcher lines or clone its
            // query just to mark the sizing animation as closed.
            let height = ctx.data_mut(|data| {
                let Some(mut state) = data.get_temp::<PaletteSize>(key) else {
                    return 420.0 * scale;
                };
                state.opened = false;
                let height = state.height;
                data.insert_temp(key, state);
                height
            });
            return [640.0 * scale, height.round()];
        }
        let epoch = self.worker.epoch.load(Ordering::Acquire);
        let discovery = self.mode == Mode::All && self.query.trim().is_empty();
        let calculator = self.calculator_scene();
        let lines = result_lines(
            &self.rows,
            discovery,
            "Results",
            RESULT_HEIGHT * scale,
            SECTION_HEIGHT * scale,
            calculator,
        );
        let content = lines.last().map_or(0.0, |line| line.top + line.height);
        let ideal = if self.mode == Mode::Settings || discovery && self.provider_pending {
            420.0 * scale
        } else {
            // A lone answer fits its row; other scenes keep a steady minimum.
            let floor = if calculator.is_some() { 0.0 } else { 224.0 };
            (content + (PALETTE_HEADER + PALETTE_FOOTER + 16.0) * scale)
                .clamp(floor * scale, 420.0 * scale)
        };
        let now = ctx.input(|input| input.time);
        let duration = f64::from(t.duration().min(0.11));
        let (height, wake_after) = ctx.data_mut(|data| {
            let previous = data.get_temp::<PaletteSize>(key);
            let mut state = previous.unwrap_or_else(|| PaletteSize {
                height: ideal,
                from: ideal,
                target: ideal,
                candidate: ideal,
                changed: now,
                started: now,
                epoch,
                scale,
                opened,
            });
            if !state.opened || (state.scale - scale).abs() > 0.001 {
                state.height = ideal;
                state.from = ideal;
                state.target = ideal;
                state.candidate = ideal;
                state.started = now;
                state.changed = now;
            }
            state.opened = true;
            state.scale = scale;
            if (state.epoch != epoch && calculator.is_none())
                || (state.candidate - ideal).abs() > 0.5
            {
                state.from = state.height;
                state.target = state.height;
                state.started = now;
                state.candidate = ideal;
                state.changed = now;
            }
            state.epoch = epoch;
            let mut wake_after = None;
            if (state.target - state.candidate).abs() > 0.5 {
                let quiet = (0.14 - (now - state.changed)).max(0.0);
                if quiet > 0.0 || self.dirty {
                    wake_after = Some(quiet.max(0.016));
                } else {
                    state.from = state.height;
                    state.target = state.candidate;
                    state.started = now;
                }
            }
            let progress = if duration == 0.0 {
                1.0
            } else {
                ((now - state.started) / duration).clamp(0.0, 1.0)
            };
            let eased = 1.0 - (1.0 - progress).powi(3);
            state.height = state.from + (state.target - state.from) * eased as f32;
            if progress < 1.0 && (state.target - state.height).abs() > 0.25 {
                wake_after = Some(0.0);
            } else {
                state.height = state.target;
            }
            let height = state.height;
            data.insert_temp(key, state);
            (height, wake_after)
        });
        if let Some(delay) = wake_after {
            if delay == 0.0 {
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(Duration::from_secs_f64(delay));
            }
        }
        [640.0 * scale, height.round()]
    }

    pub fn ui(&mut self, ui: &mut Ui, t: &Theme) -> Option<ResultAction> {
        self.ui_inner(ui, t, None)
    }

    pub fn icon_view_id() -> u64 {
        0x4649_4c45_4c41_554e
    }

    pub fn ui_with_assets(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        assets: &mut crate::assets::AssetManager,
    ) -> Option<ResultAction> {
        self.ui_inner(ui, t, Some(assets))
    }

    fn ui_inner(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        mut assets: Option<&mut crate::assets::AssetManager>,
    ) -> Option<ResultAction> {
        let ctx = ui.ctx().clone();
        self.calculator_paint = CalculatorPaint::default();
        let all = ui.max_rect();
        ui.painter().rect_filled(all, 0, Theme::color(t.surface));
        ui.painter()
            .rect_stroke(all, 0, Stroke::new(1.0, t.line()), StrokeKind::Inside);
        if self.mode == Mode::Settings {
            return Some(ResultAction::Builtin(Builtin::Settings));
        }
        let query_id = Id::new("launcher-query");
        if self.focus {
            ctx.data_mut(|data| data.remove::<QueryComposition>(query_id.with("ime")));
        }
        let mut focused = ctx.memory(|memory| memory.focused());
        let results_id = results_focus_id();
        let composing = query_composing(&ctx, query_id);
        let mut stops = vec![query_id];
        if self.mode != Mode::All {
            stops.push(Id::new("launcher-all"));
        }
        stops.extend([Id::new("launcher-close"), results_id]);
        if !composing && focused.is_some_and(|id| stops.contains(&id)) {
            let backwards = crate::commands::take_key(&ctx, Modifiers::SHIFT, Key::Tab);
            if backwards || crate::commands::take_key(&ctx, Modifiers::NONE, Key::Tab) {
                let at = stops
                    .iter()
                    .position(|id| Some(*id) == focused)
                    .unwrap_or(0);
                let next = if backwards {
                    (at + stops.len() - 1) % stops.len()
                } else {
                    (at + 1) % stops.len()
                };
                self.focus = false;
                crate::keyboard::request_id(&ctx, stops[next]);
                focused = Some(stops[next]);
            }
        }
        let result_focused = focused == Some(results_id);
        let navigate = !composing && (focused == Some(query_id) || result_focused);
        let down = navigate && consume_plain_key(&ctx, Key::ArrowDown);
        let up = navigate && consume_plain_key(&ctx, Key::ArrowUp);
        let page_down = navigate && consume_plain_key(&ctx, Key::PageDown);
        let page_up = navigate && consume_plain_key(&ctx, Key::PageUp);
        let home = !composing && result_focused && consume_plain_key(&ctx, Key::Home);
        let end = !composing && result_focused && consume_plain_key(&ctx, Key::End);
        let enter = navigate && consume_plain_key(&ctx, Key::Enter);
        let reveal = navigate && crate::commands::take_key(&ctx, Modifiers::SHIFT, Key::Enter);
        if down || up || page_down || page_up || home || end {
            ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        if result_focused
            && ctx.input(|input| {
                input.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Ime(_)
                    )
                })
            })
        {
            // Typing in the result composite resumes query editing. Clipboard
            // copy/cut chords keep their normal meaning and are never redirected.
            self.focus = true;
        }
        let m = t.metrics();
        // These are the same layout, input, icon, font and action slots used
        // by Commands and GoTo. The native launcher adds no nested search box.
        // Native resizing can lag its animated request. Anchor the footer and
        // scroll viewport to this paint's actual client rect, not the eventual
        // candidate height, so neither growth nor shrink hides the footer or
        // leaves a blank strip beneath it.
        let popup = foldex_ui::components::PopupLayout::new(all, t);
        let header = popup.header;
        let actions = if self.mode == Mode::All { 1 } else { 2 };
        foldex_ui::components::popup_input_icon(ui, header, t, Icon::Search);
        let input = foldex_ui::components::popup_input_rect(header, actions);
        let hint = match self.mode {
            Mode::Apps => "Find an application…",
            Mode::Files => "Find a file or folder…",
            Mode::Windows => "Switch to a window…",
            Mode::Calculator => "Calculate or convert…",
            Mode::Clipboard => "Find copied text…",
            Mode::Actions => "Find an action…",
            _ => "Search anything…",
        };
        let mut changed = false;
        ui.scope_builder(UiBuilder::new().max_rect(input), |ui| {
            let acquire_focus = self.focus;
            if acquire_focus {
                ui.memory_mut(|memory| memory.request_focus(query_id));
            }
            let output = foldex_ui::components::text_input_at(
                ui,
                input,
                query_id,
                t,
                &mut self.query,
                hint,
                512,
            );
            foldex_ui::components::label_control(&output.response, hint);
            if acquire_focus {
                self.focus = ui.is_sizing_pass() || !output.response.has_focus();
            }
            if output.response.has_focus() {
                ui.memory_mut(|memory| {
                    memory.set_focus_lock_filter(
                        query_id,
                        egui::EventFilter {
                            tab: false,
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            escape: true,
                        },
                    )
                });
            }
            changed = output.response.changed();
        });
        if changed {
            self.status.clear();
            self.request();
        }
        if actions == 2 {
            let back = foldex_ui::components::popup_action_rect(header, 0, actions);
            if foldex_ui::components::icon_button_at(
                ui,
                back,
                Id::new("launcher-all"),
                t,
                Icon::Back,
                false,
            )
            .on_hover_text("Search everything")
            .clicked()
            {
                self.set_mode(Mode::All);
            }
        }
        let close = foldex_ui::components::popup_action_rect(header, actions - 1, actions);
        if foldex_ui::components::icon_button_at(
            ui,
            close,
            Id::new("launcher-close"),
            t,
            Icon::Close,
            false,
        )
        .on_hover_text("Close · Escape")
        .clicked()
        {
            return Some(ResultAction::Builtin(Builtin::Dismiss));
        }
        ui.painter().line_segment(
            [header.left_bottom(), header.right_bottom()],
            Stroke::new(1.0, t.line()),
        );
        let footer = popup.footer;
        let body = Rect::from_min_max(
            popup.body.min + vec2(4.0 * m.scale, 6.0 * m.scale),
            popup.body.max - vec2(4.0 * m.scale, 6.0 * m.scale),
        );
        let results = ui.interact(body, results_id, Sense::focusable_noninteractive());
        results.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, ui.is_enabled(), "Search results")
        });
        ctx.accesskit_node_builder(results_id, |node| {
            node.set_role(egui::accesskit::Role::ListBox)
        });
        if results.has_focus() {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    results_id,
                    egui::EventFilter {
                        tab: false,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                )
            });
        }
        let calculator = self.calculator_scene();
        let compact = all.width() < 480.0 * m.scale && calculator.is_none();
        let row_height = if compact { 32.0 } else { RESULT_HEIGHT } * m.scale;
        let section_height = if compact { 24.0 } else { SECTION_HEIGHT } * m.scale;
        let readable_row = (row_height * 0.5 + t.font_size * 0.5 + 2.0 * m.scale).min(row_height);
        let discovery = self.mode == Mode::All && self.query.trim().is_empty();
        let label = match self.mode {
            Mode::Apps => "Applications",
            Mode::Files => "Files and folders",
            Mode::Windows => "Windows",
            Mode::Calculator => "Calculator",
            Mode::Clipboard => "Clipboard",
            Mode::Actions => "Actions",
            _ => "Results",
        };
        let lines = result_lines(
            &self.rows,
            discovery,
            label,
            row_height,
            section_height,
            calculator,
        );
        let visible = presented_indices(&lines);
        let page = (body.height() / row_height).floor().max(1.0) as usize;
        if !self.dirty {
            let at = visible
                .iter()
                .position(|at| *at == self.selected)
                .unwrap_or(0);
            let at = at
                .saturating_add(usize::from(down) + usize::from(page_down) * page)
                .min(visible.len().saturating_sub(1));
            let at = at.saturating_sub(usize::from(up) + usize::from(page_up) * page);
            let at = if home {
                0
            } else if end {
                visible.len().saturating_sub(1)
            } else {
                at
            };
            self.selected = visible.get(at).copied().unwrap_or(0);
            self.scroll_to |=
                down || up || page_down || page_up || home || end || results.gained_focus();
        }
        // Both activation chords must refer to the current, committed query.
        let mut activate = ((enter || reveal)
            && !changed
            && self.row_action_ready(self.selected)
            && visible.contains(&self.selected))
        .then_some((self.selected, reveal));
        let content_height = lines.last().map_or(0.0, |line| line.top + line.height);
        self.motion.set_pixels_per_point(ctx.pixels_per_point());
        self.motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(
                &ctx,
                t.duration().min(0.08),
            ));
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            ui.set_clip_rect(body.intersect(all).intersect(ui.clip_rect()));
            ui.spacing_mut().item_spacing.y = 0.0;
            result_scroll_area(ui, t, all, body, content_height).show(ui, |ui| {
                let (content, _) = ui.allocate_exact_size(
                    vec2(ui.available_width(), content_height),
                    Sense::hover(),
                );
                if self.scroll_to && body.height() >= row_height && !ui.is_sizing_pass() {
                    if let Some(line) = lines.iter().find(|line| {
                        matches!(line.kind,
                        LineKind::Result(at) | LineKind::Calculator(at) if at == self.selected)
                    }) {
                        ui.scroll_to_rect_animation(
                            Rect::from_min_size(
                                content.min + vec2(0.0, line.top),
                                vec2(1.0, line.height.min(body.height())),
                            ),
                            None,
                            egui::style::ScrollAnimation::none(),
                        );
                    }
                    self.scroll_to = false;
                }
                let clip = ui.clip_rect().intersect(body);
                let native_images = if let Some(manager) = assets.as_deref_mut() {
                    visible_icons(
                        manager,
                        self,
                        &lines,
                        content,
                        clip,
                        m.icon,
                        ctx.pixels_per_point(),
                    )
                } else {
                    Vec::new()
                };
                for (line_number, line) in lines.iter().enumerate() {
                    let target = Rect::from_min_size(
                        content.min + vec2(0.0, line.top),
                        vec2(content.width(), line.height),
                    );
                    if !target.intersects(clip.expand(row_height)) {
                        continue;
                    }
                    match line.kind {
                        LineKind::Heading(label, hint) => {
                            let companion_minimum = lines
                                .get(line_number + 1)
                                .map_or(readable_row, |next| readable_row.min(next.height));
                            if !heading_has_companion(target, clip, companion_minimum) {
                                continue;
                            }
                            let p = ui.painter().with_clip_rect(clip);
                            let font = FontId::proportional((t.font_size - 1.0).max(10.0));
                            p.text(
                                pos2(
                                    target.left() + 12.0 * m.scale,
                                    target.center().y + 2.0 * m.scale,
                                ),
                                Align2::LEFT_CENTER,
                                label,
                                font.clone(),
                                t.fg().lerp_to_gamma(t.dim(), 0.7),
                            );
                            p.text(
                                pos2(
                                    target.right() - 12.0 * m.scale,
                                    target.center().y + 2.0 * m.scale,
                                ),
                                Align2::RIGHT_CENTER,
                                hint,
                                font,
                                t.dim(),
                            );
                        }
                        LineKind::Empty(message) => {
                            if !readable_at_bottom(target, clip, readable_row.min(line.height)) {
                                continue;
                            }
                            let mut small = t.clone();
                            small.font_size = (t.font_size - 1.0).max(10.0);
                            foldex_ui::components::elided_label(
                                &ui.painter().with_clip_rect(clip),
                                target.shrink2(vec2(12.0 * m.scale, 0.0)),
                                &small,
                                message,
                                t.dim(),
                                false,
                            );
                        }
                        LineKind::Calculator(at) => {
                            if self.calculator_ui(ui, target, clip, t, at) {
                                self.selected = at;
                                activate = Some((at, false));
                            }
                        }
                        LineKind::Result(at) => {
                            let row = &self.rows[at];
                            let placement = self.motion.place_in_view(
                                Id::new(("launcher-row", row.id)),
                                target.translate(-content.min.to_vec2()),
                                false,
                                clip.translate(-content.min.to_vec2()),
                            );
                            let rect = placement.rect.translate(content.min.to_vec2());
                            if !readable_at_bottom(rect, clip, readable_row) {
                                continue;
                            }
                            let response = ui.interact(
                                rect.intersect(clip),
                                Id::new(("launcher-result", row.id)),
                                Sense::CLICK,
                            );
                            crate::commands::result_row(
                                ui,
                                &response,
                                results_id,
                                query_id,
                                &row.title,
                                self.selected == at,
                                !self.dirty,
                            );
                            let selected = self.selected == at;
                            let hover = foldex_ui::animated_state(
                                &ctx,
                                response.id.with("hover"),
                                response.hovered(),
                                t,
                                MotionProfile::Fast,
                            );
                            let p = ui.painter().with_clip_rect(clip);
                            p.rect_filled(
                                rect,
                                t.radius,
                                Theme::color(t.hover)
                                    .gamma_multiply(hover * if selected { 0.0 } else { 1.0 }),
                            );
                            p.rect_filled(
                                rect,
                                t.radius,
                                Theme::color(t.selection).gamma_multiply(if selected {
                                    1.0
                                } else {
                                    0.0
                                }),
                            );
                            if selected
                                && result_focused
                                && foldex_ui::focus::keyboard_visible(ui.ctx())
                            {
                                p.rect_stroke(
                                    rect,
                                    t.radius,
                                    Stroke::new(1.0, t.accent()),
                                    StrokeKind::Inside,
                                );
                            }
                            if let Some((_, image)) =
                                native_images.iter().find(|(index, _)| *index == at)
                            {
                                let center = pos2(rect.left() + 20.0 * m.scale, rect.center().y);
                                if image.previous.is_none() && image.opacity < 1.0 {
                                    icon(
                                        &p,
                                        center,
                                        row.icon,
                                        t.dim().gamma_multiply(1.0 - image.opacity),
                                        m.icon,
                                    );
                                }
                                if let Some(previous) = image.previous {
                                    let fit =
                                        m.icon / previous.size.x.max(previous.size.y).max(1.0);
                                    p.image(
                                        previous.texture,
                                        Rect::from_center_size(center, previous.size * fit),
                                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                        egui::Color32::WHITE.gamma_multiply(
                                            previous.opacity * (1.0 - image.opacity),
                                        ),
                                    );
                                }
                                let fit = m.icon / image.size.x.max(image.size.y).max(1.0);
                                p.image(
                                    image.texture,
                                    Rect::from_center_size(center, image.size * fit),
                                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                    egui::Color32::WHITE.gamma_multiply(image.opacity),
                                );
                            } else {
                                icon(
                                    &p,
                                    pos2(rect.left() + 20.0 * m.scale, rect.center().y),
                                    row.icon,
                                    if matches!(row.action, ResultAction::Browse(_)) {
                                        Theme::color(t.folder).lerp_to_gamma(t.fg(), 0.25)
                                    } else {
                                        t.dim()
                                    },
                                    m.icon,
                                );
                            }
                            let context = result_context(row, discovery);
                            let mut small = t.clone();
                            small.font_size = (t.font_size - 1.0).max(10.0);
                            let meta_width = p
                                .layout_no_wrap(context.clone(), small.font(), t.dim())
                                .size()
                                .x
                                .min(rect.width() * 0.34);
                            let end = rect.right() - 12.0 * m.scale;
                            let meta = Rect::from_min_max(
                                pos2(end - meta_width, rect.top()),
                                pos2(end, rect.bottom()),
                            );
                            let title = Rect::from_min_max(
                                pos2(rect.left() + 40.0 * m.scale, rect.top()),
                                pos2(
                                    (meta.left() - 18.0 * m.scale)
                                        .max(rect.left() + 40.0 * m.scale),
                                    rect.bottom(),
                                ),
                            );
                            foldex_ui::components::elided_label(
                                &p,
                                title,
                                t,
                                &row.title,
                                t.fg(),
                                false,
                            );
                            foldex_ui::components::elided_label(
                                &p,
                                meta,
                                &small,
                                &context,
                                t.dim(),
                                true,
                            );
                            if response.clicked() && !self.dirty {
                                results.request_focus();
                                self.selected = at;
                                activate = Some((at, false));
                            }
                            response.on_hover_text(&row.detail);
                        }
                    }
                }
            });
        });
        if self.motion.is_moving() {
            ctx.request_repaint();
        }
        if result_focused && visible.is_empty() && foldex_ui::focus::keyboard_visible(ui.ctx()) {
            ui.painter().rect_stroke(
                body.shrink(1.0),
                t.radius,
                Stroke::new(1.0, t.accent()),
                StrokeKind::Inside,
            );
        }
        if self.rows.is_empty() && !discovery {
            let message = match self.mode {
                Mode::Files if self.query.chars().count() < 2 => {
                    "Type two or more characters to find a file or folder."
                }
                Mode::Calculator => "Try 24 * 18, sqrt(81), or 12 km to mi.",
                _ if self.provider_pending || self.searching_files() => "Searching…",
                Mode::Clipboard => "Copied text will appear here.",
                _ => "No results. Try a different name.",
            };
            foldex_ui::components::message_at(
                ui,
                Rect::from_min_max(body.min + vec2(0.0, SECTION_HEIGHT * m.scale), body.max),
                Id::new("launcher-empty"),
                t,
                message,
                t.dim(),
            );
        }
        ui.painter().hline(
            footer.x_range(),
            footer.top(),
            Stroke::new(1.0, t.line().gamma_multiply(0.5)),
        );
        if self.pending_copy.is_some() || !self.status.is_empty() {
            let mut small = t.clone();
            small.font_size = (t.font_size - 1.0).max(10.0);
            foldex_ui::components::elided_label(
                ui.painter(),
                footer.shrink2(vec2(14.0 * m.scale, 0.0)),
                &small,
                if self.pending_copy.is_some() {
                    "Copying…"
                } else {
                    &self.status
                },
                t.dim(),
                false,
            );
        } else {
            use foldex_ui::components::PopupHint;
            let activation_focus = !composing
                && ctx.memory(|memory| {
                    memory
                        .focused()
                        .is_some_and(|id| id == query_id || id == results_id)
                });
            let selected = self.rows.get(self.selected).filter(|_| {
                activation_focus
                    && self.row_action_ready(self.selected)
                    && visible.contains(&self.selected)
            });
            let mut full = vec![PopupHint::new("Tab", "Focus")];
            let mut compact = Vec::new();
            if let Some(row) = selected {
                full.push(PopupHint::new("↑↓", "Navigate"));
                let action = match &row.action {
                    ResultAction::Copy(_) => "Copy",
                    ResultAction::Window(_) => "Switch",
                    ResultAction::Builtin(_) => "Run",
                    _ => "Open",
                };
                full.push(PopupHint::new("Enter", action));
                compact.push(PopupHint::new("Enter", action));
                if row.action.reveal_path().is_some() {
                    full.push(PopupHint::new("Shift+Enter", "Reveal"));
                    compact.push(PopupHint::new("Shift+Enter", "Reveal"));
                }
            }
            full.push(PopupHint::new("Esc", "Close"));
            compact.push(PopupHint::new("Esc", "Close"));
            foldex_ui::components::popup_hints(ui.painter(), footer, t, &full, &compact);
        }
        if let Some(id) = ctx
            .memory(|memory| memory.focused())
            .filter(|id| stops.contains(id))
        {
            ctx.memory_mut(|memory| {
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
        }
        activate.and_then(|(at, reveal)| {
            if !self.row_action_ready(at) {
                return None;
            }
            let row = self.rows.get(at)?;
            if reveal {
                row.action
                    .reveal_path()
                    .map(|path| ResultAction::Reveal(path.to_path_buf()))
            } else {
                Some(row.action.clone())
            }
        })
    }

    fn calculator_ui(&mut self, ui: &mut Ui, rect: Rect, clip: Rect, t: &Theme, at: usize) -> bool {
        let row = self.rows[at].clone();
        let ready = self.row_action_ready(at);
        let m = t.metrics();
        let p = ui.painter().with_clip_rect(clip.intersect(rect));
        let small = FontId::proportional((t.font_size - 1.0).max(10.0));
        p.text(
            pos2(
                rect.left() + 12.0 * m.scale,
                rect.top() + (SECTION_HEIGHT * 0.5 + 2.0) * m.scale,
            ),
            Align2::LEFT_CENTER,
            "Calculator",
            small.clone(),
            t.fg().lerp_to_gamma(t.dim(), 0.7),
        );
        let card = Rect::from_min_size(
            rect.min + vec2(4.0, SECTION_HEIGHT) * m.scale,
            vec2(rect.width() - 8.0 * m.scale, 56.0 * m.scale),
        );
        let selected = self.selected == at;
        let radius = t.radius.saturating_add(2);
        // Filled once hover is known, beneath the row's content.
        let background = p.add(egui::Shape::Noop);
        // An "=" mark, drawn so it sits optically centred in its badge.
        let badge = Rect::from_center_size(
            pos2(card.left() + 26.0 * m.scale, card.center().y),
            vec2(24.0, 24.0) * m.scale,
        );
        p.rect_filled(badge, t.radius, Theme::color(t.chrome));
        p.rect_stroke(
            badge,
            t.radius,
            Stroke::new(1.0, t.line()),
            StrokeKind::Inside,
        );
        for offset in [-2.5, 2.5] {
            let y = badge.center().y + offset * m.scale;
            p.line_segment(
                [
                    pos2(badge.center().x - 5.0 * m.scale, y),
                    pos2(badge.center().x + 5.0 * m.scale, y),
                ],
                Stroke::new(1.5 * m.scale, t.fg()),
            );
        }
        let hint = if ready {
            "Enter to copy"
        } else {
            "Updating…"
        };
        // Both hints share one slot so a pending answer never moves the row.
        let hint_width = ["Enter to copy", "Updating…"]
            .map(|text| {
                p.layout_no_wrap(text.into(), small.clone(), t.dim())
                    .size()
                    .x
            })
            .into_iter()
            .fold(0.0, f32::max);
        let copy = Rect::from_min_max(
            pos2(card.right() - hint_width - 28.0 * m.scale, card.top()),
            card.max,
        );
        let answer = Rect::from_min_max(
            pos2(badge.right() + 14.0 * m.scale, card.top()),
            pos2(copy.left() - 8.0 * m.scale, card.bottom()),
        );
        let font = FontId::proportional(24.0 * m.scale);
        let options = foldex_ui::rolling::RollingOptions {
            align: egui::Align::LEFT,
            ..foldex_ui::rolling::RollingOptions::from_theme(t)
        };
        let number_id = Id::new("launcher-calculator-answer");
        let measured = p
            .layout_no_wrap(row.title.clone(), font.clone(), t.fg())
            .size()
            .x;
        let rolling = if let Some((mantissa, exponent)) =
            scientific_parts(&row.title).filter(|_| measured <= answer.width())
        {
            let width = p
                .layout_no_wrap(mantissa.to_owned(), font.clone(), t.fg())
                .size()
                .x;
            let marker = p.layout_no_wrap("e".into(), font.clone(), t.fg());
            let marker_x = answer.left() + width;
            let exponent_x = marker_x + marker.size().x;
            let mantissa_rect = Rect::from_min_max(answer.min, pos2(marker_x, answer.bottom()));
            let exponent_rect = Rect::from_min_max(pos2(exponent_x, answer.top()), answer.max);
            let main = self.calculator_numbers.paint_fitted(
                &p,
                number_id,
                mantissa_rect,
                mantissa,
                font.clone(),
                t.fg(),
                options,
            );
            p.with_clip_rect(answer.intersect(p.clip_rect())).galley(
                pos2(marker_x, answer.center().y - marker.size().y * 0.5),
                marker,
                t.fg(),
            );
            let power = self.calculator_numbers.paint_fitted(
                &p,
                number_id.with("exponent"),
                exponent_rect,
                exponent,
                font,
                t.fg(),
                options,
            );
            main.moving || power.moving
        } else {
            self.calculator_numbers
                .paint(&p, number_id, answer, &row.title, font, t.fg(), options)
                .moving
        };
        self.calculator_paint = CalculatorPaint {
            answer_rect: Some(answer),
            copy_rect: Some(copy),
            rolling,
        };
        let select = Rect::from_min_max(card.min, pos2(copy.left(), card.bottom()));
        let response = ui.interact(
            select.intersect(clip),
            Id::new("launcher-calculator-result"),
            Sense::CLICK,
        );
        crate::commands::result_row(
            ui,
            &response,
            results_focus_id(),
            Id::new("launcher-query"),
            &format!("Copy result {}", row.title),
            selected,
            ready,
        );
        let hover = foldex_ui::animated_state(
            ui.ctx(),
            response.id.with("hover"),
            response.hovered(),
            t,
            MotionProfile::Fast,
        );
        p.set(
            background,
            if selected {
                egui::epaint::RectShape::new(
                    card,
                    radius,
                    Theme::color(t.selection),
                    Stroke::new(1.0, t.line()),
                    StrokeKind::Inside,
                )
            } else {
                egui::epaint::RectShape::filled(
                    card,
                    radius,
                    Theme::color(t.hover).gamma_multiply(hover),
                )
            },
        );
        if selected
            && ui.memory(|memory| memory.has_focus(results_focus_id()))
            && foldex_ui::focus::keyboard_visible(ui.ctx())
        {
            p.rect_stroke(
                card,
                radius,
                Stroke::new(1.0, t.accent()),
                StrokeKind::Inside,
            );
        }
        // The hint is also the copy target, so the row carries no extra icon.
        // Copy is this result's activation, so it shares the result composite's
        // Tab stop while retaining its explicit mouse target.
        let copied = if copy.intersects(clip) {
            let hit = ui.interact(copy.intersect(clip), calculator_copy_id(), Sense::CLICK);
            hit.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    ui.is_enabled() && ready,
                    "Copy result",
                )
            });
            let hover = foldex_ui::animated_state(
                ui.ctx(),
                hit.id.with("hover"),
                hit.hovered() && ready,
                t,
                MotionProfile::Fast,
            );
            p.text(
                pos2(copy.right() - 16.0 * m.scale, copy.center().y),
                Align2::RIGHT_CENTER,
                hint,
                small,
                t.dim().lerp_to_gamma(t.fg(), hover),
            );
            hit.on_hover_text(if ready {
                "Copy result · Enter"
            } else {
                "Updating result…"
            })
            .clicked()
        } else {
            false
        };
        let clicked = response.clicked() || copied;
        // A pending answer can still transfer focus to the result composite.
        // Otherwise a blocked copy click drops the query's focus and leaves
        // Enter without an owner after the current answer becomes ready.
        if clicked {
            crate::keyboard::request_id(ui.ctx(), results_focus_id());
        }
        ready && clicked
    }
}

#[cfg(test)]
impl Launcher {
    /// Shows a settled calculator answer for the offscreen catalog without
    /// starting providers.
    pub(crate) fn catalog_calculator(&mut self, query: &str) {
        self.worker.stop.store(true, Ordering::Release);
        self.native.state.opened.store(true, Ordering::Release);
        self.mode = Mode::All;
        self.query = query.into();
        let answer = calculator_row(Mode::All, query);
        self.calculator.begin(0, answer.clone());
        self.calculator.finish(0, answer.clone());
        self.rows = answer.into_iter().collect();
        self.selected = 0;
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            modifiers,
            physical_key: None,
            pressed: true,
            repeat: false,
        }
    }

    struct CalculatorFixture {
        launcher: Launcher,
        _hub: SearchHub,
        ctx: egui::Context,
        theme: Theme,
        frame: u64,
    }
    impl CalculatorFixture {
        fn new(mode: Mode) -> Self {
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
            let hub = SearchHub::new(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/launcher-calculator-ui-fixtures"),
                wake.clone(),
            )
            .unwrap();
            let mut launcher = Launcher::new(Settings::default(), &hub, wake, true, None).unwrap();
            launcher.mode = mode;
            launcher.query = "6 * 7".into();
            // Replies are supplied explicitly so input/reply ordering stays
            // deterministic, without waiting or starting native providers.
            launcher.worker.stop.store(true, Ordering::Release);
            let _ = launcher.worker.signal.try_send(());
            let answer = calculator_row(Mode::Calculator, &launcher.query).unwrap();
            launcher.calculator.begin(0, Some(answer.clone()));
            launcher.calculator.finish(0, Some(answer.clone()));
            launcher.rows = vec![answer];
            launcher.dirty = false;
            let ctx = egui::Context::default();
            let theme = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            theme.apply(&ctx);
            Self {
                launcher,
                _hub: hub,
                ctx,
                theme,
                frame: 0,
            }
        }

        fn with_result(action: ResultAction) -> Self {
            let mut f = Self::new(Mode::Files);
            f.launcher.calculator = CalculatorResult::default();
            f.launcher.query = "needle".into();
            f.launcher.rows = vec![Arc::new(row(
                "Needle".into(),
                "Result context".into(),
                Icon::File,
                action,
                1,
            ))];
            f
        }

        fn response(&self, epoch: u64, query: &str) -> Response {
            Response {
                epoch,
                rows: vec![],
                calculator: calculator_row(self.launcher.mode, query),
                default_roots: None,
                complete: true,
            }
        }

        fn commit(&mut self) {
            let response = self.response(
                self.launcher.worker.epoch.load(Ordering::Acquire),
                &self.launcher.query,
            );
            self.launcher.accept_response(response);
            assert!(self.launcher.poll().is_none());
        }

        fn caret_at_end(&self) {
            let query_id = Id::new("launcher-query");
            crate::keyboard::request_id(&self.ctx, query_id);
            let mut state = egui::TextEdit::load_state(&self.ctx, query_id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(self.launcher.query.chars().count()),
                )));
            state.store(&self.ctx, query_id);
        }

        fn draw(&mut self, events: Vec<egui::Event>) -> (Option<ResultAction>, Option<Rect>) {
            let (action, copy, output) = self.render(events);
            output.drop_without_applying_deltas();
            (action, copy)
        }

        fn render(
            &mut self,
            events: Vec<egui::Event>,
        ) -> (Option<ResultAction>, Option<Rect>, egui::FullOutput) {
            let mut action = None;
            let mut copy = None;
            let copy_id = self
                .launcher
                .calculator_scene()
                .map(|_| calculator_copy_id());
            let output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, 230.0))),
                    time: Some(self.frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    if let Some(next) = self.launcher.ui(ui, &self.theme) {
                        action = Some(next);
                    }
                    copy = copy_id.and_then(|id| ui.ctx().read_response(id).map(|hit| hit.rect));
                },
            );
            self.frame += 1;
            (action, copy, output)
        }
    }

    #[test]
    fn filesystem_results_enter_opens_and_shift_enter_reveals_from_query_and_results() {
        let path = std::env::temp_dir().join("foldex-launcher-result");
        for (mode, directory) in [
            (Mode::All, false),
            (Mode::All, true),
            (Mode::Files, false),
            (Mode::Files, true),
        ] {
            let action = if directory {
                ResultAction::Browse(path.clone())
            } else {
                ResultAction::Open(path.clone())
            };
            let mut f = CalculatorFixture::with_result(action);
            f.launcher.mode = mode;
            f.draw(vec![]);
            for focus in [Id::new("launcher-query"), results_focus_id()] {
                f.launcher.focus = false;
                crate::keyboard::request_id(&f.ctx, focus);
                f.draw(vec![]);
                let (action, _) = f.draw(vec![key(Key::Enter, Modifiers::NONE)]);
                assert!(match action {
                    Some(ResultAction::Browse(target)) if directory => target == path,
                    Some(ResultAction::Open(target)) if !directory => target == path,
                    _ => false,
                });
                let (action, _) = f.draw(vec![key(Key::Enter, Modifiers::SHIFT)]);
                assert!(matches!(action, Some(ResultAction::Reveal(target)) if target == path));
                assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(focus));
                assert_eq!(f.launcher.query, "needle");
                assert!(
                    f.draw(vec![key(Key::Enter, Modifiers::CTRL | Modifiers::SHIFT)])
                        .0
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn shift_enter_never_falls_back_to_copy_or_non_filesystem_activation() {
        for action in [
            ResultAction::Copy("sensitive copied text".into()),
            ResultAction::Window(0),
            ResultAction::Application(Vec::<u8>::new().into()),
            ResultAction::Builtin(Builtin::Explorer),
            ResultAction::Open(PathBuf::from("relative-result.txt")),
        ] {
            let mut f = CalculatorFixture::with_result(action);
            f.draw(vec![]);
            for focus in [Id::new("launcher-query"), results_focus_id()] {
                f.launcher.focus = false;
                crate::keyboard::request_id(&f.ctx, focus);
                f.draw(vec![]);
                assert!(f.draw(vec![key(Key::Enter, Modifiers::SHIFT)]).0.is_none());
                assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(focus));
                assert!(f.launcher.pending_copy.is_none());
                assert!(f.launcher.worker.copy.lock().unwrap().is_none());
            }
        }
        let mut calculator = CalculatorFixture::new(Mode::Calculator);
        calculator.draw(vec![]);
        for focus in [Id::new("launcher-query"), results_focus_id()] {
            calculator.launcher.focus = false;
            crate::keyboard::request_id(&calculator.ctx, focus);
            calculator.draw(vec![]);
            assert!(
                calculator
                    .draw(vec![key(Key::Enter, Modifiers::SHIFT)])
                    .0
                    .is_none()
            );
            let (action, _) = calculator.draw(vec![key(Key::Enter, Modifiers::NONE)]);
            assert!(matches!(action, Some(ResultAction::Copy(answer)) if answer == "42"));
        }
    }

    #[test]
    fn launcher_activation_rejects_dirty_edited_and_composing_queries_in_both_focus_owners() {
        let path = std::env::temp_dir().join("foldex-launcher-result.txt");
        for (focus, modifiers) in [
            (Id::new("launcher-query"), Modifiers::NONE),
            (Id::new("launcher-query"), Modifiers::SHIFT),
            (results_focus_id(), Modifiers::NONE),
            (results_focus_id(), Modifiers::SHIFT),
        ] {
            let mut dirty = CalculatorFixture::with_result(ResultAction::Open(path.clone()));
            dirty.draw(vec![]);
            dirty.launcher.focus = false;
            crate::keyboard::request_id(&dirty.ctx, focus);
            dirty.draw(vec![]);
            dirty.launcher.dirty = true;
            assert!(dirty.draw(vec![key(Key::Enter, modifiers)]).0.is_none());

            let mut edited = CalculatorFixture::with_result(ResultAction::Open(path.clone()));
            edited.draw(vec![]);
            edited.launcher.focus = false;
            crate::keyboard::request_id(&edited.ctx, focus);
            edited.draw(vec![]);
            assert!(
                edited
                    .draw(vec![
                        egui::Event::Text("0".into()),
                        key(Key::Enter, modifiers),
                    ])
                    .0
                    .is_none()
            );
            assert_ne!(edited.launcher.query, "needle");
            assert!(edited.launcher.dirty);

            let mut composing = CalculatorFixture::with_result(ResultAction::Open(path.clone()));
            composing.draw(vec![]);
            composing.launcher.focus = false;
            crate::keyboard::request_id(&composing.ctx, focus);
            composing.draw(vec![]);
            for events in [
                vec![
                    egui::Event::Ime(egui::ImeEvent::Preedit {
                        text: "に".into(),
                        active_range_chars: None,
                    }),
                    key(Key::Enter, modifiers),
                ],
                vec![key(Key::Enter, modifiers)],
                vec![
                    egui::Event::Ime(egui::ImeEvent::Commit("日本".into())),
                    key(Key::Enter, modifiers),
                ],
            ] {
                assert!(composing.draw(events).0.is_none());
            }
        }
    }

    #[test]
    fn launcher_footer_starts_at_the_left_gutter_and_only_offers_valid_reveal_actions() {
        let path = std::env::temp_dir().join("foldex-launcher-result.txt");
        for (action, dirty, revealable) in [
            (ResultAction::Open(path.clone()), false, true),
            (ResultAction::Browse(path), false, true),
            (ResultAction::Copy("42".into()), false, false),
            (ResultAction::Window(0), false, false),
            (
                ResultAction::Open(PathBuf::from("relative.txt")),
                false,
                false,
            ),
            (ResultAction::Open(std::env::temp_dir()), true, false),
        ] {
            let mut f = CalculatorFixture::with_result(action);
            f.launcher.dirty = dirty;
            f.draw(vec![]);
            let (_, _, output) = f.render(vec![]);
            let footer_top = 230.0 - PALETTE_FOOTER * f.theme.metrics().scale;
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if shape.clip_rect.top() >= footer_top => Some(text),
                    _ => None,
                })
                .collect();
            assert!(text.iter().all(|text| text.galley.text() != "Foldex"));
            assert_eq!(
                text.iter().any(|text| text.galley.text() == "Reveal"),
                revealable
            );
            assert!(text.iter().all(|text| text.fallback_color == f.theme.dim()));
            let left = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if shape.clip_rect.top() >= footer_top => {
                        Some(rect.rect.left())
                    }
                    _ => None,
                })
                .reduce(f32::min)
                .expect("Footer must expose keyboard badges");
            assert!((left - 14.0 * f.theme.metrics().scale).abs() < 0.51);
            output.drop_without_applying_deltas();
            if revealable {
                f.launcher.focus = false;
                crate::keyboard::request_id(&f.ctx, Id::new("launcher-close"));
                let (_, _, output) = f.render(vec![]);
                assert!(!output.shapes.iter().any(|shape| {
                    shape.clip_rect.top() >= footer_top
                        && matches!(&shape.shape, egui::Shape::Text(text)
                            if matches!(text.galley.text(), "Reveal" | "Enter"))
                }));
                output.drop_without_applying_deltas();
            }
        }
    }

    #[test]
    fn calculator_enter_uses_the_copy_action_from_query_and_results_without_writing_clipboard() {
        for mode in [Mode::All, Mode::Calculator] {
            let mut f = CalculatorFixture::new(mode);
            f.draw(vec![]);
            for focus in [Id::new("launcher-query"), results_focus_id()] {
                f.launcher.focus = false;
                crate::keyboard::request_id(&f.ctx, focus);
                f.draw(vec![]);
                let (action, _) = f.draw(vec![key(Key::Enter, Modifiers::NONE)]);
                assert!(matches!(action, Some(ResultAction::Copy(answer)) if answer == "42"));
                assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(focus));
                assert!(f.launcher.pending_copy.is_none());
                assert!(f.launcher.worker.copy.lock().unwrap().is_none());
            }
        }
    }

    #[test]
    fn calculator_enter_cannot_copy_during_edits_composition_or_modified_chords() {
        let mut f = CalculatorFixture::new(Mode::Calculator);
        f.draw(vec![]);
        for modifiers in [Modifiers::SHIFT, Modifiers::CTRL, Modifiers::ALT] {
            crate::keyboard::request_id(&f.ctx, Id::new("launcher-query"));
            assert!(f.draw(vec![key(Key::Enter, modifiers)]).0.is_none());
        }
        crate::keyboard::request_id(&f.ctx, Id::new("launcher-query"));
        assert!(
            f.draw(vec![
                egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: "に".into(),
                    active_range_chars: None,
                }),
                key(Key::Enter, Modifiers::NONE),
            ])
            .0
            .is_none()
        );
        assert!(f.draw(vec![key(Key::Enter, Modifiers::NONE)]).0.is_none());

        let mut edited = CalculatorFixture::new(Mode::Calculator);
        edited.draw(vec![]);
        let (action, _) = edited.draw(vec![
            egui::Event::Text("0".into()),
            key(Key::Enter, Modifiers::NONE),
        ]);
        assert_ne!(edited.launcher.query, "6 * 7");
        assert!(action.is_none());
        assert!(edited.launcher.dirty);
    }

    #[test]
    fn calculator_copy_icon_returns_the_answer_and_keeps_the_result_focus_owner() {
        let mut f = CalculatorFixture::new(Mode::Calculator);
        f.draw(vec![]);
        f.launcher.focus = false;
        crate::keyboard::request_id(&f.ctx, results_focus_id());
        let (_, copy) = f.draw(vec![]);
        let copy = copy.expect("Calculator must expose its copy target");
        assert!(copy.width() > copy.height());
        let pointer = |pressed| egui::Event::PointerButton {
            pos: copy.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        assert!(
            f.draw(vec![
                egui::Event::PointerMoved(copy.center()),
                pointer(true)
            ])
            .0
            .is_none()
        );
        let (action, _) = f.draw(vec![pointer(false)]);
        assert!(matches!(action, Some(ResultAction::Copy(answer)) if answer == "42"));
        assert_eq!(
            f.ctx.memory(|memory| memory.focused()),
            Some(results_focus_id())
        );
        assert!(f.launcher.pending_copy.is_none());
        assert!(f.launcher.worker.copy.lock().unwrap().is_none());
    }

    #[test]
    fn calculator_append_keeps_the_painted_answer_and_size_until_the_current_reply() {
        for mode in [Mode::All, Mode::Calculator] {
            let mut f = CalculatorFixture::new(mode);
            f.theme.reduced_motion = false;
            f.theme.apply(&f.ctx);
            f.launcher
                .native
                .state
                .opened
                .store(true, Ordering::Release);
            f.launcher.set_query("2+2");
            f.commit();
            f.draw(vec![]);
            let initial = f.launcher.calculator_presentation().unwrap();
            assert_eq!(initial.answer, "4");
            assert!(initial.copy_enabled);
            assert!(!initial.rolling, "The first answer renders directly");
            let size = f.launcher.render_size(&f.ctx, &f.theme, false);
            let slot = f.launcher.rows[0].id;

            f.caret_at_end();
            let (action, copy, output) = f.render(vec![
                egui::Event::Text("0".into()),
                key(Key::Enter, Modifiers::NONE),
            ]);
            assert_eq!(f.launcher.query, "2+20");
            assert!(action.is_none());
            assert!(f.launcher.dirty);
            let pending = f.launcher.calculator_presentation().unwrap();
            assert_eq!(pending.answer, "4");
            assert_eq!(pending.displayed_query, "2+2");
            assert!(pending.pending && !pending.copy_enabled && !pending.rolling);
            assert_eq!(pending.answer_rect, initial.answer_rect);
            assert_eq!(pending.copy_rect, initial.copy_rect);
            assert_eq!(f.launcher.rows[0].id, slot);
            assert!(
                output.shapes.iter().any(|shape| {
                    matches!(&shape.shape, egui::Shape::Text(text)
                    if text.galley.text() == "4" && text.fallback_color == f.theme.fg())
                }),
                "A pending valid edit must retain the fully opaque answer"
            );
            output.drop_without_applying_deltas();
            assert_eq!(f.launcher.render_size(&f.ctx, &f.theme, false), size);

            // A pending generation must remain blocked even after the ordinary
            // dirty-input guard has cleared and while either control is used.
            f.launcher.poll();
            assert!(!f.launcher.dirty);
            assert!(f.draw(vec![key(Key::Enter, Modifiers::NONE)]).0.is_none());
            let copy = copy.unwrap();
            let pointer = |pressed| egui::Event::PointerButton {
                pos: copy.center(),
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            f.draw(vec![
                egui::Event::PointerMoved(copy.center()),
                pointer(true),
            ]);
            assert!(f.draw(vec![pointer(false)]).0.is_none());
            assert_eq!(
                f.ctx.memory(|memory| memory.focused()),
                Some(results_focus_id()),
                "A blocked copy click must retain a keyboard owner for the next answer"
            );
            assert_eq!(f.launcher.calculator_presentation().unwrap().answer, "4");
            assert_eq!(f.launcher.render_size(&f.ctx, &f.theme, false), size);

            f.commit();
            f.draw(vec![]);
            let current = f.launcher.calculator_presentation().unwrap();
            assert_eq!(current.answer, "22");
            assert!(!current.pending && current.copy_enabled && current.rolling);
            assert_eq!(current.answer_rect, initial.answer_rect);
            assert_eq!(current.copy_rect, initial.copy_rect);
            assert_eq!(f.launcher.render_size(&f.ctx, &f.theme, false), size);
            let (action, _) = f.draw(vec![key(Key::Enter, Modifiers::NONE)]);
            assert!(matches!(action, Some(ResultAction::Copy(answer)) if answer == "22"));
            assert!(f.draw(vec![key(Key::Enter, Modifiers::SHIFT)]).0.is_none());
            f.draw(vec![
                egui::Event::PointerMoved(copy.center()),
                pointer(true),
            ]);
            let (action, _) = f.draw(vec![pointer(false)]);
            assert!(matches!(action, Some(ResultAction::Copy(answer)) if answer == "22"));
            assert!(f.launcher.pending_copy.is_none());
            assert!(f.launcher.worker.copy.lock().unwrap().is_none());
            f.launcher.close();
        }
    }

    #[test]
    fn calculator_async_mailbox_cannot_commit_stale_answers_or_reenable_stale_copy() {
        let mut f = CalculatorFixture::new(Mode::All);
        f.launcher.set_query("2+2");
        f.commit();
        f.draw(vec![]);
        f.launcher.set_query("2+20");
        let stale_epoch = f.launcher.worker.epoch.load(Ordering::Acquire);
        let stale = f.response(stale_epoch, "2+20");
        f.launcher.set_query("2+200");
        let current_epoch = f.launcher.worker.epoch.load(Ordering::Acquire);
        *f.launcher.worker.result.lock().unwrap() = Some(stale);
        f.launcher.poll();
        f.launcher.focus = false;
        crate::keyboard::request_id(&f.ctx, results_focus_id());
        assert!(f.draw(vec![key(Key::Enter, Modifiers::NONE)]).0.is_none());
        let pending = f.launcher.calculator_presentation().unwrap();
        assert_eq!(pending.answer, "4");
        assert!(pending.pending && !pending.copy_enabled);

        let current = f.response(current_epoch, "2+200");
        *f.launcher.worker.result.lock().unwrap() = Some(current);
        f.launcher.poll();
        let (action, _) = f.draw(vec![key(Key::Enter, Modifiers::NONE)]);
        assert!(matches!(action, Some(ResultAction::Copy(answer)) if answer == "202"));

        // Revisiting the same formula is a new generation even when its old
        // displayed query and answer happen to match again.
        f.launcher.set_query("2+20");
        f.launcher.set_query("2+200");
        let old = f.response(current_epoch, "2+200");
        *f.launcher.worker.result.lock().unwrap() = Some(old);
        f.launcher.poll();
        assert!(f.draw(vec![key(Key::Enter, Modifiers::NONE)]).0.is_none());
        assert!(!f.launcher.calculator_presentation().unwrap().copy_enabled);
        f.commit();
        assert!(matches!(f.draw(vec![key(Key::Enter, Modifiers::NONE)]).0,
            Some(ResultAction::Copy(answer)) if answer == "202"));

        for query in ["2+", "notepad", "2026-10-23", ""] {
            f.launcher.set_query(query);
            let late = f.response(current_epoch, "2+200");
            *f.launcher.worker.result.lock().unwrap() = Some(late);
            f.launcher.poll();
            assert!(f.launcher.calculator_scene().is_none());
            assert!(f.draw(vec![key(Key::Enter, Modifiers::NONE)]).0.is_none());
            assert!(
                !f.launcher
                    .rows
                    .iter()
                    .any(|row| row.group == ResultGroup::Calculator)
            );
        }
        f.launcher.set_query("2+2");
        f.commit();
        let late_epoch = f.launcher.worker.epoch.load(Ordering::Acquire);
        f.launcher.set_mode(Mode::Apps);
        let late = Response {
            epoch: late_epoch,
            rows: vec![],
            calculator: calculator_row(Mode::All, "2+2"),
            default_roots: None,
            complete: true,
        };
        *f.launcher.worker.result.lock().unwrap() = Some(late);
        f.launcher.poll();
        assert!(f.launcher.calculator_presentation().is_none());
        assert!(f.launcher.pending_copy.is_none());
        assert!(f.launcher.worker.copy.lock().unwrap().is_none());
    }

    #[test]
    fn calculator_rolls_signed_fractional_large_and_scientific_results_in_one_fixed_slot() {
        let mut f = CalculatorFixture::new(Mode::All);
        f.theme.reduced_motion = false;
        f.theme.apply(&f.ctx);
        f.draw(vec![]);
        let initial = f.launcher.calculator_presentation().unwrap();
        let slot = f.launcher.rows[0].id;
        for (query, answer) in [
            ("2+20", "22"),
            ("-20-2", "-22"),
            ("1/8", "0.125"),
            ("999999999999998+1", "999999999999999"),
            ("1e15+1e15", "2.0000000000e15"),
            ("2e15+1e15", "3.0000000000e15"),
            ("2e16+1e16", "3.0000000000e16"),
            ("12 km to mi", "7.4564543068 mi"),
            ("24 km to mi", "14.9129086137 mi"),
        ] {
            f.launcher.set_query(query);
            f.draw(vec![]);
            let held = f.launcher.calculator_presentation().unwrap();
            assert!(held.pending && !held.copy_enabled);
            f.commit();
            f.draw(vec![]);
            let current = f.launcher.calculator_presentation().unwrap();
            assert_eq!(current.answer, answer, "{query}");
            assert!(current.rolling, "{query} must reuse its numeric reels");
            assert_eq!(current.answer_rect, initial.answer_rect);
            assert_eq!(current.copy_rect, initial.copy_rect);
            assert_eq!(f.launcher.rows[0].id, slot);
            assert!(current.answer_rect.unwrap().right() < current.copy_rect.unwrap().left());
            f.frame += 120;
            f.draw(vec![]);
            assert!(
                !f.launcher.calculator_presentation().unwrap().rolling,
                "{query}"
            );
            assert!((1..=2).contains(&f.launcher.calculator_numbers.retained_numbers()));
        }
        assert!(f.launcher.pending_copy.is_none());
        assert!(f.launcher.worker.copy.lock().unwrap().is_none());
    }

    #[test]
    fn calculator_scientific_labels_keep_units_outside_the_exponent_parser() {
        for (label, parts) in [
            ("1.0000000000e15", Some(("1.0000000000", "15"))),
            ("-2.5000000000e-10 mi", Some(("-2.5000000000", "-10 mi"))),
            ("1E+20 K", Some(("1", "+20 K"))),
            ("12 meters", None),
            ("Result unavailable", None),
            ("1e", None),
            ("1eone", None),
            ("e10", None),
        ] {
            assert_eq!(scientific_parts(label), parts, "{label}");
        }
    }

    #[test]
    fn calculator_scene_paints_only_its_heading_answer_and_copy_hint_as_text() {
        let mut f = CalculatorFixture::new(Mode::Calculator);
        for font_size in [14.0, 24.0] {
            let theme = Theme {
                font_size,
                ..f.theme.clone()
            };
            theme.apply(&f.ctx);
            let scale = theme.metrics().scale;
            let rect =
                Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, CALCULATOR_HEIGHT) * scale);
            let output = f.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    ..Default::default()
                },
                |ui| {
                    f.launcher.calculator_ui(ui, rect, rect, &theme, 0);
                },
            );
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect();
            assert_eq!(text, ["Calculator", "42", "Enter to copy"]);
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn launcher_navigation_leaves_modified_caret_and_activation_chords_intact() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![
                    key(Key::ArrowUp, Modifiers::ALT),
                    key(Key::ArrowDown, Modifiers::SHIFT),
                    key(Key::Enter, Modifiers::SHIFT),
                    key(Key::Home, Modifiers::CTRL),
                ],
                ..Default::default()
            },
            |ui| {
                for key in [Key::ArrowUp, Key::ArrowDown, Key::Enter, Key::Home] {
                    assert!(!consume_plain_key(ui.ctx(), key));
                }
                assert_eq!(ui.input(|input| input.events.len()), 4);
            },
        );
        output.textures_delta.clear();
    }

    #[test]
    fn launcher_ime_owns_enter_across_frames_and_commit_sizing_passes() {
        let ctx = egui::Context::default();
        let id = Id::new("launcher-query");
        for (frame, events, blocked) in [
            (
                0,
                vec![
                    egui::Event::Ime(egui::ImeEvent::Preedit {
                        text: "に".into(),
                        active_range_chars: None,
                    }),
                    key(Key::Enter, Modifiers::NONE),
                ],
                true,
            ),
            (1, vec![key(Key::Enter, Modifiers::NONE)], true),
            (
                2,
                vec![
                    egui::Event::Ime(egui::ImeEvent::Commit("日本".into())),
                    key(Key::Enter, Modifiers::NONE),
                ],
                true,
            ),
            (3, vec![key(Key::Enter, Modifiers::NONE)], false),
        ] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    assert_eq!(query_composing(ui.ctx(), id), blocked);
                    assert_eq!(query_composing(ui.ctx(), id), blocked);
                },
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn hidden_palette_keeps_its_size_and_reopen_snaps_to_current_content() {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let hub = SearchHub::new(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/launcher-size-fixtures"),
            wake.clone(),
        )
        .unwrap();
        let mut launcher = Launcher::new(Settings::default(), &hub, wake, true, None).unwrap();
        launcher.mode = Mode::Apps;
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let scale = theme.metrics().scale;
        let key = Id::new("launcher-palette-size");
        ctx.data_mut(|data| {
            data.insert_temp(
                key,
                PaletteSize {
                    height: 280.0 * scale,
                    from: 280.0 * scale,
                    target: 400.0 * scale,
                    candidate: 400.0 * scale,
                    changed: 0.0,
                    started: 0.0,
                    epoch: 0,
                    scale,
                    opened: true,
                },
            );
        });
        assert_eq!(
            launcher.render_size(&ctx, &theme, false),
            [640.0 * scale, (280.0 * scale).round()]
        );
        let hidden = ctx.data(|data| data.get_temp::<PaletteSize>(key).unwrap());
        assert!(!hidden.opened);
        assert_eq!(hidden.candidate, 400.0 * scale);

        // The first visible pass must size the current scene immediately,
        // without continuing the previous open's in-flight animation.
        launcher.native.set_opened(true);
        let mut reopened = [0.0; 2];
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            reopened = launcher.render_size(ui.ctx(), &theme, false);
        });
        output.textures_delta.clear();
        assert_eq!(reopened, [640.0 * scale, (224.0 * scale).round()]);
        launcher.close();
    }

    #[test]
    fn launcher_resize_transitions_anchor_footer_and_scroll_body_to_current_viewport() {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let hub = SearchHub::new(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/launcher-layout-fixtures"),
            wake.clone(),
        )
        .unwrap();
        let mut launcher = Launcher::new(Settings::default(), &hub, wake, true, None).unwrap();
        for font_size in [14.0, 24.0] {
            let theme = Theme {
                font_size,
                ..Default::default()
            };
            let scale = theme.metrics().scale;
            let ctx = egui::Context::default();
            for (frame, mode, actual, requested, candidate) in [
                (0, Mode::Files, 224.0, 300.0, 420.0),
                (1, Mode::Files, 300.0, 350.0, 420.0),
                (2, Mode::Calculator, 306.0, 260.0, 224.0),
                (3, Mode::Calculator, 224.0, 224.0, 224.0),
            ] {
                launcher.mode = mode;
                launcher.rows = if mode == Mode::Files {
                    launcher.query = "manifest".into();
                    (0..40)
                        .map(|at| grouped(&format!("App manifest {at:03}.txt"), ResultGroup::Files))
                        .collect()
                } else {
                    launcher.query = "6 * 7".into();
                    let mut answer = row(
                        "42".into(),
                        launcher.query.clone(),
                        Icon::Calculator,
                        ResultAction::Copy("42".into()),
                        i32::MAX,
                    );
                    answer.group = ResultGroup::Calculator;
                    vec![Arc::new(answer)]
                };
                launcher.dirty = false;
                ctx.data_mut(|data| {
                    data.insert_temp(
                        Id::new("launcher-palette-size"),
                        PaletteSize {
                            height: requested * scale,
                            from: requested * scale,
                            target: candidate * scale,
                            candidate: candidate * scale,
                            changed: 0.0,
                            started: 0.0,
                            epoch: 0,
                            scale,
                            opened: true,
                        },
                    );
                });
                let viewport =
                    Rect::from_min_size(pos2(11.0, 17.0), vec2(640.0 * scale, actual * scale));
                let mut body = None;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(viewport),
                        time: Some(frame as f64 / 60.0),
                        ..Default::default()
                    },
                    |ui| {
                        launcher.ui(ui, &theme);
                        // Inspect the current pass before egui swaps widget maps.
                        body = ui
                            .ctx()
                            .read_response(results_focus_id())
                            .map(|hit| hit.rect);
                    },
                );
                let body = body.expect("The result composite must remain available while resizing");
                let footer_top = viewport.bottom() - PALETTE_FOOTER * scale;
                assert!(viewport.contains_rect(body), "{viewport:?}: {body:?}");
                assert!((body.bottom() + 6.0 * scale - footer_top).abs() < 0.51);

                // Keyboard badges must stay in the current native footer while
                // growing or shrinking; the old brand slot is no longer painted.
                let footer_clip = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == "Tab" => Some(shape.clip_rect),
                    _ => None,
                });
                let footer_clip = footer_clip.expect("The footer must be painted during resize");
                assert!((footer_clip.top() - footer_top).abs() < 0.51);
                assert!((footer_clip.bottom() - viewport.bottom()).abs() < 0.51);
                assert!(!output.shapes.iter().any(|shape| {
                    matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Foldex")
                }));
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn launcher_scrollbar_reservation_uses_settled_fit_during_native_resize() {
        for font_size in [14.0, 24.0] {
            let theme = Theme {
                font_size,
                reduced_motion: true,
                ..Default::default()
            };
            let scale = theme.metrics().scale;
            let ctx = egui::Context::default();
            // Keep one scroll ID across growing short results, genuine overflow,
            // filtering and shrink. Reservation must change with settled fit,
            // never with intermediate native heights or last-frame overflow.
            for (frame, (actual, candidate, rows, overflowing)) in [
                (224.0, 254.0, 4, false),
                (240.0, 254.0, 4, false),
                (254.0, 254.0, 4, false),
                (254.0, 420.0, 40, true),
                (420.0, 420.0, 40, true),
                (420.0, 254.0, 4, false),
                (254.0, 254.0, 4, false),
                (420.0, 224.0, 5, true),
                (224.0, 224.0, 5, true),
            ]
            .into_iter()
            .enumerate()
            {
                let outer =
                    Rect::from_min_size(pos2(20.0, 20.0), vec2(640.0 * scale, actual * scale));
                let popup = foldex_ui::components::PopupLayout::new(outer, &theme);
                let body = Rect::from_min_max(
                    popup.body.min + vec2(4.0 * scale, 6.0 * scale),
                    popup.body.max - vec2(4.0 * scale, 6.0 * scale),
                );
                let content_height = (SECTION_HEIGHT + rows as f32 * RESULT_HEIGHT) * scale;
                ctx.data_mut(|data| {
                    data.insert_temp(
                        Id::new("launcher-palette-size"),
                        PaletteSize {
                            height: actual * scale,
                            from: actual * scale,
                            target: candidate * scale,
                            candidate: candidate * scale,
                            changed: 0.0,
                            started: 0.0,
                            epoch: 0,
                            scale,
                            opened: true,
                        },
                    );
                });
                let mut measured = None;
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(outer),
                        time: Some(frame as f64 / 60.0),
                        ..Default::default()
                    },
                    |ui| {
                        ui.scope_builder(
                            UiBuilder::new()
                                .id_salt("launcher-transition")
                                .max_rect(body),
                            |ui| {
                                ui.set_clip_rect(body);
                                ui.spacing_mut().item_spacing.y = 0.0;
                                let output =
                                    result_scroll_area(ui, &theme, outer, body, content_height)
                                        .vertical_scroll_offset(24.0 * scale)
                                        .show(ui, |ui| {
                                            ui.allocate_exact_size(
                                                vec2(ui.available_width(), content_height),
                                                Sense::hover(),
                                            );
                                        });
                                measured = Some((output.inner_rect, output.state.offset.y));
                            },
                        );
                    },
                )
                .drop_without_applying_deltas();
                let (viewport, offset) = measured.unwrap();
                assert!((viewport.height() - body.height()).abs() < 0.51);
                let width = body.width() - if overflowing { 4.0 * scale } else { 0.0 };
                assert!((viewport.width() - width).abs() < 0.51);
                // A hidden temporary bar must not disable scrolling while a
                // short result set is still clipped by the growing window.
                let expected_offset = (24.0 * scale).min((content_height - body.height()).max(0.0));
                assert!((offset - expected_offset).abs() < 0.51);
            }
        }
    }

    fn grouped(title: &str, group: ResultGroup) -> Arc<ResultRow> {
        let mut result = row(
            title.into(),
            "Detail".into(),
            Icon::Folder,
            ResultAction::Browse(PathBuf::from(title)),
            0,
        );
        result.group = group;
        Arc::new(result)
    }
    #[test]
    fn discovery_headings_and_empty_guidance_never_change_activation_indices() {
        let rows = vec![
            grouped("Recent folder", ResultGroup::Recents),
            grouped("Application", ResultGroup::Apps),
        ];
        let lines = result_lines(&rows, true, "Results", 36.0, 28.0, None);
        assert!(matches!(lines[0].kind, LineKind::Heading("Favorites", "")));
        assert!(matches!(lines[1].kind, LineKind::Empty(_)));
        assert_eq!(presented_indices(&lines), [0, 1]);
        assert!(
            lines
                .windows(2)
                .all(|pair| pair[1].top == pair[0].top + pair[0].height)
        );
    }
    #[test]
    fn typed_search_keeps_relevance_order_under_a_single_heading() {
        let rows = vec![
            grouped("Application", ResultGroup::Apps),
            grouped("Favorite", ResultGroup::Favorites),
            grouped("File", ResultGroup::Files),
        ];
        let lines = result_lines(&rows, false, "Results", 36.0, 28.0, None);
        assert_eq!(
            lines
                .iter()
                .filter(|line| matches!(line.kind, LineKind::Heading(..)))
                .count(),
            1
        );
        assert_eq!(presented_indices(&lines), [0, 1, 2]);
    }
    #[test]
    fn discovery_app_preview_keeps_original_indices_and_search_access() {
        let mut rows = vec![grouped("Pinned folder", ResultGroup::Favorites)];
        rows.extend((0..12).map(|at| grouped(&format!("App {at}"), ResultGroup::Apps)));
        let discovery = result_lines(&rows, true, "Results", 36.0, 28.0, None);
        assert_eq!(presented_indices(&discovery), [0, 1, 2, 3, 4, 5, 6]);
        let search = result_lines(&rows, false, "Results", 36.0, 28.0, None);
        assert_eq!(presented_indices(&search), (0..13).collect::<Vec<_>>());
    }
    #[test]
    fn calculator_presentation_keeps_the_backend_copy_index() {
        let rows = vec![
            grouped("Application", ResultGroup::Apps),
            grouped("21", ResultGroup::Calculator),
        ];
        let lines = result_lines(&rows, false, "Results", 36.0, 28.0, Some(1));
        assert!(matches!(lines[0].kind, LineKind::Calculator(1)));
        assert_eq!(presented_indices(&lines), [1, 0]);
    }
    #[test]
    fn compact_discovery_fits_an_application_after_four_folder_rows() {
        let rows = vec![
            grouped("Design", ResultGroup::Favorites),
            grouped("Projects", ResultGroup::Favorites),
            grouped("Documents", ResultGroup::Recents),
            grouped("Notes.md", ResultGroup::Recents),
            grouped("Calculator", ResultGroup::Apps),
        ];
        let lines = result_lines(&rows, true, "Results", 32.0, 24.0, None);
        let first_app = lines
            .iter()
            .find(|line| matches!(line.kind, LineKind::Result(4)))
            .unwrap();
        let body = Rect::from_min_max(pos2(10.0, 45.0), pos2(370.0, 286.0));
        let app = Rect::from_min_size(
            body.min + vec2(0.0, first_app.top),
            vec2(body.width(), first_app.height),
        );
        assert!(body.contains_rect(app));
        assert!(readable_at_bottom(app, body, 24.0));
        assert_eq!(presented_indices(&lines), [0, 1, 2, 3, 4]);
    }
    #[test]
    fn bottom_clipping_keeps_readable_rows_and_natural_top_clipping() {
        let clip = Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 100.0));
        let row = |top| Rect::from_min_size(pos2(0.0, top), vec2(300.0, 32.0));
        assert!(!readable_at_bottom(row(94.0), clip, 24.0));
        assert!(readable_at_bottom(row(76.0), clip, 24.0));
        assert!(readable_at_bottom(row(-28.0), clip, 24.0));
        let heading = |top| Rect::from_min_size(pos2(0.0, top), vec2(300.0, 24.0));
        assert!(!heading_has_companion(heading(80.0), clip, 24.0));
        assert!(heading_has_companion(heading(52.0), clip, 24.0));
    }
    #[test]
    fn visible_parent_context_omits_absolute_prefixes() {
        assert_eq!(
            parent_context(std::path::Path::new("/Users/ada/Projects/Foldex/Readme.md")),
            "Projects / Foldex"
        );
        assert_eq!(
            parent_context(std::path::Path::new("/Users/ada/Documents")),
            "Home"
        );
    }

    #[test]
    fn launcher_icon_requests_are_visible_only_bounded_and_keep_backend_indices() {
        let rows = (0..300)
            .map(|at| grouped(&format!("Folder {at}"), ResultGroup::Files))
            .collect::<Vec<_>>();
        let lines = result_lines(&rows, false, "Results", 36.0, 28.0, None);
        let content = Rect::from_min_size(pos2(0.0, -3600.0), vec2(600.0, 12000.0));
        let clip = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0));
        let indices = visible_icon_indices(&lines, content, clip);
        assert!(indices.len() <= 10);
        assert!(indices.iter().all(|at| *at >= 98 && *at <= 109));
        assert!(visible_icon_indices(&lines, content, Rect::NOTHING).is_empty());
        let huge_clip = Rect::from_min_size(pos2(0.0, -4000.0), vec2(600.0, 20000.0));
        assert_eq!(
            visible_icon_indices(&lines, content, huge_clip).len(),
            foldex_assets::MAX_REQUESTS_PER_VIEW
        );
    }
}
