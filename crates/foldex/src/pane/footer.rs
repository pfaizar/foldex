use super::*;
use foldex_ui::components::icon_button_at;
use foldex_ui::rolling::{RollingNumbers, RollingOptions};
use foldex_ui::{motion::MotionProfile, view::ViewMode};

#[derive(Clone, Default)]
struct FilterKeyboard {
    composing: bool,
}

#[derive(PartialEq)]
struct CountLabelKey {
    counts: Option<(usize, usize)>,
    selected: (usize, usize),
    has_selection: bool,
    font: egui::FontId,
    scale: u32,
    pixels_per_point: u32,
    fonts: usize,
}

pub(super) struct CountLabels {
    key: Option<CountLabelKey>,
    labels: [String; 2],
    widths: [f32; 2],
    target_widths: [f32; 2],
    width_scope: Option<Id>,
    timer_label: String,
    timer_width: f32,
    timer_target_width: f32,
    rolling: RollingNumbers,
}

impl Default for CountLabels {
    fn default() -> Self {
        Self {
            key: None,
            labels: Default::default(),
            widths: [0.0; 2],
            target_widths: [0.0; 2],
            width_scope: None,
            timer_label: String::new(),
            timer_width: 0.0,
            timer_target_width: 0.0,
            rolling: RollingNumbers::new(4),
        }
    }
}

impl CountLabels {
    fn prepare(
        &mut self,
        ui: &Ui,
        t: &Theme,
        counts: Option<(usize, usize)>,
        selected: (usize, usize),
        has_selection: bool,
        scope: Id,
    ) -> [f32; 2] {
        let scale = t.metrics().scale;
        let key = CountLabelKey {
            counts,
            selected,
            has_selection,
            font: t.font(),
            scale: scale.to_bits(),
            pixels_per_point: ui.ctx().pixels_per_point().to_bits(),
            fonts: foldex_ui::fonts::revision(ui.ctx()),
        };
        let reset = self.key.as_ref().is_none_or(|old| {
            old.font != key.font
                || old.scale != key.scale
                || old.pixels_per_point != key.pixels_per_point
                || old.fonts != key.fonts
        });
        if reset || self.width_scope != Some(scope) {
            self.width_scope = Some(scope);
            self.rolling.clear();
            self.timer_label.clear();
            self.timer_width = 0.0;
        }
        if self.key.as_ref() != Some(&key) {
            self.labels = [
                count_label(counts.map(|counts| counts.0), selected.0, has_selection),
                count_label(counts.map(|counts| counts.1), selected.1, has_selection),
            ];
            self.target_widths = self.labels.each_ref().map(|label| {
                ui.painter()
                    .layout_no_wrap(label.clone(), key.font.clone(), t.fg())
                    .size()
                    .x
                    + 26.0 * scale
            });
            self.key = Some(key);
        }
        if reset {
            self.widths = self.target_widths;
        } else {
            let gains = foldex_ui::motion::layout_gains(ui.ctx(), t.duration());
            let mut moving = false;
            for (width, target) in self.widths.iter_mut().zip(self.target_widths) {
                moving |= gains.step(
                    width,
                    target,
                    MotionProfile::Slow,
                    0.1 / ui.ctx().pixels_per_point(),
                );
            }
            if moving {
                ui.ctx().request_repaint();
            }
        }
        self.widths
    }

    fn prepare_timer(&mut self, ui: &Ui, t: &Theme, elapsed: Option<Duration>) -> Option<f32> {
        let elapsed = elapsed?;
        let label = elapsed_label(elapsed);
        if self.timer_label != label {
            self.timer_target_width = ui
                .painter()
                .layout_no_wrap(label.clone(), t.font(), t.dim())
                .size()
                .x
                + 26.0 * t.metrics().scale;
            self.timer_label = label;
        }
        if self.timer_width == 0.0 {
            self.timer_width = self.timer_target_width;
        } else if foldex_ui::motion::layout_gains(ui.ctx(), t.duration()).step(
            &mut self.timer_width,
            self.timer_target_width,
            MotionProfile::Slow,
            0.1 / ui.ctx().pixels_per_point(),
        ) {
            ui.ctx().request_repaint();
        }
        Some(self.timer_width)
    }
}

#[derive(Clone, Copy)]
pub(super) struct FooterLayout {
    pub search: Rect,
    pub filter: Rect,
    pub expand: Rect,
    pub clear: Rect,
    pub status: Rect,
    pub folders: Rect,
    pub files: Rect,
    pub view: Rect,
    pub timer: Option<Rect>,
}

impl Default for FooterLayout {
    fn default() -> Self {
        Self {
            search: Rect::ZERO,
            filter: Rect::ZERO,
            expand: Rect::ZERO,
            clear: Rect::ZERO,
            status: Rect::ZERO,
            folders: Rect::ZERO,
            files: Rect::ZERO,
            view: Rect::ZERO,
            timer: None,
        }
    }
}

impl FooterLayout {
    // Visibility follows the viewport, never a transient count or reel width.
    fn timer_visible(footer: Rect, scale: f32) -> bool {
        footer.width() >= 440.0 * scale
    }

    fn new(
        footer: Rect,
        scale: f32,
        clear_reveal: f32,
        counts: [f32; 2],
        timer: Option<f32>,
        expand_control: bool,
    ) -> Self {
        let inset = (2.0 * scale).min(footer.width().max(0.0) * 0.02);
        let available = (footer.width() - inset * 2.0).max(1.0);
        let gap = (8.0 * scale).min(available * 0.025);
        let height = (27.0 * scale).min(footer.height().max(1.0));
        let top = footer.center().y - height * 0.5;
        let padding = 3.0 * scale;
        let percent = 42.0 * scale;
        // Reserve Clear's full reveal independently of the status animation.
        // Count changes only move the status island's left edge; its right edge,
        // percentage control and the editor stay anchored to the viewport.
        let preferred_search =
            (footer.width() * 0.30).clamp(180.0 * scale, 248.0 * scale) + 24.0 * scale;
        let search_max = preferred_search.min((available - gap).max(0.0) * 0.48);
        let clear_slot = (24.0 * scale).min(search_max * 0.18);
        let search_width = (search_max - clear_slot).max(0.0);
        let status_max = (available - search_max - gap).max(0.0);
        let percent = percent.min((status_max - padding * 2.0).max(0.0) * 0.38);
        let status_width =
            (counts[0] + counts[1] + timer.unwrap_or(0.0) + percent + padding * 2.0 + 4.0 * scale)
                .min(status_max);
        let base_search =
            Rect::from_min_size(pos2(footer.left() + inset, top), vec2(search_width, height));
        let search = Rect::from_min_max(
            base_search.min,
            base_search.max + vec2(clear_slot * clear_reveal.clamp(0.0, 1.0), 0.0),
        );
        let status = Rect::from_min_size(
            pos2(footer.right() - inset - status_width, top),
            vec2(status_width, height),
        );
        let inner = status.shrink2(vec2(padding.min(status_width * 0.05), 2.0 * scale));
        let percent = percent.min(inner.width().max(0.0));
        let view = Rect::from_min_max(pos2(inner.right() - percent, inner.top()), inner.max);
        let slots_width = (view.left() - 4.0 * scale - inner.left()).max(0.0);
        let compression =
            (slots_width / (counts[0] + counts[1] + timer.unwrap_or(0.0)).max(1.0)).min(1.0);
        let timer = timer
            .map(|width| Rect::from_min_size(inner.min, vec2(width * compression, inner.height())));
        let count_left = timer.map_or(inner.left(), |r| r.right());
        let count_right = (view.left() - 4.0 * scale).max(count_left);
        let count_width = (count_right - count_left).max(0.0);
        let split = count_left + count_width * counts[0] / (counts[0] + counts[1]).max(1.0);
        let folders =
            Rect::from_min_max(pos2(count_left, inner.top()), pos2(split, inner.bottom()));
        let files = Rect::from_min_max(pos2(split, inner.top()), pos2(count_right, inner.bottom()));
        let expand_width = if expand_control {
            (24.0 * scale).min(search_width * 0.28)
        } else {
            0.0
        };
        let expand = Rect::from_min_max(
            pos2(
                base_search.right() - expand_width - 2.0 * scale,
                search.top() + 2.0 * scale,
            ),
            base_search.max - vec2(2.0, 2.0) * scale,
        );
        let clear = Rect::from_min_size(
            pos2(base_search.right() - 2.0 * scale, expand.top()),
            vec2(clear_slot, expand.height()),
        );
        let filter = Rect::from_min_max(
            search.min + vec2(1.0, 1.0) * scale,
            pos2(
                (expand.left() - 2.0 * scale).max(search.left() + scale),
                search.bottom() - scale,
            ),
        );
        Self {
            search,
            filter,
            expand,
            clear,
            status,
            folders,
            files,
            view,
            timer,
        }
    }
}

