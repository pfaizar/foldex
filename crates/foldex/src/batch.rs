//! Bounded background batch-rename planning and explicit, identity-checked Apply.
//! No filesystem work runs in a frame. Apply/recovery share the file history.

use crate::commands::{result_row, result_surface, take_key};
use egui::RichText;
use foldex_ui::Theme;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_ITEMS: usize = 1_024;
const MAX_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_RULE_BYTES: usize = 8 * 1024;

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
pub struct BatchUpdate {
    pub refresh: bool,
    pub origin: Option<u64>,
    pub message: Option<String>,
    pub errors: Vec<String>,
}

pub struct BatchRename {
    worker: Worker,
    popup: Option<Popup>,
    sequence: u64,
    pending: Option<PreviewRequest>,
    return_focus: Option<egui::Id>,
    capture_focus: bool,
}

impl BatchRename {
    pub fn new_with_history(
        wake: Wake,
        history: crate::operation_history::HistoryHandle,
    ) -> io::Result<Self> {
        Ok(Self {
            worker: Worker::new_with_history(wake, history)?,
            popup: None,
            sequence: 0,
            pending: None,
            return_focus: None,
            capture_focus: true,
        })
    }

    pub fn open_for_pane(&mut self, origin: u64, paths: Vec<PathBuf>) {
        self.capture_focus = self.popup.is_none();
        self.sequence = self.sequence.wrapping_add(1).max(1);
        let request = PreviewRequest {
            selection: self.sequence,
            generation: self.sequence,
            origin,
            paths: Arc::new(paths),
            rules: Rules::default(),
            now: SystemTime::now(),
        };
        self.pending = Some(request.clone());
        self.popup = Some(Popup {
            request,
            preview: None,
            error: None,
            loading: true,
            caret: 0,
            applied: false,
            submitted: false,
            case_menu: Default::default(),
            case_anchor: egui::Rect::NOTHING,
            last_warning: String::new(),
            focus_template: true,
            preview_cursor: 0,
            reveal_preview: false,
            preview_prefix: String::new(),
            preview_prefix_until: 0.0,
        });
    }

    pub fn is_open(&self) -> bool {
        self.popup.is_some()
    }

    pub fn close(&mut self) {
        self.popup = None;
        self.pending = None;
        self.worker.cancel_preview();
    }

    pub fn set_return_focus(&mut self, id: egui::Id) {
        self.return_focus = Some(id);
        self.capture_focus = false;
    }

    pub fn release_focus(&mut self, ctx: &egui::Context) {
        let previous = self.return_focus.take();
        let focused = ctx.memory(|memory| memory.focused());
        let owns = focused.is_none()
            || focused == Some(egui::Id::new("batch-case-menu").with("keyboard"))
            || focused
                .and_then(|id| ctx.read_response(id))
                .is_some_and(|response| response.layer_id.id == egui::Id::new("batch-rename"));
        if owns && let Some(previous) = previous {
            foldex_ui::components::return_focus(ctx, previous);
        }
    }

    pub fn undo(&self) -> Result<(), String> {
        self.worker.action(Action::Undo)
    }

    pub fn ui(&mut self, ctx: &egui::Context, theme: &Theme) -> BatchUpdate {
        let mut update = BatchUpdate::default();
        if self.popup.is_some() && self.capture_focus {
            self.return_focus = ctx.memory(|memory| memory.focused());
            self.capture_focus = false;
        }
        if let Some(request) = &self.pending {
            match self.worker.preview(request) {
                Ok(()) => self.pending = None,
                Err(error) if error.contains("busy") => {
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
                Err(error) => {
                    if let Some(popup) = &mut self.popup {
                        popup.error = Some(error);
                        popup.loading = false;
                    }
                    self.pending = None;
                }
            }
        }
        if let Some(preview) = self.worker.take_preview() {
            if let Some(popup) = &mut self.popup {
                if popup.request.generation == preview.generation {
                    popup.error = preview.error.clone();
                    popup.preview = Some(preview);
                    popup.loading = false;
                }
            }
        }
        if let Some(result) = self.worker.take_result() {
            if let Some(popup) = &mut self.popup {
                popup.operation_finished(&result);
            }
            update.origin = Some(result.origin);
            update.refresh = result.changed;
            update.message = Some(result.message);
            update.errors = result.errors;
        }
        let Some(popup) = &mut self.popup else {
            self.release_focus(ctx);
            return update;
        };
        let working = self.worker.working();
        let mut open = true;
        let mut changed = false;
        let mut apply = false;
        let mut undo = false;
        let mut reload = false;
        let m = theme.metrics();
        let interactive = !popup.case_menu.open;
        foldex_ui::components::dialog_interactive(
            ctx,
            egui::Id::new("batch-rename"),
            theme,
            "Rename files",
            760.0 * m.scale,
            &mut open,
            interactive,
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("{} selected items", popup.request.paths.len()))
                            .color(theme.dim()),
                    );
                    reload = foldex_ui::components::text_button_enabled(
                        ui,
                        theme,
                        "Reload preview",
                        false,
                        !working && !popup.applied,
                    )
                    .clicked();
                });
                ui.add_enabled_ui(!working && !popup.applied, |ui| {
                    let template_id = egui::Id::new("batch-name-template");
                    if popup.focus_template
                        && foldex_ui::components::can_focus(ui)
                        && !ui.memory(|memory| memory.has_focus(template_id))
                    {
                        ui.memory_mut(|memory| memory.request_focus(template_id));
                    }
                    let output = foldex_ui::components::form_input(
                        ui,
                        egui::Id::new("batch-name-template"),
                        theme,
                        "Name template",
                        &mut popup.request.rules.template,
                        1024,
                    );
                    if popup.focus_template && foldex_ui::components::can_focus(ui) {
                        popup.focus_template = ui.is_sizing_pass() || !output.response.has_focus();
                    }
                    changed |= output.response.changed();
                    if let Some(range) = output.cursor_range {
                        popup.caret = range.primary.index.into();
                    }
                    ui.horizontal_wrapped(|ui| {
                        for (name, token) in [
                            ("Original name", "{stem}"),
                            ("Incremental ID", "{id}"),
                            ("Current date", "{date}"),
                            ("Created", "{created}"),
                            ("Modified", "{modified}"),
                        ] {
                            if foldex_ui::components::text_button(ui, theme, name, false).clicked()
                            {
                                insert_at_character(
                                    &mut popup.request.rules.template,
                                    &mut popup.caret,
                                    token,
                                );
                                let mut state =
                                    egui::text_edit::TextEditState::load(ctx, template_id)
                                        .unwrap_or_default();
                                state
                                    .cursor
                                    .set_char_range(Some(egui::text::CCursorRange::one(
                                        egui::text::CCursor::new(popup.caret),
                                    )));
                                state.store(ctx, template_id);
                                popup.focus_template = true;
                                changed = true;
                            }
                        }
                    });
                    for (label, value) in [
                        ("Prefix", &mut popup.request.rules.prefix),
                        ("Suffix", &mut popup.request.rules.suffix),
                        ("Find", &mut popup.request.rules.find),
                        ("Replace", &mut popup.request.rules.replace),
                    ] {
                        changed |= foldex_ui::components::form_input(
                            ui,
                            egui::Id::new(("batch-rule", label)),
                            theme,
                            label,
                            value,
                            1024,
                        )
                        .response
                        .changed();
                    }
                    ui.horizontal_wrapped(|ui| {
                        changed |= foldex_ui::components::checkbox(
                            ui,
                            theme,
                            &mut popup.request.rules.keep_extension,
                            "Keep file extension",
                        )
                        .changed();
                        let case = foldex_ui::components::select_button(
                            ui,
                            theme,
                            popup.request.rules.case.label(),
                            145.0 * m.scale,
                            popup.case_menu.open,
                        );
                        popup.case_anchor = case.rect;
                        let keyboard_open = case.has_focus()
                            && (take_key(ctx, egui::Modifiers::NONE, egui::Key::ArrowDown)
                                || take_key(ctx, egui::Modifiers::NONE, egui::Key::ArrowUp)
                                || take_key(ctx, egui::Modifiers::ALT, egui::Key::ArrowDown)
                                || take_key(ctx, egui::Modifiers::NONE, egui::Key::F4));
                        if case.clicked() || keyboard_open {
                            popup.case_menu.toggle(ctx);
                            popup.case_menu.select(popup.request.rules.case as usize);
                        }
                        changed |= foldex_ui::components::integer_field(
                            ui,
                            theme,
                            "ID start",
                            &mut popup.request.rules.start,
                            0..=u64::MAX,
                        )
                        .changed();
                        changed |= foldex_ui::components::integer_field(
                            ui,
                            theme,
                            "Step",
                            &mut popup.request.rules.step,
                            1..=1_000_000,
                        )
                        .changed();
                        changed |= foldex_ui::components::integer_field(
                            ui,
                            theme,
                            "Digits",
                            &mut popup.request.rules.padding,
                            1..=20,
                        )
                        .changed();
                    });
                });
                ui.label(RichText::new("Tokens: {stem}, {name}, {ext}, {id}, {date}, {created}, {modified}. Dates use UTC. Find/replace is literal and case sensitive.").small().color(theme.dim()));
                ui.separator();
                let warning = popup.error.clone().or_else(|| {
                    popup
                        .preview
                        .as_ref()
                        .and_then(|preview| preview.rows.iter().find_map(|row| row.error.clone()))
                });
                if let Some(message) = &warning {
                    popup.last_warning.clone_from(message);
                }
                foldex_ui::components::reveal(
                    ui,
                    egui::Id::new("batch-validation-message"),
                    theme,
                    warning.is_some(),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&popup.last_warning)
                                    .color(Theme::color(theme.danger)),
                            )
                            .wrap(),
                        );
                    },
                );
                if popup.loading {
                    ui.label(RichText::new("Updating preview…").color(theme.dim()));
                }
                if let Some(preview) = &popup.preview {
                    let row_height = theme.file_row_height();
                    let preview_id = egui::Id::new("batch-preview-focus");
                    popup.preview_cursor = popup
                        .preview_cursor
                        .min(preview.rows.len().saturating_sub(1));
                    // The preview is one read-only composite; arrows inspect rows
                    // without changing any rename rule or submitting an operation.
                    let page = ((ctx.content_rect().height() * 0.3)
                        .clamp(80.0 * m.scale, 320.0 * m.scale)
                        / row_height)
                        .floor()
                        .max(1.0) as usize;
                    popup.preview_input(ctx, page);
                    let preview = popup.preview.as_ref().unwrap();
                    let column_width = (ui.available_width() - m.gap) * 0.45;
                    let (header, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), m.control),
                        egui::Sense::hover(),
                    );
                    foldex_ui::components::elided_label(
                        ui.painter(),
                        egui::Rect::from_min_size(header.min, egui::vec2(column_width, m.control)),
                        theme,
                        "Current name",
                        theme.dim(),
                        false,
                    );
                    foldex_ui::components::elided_label(
                        ui.painter(),
                        egui::Rect::from_min_max(
                            header.min + egui::vec2(column_width + m.gap, 0.0),
                            header.max,
                        ),
                        theme,
                        "New name",
                        theme.dim(),
                        false,
                    );
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        egui::ScrollArea::vertical()
                            .id_salt("batch-preview")
                            .max_height(
                                (ctx.content_rect().height() * 0.3)
                                    .clamp(80.0 * m.scale, 320.0 * m.scale),
                            )
                            .auto_shrink([false, true])
                            .show_rows(ui, row_height, preview.rows.len(), |ui, range| {
                                let viewport = ui.clip_rect();
                                result_surface(ui, viewport, preview_id, "Rename preview");
                                if popup.reveal_preview && !preview.rows.is_empty() {
                                    foldex_ui::components::reveal_menu_row(
                                        ui,
                                        theme,
                                        popup.preview_cursor,
                                        range.start,
                                        row_height,
                                    );
                                    popup.reveal_preview = false;
                                }
                                for index in range {
                                    let row = &preview.rows[index];
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), row_height),
                                        egui::Sense::hover(),
                                    );
                                    let hit = ui.interact(
                                        rect.intersect(viewport),
                                        preview_id.with(index),
                                        egui::Sense::CLICK,
                                    );
                                    let label = format!(
                                        "{} → {}{}",
                                        row.before,
                                        row.after,
                                        row.error
                                            .as_ref()
                                            .map(|error| format!(". {error}"))
                                            .unwrap_or_default()
                                    );
                                    result_row(
                                        ui,
                                        &hit,
                                        preview_id,
                                        preview_id,
                                        &label,
                                        popup.preview_cursor == index,
                                        true,
                                    );
                                    if hit.clicked() {
                                        popup.preview_cursor = index;
                                        ui.memory_mut(|memory| memory.request_focus(preview_id));
                                    }
                                    let hover = foldex_ui::animated_state(
                                        ui.ctx(),
                                        egui::Id::new(("batch-preview-hover", index)),
                                        hit.hovered(),
                                        theme,
                                        foldex_ui::motion::MotionProfile::Fast,
                                    );
                                    ui.painter().rect_filled(
                                        rect,
                                        theme.radius,
                                        Theme::color(theme.hover).gamma_multiply(hover),
                                    );
                                    foldex_ui::components::focus_ring(
                                        ui,
                                        rect,
                                        preview_id.with((index, "cursor")),
                                        theme,
                                        popup.preview_cursor == index
                                            && ui.memory(|memory| memory.has_focus(preview_id)),
                                    );
                                    let before = egui::Rect::from_min_size(
                                        rect.min,
                                        egui::vec2(column_width, row_height),
                                    );
                                    let after = egui::Rect::from_min_max(
                                        rect.min + egui::vec2(column_width + m.gap, 0.0),
                                        rect.max
                                            - egui::vec2(
                                                if row.error.is_some() {
                                                    75.0 * m.scale
                                                } else {
                                                    0.0
                                                },
                                                0.0,
                                            ),
                                    );
                                    foldex_ui::components::elided_label(
                                        ui.painter(),
                                        before,
                                        theme,
                                        &row.before,
                                        theme.fg(),
                                        false,
                                    );
                                    let color = if row.error.is_some() {
                                        Theme::color(theme.danger)
                                    } else if row.changed {
                                        theme.accent()
                                    } else {
                                        theme.dim()
                                    };
                                    foldex_ui::components::elided_label(
                                        ui.painter(),
                                        after,
                                        theme,
                                        &row.after,
                                        color,
                                        false,
                                    );
                                    if let Some(error) = &row.error {
                                        foldex_ui::components::elided_label(
                                            ui.painter(),
                                            egui::Rect::from_min_max(
                                                egui::pos2(after.right() + m.gap, rect.top()),
                                                rect.max,
                                            ),
                                            theme,
                                            "Conflict",
                                            Theme::color(theme.danger),
                                            true,
                                        );
                                        hit.on_hover_text(error);
                                    } else {
                                        hit.on_hover_text(format!(
                                            "{} → {}",
                                            row.before, row.after
                                        ));
                                    }
                                }
                            });
                    });
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!(
                            "{} changes · {} conflicts",
                            preview.changes, preview.conflicts
                        ));
                        if working {
                            ui.label(RichText::new("Applying changes…").color(theme.dim()));
                        }
                        apply = foldex_ui::components::text_button_enabled(
                            ui,
                            theme,
                            "Apply rename",
                            true,
                            popup.can_apply(working),
                        )
                        .clicked();
                        undo = foldex_ui::components::text_button_enabled(
                            ui,
                            theme,
                            &self.worker.shared.history.state().undo_label(),
                            false,
                            !working && self.worker.can_undo(),
                        )
                        .clicked();
                    });
                }
            },
        );
        let cases = [NameCase::Original, NameCase::Lower, NameCase::Upper];
        let items = std::array::from_fn::<_, 3, _>(|i| foldex_ui::components::MenuItem {
            id: i as u32,
            label: cases[i].label(),
            shortcut: "",
            glyph: foldex_ui::Icon::Empty,
            checked: Some(popup.request.rules.case == cases[i]),
            enabled: true,
        });
        if let Some(choice) = popup.case_menu.show_width(
            ctx,
            egui::Id::new("batch-case-menu"),
            theme,
            popup.case_anchor,
            &items,
            180.0 * m.scale,
        ) {
            popup.request.rules.case = cases[choice as usize];
            changed = true;
        }
        if changed || reload {
            self.sequence = self.sequence.wrapping_add(1).max(1);
            popup.request.generation = self.sequence;
            if reload {
                popup.request.selection = self.sequence;
            }
            popup.loading = true;
            popup.error = None;
            self.pending = Some(popup.request.clone());
            ctx.request_repaint();
        }
        if apply {
            match self.worker.action(Action::Apply(popup.request.generation)) {
                Ok(()) => {
                    popup.submitted = true;
                    ctx.request_repaint();
                }
                Err(error) => popup.error = Some(error),
            }
        }
        if undo {
            match self.worker.action(Action::Undo) {
                Ok(()) => {
                    popup.submitted = true;
                    ctx.request_repaint();
                }
                Err(error) => popup.error = Some(error),
            }
        }
        if !open {
            self.release_focus(ctx);
            self.close();
        }
        update
    }
}