impl Pane {
    /// egui's consume_key intentionally ignores extra Shift/Alt modifiers.
    /// Collection navigation and editor transfers must match the actual chord.
    pub(crate) fn consume_navigation_key(
        ctx: &egui::Context,
        modifiers: egui::Modifiers,
        key: Key,
    ) -> bool {
        let taken = ctx.input_mut(|input| {
            let event = input.events.iter().position(|event| {
                matches!(event,
                egui::Event::Key { key: actual, pressed: true, modifiers: actual_modifiers, .. }
                    if *actual == key && actual_modifiers.matches_exact(modifiers))
            });
            if let Some(event) = event {
                input.events.remove(event);
                true
            } else {
                false
            }
        });
        if taken
            && matches!(
                key,
                Key::Tab | Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
            )
        {
            ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        taken
    }

    pub(crate) fn take_composite_key(ctx: &egui::Context, keys: &[Key]) -> Option<Key> {
        let key = ctx.input_mut(|input| {
            let at = input.events.iter().position(|event| matches!(event,
                egui::Event::Key { key, pressed: true, modifiers, .. }
                    if keys.contains(key) && (modifiers.matches_exact(egui::Modifiers::NONE)
                        && *key != Key::F10 || *key == Key::F10 && modifiers.matches_exact(egui::Modifiers::SHIFT))))?;
            match input.events.remove(at) { egui::Event::Key { key, .. } => Some(key), _ => None }
        });
        if key.is_some() {
            ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        key
    }

    pub(super) fn start_search_timer(&mut self) {
        self.search_timer_started = Some(Instant::now());
        self.search_elapsed = Duration::ZERO;
    }

    pub(super) fn finish_search_timer(&mut self) {
        if let Some(started) = self.search_timer_started.take() {
            self.search_elapsed = started.elapsed();
        }
    }

    pub(crate) fn benchmark_search_elapsed(&self) -> Duration {
        self.search_timer_started
            .map_or(self.search_elapsed, |started| started.elapsed())
    }

    pub(crate) fn benchmark_footer_view_rect(&self) -> Rect {
        self.footer_layout.view
    }

    pub(crate) fn benchmark_footer_kind_rect(&self, folders: bool) -> Rect {
        if folders {
            self.footer_layout.folders
        } else {
            self.footer_layout.files
        }
    }

    pub(crate) fn benchmark_footer_search_rect(&self) -> Rect {
        self.footer_layout.filter
    }

    pub(crate) fn benchmark_footer_expand_rect(&self) -> Rect {
        self.footer_layout.expand
    }

    pub(crate) fn benchmark_footer_clear_rect(&self) -> Rect {
        self.footer_layout.clear
    }

    pub(crate) fn benchmark_footer_counts(&self) -> (Option<(usize, usize)>, (usize, usize)) {
        (
            self.displayed_kind_counts()
                .filter(|_| self.query == self.rendered_query),
            self.selected_kind_counts(),
        )
    }

    pub(crate) fn benchmark_footer_view_open(&self) -> bool {
        self.zoom_open
    }

    pub(crate) fn toggle_kind_visibility(&mut self, folders: bool) {
        let show_folders = self.query.kind.includes(true) ^ folders;
        let show_files = self.query.kind.includes(false) ^ !folders;
        self.query.kind = match (show_folders, show_files) {
            (true, true) => foldex_core::KindFilter::All,
            (true, false) => foldex_core::KindFilter::Folders,
            (false, true) => foldex_core::KindFilter::Files,
            (false, false) => foldex_core::KindFilter::None,
        };
        if !self.is_home() {
            self.refilter();
        }
    }

    fn displayed_kind_counts(&self) -> Option<(usize, usize)> {
        self.kind_counts.or_else(|| {
            (!self.recursive
                && self.rendered_query.text.is_empty()
                && self.rendered_query.type_filter.is_none())
            .then(|| {
                self.snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.kind_counts(self.rendered_query.show_hidden))
            })
            .flatten()
        })
    }

    fn selected_kind_counts(&self) -> (usize, usize) {
        if self.selected.is_empty() {
            return (0, 0);
        }
        match self.rendered_query.kind {
            foldex_core::KindFilter::Folders => return (self.selected.len(), 0),
            foldex_core::KindFilter::Files => return (0, self.selected.len()),
            foldex_core::KindFilter::None => return (0, 0),
            foldex_core::KindFilter::All => {}
        }
        selected_counts(
            self.selected.intervals_iter(),
            self.selection_folder_boundary(),
        )
    }

    fn selection_folder_boundary(&self) -> usize {
        if self.rendered_query.kind.includes(true) {
            self.kind_counts.map_or_else(
                || {
                    self.snapshot.as_ref().map_or(0, |snapshot| {
                        if self.recursive {
                            0
                        } else {
                            self.indices
                                .partition_point(|&index| snapshot.entries[index as usize].is_dir)
                        }
                    })
                },
                |counts| counts.0,
            )
        } else {
            0
        }
    }

    fn footer_status(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self.pending_action.is_some() {
            return "Preparing selected files…".into();
        }
        if self.select_all_pending || self.invert_pending {
            return "Selecting all matches…".into();
        }
        if self.recursive
            && let Some(status) = &self.search_status
        {
            if status.from_cache {
                return format!("{} matches · checking saved results…", self.view_len());
            }
            if self.scanning {
                return format!(
                    "{} matches · scanning {} items",
                    self.view_len(),
                    status.entries
                );
            }
            if !status.complete || status.excluded_links > 0 {
                return format!(
                    "{} matches · {} unreadable · {} links excluded",
                    self.view_len(),
                    status.skipped,
                    status.excluded_links
                );
            }
        }
        if self.loading {
            return "Loading…".into();
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.limit.is_some())
        {
            return format!("{} matches · scope limit", self.view_len());
        }
        format!(
            "{} selected / {}",
            self.selected.len(),
            foldex_ui::item_count(self.view_len())
        )
    }

    pub(super) fn footer(&mut self, ui: &mut Ui, t: &Theme, footer: Rect) {
        let counts = self
            .displayed_kind_counts()
            .filter(|_| self.query == self.rendered_query);
        let selected = self.selected_kind_counts();
        self.footer_content(
            ui,
            t,
            footer,
            counts,
            selected,
            !self.selected.is_empty(),
            false,
        );
    }

    /// Home owns its cached counts and filtering; the controls and editor are shared.
    /// Kind buttons update `query.kind` without touching a filesystem snapshot.
    pub(crate) fn home_footer(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        footer: Rect,
        counts: Option<(usize, usize)>,
        selected: (usize, usize),
    ) {
        self.footer_content(ui, t, footer, counts, selected, selected != (0, 0), true);
    }

    fn footer_content(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        footer: Rect,
        counts: Option<(usize, usize)>,
        selected: (usize, usize),
        has_selection: bool,
        home: bool,
    ) {
        let scale = t.metrics().scale;
        let count_widths = self.footer_counts.prepare(
            ui,
            t,
            counts,
            selected,
            has_selection,
            Id::new((self.id, self.nav_generation, home)),
        );
        let elapsed = (!home && self.recursive && FooterLayout::timer_visible(footer, scale))
            .then(|| self.benchmark_search_elapsed());
        let timer_width = self.footer_counts.prepare_timer(ui, t, elapsed);
        let clear_reveal = foldex_ui::animated_state(
            ui.ctx(),
            Id::new((self.id, "footer-clear-reveal")),
            !self.filter.is_empty(),
            t,
            MotionProfile::Fast,
        );
        let layout = FooterLayout::new(
            footer,
            scale,
            clear_reveal,
            count_widths,
            timer_width,
            !home && !self.is_recycle_bin(),
        );
        self.footer_layout = layout;
        let home_status = home.then_some((counts, selected.0.saturating_add(selected.1)));
        let radius = t.radius.saturating_add(3);
        for rect in [layout.search, layout.status] {
            ui.painter()
                .rect_filled(rect, radius, Theme::color(t.surface));
            ui.painter()
                .rect_stroke(rect, radius, Stroke::new(1.0, t.line()), StrokeKind::Inside);
        }
        if !home && !self.is_recycle_bin() {
            let expand = icon_button_at(
                ui,
                layout.expand,
                Id::new((self.id, "expand-filter")),
                t,
                Icon::Recursive,
                self.recursive,
            )
            .on_hover_text(if self.recursive {
                "Return to folder filter · Ctrl+E"
            } else {
                "Search this folder and subfolders · Ctrl+E"
            });
            crate::keyboard::register(
                ui,
                crate::keyboard::Region::Footer(self.id),
                expand.id,
                layout.expand,
            );
            expand.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    ui.is_enabled(),
                    self.recursive,
                    "Search this folder and subfolders",
                )
            });
            if expand.clicked() {
                self.toggle_recursive();
            }
        }
        if let Some(timer) = layout.timer {
            let painter = ui.painter().with_clip_rect(timer.intersect(ui.clip_rect()));
            let text_space = timer.width() > 26.0 * scale;
            icon(
                &painter,
                pos2(
                    if text_space {
                        timer.left() + 10.0 * scale
                    } else {
                        timer.center().x
                    },
                    timer.center().y,
                ),
                Icon::Clock,
                t.dim(),
                (13.0 * scale).min(timer.width()),
            );
            if text_space {
                paint_footer_number(
                    &mut self.footer_counts.rolling,
                    &painter,
                    Id::new((self.id, "search-elapsed-value")),
                    Rect::from_min_max(
                        pos2(timer.left() + 22.0 * scale, timer.top()),
                        timer.max - vec2(4.0 * scale, 0.0),
                    ),
                    &self.footer_counts.timer_label,
                    t.font(),
                    t.dim(),
                    RollingOptions::from_theme(t),
                    timer.width() + 0.25 >= timer_width.unwrap_or(0.0),
                );
            }
            ui.interact(timer, Id::new((self.id, "search-elapsed")), Sense::hover())
                .on_hover_ui(|ui| {
                    ui.set_max_width(ui.spacing().tooltip_width);
                    ui.label(format!(
                        "Expanded search: {:.3} seconds\n{}",
                        self.benchmark_search_elapsed().as_secs_f64(),
                        self.footer_status()
                    ));
                });
        }
        if layout.timer.is_some() && self.recursive && self.search_timer_started.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        let mut toggled = [false; 2];
        for (folders, rect, label, count, selected, width) in [
            (
                true,
                layout.folders,
                self.footer_counts.labels[0].as_str(),
                counts.map(|counts| counts.0),
                selected.0,
                count_widths[0],
            ),
            (
                false,
                layout.files,
                self.footer_counts.labels[1].as_str(),
                counts.map(|counts| counts.1),
                selected.1,
                count_widths[1],
            ),
        ] {
            let shown = self.query.kind.includes(folders);
            let response = kind_button(
                ui,
                t,
                rect,
                Id::new((self.id, "kind-count", folders)),
                folders,
                label,
                shown,
                &mut self.footer_counts.rolling,
                rect.width() + 0.25 >= width,
            );
            crate::keyboard::register(
                ui,
                crate::keyboard::Region::Footer(self.id),
                response.id,
                rect,
            );
            toggled[usize::from(!folders)] = response
                .on_hover_ui(|ui| {
                    ui.set_max_width(ui.spacing().tooltip_width);
                    let noun = if folders { "folders" } else { "files" };
                    let total = count.map_or_else(|| "Counting…".into(), |count| count.to_string());
                    let selected = if count.is_some() {
                        selected.to_string()
                    } else {
                        "…".into()
                    };
                    let status = home_status.map_or_else(
                        || self.footer_status(),
                        |(counts, selected)| {
                            format!(
                                "{selected} selected / {}",
                                counts.map_or_else(
                                    || "Counting…".into(),
                                    |counts| foldex_ui::item_count(
                                        counts.0.saturating_add(counts.1)
                                    )
                                )
                            )
                        },
                    );
                    ui.label(format!(
                        "{selected} selected / {total} {noun}\nClick to {} {noun}\n{}",
                        if shown { "hide" } else { "show" },
                        status
                    ));
                })
                .clicked();
        }
        for (folders, toggled) in [true, false].into_iter().zip(toggled) {
            if toggled {
                self.toggle_kind_visibility(folders);
                ui.ctx().request_repaint();
            }
        }
        self.view_button_at(ui, t, layout.view);
        // The search field follows its commands in tab order. Its Tab/Down
        // transfer to the listing therefore cannot skip the footer commands.
        self.footer_filter(ui, t, layout, clear_reveal, home);
        self.zoom_popup(ui.ctx(), t);
    }

    fn footer_filter(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        layout: FooterLayout,
        clear_reveal: f32,
        home: bool,
    ) {
        let filter_id = Id::new((self.id, "filter"));
        let mut clear = false;
        // Register Clear before the field so Tab from the field can go straight
        // to results while every visible command remains in the tab sequence.
        if !self.filter.is_empty() && clear_reveal > 0.01 {
            let mut action_ui = ui.new_child(
                UiBuilder::new()
                    .id_salt(filter_id.with("clear-surface"))
                    .max_rect(layout.clear),
            );
            action_ui.set_clip_rect(layout.search.intersect(ui.clip_rect()));
            action_ui.multiply_opacity(clear_reveal);
            let action = icon_button_at(
                &action_ui,
                layout.clear,
                filter_id.with("clear"),
                t,
                Icon::Close,
                false,
            )
            .on_hover_text("Clear filter");
            action.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    foldex_ui::components::can_focus(&action_ui),
                    "Clear filter",
                )
            });
            crate::keyboard::register(
                &action_ui,
                crate::keyboard::Region::Footer(self.id),
                action.id,
                layout.clear,
            );
            clear = action.clicked();
            if clear {
                self.filter.clear();
            }
        }
        let acquire_filter = (self.focus_filter || clear) && foldex_ui::components::can_focus(ui);
        if acquire_filter {
            crate::keyboard::request_id(ui.ctx(), filter_id);
        }
        let owned = ui.memory(|memory| memory.has_focus(filter_id));
        let keyboard_id = filter_id.with("navigation");
        let mut keyboard = ui
            .ctx()
            .data(|data| data.get_temp::<FilterKeyboard>(keyboard_id))
            .unwrap_or_default();
        if owned {
            ui.input(|input| {
                for event in &input.events {
                    match event {
                        egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                            keyboard.composing = !text.is_empty()
                        }
                        egui::Event::Ime(egui::ImeEvent::Commit(_)) => keyboard.composing = false,
                        _ => {}
                    }
                }
            });
        } else {
            keyboard.composing = false;
        }
        if owned && keyboard.composing {
            ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        let transfer = owned
            && !keyboard.composing
            && [Key::ArrowDown, Key::Tab, Key::Escape]
                .into_iter()
                .any(|key| Self::consume_navigation_key(ui.ctx(), egui::Modifiers::NONE, key));
        if transfer {
            ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        let hint = if home {
            "Filter recent…"
        } else if self.is_recycle_bin() {
            "Filter Recycle Bin…"
        } else if self.recursive {
            "Search this folder and subfolders…"
        } else {
            "Filter files…"
        };
        let mut response = foldex_ui::components::search_input_unframed(
            ui,
            layout.filter,
            filter_id,
            t,
            &mut self.filter,
            hint,
            1024,
        )
        .response;
        if clear {
            response.mark_changed();
        }
        crate::keyboard::register(
            ui,
            crate::keyboard::Region::Search(self.id),
            filter_id,
            layout.filter,
        );
        if response.has_focus() {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    filter_id,
                    egui::EventFilter {
                        tab: keyboard.composing,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }
        if acquire_filter {
            self.focus_filter = ui.is_sizing_pass() || !response.has_focus() && !transfer;
            if self.focus_filter {
                ui.ctx().request_repaint();
            }
        }
        self.filter_input_frame =
            (owned || response.changed()).then(|| ui.ctx().cumulative_frame_nr());
        if response.changed() {
            if !home {
                self.refilter();
                self.focused = None;
            }
            ui.ctx().request_repaint();
        }
        if transfer {
            crate::keyboard::request_id(ui.ctx(), crate::keyboard::listing_id(self.id));
            if !home {
                if self.focused.is_none() && self.view_len() > 0 {
                    self.focused = Some(0);
                }
                self.scroll_to = self.focused;
            }
        }
        let focus = foldex_ui::animated_state(
            ui.ctx(),
            filter_id.with("island-focus"),
            response.has_focus(),
            t,
            MotionProfile::Fast,
        );
        if focus > 0.0 {
            ui.painter().rect_stroke(
                layout.search,
                t.radius.saturating_add(3),
                Stroke::new(1.0, t.accent().gamma_multiply(focus)),
                StrokeKind::Inside,
            );
        }
        ui.ctx()
            .data_mut(|data| data.insert_temp(keyboard_id, keyboard));
    }

    pub(super) fn view_button_at(&mut self, ui: &mut Ui, t: &Theme, rect: Rect) {
        let button =
            foldex_ui::components::button_response(ui, rect, Id::new((self.id, "view-button")));
        crate::keyboard::register(
            ui,
            crate::keyboard::Region::Footer(self.id),
            button.id,
            rect,
        );
        button.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!(
                    "View size: {} ({}%)",
                    ViewMode::from_zoom(self.zoom).label(),
                    (self.zoom * 100.0).round() as i32
                ),
            )
        });
        // Hover only highlights the control. Opening belongs to an explicit
        // pointer click or the focused button's Enter/Space activation.
        if ui.is_enabled() && button.clicked() {
            if !self.zoom_open {
                self.zoom_open = true;
                self.zoom_thumb = self.zoom;
                self.zoom_opened = ui.ctx().cumulative_frame_nr();
                ui.ctx().request_repaint();
            }
        }
        let hover = foldex_ui::animated_state(
            ui.ctx(),
            button.id.with("hover"),
            button.hovered() || self.zoom_open,
            t,
            MotionProfile::Fast,
        );
        ui.painter().rect_filled(
            rect,
            t.radius.saturating_sub(2),
            Theme::color(t.chrome).lerp_to_gamma(Theme::color(t.hover), hover),
        );
        ui.painter().rect_stroke(
            rect,
            t.radius.saturating_sub(2),
            Stroke::new(1.0, t.line().lerp_to_gamma(t.accent(), hover)),
            StrokeKind::Inside,
        );
        self.footer_counts.rolling.paint(
            ui.painter(),
            button.id.with("value"),
            rect.shrink2(vec2(4.0 * t.metrics().scale, 0.0)),
            &format!("{}%", (self.zoom * 100.0).round() as i32),
            t.font(),
            t.fg(),
            RollingOptions::from_theme(t),
        );
        foldex_ui::components::focus_ring(ui, rect, button.id, t, button.has_focus());
        self.zoom_anchor = button.rect;
    }

    pub(super) fn zoom_popup(&mut self, ctx: &egui::Context, t: &Theme) {
        let m = t.metrics();
        let scale = m.scale;
        let focus_id = Id::new((self.id, "view-keyboard"));
        self.zoom_focus.update(ctx, focus_id, self.zoom_open);
        if self.zoom_open {
            self.zoom_focus
                .set_return_focus(Id::new((self.id, "view-button")));
        }
        let mut keyboard_change = false;
        if self.zoom_open && self.zoom_focus.owns_keyboard(ctx, focus_id) {
            let opening = ctx.cumulative_frame_nr() == self.zoom_opened;
            // Preserve event order (including several repeats before Enter),
            // while bounding a native-input batch to a small amount of work.
            for _ in 0..32 {
                let key = ctx.input_mut(|input| {
                    let at = input.events.iter().position(zoom_navigation_key)?;
                    match input.events.remove(at) {
                        egui::Event::Key { key, .. } => Some(key),
                        _ => None,
                    }
                });
                let Some(key) = key else {
                    break;
                };
                ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
                if matches!(key, Key::Enter | Key::Space) && opening {
                    continue;
                }
                if matches!(key, Key::Escape | Key::Tab | Key::Enter | Key::Space) {
                    self.zoom_open = false;
                    self.zoom_hover_blocked = true;
                    ctx.request_repaint();
                    break;
                }
                let stops = ViewMode::stops();
                let current = stops
                    .iter()
                    .position(|(mode, _)| *mode == ViewMode::from_zoom(self.zoom))
                    .unwrap_or(0);
                let next = if key == Key::Home {
                    stops.len() - 1
                } else if key == Key::End {
                    0
                } else if matches!(key, Key::ArrowUp | Key::ArrowRight) {
                    (current + 1).min(stops.len() - 1)
                } else {
                    current.saturating_sub(1)
                };
                self.set_zoom(stops[next].1);
                keyboard_change = true;
                ctx.request_repaint();
            }
        }
        let screen = ctx.content_rect();
        let anchor = self.zoom_anchor;
        let desired = 240.0 * scale;
        let available = (anchor.top() - screen.top() - 36.0 - 6.0 * scale).max(m.control);
        let target = if available < desired {
            (((available - 8.0 * scale) / m.control).floor().max(1.0) * m.control + 8.0 * scale)
                .min(available)
        } else {
            desired
        };
        if foldex_ui::motion::popup_gains(ctx, t.duration()).step_pixels(
            &mut self.zoom_height,
            if self.zoom_open { target } else { 0.0 },
            MotionProfile::Standard,
        ) {
            ctx.request_repaint();
        }
        if self.zoom_height <= 0.25 {
            self.zoom_focus.update(ctx, focus_id, self.zoom_open);
            return;
        }
        let width = (170.0 * scale).min((screen.width() - 12.0 * scale).max(1.0));
        let left = (anchor.right() - width).clamp(
            screen.left() + 6.0 * scale,
            (screen.right() - width - 6.0 * scale).max(screen.left() + 6.0 * scale),
        );
        let bottom = anchor.top() - 6.0 * scale;
        let top = bottom - target;
        let outer = Rect::from_min_size(
            pos2(left, bottom - self.zoom_height),
            vec2(width, self.zoom_height),
        );
        foldex_ui::components::popup_surface(
            ctx,
            egui::Area::new(Id::new((self.id, "view-popup")))
                .order(egui::Order::Foreground)
                .fixed_pos(outer.min)
                .fade_in(false),
            self.zoom_open,
            |ui| {
                if self.zoom_open {
                    foldex_ui::components::PopupFocus::surface(
                        ui,
                        outer,
                        focus_id,
                        "View size menu",
                    );
                }
                ui.set_min_size(outer.size());
                ui.set_max_size(outer.size());
                ui.set_clip_rect(outer.intersect(screen));
                ui.painter().rect_filled(
                    outer,
                    t.radius.saturating_add(2),
                    Theme::color(t.surface),
                );
                ui.painter().rect_stroke(
                    outer,
                    t.radius.saturating_add(2),
                    Stroke::new(1.0, t.line()),
                    StrokeKind::Inside,
                );
                if target < desired - 0.5 {
                    let body = Rect::from_min_size(
                        pos2(left + 12.0 * scale, top + 4.0 * scale),
                        vec2(120.0 * scale, (target - 8.0 * scale).max(1.0)),
                    );
                    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
                        ui.set_clip_rect(body.intersect(outer));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        foldex_ui::components::menu_scroll_area_for_content(
                            ui,
                            t,
                            (self.id, "view-modes-scroll"),
                            body.height(),
                            ViewMode::stops().len() as f32 * m.control,
                        )
                        .show(ui, |ui| {
                            for &(mode, zoom) in ViewMode::stops().iter().rev() {
                                let (rect, _) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), m.control),
                                    Sense::hover(),
                                );
                                let selected = (self.zoom - zoom).abs() < 0.001;
                                if selected
                                    && (keyboard_change
                                        || ctx.cumulative_frame_nr() <= self.zoom_opened + 2)
                                {
                                    ui.scroll_to_rect(rect, None);
                                }
                                if foldex_ui::components::choice_row(
                                    ui,
                                    rect,
                                    Id::new((self.id, "view-mode", mode as u8)),
                                    t,
                                    mode.label(),
                                    mode.icon(),
                                    selected,
                                )
                                .clicked()
                                {
                                    self.set_zoom(zoom);
                                    crate::keyboard::request_id(ctx, focus_id);
                                }
                                if selected && self.zoom_focus.owns_keyboard(ctx, focus_id) {
                                    foldex_ui::components::focus_ring(
                                        ui,
                                        rect,
                                        focus_id.with("mode-ring"),
                                        t,
                                        true,
                                    );
                                }
                            }
                        });
                    });
                } else {
                    for &(mode, zoom) in ViewMode::stops().iter().rev() {
                        let y = top + (20.0 + (1.0 - zoom) * 200.0) * scale;
                        let rect = Rect::from_center_size(
                            pos2(left + 72.0 * scale, y),
                            vec2(120.0 * scale, 20.0 * scale),
                        );
                        ui.painter().hline(
                            (rect.left() + 4.0 * scale)..=(rect.right() - 4.0 * scale),
                            rect.bottom() - 0.5,
                            Stroke::new(1.0, t.line()),
                        );
                        if foldex_ui::components::choice_row(
                            ui,
                            rect,
                            Id::new((self.id, "view-mode", mode as u8)),
                            t,
                            mode.label(),
                            mode.icon(),
                            (self.zoom - zoom).abs() < 0.001,
                        )
                        .clicked()
                        {
                            self.set_zoom(zoom);
                            crate::keyboard::request_id(ctx, focus_id);
                        }
                        if (self.zoom - zoom).abs() < 0.001
                            && self.zoom_focus.owns_keyboard(ctx, focus_id)
                        {
                            foldex_ui::components::focus_ring(
                                ui,
                                rect,
                                focus_id.with("mode-ring"),
                                t,
                                true,
                            );
                        }
                    }
                }
                let track_inset = (20.0 * scale).min(target * 0.30);
                let track = Rect::from_min_max(
                    pos2(left + 144.0 * scale, top + track_inset),
                    pos2(left + 156.0 * scale, bottom - track_inset),
                );
                let hit = track.expand2(vec2(5.0, 10.0) * scale).intersect(outer);
                let response = ui.interact(
                    hit,
                    Id::new((self.id, "zoom-slider")),
                    Sense::CLICK | Sense::DRAG,
                );
                response.widget_info(|| {
                    egui::WidgetInfo::slider(ui.is_enabled(), self.zoom as f64, "View size")
                });
                if let Some(point) = foldex_ui::components::drag_position(ui, hit, &response) {
                    if !self.zoom_focus.owns_keyboard(ctx, focus_id) {
                        crate::keyboard::request_id(ctx, focus_id);
                    }
                    self.set_zoom((track.bottom() - point.y) / track.height().max(1.0));
                    self.zoom_thumb = self.zoom;
                } else if foldex_ui::motion::popup_gains(ctx, t.duration()).step_normalized(
                    &mut self.zoom_thumb,
                    self.zoom,
                    MotionProfile::Standard,
                ) {
                    ctx.request_repaint();
                }
                ui.painter()
                    .rect_filled(track, 6.0 * scale, Theme::color(t.chrome));
                ui.painter().rect_stroke(
                    track,
                    6.0 * scale,
                    Stroke::new(1.0, t.line()),
                    StrokeKind::Inside,
                );
                let hover = foldex_ui::animated_state(
                    ui.ctx(),
                    response.id.with("hover"),
                    response.hovered() || response.dragged(),
                    t,
                    MotionProfile::Fast,
                );
                let y = track.bottom() - self.zoom_thumb * track.height();
                ui.painter().rect_filled(
                    Rect::from_min_max(pos2(track.left(), y), track.max),
                    6.0 * scale,
                    t.accent(),
                );
                ui.painter().circle_filled(
                    pos2(track.center().x, y),
                    (9.0 + hover) * scale,
                    t.accent(),
                );
            },
        );
        if self.zoom_open
            && ctx.cumulative_frame_nr() > self.zoom_opened
            && ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer
                        .interact_pos()
                        .is_some_and(|p| !outer.contains(p) && !anchor.contains(p))
            })
        {
            self.zoom_open = false;
            self.zoom_hover_blocked = true;
            ctx.request_repaint();
        }
        self.zoom_focus.update(ctx, focus_id, self.zoom_open);
    }
}