struct Popup {
    request: PreviewRequest,
    preview: Option<Arc<Preview>>,
    error: Option<String>,
    loading: bool,
    caret: usize,
    applied: bool,
    submitted: bool,
    case_menu: foldex_ui::components::MenuState,
    case_anchor: egui::Rect,
    last_warning: String,
    focus_template: bool,
    preview_cursor: usize,
    reveal_preview: bool,
    preview_prefix: String,
    preview_prefix_until: f64,
}
impl Popup {
    fn preview_input(&mut self, ctx: &egui::Context, page: usize) {
        let id = egui::Id::new("batch-preview-focus");
        if self.case_menu.open
            || !ctx.memory(|memory| memory.has_focus(id))
            || ctx.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Ime(_)))
            })
        {
            return;
        }
        ctx.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            )
        });
        let Some(preview) = &self.preview else {
            return;
        };
        let len = preview.rows.len();
        if len == 0 {
            return;
        }
        let old = self.preview_cursor;
        for (key, step) in [
            (egui::Key::ArrowDown, 1),
            (egui::Key::ArrowUp, -1),
            (egui::Key::PageDown, page.max(1) as isize),
            (egui::Key::PageUp, -(page.max(1) as isize)),
        ] {
            if take_key(ctx, egui::Modifiers::NONE, key) {
                self.preview_cursor =
                    (self.preview_cursor as isize + step).clamp(0, len as isize - 1) as usize;
            }
        }
        if take_key(ctx, egui::Modifiers::NONE, egui::Key::Home) {
            self.preview_cursor = 0;
        }
        if take_key(ctx, egui::Modifiers::NONE, egui::Key::End) {
            self.preview_cursor = len - 1;
        }
        let now = ctx.input(|input| input.time);
        let typed = ctx.input_mut(|input| {
            let mut typed = String::new();
            if input.modifiers.matches_exact(egui::Modifiers::NONE) {
                input.events.retain(|event| {
                    if let egui::Event::Text(text) = event {
                        let remaining = 64usize.saturating_sub(typed.chars().count());
                        typed.extend(text.chars().filter(|ch| !ch.is_control()).take(remaining));
                        false
                    } else {
                        true
                    }
                });
            }
            typed
        });
        if !typed.is_empty() {
            if now > self.preview_prefix_until {
                self.preview_prefix.clear();
            }
            if self.preview_prefix.chars().count() + typed.chars().count() > 128 {
                self.preview_prefix.clear();
            }
            let repeated = self.preview_prefix == typed.to_lowercase();
            if !repeated {
                self.preview_prefix.push_str(&typed.to_lowercase());
            }
            self.preview_prefix_until = now + 0.8;
            for offset in 1..=len {
                let row = (self.preview_cursor + offset) % len;
                if preview.rows[row]
                    .before
                    .to_lowercase()
                    .starts_with(&self.preview_prefix)
                {
                    self.preview_cursor = row;
                    break;
                }
            }
        }
        // Preview inspection has no default activation. Keep Enter/Space from
        // reaching the form's Apply button on this frame.
        take_key(ctx, egui::Modifiers::NONE, egui::Key::Enter);
        take_key(ctx, egui::Modifiers::NONE, egui::Key::Space);
        self.reveal_preview |= self.preview_cursor != old;
    }
    fn operation_finished(&mut self, result: &OperationResult) {
        if self.submitted {
            self.applied = result.errors.is_empty() || result.needs_recovery;
            self.error = result.errors.first().cloned();
            self.submitted = false;
        }
    }
    fn can_apply(&self, working: bool) -> bool {
        !working
            && !self.applied
            && !self.loading
            && self.preview.as_ref().is_some_and(|preview| {
                preview.generation == self.request.generation && preview.can_apply
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NameCase {
    Original,
    Lower,
    Upper,
}
impl NameCase {
    fn label(self) -> &'static str {
        match self {
            Self::Original => "Original case",
            Self::Lower => "lowercase",
            Self::Upper => "UPPERCASE",
        }
    }
}

#[derive(Clone, Debug)]
struct Rules {
    template: String,
    prefix: String,
    suffix: String,
    find: String,
    replace: String,
    keep_extension: bool,
    case: NameCase,
    start: u64,
    step: u64,
    padding: u8,
}
impl Default for Rules {
    fn default() -> Self {
        Self {
            template: "{stem}".into(),
            prefix: String::new(),
            suffix: String::new(),
            find: String::new(),
            replace: String::new(),
            keep_extension: true,
            case: NameCase::Original,
            start: 1,
            step: 1,
            padding: 3,
        }
    }
}

#[derive(Clone)]
struct PreviewRequest {
    selection: u64,
    generation: u64,
    origin: u64,
    paths: Arc<Vec<PathBuf>>,
    rules: Rules,
    now: SystemTime,
}
struct Preview {
    generation: u64,
    rows: Vec<PreviewRow>,
    changes: usize,
    conflicts: usize,
    can_apply: bool,
    error: Option<String>,
}
struct PreviewRow {
    before: String,
    after: String,
    changed: bool,
    error: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Identity {
    volume: u64,
    id: [u8; 16],
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Fingerprint {
    identity: Identity,
    bytes: u64,
    modified: u64,
    created: u64,
    directory: bool,
}
#[derive(Clone)]
struct Source {
    path: PathBuf,
    parent: PathBuf,
    parent_identity: Identity,
    name: OsString,
    fingerprint: Fingerprint,
}
struct Selection {
    id: u64,
    sources: Vec<Result<Source, String>>,
}
#[derive(Clone)]
struct Entry {
    source: Source,
    target: OsString,
}
struct Plan {
    generation: u64,
    origin: u64,
    entries: Vec<Entry>,
    preview: Arc<Preview>,
}
#[derive(Clone)]
struct UndoPlan {
    origin: u64,
    entries: Vec<Entry>,
}

/// Opaque identity-checked rename transaction, including interrupted recovery.
/// The common file journal owns this plan; opening another Rename never loses it.
#[derive(Clone)]
pub(crate) struct HistoryPlan {
    plan: UndoPlan,
    recovery: bool,
}
impl HistoryPlan {
    pub(crate) fn bytes(&self) -> usize {
        self.plan
            .entries
            .iter()
            .map(|entry| {
                (entry.source.path.as_os_str().len()
                    + entry.source.parent.as_os_str().len()
                    + entry.source.name.len()
                    + entry.target.len())
                .saturating_mul(2)
                    + 256
            })
            .sum()
    }
    pub(crate) fn replay(self) -> crate::operation_history::Replay {
        let (result, next) = execute(self.plan.origin, self.plan.entries, false);
        let recovery = self.recovery || result.needs_recovery;
        let step = next.map(|plan| crate::operation_history::Step::Batch(Self { plan, recovery }));
        if result.errors.is_empty() {
            // Finishing a failed rename's recovery must not offer to recreate
            // its provisional .foldex-rename names through Redo.
            crate::operation_history::Replay {
                inverse: if recovery { None } else { step },
                retry: None,
                error: None,
            }
        } else {
            crate::operation_history::Replay {
                inverse: None,
                retry: step,
                error: Some(format!("{}. {}", result.message, result.errors.join(". "))),
            }
        }
    }
}
struct OperationResult {
    origin: u64,
    changed: bool,
    message: String,
    errors: Vec<String>,
    needs_recovery: bool,
}

enum Action {
    Apply(u64),
    Undo,
}
struct Mailbox {
    pending: Option<PreviewRequest>,
    action: Option<Action>,
    preview: Option<Arc<Preview>>,
    result: Option<OperationResult>,
}
struct Shared {
    state: Mutex<Mailbox>,
    desired: AtomicU64,
    working: AtomicBool,
    history: crate::operation_history::HistoryHandle,
    stop: AtomicBool,
    thread: OnceLock<Thread>,
}
impl Shared {
    fn notify(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }
}
struct Worker {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}
impl Worker {
    #[cfg(test)]
    fn new(wake: Wake) -> io::Result<Self> {
        Self::new_with_history(wake, crate::operation_history::HistoryHandle::default())
    }
    fn new_with_history(
        wake: Wake,
        history: crate::operation_history::HistoryHandle,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(Mailbox {
                pending: None,
                action: None,
                preview: None,
                result: None,
            }),
            desired: AtomicU64::new(0),
            working: AtomicBool::new(false),
            history,
            stop: AtomicBool::new(false),
            thread: OnceLock::new(),
        });
        let child = shared.clone();
        let handle = thread::Builder::new()
            .name("foldex-batch-rename".into())
            .spawn(move || worker(child, wake))?;
        let _ = shared.thread.set(handle.thread().clone());
        shared.notify();
        Ok(Self {
            shared,
            handle: Some(handle),
        })
    }
    fn preview(&self, request: &PreviewRequest) -> Result<(), String> {
        validate_request(request)?;
        let mut state = self
            .shared
            .state
            .try_lock()
            .map_err(|_| "Rename preview is busy; retry".to_owned())?;
        self.shared
            .desired
            .store(request.generation, Ordering::Release);
        state.pending = Some(request.clone());
        drop(state);
        self.shared.notify();
        Ok(())
    }
    fn cancel_preview(&self) {
        self.shared.desired.store(0, Ordering::Release);
        self.shared.notify();
    }
    fn action(&self, action: Action) -> Result<(), String> {
        let mut state = self
            .shared
            .state
            .try_lock()
            .map_err(|_| "Rename worker is busy; retry".to_owned())?;
        if self.shared.working.load(Ordering::Acquire) {
            return Err("A rename operation is already in progress".into());
        }
        match action {
            Action::Apply(generation)
                if generation != self.shared.desired.load(Ordering::Acquire)
                    || state
                        .preview
                        .as_ref()
                        .is_some_and(|p| p.generation != generation) =>
            {
                return Err("Rename preview changed; wait for the latest preview".into());
            }
            Action::Undo if !self.can_undo() => {
                return Err("No completed rename is available to undo".into());
            }
            _ => {}
        }
        self.shared.working.store(true, Ordering::Release);
        state.action = Some(action);
        drop(state);
        self.shared.notify();
        Ok(())
    }
    fn take_preview(&self) -> Option<Arc<Preview>> {
        self.shared.state.try_lock().ok()?.preview.take()
    }
    fn take_result(&self) -> Option<OperationResult> {
        let result = self.shared.state.try_lock().ok()?.result.take()?;
        self.shared.working.store(false, Ordering::Release);
        Some(result)
    }
    fn working(&self) -> bool {
        self.shared.working.load(Ordering::Acquire)
    }
    fn can_undo(&self) -> bool {
        self.shared.history.state().can_undo()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.notify();
        let _ = self.handle.take(); // never join filesystem work from a frame
    }
}

fn worker(shared: Arc<Shared>, wake: Wake) {
    let mut selection: Option<Selection> = None;
    let mut plan: Option<Plan> = None;
    loop {
        let (action, request) = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let action = state.action.take();
            // Accepted operations finish/roll back even if the popup is hidden.
            if action.is_none() && shared.stop.load(Ordering::Acquire) {
                break;
            }
            let request = if action.is_none() {
                state.pending.take()
            } else {
                None
            };
            (action, request)
        };
        if let Some(action) = action {
            let result = match action {
                Action::Apply(generation) => {
                    if let Some(current) = plan
                        .take()
                        .filter(|plan| plan.generation == generation && plan.preview.can_apply)
                    {
                        let mut transaction = shared.history.begin();
                        let (result, next_undo) = execute(current.origin, current.entries, true);
                        let mut mutation = crate::operation_history::Mutation {
                            result: Ok(String::new()),
                            changed: result.changed,
                            steps: next_undo
                                .map(|plan| {
                                    crate::operation_history::Step::Batch(HistoryPlan {
                                        plan,
                                        recovery: result.needs_recovery,
                                    })
                                })
                                .into_iter()
                                .collect(),
                        };
                        transaction.record("Batch Rename", &mut mutation);
                        result
                    } else {
                        OperationResult {
                            origin: 0,
                            changed: false,
                            message: "Rename was not applied".into(),
                            errors: vec!["The displayed preview is no longer applicable".into()],
                            needs_recovery: false,
                        }
                    }
                }
                Action::Undo => {
                    let result = shared
                        .history
                        .replay(crate::operation_history::Direction::Undo);
                    OperationResult {
                        origin: 0,
                        changed: true,
                        message: result.as_ref().ok().cloned().unwrap_or_default(),
                        errors: result
                            .err()
                            .map(|error| error.to_string())
                            .into_iter()
                            .collect(),
                        needs_recovery: false,
                    }
                }
            };
            shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .result = Some(result);
            wake();
            continue;
        }
        if let Some(request) = request {
            let current = || {
                shared.desired.load(Ordering::Acquire) == request.generation
                    && !shared.stop.load(Ordering::Acquire)
            };
            if !current() {
                continue;
            }
            if selection
                .as_ref()
                .is_none_or(|selection| selection.id != request.selection)
            {
                let loaded = load_selection(&request, &current);
                if !current() || loaded.sources.len() != request.paths.len() {
                    selection = None;
                    continue;
                }
                selection = Some(loaded);
            }
            if let Some(next) = make_plan(&request, selection.as_ref().unwrap(), &current) {
                if current() {
                    shared
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .preview = Some(next.preview.clone());
                    plan = Some(next);
                    wake();
                }
            }
            continue;
        }
        if shared.desired.load(Ordering::Acquire) == 0 {
            selection = None;
            plan = None;
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.preview = None;
        }
        thread::park();
    }
}

fn validate_request(request: &PreviewRequest) -> Result<(), String> {
    if request.paths.is_empty() || request.paths.len() > MAX_ITEMS {
        return Err(format!("Select between 1 and {MAX_ITEMS} items per batch"));
    }
    if request
        .paths
        .iter()
        .map(|p| p.as_os_str().len().saturating_mul(2))
        .sum::<usize>()
        > MAX_PATH_BYTES
    {
        return Err("Batch paths exceed the 8 MiB limit".into());
    }
    let rules = &request.rules;
    if [
        &rules.template,
        &rules.prefix,
        &rules.suffix,
        &rules.find,
        &rules.replace,
    ]
    .iter()
    .map(|s| s.len())
    .sum::<usize>()
        > MAX_RULE_BYTES
    {
        return Err("Rename rules exceed the 8 KiB limit".into());
    }
    if rules.step == 0 || rules.padding > 20 {
        return Err("Use a positive ID step and at most 20 digits".into());
    }
    Ok(())
}

fn insert_at_character(text: &mut String, caret: &mut usize, token: &str) {
    if text.len().saturating_add(token.len()) > MAX_RULE_BYTES {
        return;
    }
    let byte = text
        .char_indices()
        .nth(*caret)
        .map_or(text.len(), |(byte, _)| byte);
    text.insert_str(byte, token);
    *caret += token.chars().count();
}

fn load_selection(request: &PreviewRequest, current: &dyn Fn() -> bool) -> Selection {
    let mut sources = Vec::with_capacity(request.paths.len());
    for path in request.paths.iter() {
        if !current() {
            break;
        }
        sources.push(load_source(path));
    }
    Selection {
        id: request.selection,
        sources,
    }
}

fn load_source(path: &Path) -> Result<Source, String> {
    if !path.is_absolute() {
        return Err("Select an absolute filesystem path".into());
    }
    let name = path
        .file_name()
        .ok_or("A filesystem root cannot be renamed")?
        .to_os_string();
    if name.to_str().is_none() {
        return Err(
            "This filename contains non-Unicode units and cannot be templated safely".into(),
        );
    }
    let parent =
        fs::canonicalize(path.parent().ok_or("No parent folder")?).map_err(|e| e.to_string())?;
    let parent_info = native::open(&parent, native::Mode::Inspect)
        .map_err(|e| e.to_string())?
        .fingerprint;
    if !parent_info.directory {
        return Err("The parent is not a folder".into());
    }
    let path = parent.join(&name);
    let fingerprint = native::open(&path, native::Mode::Inspect)
        .map_err(|e| e.to_string())?
        .fingerprint;
    Ok(Source {
        path,
        parent,
        parent_identity: parent_info.identity,
        name,
        fingerprint,
    })
}

fn make_plan(
    request: &PreviewRequest,
    selection: &Selection,
    current: &dyn Fn() -> bool,
) -> Option<Plan> {
    let mut rows = Vec::with_capacity(selection.sources.len());
    let mut entries = Vec::with_capacity(selection.sources.len());
    let mut source_keys = BTreeMap::<OrdinalPath, usize>::new();
    let mut identities = BTreeMap::<(u64, [u8; 16]), usize>::new();
    let mut target_keys = BTreeMap::<OrdinalPath, usize>::new();
    let mut pending_errors = Vec::<(usize, String)>::new();
    let mut row_to_entry = Vec::<Option<usize>>::new();
    let mut path_weight = 0usize;
    for (index, source) in selection.sources.iter().enumerate() {
        if !current() {
            return None;
        }
        let before = request.paths[index]
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match source {
            Ok(source) => {
                let after = transformed_name(source, &request.rules, index, request.now);
                let mut row = PreviewRow {
                    before,
                    after: String::new(),
                    changed: false,
                    error: None,
                };
                let key = OrdinalPath::new(&source.path);
                if let Some(other) = source_keys.insert(key, index) {
                    pending_errors.push((
                        other,
                        "The selection refers to the same Windows path more than once".into(),
                    ));
                    row.error = Some("Duplicate source path".into());
                }
                let identity = &source.fingerprint.identity;
                if let Some(other) = identities.insert((identity.volume, identity.id), index) {
                    pending_errors.push((
                        other,
                        "Multiple selected names refer to the same file object".into(),
                    ));
                    row.error = Some("Duplicate file identity or hard-link alias".into());
                }
                if let Ok(after) = &after {
                    row.after = after.clone();
                    row.changed = source.name != OsStr::new(after);
                    let target = source.parent.join(after);
                    path_weight = path_weight
                        .saturating_add(source.path.as_os_str().len().saturating_mul(2))
                        .saturating_add(target.as_os_str().len().saturating_mul(2));
                    if let Some(other) = target_keys.insert(OrdinalPath::new(&target), index) {
                        pending_errors.push((
                            other,
                            "Two items would have the same destination name".into(),
                        ));
                        row.error = Some("Duplicate destination name".into());
                    }
                    row_to_entry.push(Some(entries.len()));
                    entries.push(Entry {
                        source: source.clone(),
                        target: OsString::from(after),
                    });
                } else {
                    row.error = after.err();
                    row_to_entry.push(None);
                }
                rows.push(row);
            }
            Err(error) => {
                rows.push(PreviewRow {
                    before,
                    after: String::new(),
                    changed: false,
                    error: Some(error.clone()),
                });
                row_to_entry.push(None);
            }
        }
    }
    // Ancestor/descendant selections would change another selected item's path.
    // Refuse these explicitly rather than relying on a sharing violation later.
    for (index, entry) in entries.iter().enumerate() {
        if !entry.source.fingerprint.directory {
            continue;
        }
        let prefix = OrdinalPath::new(&entry.source.path);
        for (other, candidate) in entries.iter().enumerate() {
            if index != other && prefix.is_ancestor_of(&OrdinalPath::new(&candidate.source.path)) {
                let row = row_to_entry
                    .iter()
                    .position(|entry| *entry == Some(index))
                    .unwrap();
                let other_row = row_to_entry
                    .iter()
                    .position(|entry| *entry == Some(other))
                    .unwrap();
                pending_errors.push((
                    row,
                    "Select this folder separately from its descendants".into(),
                ));
                pending_errors.push((
                    other_row,
                    "A selected ancestor is also being renamed".into(),
                ));
            }
        }
    }
    for (row_index, entry_index) in row_to_entry.iter().enumerate() {
        if !current() {
            return None;
        }
        let Some(entry) = entry_index.map(|index| &entries[index]) else {
            continue;
        };
        if !rows[row_index].changed {
            continue;
        }
        let target = entry.source.parent.join(&entry.target);
        match native::open(&target, native::Mode::Inspect) {
            Ok(occupied) => {
                let selected = source_keys
                    .get(&OrdinalPath::new(&target))
                    .and_then(|index| selection.sources[*index].as_ref().ok());
                if selected.is_none_or(|source| {
                    source.fingerprint.identity != occupied.fingerprint.identity
                        || source.name
                            == entries
                                .iter()
                                .find(|entry| entry.source.path == source.path)
                                .map_or(source.name.as_os_str(), |entry| entry.target.as_os_str())
                }) {
                    pending_errors.push((
                        row_index,
                        "The destination already exists and will not be vacated by this batch"
                            .into(),
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                pending_errors.push((row_index, format!("Cannot validate destination: {error}")))
            }
        }
    }
    for (index, error) in pending_errors {
        rows[index].error = Some(error);
    }
    let changes = rows.iter().filter(|row| row.changed).count();
    let conflicts = rows.iter().filter(|row| row.error.is_some()).count();
    let error = if path_weight > MAX_PATH_BYTES {
        Some("Planned paths exceed the 8 MiB batch limit".into())
    } else {
        None
    };
    let can_apply =
        changes > 0 && conflicts == 0 && error.is_none() && rows.len() == request.paths.len();
    let preview = Arc::new(Preview {
        generation: request.generation,
        rows,
        changes,
        conflicts,
        can_apply,
        error,
    });
    Some(Plan {
        generation: request.generation,
        origin: request.origin,
        entries,
        preview,
    })
}

fn transformed_name(
    source: &Source,
    rules: &Rules,
    index: usize,
    now: SystemTime,
) -> Result<String, String> {
    let name = source.name.to_str().ok_or("Filename is not Unicode")?;
    let (stem, extension) = if source.fingerprint.directory {
        (name, "")
    } else {
        let path = Path::new(name);
        (
            path.file_stem().and_then(OsStr::to_str).unwrap_or(name),
            path.extension().and_then(OsStr::to_str).unwrap_or(""),
        )
    };
    let id = (index as u64)
        .checked_mul(rules.step)
        .and_then(|value| value.checked_add(rules.start))
        .ok_or("Incremental ID overflow")?;
    let mut base = String::with_capacity(rules.template.len().min(MAX_RULE_BYTES));
    let mut input = rules.template.as_str();
    while let Some(at) = input.find(['{', '}']) {
        push_limited(&mut base, &input[..at])?;
        input = &input[at..];
        if input.starts_with("{{") {
            push_limited(&mut base, "{")?;
            input = &input[2..];
            continue;
        }
        if input.starts_with("}}") {
            push_limited(&mut base, "}")?;
            input = &input[2..];
            continue;
        }
        if input.starts_with('}') {
            return Err("Unmatched } in name template; use }} for a literal brace".into());
        }
        let end = input.find('}').ok_or("Unclosed template token")?;
        let token = &input[1..end];
        match token {
            "stem" => push_limited(&mut base, stem)?,
            "name" => push_limited(&mut base, name)?,
            "ext" => push_limited(&mut base, extension)?,
            "id" => push_limited(
                &mut base,
                &format!("{id:0width$}", width = rules.padding as usize),
            )?,
            "date" => push_limited(&mut base, &date_utc(now)?)?,
            "created" => push_limited(&mut base, &file_date(source.fingerprint.created)?)?,
            "modified" => push_limited(&mut base, &file_date(source.fingerprint.modified)?)?,
            _ => return Err(format!("Unknown template token {{{token}}}")),
        }
        input = &input[end + 1..];
    }
    push_limited(&mut base, input)?;
    if !rules.find.is_empty() {
        let mut replaced = String::new();
        let mut start = 0;
        for (at, _) in base.match_indices(&rules.find) {
            push_limited(&mut replaced, &base[start..at])?;
            push_limited(&mut replaced, &rules.replace)?;
            start = at + rules.find.len();
        }
        push_limited(&mut replaced, &base[start..])?;
        base = replaced;
    }
    let mut output = String::new();
    push_limited(&mut output, &rules.prefix)?;
    push_limited(&mut output, &base)?;
    push_limited(&mut output, &rules.suffix)?;
    output = match rules.case {
        NameCase::Original => output,
        NameCase::Lower => output.to_lowercase(),
        NameCase::Upper => output.to_uppercase(),
    };
    if rules.keep_extension && !extension.is_empty() {
        push_limited(&mut output, ".")?;
        push_limited(&mut output, extension)?;
    }
    validate_name(&output)?;
    Ok(output)
}

fn push_limited(output: &mut String, text: &str) -> Result<(), String> {
    if output.len().saturating_add(text.len()) > 2_048 {
        return Err("Generated name is too long".into());
    }
    output.push_str(text);
    Ok(())
}
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || name.encode_utf16().count() > 255
        || name.chars().any(|c| {
            c <= '\u{1f}' || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return Err("Use a nonempty name up to 255 UTF-16 units, without invalid characters or a final dot/space".into());
    }
    let base = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        base.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    }) {
        return Err("Windows reserves this device name".into());
    }
    Ok(())
}

fn file_date(ticks: u64) -> Result<String, String> {
    let seconds = ticks / 10_000_000;
    let time = if seconds >= 11_644_473_600 {
        UNIX_EPOCH.checked_add(Duration::from_secs(seconds - 11_644_473_600))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(11_644_473_600 - seconds))
    }
    .ok_or("File date is out of range")?;
    date_utc(time)
}
fn date_utc(time: SystemTime) -> Result<String, String> {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => i64::try_from(delta.as_secs()).map_err(|_| "Date is out of range")?,
        Err(error) => {
            -i64::try_from(error.duration().as_secs()).map_err(|_| "Date is out of range")?
        }
    };
    let days = seconds.div_euclid(86_400);
    let z = days.checked_add(719_468).ok_or("Date is out of range")?;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    if !(1601..=9999).contains(&year) {
        return Err("Date is outside 1601–9999".into());
    }
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

#[derive(Clone)]
struct OrdinalPath(Vec<u16>);
impl PartialEq for OrdinalPath {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for OrdinalPath {}
impl OrdinalPath {
    fn new(path: &Path) -> Self {
        Self(native::wide_units(path.as_os_str()))
    }
    fn is_ancestor_of(&self, other: &Self) -> bool {
        other.0.len() > self.0.len()
            && matches!(other.0[self.0.len()], 47 | 92)
            && native::compare(&self.0, &other.0[..self.0.len()]) == std::cmp::Ordering::Equal
    }
}
impl Ord for OrdinalPath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        native::compare(&self.0, &other.0)
    }
}
impl PartialOrd for OrdinalPath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn execute(origin: u64, entries: Vec<Entry>, strict: bool) -> (OperationResult, Option<UndoPlan>) {
    execute_using(origin, entries, strict, &mut native::rename)
}

fn execute_using(
    origin: u64,
    entries: Vec<Entry>,
    strict: bool,
    rename: &mut dyn FnMut(&native::Handle, &native::Handle, &OsStr) -> io::Result<()>,
) -> (OperationResult, Option<UndoPlan>) {
    let entries: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.source.name != entry.target)
        .collect();
    let failed = |message: String, entries: Vec<Entry>| {
        (
            OperationResult {
                origin,
                changed: false,
                message: "No names changed".into(),
                errors: vec![message],
                needs_recovery: false,
            },
            if strict {
                None
            } else {
                Some(UndoPlan { origin, entries })
            },
        )
    };
    if entries.is_empty() {
        return failed("All names are unchanged".into(), entries);
    }
    let prepared = prepare(&entries, strict);
    let (handles, parents, parent_indices) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return failed(error, entries),
    };
    let mut current: Vec<OsString> = entries
        .iter()
        .map(|entry| entry.source.name.clone())
        .collect();
    let mut journal = Vec::<(usize, OsString, OsString)>::with_capacity(entries.len() * 2);
    let ticket = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let mut failure = None;
    'stages: for phase in 0..2 {
        for (index, entry) in entries.iter().enumerate() {
            let next = if phase == 0 {
                OsString::from(format!(
                    ".foldex-rename-{}-{ticket:016x}-{index:04x}",
                    std::process::id()
                ))
            } else {
                entry.target.clone()
            };
            match rename(&handles[index], &parents[parent_indices[index]], &next) {
                Ok(()) => {
                    journal.push((index, current[index].clone(), next.clone()));
                    current[index] = next;
                }
                Err(error) => {
                    failure = Some(format!(
                        "Cannot rename {}: {error}",
                        entry.source.path.display()
                    ));
                    break 'stages;
                }
            }
        }
    }
    let changed = !journal.is_empty();
    let mut errors = Vec::new();
    if let Some(error) = failure.as_ref() {
        errors.push(error.clone());
        for (index, from, to) in journal.iter().rev() {
            if current[*index] != *to {
                continue;
            }
            match rename(&handles[*index], &parents[parent_indices[*index]], from) {
                Ok(()) => current[*index] = from.clone(),
                Err(error) => {
                    if errors.len() < 9 {
                        errors.push(format!(
                            "Rollback cannot restore {}: {error}",
                            entries[*index].source.parent.join(from).display()
                        ));
                    }
                }
            }
        }
    }
    let undo_entries: Vec<_> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let desired = if failure.is_some() && !strict {
                &entry.target
            } else {
                &entry.source.name
            };
            if &current[index] == desired {
                return None;
            }
            let mut source = entry.source.clone();
            source.name = current[index].clone();
            source.path = source.parent.join(&source.name);
            if failure.is_some() && current[index] != entry.source.name && errors.len() < 9 {
                errors.push(format!(
                    "Recovery item remains at {}",
                    source.path.display()
                ));
            }
            Some(Entry {
                source,
                target: desired.clone(),
            })
        })
        .collect();
    let remaining = current
        .iter()
        .zip(&entries)
        .filter(|(name, entry)| **name != entry.source.name)
        .count();
    let message = if failure.is_some() {
        if remaining == 0 {
            "Rename failed; all completed steps were rolled back".into()
        } else {
            format!(
                "Rename interrupted; {remaining} items need recovery. Undo can retry without replacing files."
            )
        }
    } else if strict {
        format!("Renamed {} items", entries.len())
    } else {
        format!("Restored {} original names", entries.len())
    };
    let undo = if undo_entries.is_empty() {
        None
    } else {
        Some(UndoPlan {
            origin,
            entries: undo_entries,
        })
    };
    (
        OperationResult {
            origin,
            changed,
            message,
            errors,
            needs_recovery: failure.is_some() && remaining > 0,
        },
        undo,
    )
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
type Prepared = (Vec<native::Handle>, Vec<native::Handle>, Vec<usize>);
fn prepare(entries: &[Entry], strict: bool) -> Result<Prepared, String> {
    if entries.len() > MAX_ITEMS {
        return Err("Rename batch is too large".into());
    }
    let mut parents = Vec::new();
    let mut parent_map = BTreeMap::<OrdinalPath, usize>::new();
    let mut source_map = BTreeMap::<OrdinalPath, usize>::new();
    let mut targets = BTreeSet::new();
    let mut parent_indices = Vec::with_capacity(entries.len());
    let mut handles = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        validate_name(entry.target.to_str().ok_or("Target name is not Unicode")?)?;
        let parent_key = OrdinalPath::new(&entry.source.parent);
        let parent_index = if let Some(index) = parent_map.get(&parent_key) {
            *index
        } else {
            let parent = native::open(&entry.source.parent, native::Mode::Parent)
                .map_err(|e| e.to_string())?;
            if !parent.fingerprint.directory
                || parent.fingerprint.identity != entry.source.parent_identity
            {
                return Err("A parent folder changed since preview; reopen Rename".into());
            }
            let index = parents.len();
            parents.push(parent);
            parent_map.insert(parent_key, index);
            index
        };
        parent_indices.push(parent_index);
        let handle = native::open(&entry.source.path, native::Mode::Rename)
            .map_err(|error| format!("Cannot lock {}: {error}", entry.source.path.display()))?;
        if handle.fingerprint.identity != entry.source.fingerprint.identity
            || (strict && handle.fingerprint != entry.source.fingerprint)
        {
            return Err(format!(
                "{} changed since preview; reopen Rename",
                entry.source.path.display()
            ));
        }
        if source_map
            .insert(OrdinalPath::new(&entry.source.path), index)
            .is_some()
        {
            return Err("Duplicate source path".into());
        }
        if !targets.insert(OrdinalPath::new(&entry.source.parent.join(&entry.target))) {
            return Err("Duplicate target names".into());
        }
        handles.push(handle);
    }
    for entry in entries {
        let path = entry.source.parent.join(&entry.target);
        if let Some(index) = source_map.get(&OrdinalPath::new(&path)) {
            if handles[*index].fingerprint.identity != entries[*index].source.fingerprint.identity {
                return Err("A selected destination changed since preview".into());
            }
            continue;
        }
        match native::open(&path, native::Mode::Inspect) {
            Ok(_) => return Err(format!("Destination already exists: {}", path.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot validate {}: {error}", path.display())),
        }
    }
    Ok((handles, parents, parent_indices))
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        mem::size_of,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE, Globalization::CompareStringOrdinal,
        Storage::FileSystem::*,
    };
    pub(super) enum Mode {
        Inspect,
        Parent,
        Rename,
    }
    pub(super) struct Handle {
        handle: OwnedHandle,
        pub fingerprint: Fingerprint,
    }
    pub(super) fn wide_units(name: &OsStr) -> Vec<u16> {
        name.encode_wide().collect()
    }
    pub(super) fn compare(a: &[u16], b: &[u16]) -> std::cmp::Ordering {
        match unsafe {
            CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1)
        } {
            1 => std::cmp::Ordering::Less,
            3 => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        }
    }
    pub(super) fn open(path: &Path, mode: Mode) -> io::Result<Handle> {
        let mut wide = wide_units(path.as_os_str());
        if wide.len() > 32_766 || wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid Windows path",
            ));
        }
        wide.push(0);
        let access = FILE_READ_ATTRIBUTES
            | match mode {
                Mode::Rename => DELETE,
                Mode::Parent => FILE_TRAVERSE,
                Mode::Inspect => 0,
            };
        let share = FILE_SHARE_READ
            | FILE_SHARE_WRITE
            | if matches!(mode, Mode::Inspect) {
                FILE_SHARE_DELETE
            } else {
                0
            };
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                share,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        let mut basic = BY_HANDLE_FILE_INFORMATION::default();
        let mut identity = FILE_ID_INFO::default();
        if unsafe { GetFileInformationByHandle(raw, &mut basic) } == 0
            || unsafe {
                GetFileInformationByHandleEx(
                    raw,
                    FileIdInfo,
                    (&mut identity as *mut FILE_ID_INFO).cast(),
                    size_of::<FILE_ID_INFO>() as u32,
                )
            } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if basic.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Reparse points require a separate rename operation",
            ));
        }
        let ticks = |time: windows_sys::Win32::Foundation::FILETIME| {
            (time.dwHighDateTime as u64) << 32 | time.dwLowDateTime as u64
        };
        Ok(Handle {
            handle,
            fingerprint: Fingerprint {
                identity: Identity {
                    volume: identity.VolumeSerialNumber,
                    id: identity.FileId.Identifier,
                },
                bytes: (basic.nFileSizeHigh as u64) << 32 | basic.nFileSizeLow as u64,
                modified: ticks(basic.ftLastWriteTime),
                created: ticks(basic.ftCreationTime),
                directory: basic.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
            },
        })
    }
    pub(super) fn rename(file: &Handle, parent: &Handle, name: &OsStr) -> io::Result<()> {
        let component = wide_units(name);
        if component.is_empty()
            || component.len() > 255
            || component.contains(&0)
            || component.iter().any(|c| matches!(*c, 47 | 92 | 58))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected one filename component",
            ));
        }
        // Win32's FileRenameInfo wrapper resolves a relative name against the
        // process working directory and rejects the NT relative RootDirectory
        // form. Obtain an absolute target from the retained parent handle.
        let mut wide = final_path(parent)?;
        if wide.last() != Some(&92) {
            wide.push(92);
        }
        wide.extend_from_slice(&component);
        if wide.len() > 32_766 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Target path is too long",
            ));
        }
        let bytes = size_of::<FILE_RENAME_INFO>() + (wide.len() + 1) * 2;
        let mut storage = vec![0u64; bytes.div_ceil(size_of::<u64>())];
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*info).Anonymous.ReplaceIfExists = false;
            // Full Unicode Windows path; never process-relative or replacement.
            (*info).RootDirectory = ptr::null_mut();
            (*info).FileNameLength = (wide.len() * 2) as u32;
            ptr::copy_nonoverlapping(
                wide.as_ptr(),
                ptr::addr_of_mut!((*info).FileName).cast(),
                wide.len(),
            );
        }
        if unsafe {
            SetFileInformationByHandle(
                file.handle.as_raw_handle().cast(),
                FileRenameInfo,
                info.cast(),
                bytes as u32,
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn final_path(parent: &Handle) -> io::Result<Vec<u16>> {
        let mut buffer = vec![0u16; 512];
        loop {
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    parent.handle.as_raw_handle().cast(),
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    0,
                )
            } as usize;
            if length == 0 {
                return Err(io::Error::last_os_error());
            }
            if length < buffer.len() {
                buffer.truncate(length);
                return Ok(buffer);
            }
            if length >= 32_767 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Parent path is too long",
                ));
            }
            buffer.resize(length + 1, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn example(name: &str) -> Source {
        let parent = PathBuf::from("D:/owned-fixture");
        Source {
            path: parent.join(name),
            parent,
            parent_identity: Identity {
                volume: 1,
                id: [1; 16],
            },
            name: OsString::from(name),
            fingerprint: Fingerprint {
                identity: Identity {
                    volume: 1,
                    id: [2; 16],
                },
                bytes: 1,
                modified: 116_444_736_000_000_000,
                created: 116_444_736_000_000_000,
                directory: false,
            },
        }
    }

    fn popup_model() -> Popup {
        Popup {
            request: PreviewRequest {
                selection: 2,
                generation: 10,
                origin: 1,
                paths: Arc::new(vec![]),
                rules: Rules::default(),
                now: UNIX_EPOCH,
            },
            preview: Some(Arc::new(Preview {
                generation: 9,
                rows: vec![],
                changes: 1,
                conflicts: 0,
                can_apply: true,
                error: None,
            })),
            error: None,
            loading: false,
            caret: 0,
            applied: false,
            submitted: false,
            case_menu: Default::default(),
            case_anchor: egui::Rect::NOTHING,
            last_warning: String::new(),
            focus_template: false,
            preview_cursor: 0,
            reveal_preview: false,
            preview_prefix: String::new(),
            preview_prefix_until: 0.0,
        }
    }

    fn preview_frame(ctx: &egui::Context, popup: &mut Popup, frame: u32, events: Vec<egui::Event>) {
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(frame as f64 / 60.0),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                result_surface(
                    ui,
                    egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(600.0, 300.0)),
                    egui::Id::new("batch-preview-focus"),
                    "Rename preview",
                );
                popup.preview_input(ui.ctx(), 10);
            },
        );
        output.textures_delta.clear();
    }

    #[test]
    fn preview_keyboard_inspects_rows_without_submitting_or_editing_rules() {
        let ctx = egui::Context::default();
        let mut popup = popup_model();
        popup.preview = Some(Arc::new(Preview {
            generation: 10,
            rows: (0..24)
                .map(|index| PreviewRow {
                    before: format!("Document {index:02}.txt"),
                    after: format!("Renamed {index:02}.txt"),
                    changed: true,
                    error: None,
                })
                .collect(),
            changes: 24,
            conflicts: 0,
            can_apply: true,
            error: None,
        }));
        let template = popup.request.rules.template.clone();
        preview_frame(&ctx, &mut popup, 0, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("batch-preview-focus")));
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        preview_frame(
            &ctx,
            &mut popup,
            1,
            vec![key(egui::Key::End, egui::Modifiers::NONE)],
        );
        assert_eq!(popup.preview_cursor, 23);
        preview_frame(
            &ctx,
            &mut popup,
            2,
            vec![key(egui::Key::PageUp, egui::Modifiers::NONE)],
        );
        assert_eq!(popup.preview_cursor, 13);
        preview_frame(
            &ctx,
            &mut popup,
            3,
            vec![
                key(egui::Key::Home, egui::Modifiers::CTRL),
                key(egui::Key::Enter, egui::Modifiers::NONE),
            ],
        );
        assert_eq!(popup.preview_cursor, 13);
        assert_eq!(popup.request.rules.template, template);
        assert!(!popup.submitted && !popup.applied);
        assert!(popup.reveal_preview);
    }

    #[test]
    fn preview_typeahead_is_bounded_and_does_not_run_when_a_child_menu_owns_input() {
        let ctx = egui::Context::default();
        let mut popup = popup_model();
        popup.preview = Some(Arc::new(Preview {
            generation: 10,
            rows: ["Apple", "Banana", "Blueberry"]
                .into_iter()
                .map(|name| PreviewRow {
                    before: name.into(),
                    after: name.into(),
                    changed: false,
                    error: None,
                })
                .collect(),
            changes: 0,
            conflicts: 0,
            can_apply: false,
            error: None,
        }));
        preview_frame(&ctx, &mut popup, 0, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("batch-preview-focus")));
        preview_frame(&ctx, &mut popup, 1, vec![egui::Event::Text("b".into())]);
        assert_eq!(popup.preview_cursor, 1);
        preview_frame(&ctx, &mut popup, 2, vec![egui::Event::Text("b".into())]);
        assert_eq!(popup.preview_cursor, 2);
        popup.case_menu.open = true;
        preview_frame(&ctx, &mut popup, 3, vec![egui::Event::Text("a".into())]);
        assert_eq!(popup.preview_cursor, 2);
        popup.case_menu.open = false;
        preview_frame(
            &ctx,
            &mut popup,
            4,
            vec![egui::Event::Text("x".repeat(20_000))],
        );
        assert!(popup.preview_prefix.chars().count() <= 128);
    }

    #[test]
    fn stale_preview_cannot_enable_apply_and_old_completion_does_not_disable_new_popup() {
        let mut popup = popup_model();
        assert!(!popup.can_apply(false));
        let completion = OperationResult {
            origin: 99,
            changed: true,
            message: "Older operation completed".into(),
            errors: vec![],
            needs_recovery: false,
        };
        popup.operation_finished(&completion);
        assert!(!popup.applied && !popup.submitted);
        popup.submitted = true;
        popup.operation_finished(&completion);
        assert!(popup.applied && !popup.submitted);
    }

    #[test]
    fn preflight_failure_keeps_rules_editable_but_partial_recovery_blocks_another_apply() {
        let mut popup = popup_model();
        popup.submitted = true;
        let mut failure = OperationResult {
            origin: 1,
            changed: false,
            message: "Not applied".into(),
            errors: vec!["Changed since preview".into()],
            needs_recovery: false,
        };
        popup.operation_finished(&failure);
        assert!(!popup.applied);
        assert_eq!(popup.error.as_deref(), Some("Changed since preview"));
        popup.submitted = true;
        failure.changed = true;
        failure.needs_recovery = true;
        popup.operation_finished(&failure);
        assert!(popup.applied);
    }

    #[test]
    fn tokens_dates_ids_replacement_extension_and_unicode_caret_are_defined() {
        let source = example("report.txt");
        let rules = Rules {
            template: "{stem}-{id}-{date}".into(),
            prefix: "pre-".into(),
            suffix: "-done".into(),
            find: "report".into(),
            replace: "summary".into(),
            start: 7,
            step: 3,
            padding: 4,
            ..Default::default()
        };
        assert_eq!(
            transformed_name(&source, &rules, 2, UNIX_EPOCH).unwrap(),
            "pre-summary-0013-1970-01-01-done.txt"
        );
        let rules = Rules {
            template: "{created}_{modified}_{{literal}}".into(),
            ..Default::default()
        };
        assert_eq!(
            transformed_name(&source, &rules, 0, UNIX_EPOCH).unwrap(),
            "1970-01-01_1970-01-01_{literal}.txt"
        );
        let rules = Rules {
            template: "{unknown}".into(),
            ..Default::default()
        };
        assert!(transformed_name(&source, &rules, 0, UNIX_EPOCH).is_err());
        assert_eq!(file_date(0).unwrap(), "1601-01-01");
        assert_eq!(
            date_utc(UNIX_EPOCH + Duration::from_secs(951_782_400)).unwrap(),
            "2000-02-29"
        );
        let mut text = "αβ".to_owned();
        let mut caret = 1;
        insert_at_character(&mut text, &mut caret, "{id}");
        assert_eq!(text, "α{id}β");
        assert_eq!(caret, 5);
    }

    #[test]
    fn invalid_names_reserved_devices_overflows_and_unbounded_rules_are_rejected() {
        for invalid in [
            "",
            ".",
            "..",
            "trailing.",
            "trailing ",
            "a/b",
            "C:stream",
            "NUL.txt",
            "com¹.log",
            "LPT9",
            "CONIN$",
            "a\0b",
        ] {
            assert!(validate_name(invalid).is_err(), "accepted {invalid:?}");
        }
        for valid in [
            "COM0",
            "normal.txt",
            "名称",
            ".gitignore",
            "report (copy).txt",
        ] {
            assert!(validate_name(valid).is_ok());
        }
        let rules = Rules {
            template: "{id}".into(),
            start: u64::MAX,
            step: 2,
            ..Default::default()
        };
        assert!(transformed_name(&example("a.txt"), &rules, 1, UNIX_EPOCH).is_err());
        let request = PreviewRequest {
            selection: 1,
            generation: 1,
            origin: 1,
            paths: Arc::new(vec![PathBuf::from("D:/a"); MAX_ITEMS + 1]),
            rules: Rules::default(),
            now: UNIX_EPOCH,
        };
        assert!(validate_request(&request).is_err());
        let mut request = request;
        request.paths = Arc::new(vec![PathBuf::from("D:/a")]);
        request.rules.prefix = "x".repeat(MAX_RULE_BYTES + 1);
        assert!(validate_request(&request).is_err());
    }

    #[cfg(windows)]
    struct Fixture {
        root: PathBuf,
        base: PathBuf,
    }
    #[cfg(windows)]
    impl Fixture {
        fn new() -> Self {
            let base =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/batch-fixtures");
            fs::create_dir_all(&base).unwrap();
            let base = fs::canonicalize(base).unwrap();
            let tag = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let root = base.join(format!(
                "owned-{}-{tag}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&root).unwrap();
            Self { root, base }
        }
        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.root.join(name);
            fs::write(&path, bytes).unwrap();
            path
        }
        fn entry(&self, from: &str, to: &str) -> Entry {
            Entry {
                source: load_source(&self.root.join(from)).unwrap(),
                target: OsString::from(to),
            }
        }
        fn read(&self, name: &str) -> Vec<u8> {
            fs::read(self.root.join(name)).unwrap()
        }
        fn temp_names(&self) -> Vec<PathBuf> {
            fs::read_dir(&self.root)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".foldex-rename-")
                })
                .collect()
        }
    }
    #[cfg(windows)]
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Recursive removal is restricted to this generated, resolved root.
            let resolved = fs::canonicalize(&self.root).unwrap();
            assert_eq!(resolved, self.root);
            assert!(resolved.starts_with(&self.base));
            assert!(
                resolved
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("owned-")
            );
            fs::remove_dir_all(resolved).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn real_handle_renames_swap_names_and_undo_preserves_the_file_objects() {
        let fixture = Fixture::new();
        fixture.write("a.txt", b"A");
        fixture.write("b.txt", b"B");
        let (result, undo) = execute(
            42,
            vec![
                fixture.entry("a.txt", "b.txt"),
                fixture.entry("b.txt", "a.txt"),
            ],
            true,
        );
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(result.changed);
        assert_eq!(fixture.read("a.txt"), b"B");
        assert_eq!(fixture.read("b.txt"), b"A");
        let (result, redo) = execute(42, undo.unwrap().entries, false);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(redo.is_some());
        assert_eq!(fixture.read("a.txt"), b"A");
        assert_eq!(fixture.read("b.txt"), b"B");
        let (result, undo_again) = execute(42, redo.unwrap().entries, false);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(undo_again.is_some());
        assert_eq!(fixture.read("a.txt"), b"B");
        assert_eq!(fixture.read("b.txt"), b"A");
        assert!(fixture.temp_names().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn case_only_and_directory_renames_preserve_names_contents_and_undo() {
        let fixture = Fixture::new();
        fixture.write("Alpha.TXT", b"case");
        fs::create_dir(fixture.root.join("folder")).unwrap();
        fs::write(fixture.root.join("folder/child.txt"), b"child").unwrap();
        let (result, undo) = execute(
            1,
            vec![
                fixture.entry("Alpha.TXT", "alpha.txt"),
                fixture.entry("folder", "new-folder"),
            ],
            true,
        );
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let names: Vec<_> = fs::read_dir(&fixture.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(names.contains(&OsString::from("alpha.txt")));
        assert_eq!(
            fs::read(fixture.root.join("new-folder/child.txt")).unwrap(),
            b"child"
        );
        let (result, _) = execute(1, undo.unwrap().entries, false);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(fixture.root.join("folder/child.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn occupied_target_and_changed_source_abort_before_any_rename() {
        let fixture = Fixture::new();
        fixture.write("a.txt", b"A");
        fixture.write("occupied.txt", b"EXTERNAL");
        let (result, undo) = execute(1, vec![fixture.entry("a.txt", "occupied.txt")], true);
        assert!(!result.changed && undo.is_none());
        assert_eq!(fixture.read("a.txt"), b"A");
        assert_eq!(fixture.read("occupied.txt"), b"EXTERNAL");
        let entry = fixture.entry("a.txt", "new.txt");
        fixture.write("a.txt", b"modified after preview");
        let (result, _) = execute(1, vec![entry], true);
        assert!(
            !result.changed
                && result
                    .errors
                    .iter()
                    .any(|error| error.contains("changed since preview"))
        );
        assert!(fixture.temp_names().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn held_source_identity_blocks_external_replacement_until_teardown() {
        let fixture = Fixture::new();
        fixture.write("a.txt", b"A");
        let entries = vec![fixture.entry("a.txt", "new.txt")];
        let held = prepare(&entries, true).unwrap();
        assert!(
            fs::rename(
                fixture.root.join("a.txt"),
                fixture.root.join("external-move.txt")
            )
            .is_err()
        );
        assert_eq!(fixture.read("a.txt"), b"A");
        drop(held);
        fs::rename(
            fixture.root.join("a.txt"),
            fixture.root.join("external-move.txt"),
        )
        .unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn destination_created_after_preflight_is_never_replaced_and_stage_rolls_back() {
        let fixture = Fixture::new();
        fixture.write("a.txt", b"A");
        let mut calls = 0;
        let (result, undo) = execute_using(
            1,
            vec![fixture.entry("a.txt", "new.txt")],
            true,
            &mut |file, parent, name| {
                calls += 1;
                if calls == 2 {
                    fixture.write("new.txt", b"EXTERNAL");
                }
                native::rename(file, parent, name)
            },
        );
        assert!(result.changed && !result.errors.is_empty());
        assert!(
            result
                .message
                .contains("all completed steps were rolled back")
        );
        assert!(undo.is_none());
        assert_eq!(fixture.read("new.txt"), b"EXTERNAL");
        assert_eq!(fixture.read("a.txt"), b"A");
        assert!(fixture.temp_names().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn interrupted_swap_reverses_completed_journal_in_dependency_order() {
        let fixture = Fixture::new();
        fixture.write("a.txt", b"A");
        fixture.write("b.txt", b"B");
        let mut calls = 0;
        let (result, undo) = execute_using(
            1,
            vec![
                fixture.entry("a.txt", "b.txt"),
                fixture.entry("b.txt", "a.txt"),
            ],
            true,
            &mut |file, parent, name| {
                calls += 1;
                if calls == 4 {
                    return Err(io::Error::other("injected finalization failure"));
                }
                native::rename(file, parent, name)
            },
        );
        assert!(
            result
                .message
                .contains("all completed steps were rolled back"),
            "{} {:?}",
            result.message,
            result.errors
        );
        assert!(undo.is_none());
        assert_eq!(fixture.read("a.txt"), b"A");
        assert_eq!(fixture.read("b.txt"), b"B");
        assert!(fixture.temp_names().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn obstructed_rollback_reports_actual_recovery_path_and_undo_retries_safely() {
        let fixture = Fixture::new();
        fixture.write("a.txt", b"A");
        fixture.write("b.txt", b"B");
        let mut calls = 0;
        let (result, recovery) = execute_using(
            1,
            vec![
                fixture.entry("a.txt", "new-a.txt"),
                fixture.entry("b.txt", "new-b.txt"),
            ],
            true,
            &mut |file, parent, name| {
                calls += 1;
                if calls == 4 {
                    fixture.write("a.txt", b"EXTERNAL");
                    return Err(io::Error::other("injected collision during finalization"));
                }
                native::rename(file, parent, name)
            },
        );
        assert!(
            result.message.contains("need recovery"),
            "{} {:?}",
            result.message,
            result.errors
        );
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("Recovery item remains at"))
        );
        assert_eq!(fixture.read("a.txt"), b"EXTERNAL");
        assert_eq!(fixture.read("b.txt"), b"B");
        let recovery = recovery.unwrap();
        assert_eq!(recovery.entries.len(), 1);
        assert_eq!(fs::read(&recovery.entries[0].source.path).unwrap(), b"A");
        fs::rename(
            fixture.root.join("a.txt"),
            fixture.root.join("external-saved.txt"),
        )
        .unwrap();
        let (result, _) = execute(1, recovery.entries, false);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(fixture.read("a.txt"), b"A");
        assert_eq!(fixture.read("external-saved.txt"), b"EXTERNAL");
        assert!(fixture.temp_names().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn undo_refuses_a_replacement_object_but_allows_content_edits_on_the_same_object() {
        let fixture = Fixture::new();
        fixture.write("a.txt", b"A");
        let (_, undo) = execute(1, vec![fixture.entry("a.txt", "new.txt")], true);
        let undo = undo.unwrap();
        fixture.write("new.txt", b"edited content");
        let (result, _) = execute(1, undo.entries, false);
        assert!(result.errors.is_empty());
        assert_eq!(fixture.read("a.txt"), b"edited content");
        let (_, undo) = execute(1, vec![fixture.entry("a.txt", "new.txt")], true);
        fs::rename(
            fixture.root.join("new.txt"),
            fixture.root.join("saved-object.txt"),
        )
        .unwrap();
        fixture.write("new.txt", b"replacement");
        let (result, retry) = execute(1, undo.unwrap().entries, false);
        assert!(!result.changed && retry.is_some());
        assert_eq!(fixture.read("new.txt"), b"replacement");
        assert_eq!(fixture.read("saved-object.txt"), b"edited content");
    }

    #[cfg(windows)]
    #[test]
    fn preview_finds_duplicate_occupied_and_nested_targets_without_mutating_files() {
        let fixture = Fixture::new();
        let a = fixture.write("a.txt", b"A");
        let b = fixture.write("b.txt", b"B");
        let request = PreviewRequest {
            selection: 1,
            generation: 1,
            origin: 1,
            paths: Arc::new(vec![a, b]),
            rules: Rules {
                template: "same".into(),
                ..Default::default()
            },
            now: UNIX_EPOCH,
        };
        let selection = load_selection(&request, &|| true);
        let plan = make_plan(&request, &selection, &|| true).unwrap();
        assert_eq!(plan.preview.conflicts, 2);
        assert!(!plan.preview.can_apply);
        let mut request = request;
        request.rules.template = "a".into();
        let plan = make_plan(&request, &selection, &|| true).unwrap();
        assert!(!plan.preview.can_apply);
        assert_eq!(fixture.read("a.txt"), b"A");
        assert_eq!(fixture.read("b.txt"), b"B");
        fs::create_dir(fixture.root.join("folder")).unwrap();
        let child = fixture.write("folder/child.txt", b"child");
        request.paths = Arc::new(vec![fixture.root.join("folder"), child]);
        request.rules = Rules {
            prefix: "new-".into(),
            ..Default::default()
        };
        let plan = make_plan(&request, &load_selection(&request, &|| true), &|| true).unwrap();
        assert_eq!(plan.preview.conflicts, 2);
        assert!(!plan.preview.can_apply);
        assert!(fixture.temp_names().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn async_preview_is_latest_only_never_mutates_and_idle_shutdown_wakes() {
        let fixture = Fixture::new();
        let paths: Vec<_> = (0..64)
            .map(|index| fixture.write(&format!("file-{index}.txt"), b"owned"))
            .collect();
        let worker = Worker::new(Arc::new(|| {})).unwrap();
        for generation in 1..=50 {
            let request = PreviewRequest {
                selection: 1,
                generation,
                origin: 1,
                paths: Arc::new(paths.clone()),
                rules: Rules {
                    prefix: format!("g{generation}-"),
                    ..Default::default()
                },
                now: UNIX_EPOCH,
            };
            let start = Instant::now();
            loop {
                match worker.preview(&request) {
                    Ok(()) => break,
                    Err(_) if start.elapsed() < Duration::from_secs(5) => thread::yield_now(),
                    Err(error) => panic!("{error}"),
                }
            }
        }
        let start = Instant::now();
        loop {
            if let Some(preview) = worker.take_preview() {
                if preview.generation == 50 {
                    assert!(preview.can_apply, "{:?}", preview.error);
                    assert_eq!(preview.rows.len(), 64);
                    assert!(preview.rows.iter().all(|row| row.after.starts_with("g50-")));
                    break;
                }
            }
            assert!(start.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(1));
        }
        assert!(paths.iter().all(|path| path.exists()));
        assert!(fixture.temp_names().is_empty());
        worker.cancel_preview();
        worker.shared.stop.store(true, Ordering::Release);
        worker.shared.notify();
        while !worker.handle.as_ref().unwrap().is_finished() {
            assert!(start.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(not(windows))]
mod native {
    use super::*;
    pub(super) enum Mode {
        Inspect,
        Parent,
        Rename,
    }
    pub(super) struct Handle {
        pub fingerprint: Fingerprint,
    }
    pub(super) fn wide_units(name: &OsStr) -> Vec<u16> {
        name.to_string_lossy().encode_utf16().collect()
    }
    pub(super) fn compare(a: &[u16], b: &[u16]) -> std::cmp::Ordering {
        String::from_utf16_lossy(a)
            .to_uppercase()
            .cmp(&String::from_utf16_lossy(b).to_uppercase())
    }
    pub(super) fn open(_: &Path, _: Mode) -> io::Result<Handle> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Batch renaming requires Windows object identities",
        ))
    }
    pub(super) fn rename(_: &Handle, _: &Handle, _: &OsStr) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Batch renaming requires Windows",
        ))
    }
}