fn zoom_navigation_key(event: &egui::Event) -> bool {
    matches!(event, egui::Event::Key { key, pressed: true, modifiers, .. }
        if (modifiers.matches_exact(egui::Modifiers::NONE)
            && matches!(key, Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
                | Key::Home | Key::End | Key::Escape | Key::Enter | Key::Space | Key::Tab))
            || (*key == Key::Tab && modifiers.matches_exact(egui::Modifiers::SHIFT)))
}

fn selected_counts(ranges: impl IntoIterator<Item = (u32, u32)>, folders: usize) -> (usize, usize) {
    ranges.into_iter().fold(
        (0usize, 0usize),
        |(folder_count, file_count), (first, last)| {
            let first = first as usize;
            let end = (last as usize).saturating_add(1);
            let selected_folders = end.min(folders).saturating_sub(first.min(folders));
            let selected_files = end.saturating_sub(first.max(folders));
            (
                folder_count.saturating_add(selected_folders),
                file_count.saturating_add(selected_files),
            )
        },
    )
}

fn count_label(total: Option<usize>, selected: usize, has_selection: bool) -> String {
    let Some(total) = total else {
        return "…".into();
    };
    let total = total.to_string();
    if has_selection {
        format!("{selected}/{total}")
    } else {
        total
    }
}

fn elapsed_label(elapsed: Duration) -> String {
    if elapsed.as_secs() < 60 {
        format!("{:.2} sec", elapsed.as_secs_f64())
    } else {
        format!(
            "{}:{:04.1}",
            elapsed.as_secs() / 60,
            elapsed.as_secs_f64() % 60.0
        )
    }
}

fn kind_button(
    ui: &Ui,
    t: &Theme,
    rect: Rect,
    id: Id,
    folders: bool,
    label: &str,
    shown: bool,
    rolling: &mut RollingNumbers,
    fitted: bool,
) -> egui::Response {
    let scale = t.metrics().scale;
    let response = foldex_ui::components::button_response(ui, rect, id);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            ui.is_enabled(),
            shown,
            format!(
                "{}: {label}",
                if folders {
                    "Show folders"
                } else {
                    "Show files"
                }
            ),
        )
    });
    let hover = foldex_ui::animated_state(
        ui.ctx(),
        id.with("hover"),
        response.hovered(),
        t,
        MotionProfile::Fast,
    );
    let opacity =
        foldex_ui::animated_state(ui.ctx(), id.with("shown"), shown, t, MotionProfile::Fast);
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.rect_filled(
        rect,
        t.radius.saturating_sub(1),
        Theme::color(t.hover).gamma_multiply(hover),
    );
    let color = t.dim().gamma_multiply(0.5).lerp_to_gamma(t.fg(), opacity);
    let text_space = rect.width() > 26.0 * scale;
    icon(
        &painter,
        pos2(
            if text_space {
                rect.left() + 10.0 * scale
            } else {
                rect.center().x
            },
            rect.center().y,
        ),
        if folders { Icon::Folder } else { Icon::File },
        color,
        (13.0 * scale).min(rect.width()),
    );
    if text_space {
        paint_footer_number(
            rolling,
            &painter,
            id.with("value"),
            Rect::from_min_max(
                pos2(rect.left() + 22.0 * scale, rect.top()),
                rect.max - vec2(4.0 * scale, 0.0),
            ),
            label,
            t.font(),
            color,
            RollingOptions::from_theme(t),
            fitted,
        );
    }
    foldex_ui::components::focus_ring(ui, rect, id, t, response.has_focus());
    response
}

#[allow(clippy::too_many_arguments)]
fn paint_footer_number(
    rolling: &mut RollingNumbers,
    painter: &egui::Painter,
    id: Id,
    rect: Rect,
    label: &str,
    font: egui::FontId,
    color: Color32,
    options: RollingOptions,
    fitted: bool,
) {
    if fitted {
        rolling.paint_fitted(painter, id, rect, label, font, color, options);
    } else {
        rolling.paint(painter, id, rect, label, font, color, options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_slots_fit_intrinsic_labels_beyond_six_digits_and_shrink_in_place() {
        let ctx = egui::Context::default();
        let t = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let mut labels = CountLabels::default();
        let footer = Rect::from_min_size(Pos2::ZERO, vec2(1800.0, 31.0));
        ctx.run_ui(Default::default(), |ui| {
            let compact = labels.prepare(ui, &t, Some((1, 2)), (0, 0), false, Id::new("folder-a"));
            assert!(compact.iter().all(|width| *width < 42.0));
            let wide = labels.prepare(
                ui,
                &t,
                Some((usize::MAX, usize::MAX)),
                (usize::MAX, usize::MAX),
                true,
                Id::new("folder-a"),
            );
            assert!(wide[0] > compact[0] && wide[1] > compact[1]);
            assert!(wide.iter().all(|width| *width > 108.0));
            for (label, width) in labels.labels.iter().zip(wide) {
                let text = ui.painter().layout_no_wrap(label.clone(), t.font(), t.fg());
                assert!((width - text.size().x - 26.0).abs() < 0.001);
            }
            let wide_layout = FooterLayout::new(footer, 1.0, 1.0, wide, None, true);
            assert!((wide_layout.folders.width() - wide[0]).abs() < 0.001);
            assert!((wide_layout.files.width() - wide[1]).abs() < 0.001);
            assert_eq!(
                labels.prepare(ui, &t, Some((1, 2)), (0, 0), false, Id::new("folder-a"),),
                compact
            );
            let mut anchored = None;
            for counts in [
                Some((1, 2)),
                None,
                Some((0, 0)),
                Some((1_000_000, 90_000_000)),
                Some((9, 99)),
            ] {
                let widths = labels.prepare(ui, &t, counts, (3, 5), true, Id::new("search"));
                let timer = labels.prepare_timer(ui, &t, Some(Duration::from_millis(1234)));
                let layout = FooterLayout::new(footer, 1.0, 1.0, widths, timer, true);
                let geometry = (layout.search, layout.status.right(), layout.view);
                assert_eq!(*anchored.get_or_insert(geometry), geometry);
                assert!((layout.folders.width() - widths[0]).abs() < 0.001);
                assert!((layout.files.width() - widths[1]).abs() < 0.001);
                assert!((layout.timer.unwrap().width() - timer.unwrap()).abs() < 0.001);
            }
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn timer_slot_fits_seconds_and_minutes_without_a_fixed_reservation() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let mut labels = CountLabels::default();
        ctx.run_ui(Default::default(), |ui| {
            let mut previous = 0.0;
            for elapsed in [
                Duration::from_secs(1),
                Duration::from_secs(59),
                Duration::from_secs(60),
                Duration::from_secs(360_000),
                Duration::from_secs(1),
            ] {
                let width = labels.prepare_timer(ui, &theme, Some(elapsed)).unwrap();
                let text =
                    ui.painter()
                        .layout_no_wrap(elapsed_label(elapsed), theme.font(), theme.dim());
                assert!((width - text.size().x - 26.0).abs() < 0.001);
                if elapsed == Duration::from_secs(360_000) {
                    assert!(width > previous);
                }
                previous = width;
            }
            let retained_width = labels.timer_width;
            assert_eq!(labels.prepare_timer(ui, &theme, None), None);
            assert_eq!(labels.timer_width, retained_width);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn actual_footer_rapid_count_updates_retarget_growth_and_shrink_without_timer_churn() {
        for width in [360.0, 520.0, 880.0, 1400.0] {
            let engine = Engine::with_options(
                Arc::new(|| {}),
                foldex_core::EngineOptions {
                    poll_interval: None,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-adaptive-footer"));
            pane.recursive = true;
            pane.kind_counts = Some((1, 2));
            pane.search_elapsed = Duration::from_secs(12);
            let ctx = egui::Context::default();
            let theme = Theme::default();
            theme.apply(&ctx);
            // Warm every footer glyph before observing geometry. Atlas replacement
            // intentionally resets both the text metrics and numeric animation.
            ctx.run_ui(Default::default(), |ui| {
                let _ = ui.painter().layout_no_wrap(
                    "0123456789/…% sec:.".into(),
                    theme.font(),
                    theme.fg(),
                );
            })
            .drop_without_applying_deltas();
            let footer = Rect::from_min_size(pos2(10.0, 450.0), vec2(width, 31.0));
            let mut anchored = None;
            let mut delay = Duration::ZERO;
            for frame in 0..100 {
                let counts = match frame {
                    0 => (1, 2),
                    1 => (9_999_999, 88_888_888),
                    2 => (usize::MAX, usize::MAX),
                    3 => (8, 9),
                    4 => (99_999_999, 9_999_999),
                    _ => (1, 2),
                };
                pane.kind_counts = Some(counts);
                let previous = pane.footer_counts.widths;
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1500.0, 500.0))),
                        time: Some(f64::from(frame + 1) / 60.0),
                        ..Default::default()
                    },
                    |ui| {
                        pane.footer(ui, &theme, footer);
                    },
                );
                let layout = pane.footer_layout;
                assert_eq!(layout.timer.is_some(), width >= 440.0);
                assert_eq!(
                    *anchored.get_or_insert((layout.search, layout.view, layout.status.right())),
                    (layout.search, layout.view, layout.status.right())
                );
                assert!(layout.search.right() < layout.status.left());
                assert!(layout.status.contains_rect(layout.folders));
                assert!(layout.status.contains_rect(layout.files));
                if width >= 880.0 {
                    assert_eq!(
                        pane.footer_counts.rolling.retained_numbers(),
                        4,
                        "growing slots must keep all numeric identities"
                    );
                }
                if frame > 0 {
                    for ((before, after), target) in previous
                        .into_iter()
                        .zip(pane.footer_counts.widths)
                        .zip(pane.footer_counts.target_widths)
                    {
                        assert!(
                            after >= before.min(target) - 0.001
                                && after <= before.max(target) + 0.001
                        );
                        if frame <= 5 && (before - target).abs() > 1.0 {
                            assert!(
                                (after - target).abs() > 0.1,
                                "retargeting must start from the displayed width"
                            );
                        }
                    }
                }
                delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
                output.drop_without_applying_deltas();
            }
            assert_eq!(pane.footer_counts.widths, pane.footer_counts.target_widths);
            assert!(pane.footer_counts.widths.iter().all(|width| *width < 42.0));
            assert_eq!(delay, Duration::MAX, "settled footer must become idle");
        }
    }

    #[test]
    fn home_footer_uses_supplied_counts_and_kind_controls_without_refiltering_a_snapshot() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-home-footer"));
        pane.navigate_home(true);
        pane.kind_counts = Some((700, 900));
        pane.query.text = "filesystem-query-sentinel".into();
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        theme.apply(&ctx);
        let folder_id = Id::new((pane.id, "kind-count", true));
        for frame in 0..2 {
            if frame == 1 {
                crate::keyboard::request_id(&ctx, folder_id);
            }
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 500.0))),
                    time: Some(f64::from(frame) / 60.0),
                    events: if frame == 1 {
                        vec![egui::Event::Key {
                            key: Key::Enter,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        }]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                },
                |ui| {
                    pane.home_footer(
                        ui,
                        &theme,
                        Rect::from_min_size(pos2(10.0, 450.0), vec2(880.0, 31.0)),
                        Some((3, 8)),
                        (2, 0),
                    );
                },
            )
            .drop_without_applying_deltas();
            assert_eq!(pane.footer_counts.labels, ["2/3", "0/8"]);
            assert_eq!(pane.footer_layout.expand.width(), 0.0);
            assert!(pane.footer_layout.timer.is_none());
            assert_eq!(pane.kind_counts, Some((700, 900)));
            assert_eq!(pane.query.text, "filesystem-query-sentinel");
        }
        assert_eq!(pane.query.kind, foldex_core::KindFilter::Files);
    }

    #[test]
    fn retained_footer_labels_follow_equal_sized_selections_and_pending_counts() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-count-cache"));
        pane.kind_counts = Some((2, 2));
        pane.selected.insert(0);
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        theme.apply(&ctx);
        let draw = |pane: &mut Pane, frame: u64| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 500.0))),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| {
                    pane.footer(
                        ui,
                        &theme,
                        Rect::from_min_size(pos2(10.0, 450.0), vec2(880.0, 31.0)),
                    );
                },
            )
            .drop_without_applying_deltas();
        };
        draw(&mut pane, 0);
        assert_eq!(pane.footer_counts.labels, ["1/2", "0/2"]);
        // Shift selection and marquee can restore a cloned base. Its revision
        // and cardinality do not uniquely identify the selected folder count.
        let revision = pane.selected.revision();
        pane.selected = Default::default();
        pane.selected.insert(3);
        assert_eq!(pane.selected.revision(), revision);
        draw(&mut pane, 1);
        assert_eq!(pane.footer_counts.labels, ["0/2", "1/2"]);
        pane.query.text = "pending".into();
        draw(&mut pane, 2);
        assert_eq!(pane.footer_counts.labels, ["…", "…"]);
        pane.rendered_query = pane.query.clone();
        pane.kind_counts = Some((0, 42));
        draw(&mut pane, 3);
        assert_eq!(pane.footer_counts.labels, ["0/0", "1/42"]);
    }

    #[test]
    fn hidden_search_timer_does_not_keep_a_narrow_footer_repainting() {
        for width in [360.0, 880.0] {
            let engine = Engine::with_options(
                Arc::new(|| {}),
                foldex_core::EngineOptions {
                    poll_interval: None,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-hidden-timer"));
            pane.recursive = true;
            pane.kind_counts = Some((2, 2));
            pane.start_search_timer();
            let ctx = egui::Context::default();
            let theme = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            theme.apply(&ctx);
            let mut delay = Duration::ZERO;
            for frame in 0..12 {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 500.0))),
                        time: Some(frame as f64 / 60.0),
                        ..Default::default()
                    },
                    |ui| {
                        pane.footer(
                            ui,
                            &theme,
                            Rect::from_min_size(pos2(10.0, 450.0), vec2(width, 31.0)),
                        );
                    },
                );
                delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
                output.drop_without_applying_deltas();
            }
            if width < 400.0 {
                assert!(pane.footer_layout.timer.is_none());
                assert_eq!(delay, Duration::MAX);
            } else {
                assert!(pane.footer_layout.timer.is_some());
                assert!(delay <= Duration::from_millis(33));
            }
        }
    }

    #[test]
    fn navigation_chords_do_not_consume_modified_editor_or_window_keys() {
        let ctx = egui::Context::default();
        let modifiers = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: Key::ArrowDown,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            },
            |ui| {
                assert!(!Pane::consume_navigation_key(
                    ui.ctx(),
                    egui::Modifiers::NONE,
                    Key::ArrowDown
                ));
                assert!(Pane::consume_navigation_key(
                    ui.ctx(),
                    modifiers,
                    Key::ArrowDown
                ));
            },
        )
        .drop_without_applying_deltas();
    }

    #[test]
    fn footer_search_transfers_keep_query_modes_and_selection() {
        for transfer in [Key::Tab, Key::ArrowDown, Key::Escape] {
            let engine = Engine::with_options(
                Arc::new(|| {}),
                foldex_core::EngineOptions {
                    poll_interval: None,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut pane = Pane::dormant(711, &engine, PathBuf::from("virtual-footer-fixture"));
            pane.filter = "report".into();
            pane.recursive = true;
            pane.toggle_kind_visibility(true);
            pane.selected.select(4, 4);
            let ctx = egui::Context::default();
            let t = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            t.apply(&ctx);
            let mut frame = 0;
            let draw = |pane: &mut Pane, frame: &mut u64, events: Vec<egui::Event>| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 500.0))),
                        time: Some(*frame as f64 / 60.0),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        crate::keyboard::composite(
                            ui,
                            crate::keyboard::Region::Listing(pane.id),
                            crate::keyboard::listing_id(pane.id),
                            Rect::from_min_size(pos2(10.0, 10.0), vec2(880.0, 420.0)),
                            "Files",
                        );
                        pane.footer(
                            ui,
                            &t,
                            Rect::from_min_size(pos2(10.0, 450.0), vec2(880.0, 31.0)),
                        );
                    },
                )
                .drop_without_applying_deltas();
                *frame += 1;
            };
            draw(&mut pane, &mut frame, vec![]);
            let filter = Id::new((pane.id, "filter"));
            crate::keyboard::request_id(&ctx, filter);
            draw(&mut pane, &mut frame, vec![]);
            draw(
                &mut pane,
                &mut frame,
                vec![egui::Event::Key {
                    key: transfer,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(pane.filter, "report");
            assert!(pane.recursive);
            assert_eq!(pane.query.kind, foldex_core::KindFilter::Files);
            assert_eq!(pane.selected.intervals(), vec![(4, 4)]);
            assert_eq!(
                ctx.memory(|memory| memory.focused()),
                Some(crate::keyboard::listing_id(pane.id))
            );
        }
    }

    #[test]
    fn footer_tab_sequence_reaches_every_command_then_returns_to_results() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(712, &engine, PathBuf::from("virtual-footer-fixture"));
        pane.filter = "report".into();
        let ctx = egui::Context::default();
        let t = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        t.apply(&ctx);
        let draw = |pane: &mut Pane, frame: u64, events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 500.0))),
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    crate::keyboard::composite(
                        ui,
                        crate::keyboard::Region::Listing(pane.id),
                        crate::keyboard::listing_id(pane.id),
                        Rect::from_min_size(pos2(10.0, 10.0), vec2(880.0, 420.0)),
                        "Files",
                    );
                    pane.footer(
                        ui,
                        &t,
                        Rect::from_min_size(pos2(10.0, 450.0), vec2(880.0, 31.0)),
                    );
                },
            )
            .drop_without_applying_deltas();
        };
        draw(&mut pane, 0, vec![]);
        crate::keyboard::request_id(&ctx, crate::keyboard::listing_id(pane.id));
        draw(&mut pane, 1, vec![]);
        let expected = [
            Id::new((pane.id, "expand-filter")),
            Id::new((pane.id, "kind-count", true)),
            Id::new((pane.id, "kind-count", false)),
            Id::new((pane.id, "view-button")),
            Id::new((pane.id, "filter")).with("clear"),
            Id::new((pane.id, "filter")),
            crate::keyboard::listing_id(pane.id),
        ];
        for (offset, expected) in expected.into_iter().enumerate() {
            draw(
                &mut pane,
                offset as u64 + 2,
                vec![egui::Event::Key {
                    key: Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(expected));
            assert_eq!(pane.filter, "report");
        }
    }

    #[test]
    fn narrow_footer_islands_and_controls_remain_disjoint() {
        for scale in [1.0, 1.5, 2.0] {
            for width in [180.0, 240.0, 360.0, 520.0, 844.0] {
                for timer in [false, true] {
                    let footer =
                        Rect::from_min_size(pos2(20.0, 30.0), vec2(width * scale, 31.0 * scale));
                    let timer = (timer && FooterLayout::timer_visible(footer, scale))
                        .then_some(70.0 * scale);
                    let layout =
                        FooterLayout::new(footer, scale, 1.0, [108.0 * scale; 2], timer, true);
                    let ordinary =
                        FooterLayout::new(footer, scale, 1.0, [108.0 * scale; 2], None, true);
                    assert_eq!(layout.filter, ordinary.filter);
                    assert_eq!(layout.expand, ordinary.expand);
                    assert_eq!(layout.search, ordinary.search);
                    assert!(layout.search.right() < layout.status.left());
                    assert!(footer.contains_rect(layout.search));
                    assert!(footer.contains_rect(layout.status));
                    assert!(layout.filter.right() <= layout.expand.left());
                    assert!(layout.folders.right() <= layout.files.left());
                    assert!(layout.files.right() <= layout.view.left());
                    assert!(layout.status.contains_rect(layout.view));
                }
            }
        }
    }

    #[test]
    fn trailing_clear_reveal_moves_only_the_search_island_right_edge() {
        for scale in [1.0, 1.5, 2.0] {
            for width in [180.0, 360.0, 844.0, 1400.0] {
                for timer in [false, true] {
                    let footer =
                        Rect::from_min_size(pos2(20.0, 30.0), vec2(width * scale, 31.0 * scale));
                    let counts = [64.0 * scale, 72.0 * scale];
                    let timer = (timer && FooterLayout::timer_visible(footer, scale))
                        .then_some(70.0 * scale);
                    let closed = FooterLayout::new(footer, scale, 0.0, counts, timer, true);
                    for reveal in [0.1, 0.5, 1.0] {
                        let shown = FooterLayout::new(footer, scale, reveal, counts, timer, true);
                        assert_eq!(shown.filter, closed.filter);
                        assert_eq!(shown.expand, closed.expand);
                        assert_eq!(shown.status, closed.status);
                        assert_eq!(shown.folders, closed.folders);
                        assert_eq!(shown.files, closed.files);
                        assert_eq!(shown.view, closed.view);
                        assert_eq!(shown.timer, closed.timer);
                        assert_eq!(shown.clear, closed.clear);
                        assert!(shown.expand.right() <= shown.clear.left());
                        assert!(shown.search.right() > closed.search.right());
                        assert!(shown.search.right() < shown.status.left());
                        assert!(
                            (shown.search.width()
                                - closed.search.width()
                                - shown.clear.width() * reveal)
                                .abs()
                                < 0.001
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn trailing_clear_removes_text_and_preserves_search_and_kind_modes() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.filter = "report".into();
        pane.recursive = true;
        pane.toggle_kind_visibility(true);
        pane.refilter();
        let ctx = egui::Context::default();
        let t = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        t.apply(&ctx);
        let footer = Rect::from_min_size(pos2(20.0, 450.0), vec2(800.0, 31.0));
        let draw = |pane: &mut Pane, frame: u64, events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 500.0))),
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    pane.footer(ui, &t, footer);
                },
            )
            .drop_without_applying_deltas();
        };
        draw(&mut pane, 0, vec![]);
        let clear = pane.footer_layout.clear.center();
        draw(&mut pane, 1, vec![egui::Event::PointerMoved(clear)]);
        for (frame, pressed) in [(2, true), (3, false)] {
            draw(
                &mut pane,
                frame,
                vec![egui::Event::PointerButton {
                    pos: clear,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        assert!(pane.filter.is_empty());
        assert!(pane.query.text.is_empty());
        assert!(pane.recursive);
        assert_eq!(pane.query.kind, foldex_core::KindFilter::Files);
        draw(&mut pane, 4, vec![]);
        draw(&mut pane, 5, vec![]);
        assert!(
            ctx.read_response(Id::new((pane.id, "filter")).with("clear"))
                .is_none()
        );
    }

    #[test]
    fn global_selection_counts_split_at_folder_boundary_without_materializing_rows() {
        assert_eq!(
            selected_counts([(3, 6), (25, 28), (900_000, 999_999)], 26),
            (5, 100_003)
        );
        assert_eq!(selected_counts([(0, 999_999)], 300_000), (300_000, 700_000));
        assert_eq!(selected_counts([(0, 9)], 0), (0, 10));
        assert_eq!(count_label(Some(26), 23, true), "23/26");
        assert_eq!(count_label(Some(26), 0, false), "26");
    }

    #[test]
    fn mixed_kind_counts_borrow_intervals_across_the_last_representable_row() {
        let mut selected = crate::selection::Selection::default();
        selected.select(0, u32::MAX);
        let rows = u32::MAX as usize + 1;
        assert_eq!(
            selected_counts(selected.intervals_iter(), 300_000),
            (300_000, rows - 300_000)
        );
        assert!(selected.remove(&299_999));
        assert!(selected.remove(&300_000));
        assert!(selected.remove(&u32::MAX));
        assert_eq!(
            selected_counts(selected.intervals_iter(), 300_000),
            (299_999, rows - 300_000 - 2)
        );
        assert_eq!(
            selected_counts(selected.intervals_iter(), rows),
            (rows - 3, 0)
        );
    }

    #[test]
    fn kind_toggles_preserve_text_and_allow_each_visibility_combination() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.filter = "report".into();
        pane.refilter();
        pane.toggle_kind_visibility(true);
        assert_eq!(pane.query.kind, foldex_core::KindFilter::Files);
        pane.toggle_kind_visibility(false);
        assert_eq!(pane.query.kind, foldex_core::KindFilter::None);
        pane.toggle_kind_visibility(true);
        assert_eq!(pane.query.kind, foldex_core::KindFilter::Folders);
        pane.filter.clear();
        pane.refilter();
        assert!(pane.query.text.is_empty());
        assert_eq!(pane.query.kind, foldex_core::KindFilter::Folders);
        pane.toggle_kind_visibility(false);
        assert_eq!(pane.query.kind, foldex_core::KindFilter::All);
    }

    #[test]
    fn view_button_keyboard_open_consumes_enter_and_space_before_the_popup() {
        for opening in [Key::Enter, Key::Space] {
            let engine = Engine::with_options(
                Arc::new(|| {}),
                foldex_core::EngineOptions {
                    poll_interval: None,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut pane = Pane::dormant(715, &engine, PathBuf::from("virtual-view-fixture"));
            let ctx = egui::Context::default();
            let t = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            t.apply(&ctx);
            let trigger = Id::new((pane.id, "view-button"));
            let key = |key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            let draw = |pane: &mut Pane, frame: u64, events: Vec<egui::Event>| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 500.0))),
                        time: Some(frame as f64 / 60.0),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        pane.view_button_at(
                            ui,
                            &t,
                            Rect::from_min_size(pos2(500.0, 450.0), vec2(42.0, 23.0)),
                        );
                        pane.zoom_popup(ui.ctx(), &t);
                    },
                )
                .drop_without_applying_deltas();
            };
            draw(&mut pane, 0, vec![]);
            crate::keyboard::request_id(&ctx, trigger);
            draw(&mut pane, 1, vec![]);
            let mut events = vec![key(opening)];
            if opening == Key::Space {
                events.push(egui::Event::Text(" ".into()));
            }
            draw(&mut pane, 2, events);
            assert!(
                pane.zoom_open,
                "the view button's opening key must leave its popup open"
            );
            assert!(!ctx.input(|input| input.key_pressed(opening)));
            assert!(!ctx.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Text(text) if text == " "))
            }));
            assert_eq!(
                ctx.memory(|memory| memory.focused()),
                Some(Id::new((pane.id, "view-keyboard")))
            );
            draw(&mut pane, 3, vec![]);
            assert!(pane.zoom_open);
            draw(&mut pane, 4, vec![key(Key::Escape)]);
            assert!(!pane.zoom_open);
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(trigger));
        }
    }

    #[test]
    fn kind_buttons_consume_only_unmodified_enter_and_space() {
        for folders in [false, true] {
            for key in [Key::Enter, Key::Space] {
                for modifiers in [
                    egui::Modifiers::NONE,
                    egui::Modifiers::SHIFT,
                    egui::Modifiers::ALT,
                ] {
                    let ctx = egui::Context::default();
                    let t = Theme {
                        reduced_motion: true,
                        ..Default::default()
                    };
                    t.apply(&ctx);
                    let id = Id::new(("kind-keyboard-fixture", folders));
                    let mut clicked = false;
                    let mut rolling = RollingNumbers::new(1);
                    let mut draw = |frame: u64, events: Vec<egui::Event>| {
                        ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(Rect::from_min_size(
                                    Pos2::ZERO,
                                    vec2(300.0, 200.0),
                                )),
                                time: Some(frame as f64 / 60.0),
                                events,
                                ..Default::default()
                            },
                            |ui| {
                                clicked = kind_button(
                                    ui,
                                    &t,
                                    Rect::from_min_size(pos2(20.0, 30.0), vec2(90.0, 24.0)),
                                    id,
                                    folders,
                                    "10",
                                    true,
                                    &mut rolling,
                                    false,
                                )
                                .clicked();
                            },
                        )
                        .drop_without_applying_deltas();
                    };
                    draw(0, vec![]);
                    crate::keyboard::request_id(&ctx, id);
                    draw(
                        1,
                        vec![egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers,
                        }],
                    );
                    assert_eq!(clicked, modifiers == egui::Modifiers::NONE);
                    assert_eq!(
                        ctx.input(|input| input.key_pressed(key)),
                        modifiers != egui::Modifiers::NONE
                    );
                }
            }
        }
    }

    #[test]
    fn zoom_menu_owns_exact_keys_and_tab_escape_enter_space_restore_its_trigger() {
        for (close, modifiers) in [
            (Key::Escape, egui::Modifiers::NONE),
            (Key::Enter, egui::Modifiers::NONE),
            (Key::Space, egui::Modifiers::NONE),
            (Key::Tab, egui::Modifiers::NONE),
            (Key::Tab, egui::Modifiers::SHIFT),
        ] {
            let engine = Engine::with_options(
                Arc::new(|| {}),
                foldex_core::EngineOptions {
                    poll_interval: None,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut pane = Pane::dormant(713, &engine, PathBuf::from("virtual-view-fixture"));
            let ctx = egui::Context::default();
            let t = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            t.apply(&ctx);
            let trigger = Id::new((pane.id, "view-button"));
            let key = |key, modifiers| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            };
            let draw = |pane: &mut Pane, frame: u64, events: Vec<egui::Event>| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 500.0))),
                        time: Some(frame as f64 / 60.0),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        pane.view_button_at(
                            ui,
                            &t,
                            Rect::from_min_size(pos2(500.0, 450.0), vec2(42.0, 23.0)),
                        );
                        pane.zoom_popup(ui.ctx(), &t);
                    },
                )
                .drop_without_applying_deltas();
            };
            draw(&mut pane, 0, vec![]);
            crate::keyboard::request_id(&ctx, trigger);
            draw(&mut pane, 1, vec![key(Key::Enter, egui::Modifiers::NONE)]);
            assert!(
                pane.zoom_open,
                "opening Enter must not close the popup immediately"
            );
            assert_eq!(
                ctx.memory(|memory| memory.focused()),
                Some(Id::new((pane.id, "view-keyboard")))
            );
            let before = pane.zoom;
            draw(
                &mut pane,
                2,
                vec![
                    key(Key::ArrowUp, egui::Modifiers::CTRL),
                    key(Key::Home, egui::Modifiers::SHIFT),
                ],
            );
            assert_eq!(pane.zoom, before);
            assert!(pane.zoom_open);
            draw(&mut pane, 3, vec![key(Key::Home, egui::Modifiers::NONE)]);
            assert_eq!(pane.zoom, ViewMode::stops().last().unwrap().1);
            draw(&mut pane, 4, vec![key(close, modifiers)]);
            assert!(!pane.zoom_open);
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(trigger));
        }
    }

    #[test]
    fn zoom_menu_does_not_consume_keys_when_another_surface_owns_focus() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(714, &engine, PathBuf::from("virtual-view-fixture"));
        let ctx = egui::Context::default();
        let t = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        t.apply(&ctx);
        pane.zoom_open = true;
        pane.zoom_anchor = Rect::from_min_size(pos2(500.0, 450.0), vec2(42.0, 23.0));
        let draw = |pane: &mut Pane, frame: u64, events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 500.0))),
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    pane.zoom_popup(ui.ctx(), &t);
                },
            )
            .drop_without_applying_deltas();
        };
        draw(&mut pane, 0, vec![]);
        let before = pane.zoom;
        crate::keyboard::request_id(&ctx, Id::new("nested-view-test-surface"));
        draw(
            &mut pane,
            1,
            vec![egui::Event::Key {
                key: Key::ArrowUp,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(pane.zoom, before);
        assert!(ctx.input(|input| input.key_pressed(Key::ArrowUp)));
    }

    #[test]
    fn percentage_hover_stays_closed_and_click_opens_a_reachable_view_menu() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        theme.apply(&ctx);
        let trigger = Rect::from_min_size(pos2(500.0, 450.0), vec2(42.0, 23.0));
        let draw = |pane: &mut Pane, frame: u64, events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 500.0))),
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    pane.view_button_at(ui, &theme, trigger);
                    pane.zoom_popup(ui.ctx(), &theme);
                },
            )
            .drop_without_applying_deltas();
        };
        draw(&mut pane, 0, vec![]);
        draw(
            &mut pane,
            1,
            vec![egui::Event::PointerMoved(trigger.center())],
        );
        assert!(!pane.zoom_open);
        for (frame, pressed) in [(2, true), (3, false)] {
            draw(
                &mut pane,
                frame,
                vec![egui::Event::PointerButton {
                    pos: trigger.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(pane.zoom_open, !pressed);
        }
        let opened = pane.zoom_opened;
        draw(
            &mut pane,
            4,
            vec![egui::Event::PointerMoved(pos2(
                trigger.right() - 70.0,
                trigger.top() - 40.0,
            ))],
        );
        assert!(pane.zoom_open);
        assert_eq!(pane.zoom_opened, opened);
        draw(
            &mut pane,
            5,
            vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        draw(
            &mut pane,
            6,
            vec![egui::Event::PointerMoved(trigger.center())],
        );
        assert!(!pane.zoom_open);
    }
}
