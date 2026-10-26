//! One keyboard owner for the virtual file list. Rows retain pointer hit targets,
//! while focus and interval selection remain independent of loaded result pages.
use super::*;
use foldex_ui::view::GridMetrics;

#[derive(Default)]
pub(super) struct ListingKeyboard {
    range: Option<KeyboardRange>,
    viewport: Option<Viewport>,
    grid: Option<GridMetrics>,
    pub(super) reveal: Option<usize>,
    was_owned: bool,
}

struct KeyboardRange {
    anchor: usize,
    focus: usize,
    additive: bool,
    selection_revision: u64,
    // Disjoint intervals, never individual paths or a materialized result set.
    base: crate::selection::Selection,
}

#[derive(Clone, Copy)]
struct Viewport {
    offset: f32,
    rect: Rect,
}

impl ListingKeyboard {
    pub(super) fn cancel_range(&mut self) {
        self.range = None;
    }
}

impl Pane {
    pub fn listing_focus_id(&self) -> Id {
        crate::keyboard::listing_id(self.id)
    }

    pub fn listing_owns_keyboard(&self, ctx: &egui::Context) -> bool {
        ctx.input(|input| input.focused) && crate::keyboard::listing_owns(ctx, self.id)
    }

    pub(crate) fn focus_search_from_typing(&mut self, ctx: &egui::Context, painted: bool) {
        let id = Id::new((self.id, "filter"));
        if painted {
            // Pointer presses choose their pane during paint. That pane's
            // editor has already run, so commit its text now, once, and let a
            // discard pass display the new query in this same native frame.
            let mut remaining = 1024usize.saturating_sub(self.filter.chars().count());
            let before = self.filter.len();
            ctx.input_mut(|input| {
                input.events.retain(|event| {
                    if let egui::Event::Text(text) = event {
                        for character in text.chars().take(remaining) {
                            self.filter.push(character);
                            remaining -= 1;
                        }
                        false
                    } else {
                        true
                    }
                });
            });
            if self.filter.len() != before {
                self.refilter();
                self.focused = None;
            }
            self.filter_input_frame = Some(ctx.cumulative_frame_nr());
            ctx.request_discard("Display search text after a pointer chose its pane");
        }
        // Returning from results continues the existing query at its end,
        // independently of a previous mouse selection inside the text field.
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(self.filter.chars().count()),
            )));
        state.store(ctx, id);
        self.focus_filter = true;
        crate::keyboard::request_id(ctx, id);
    }

    /// The application calls this only for the list's focus region, after its
    /// command dispatcher has had first refusal. Consumed key events cannot be
    /// handled again by the application's late keyboard pass. Tab and Escape
    /// remain with the application's focus/surface routing; plain Space remains
    /// the existing preview command.
    pub fn keyboard_navigation(&mut self, ctx: &egui::Context) -> Option<Action> {
        if self.is_home() || self.has_popup() || !self.listing_owns_keyboard(ctx) {
            return None;
        }
        self.clamp_keyboard_state();
        // Bound work even when a native-input batch contains many repeats.
        for _ in 0..32 {
            let stroke = ctx.input_mut(|input| {
                let at = input.events.iter().position(owned_key)?;
                match input.events.remove(at) {
                    egui::Event::Key { key, modifiers, .. } => Some((key, modifiers)),
                    _ => None,
                }
            });
            let Some((key, modifiers)) = stroke else {
                break;
            };
            // An old permutation or a pending query must never address a new
            // snapshot, including when Enter arrives during an async refresh.
            if !self.displayed_view_is_current() {
                continue;
            }
            let len = self.keyboard_len();
            if key == Key::F10 {
                if len == 0 {
                    if self.is_recycle_bin() {
                        return Some(Action::RecycleBin(crate::recycle_bin::Intent::Context(
                            self.keyboard_context_position(ctx, None),
                        )));
                    }
                    return Some(Action::Context {
                        paths: vec![self.path.clone()],
                        background: true,
                        position: self.keyboard_context_position(ctx, None),
                    });
                }
                let Some(row) = self.focused else {
                    continue;
                };
                if !self.selected.contains(&(row as u32)) {
                    self.keyboard.cancel_range();
                    self.select_all_pending = false;
                    self.invert_pending = false;
                    self.selected.clear();
                    self.selected.insert(row as u32);
                    self.anchor = Some(row);
                    self.sync_selection();
                    self.last_selection = self.primary_selected_path();
                }
                let position = self.keyboard_context_position(ctx, Some(row));
                self.reveal_keyboard_row(row);
                ctx.request_repaint();
                return self.request_selection(SelectionCommand::Context(position));
            }
            if len == 0 {
                continue;
            }
            let ctrl = modifiers.ctrl || modifiers.command;
            if key == Key::Enter {
                let Some(row) = self.focused else {
                    continue;
                };
                if self.is_recycle_bin() {
                    if !self.selected.contains(&(row as u32)) {
                        self.selected.clear();
                        self.selected.insert(row as u32);
                    }
                    return self.request_selection(SelectionCommand::Properties);
                }
                if let Some(entry) = self.entry_at(row) {
                    let directory = entry.is_dir;
                    let path = self.path_at(row)?;
                    return Some(if directory && ctrl {
                        Action::NewTab(path)
                    } else if directory {
                        Action::Navigate(path)
                    } else {
                        Action::Open(path)
                    });
                }
                // No deferred open: the user can activate after the bounded
                // window is displayed, without a stale action racing its load.
                self.reveal_keyboard_row(row);
                ctx.request_repaint();
                continue;
            }
            if key == Key::Space {
                let row = self.focused.unwrap_or_else(|| self.initial_keyboard_row());
                self.keyboard.cancel_range();
                self.select_all_pending = false;
                self.invert_pending = false;
                if !self.selected.remove(&(row as u32)) {
                    self.selected.insert(row as u32);
                }
                self.focused = Some(row);
                self.anchor = Some(row);
                self.sync_selection();
                self.last_selection = self.primary_selected_path();
                self.reveal_keyboard_row(row);
                ctx.request_repaint();
                continue;
            }
            let columns = self.grid_columns.max(1);
            let visible = self
                .keyboard
                .viewport
                .and_then(|viewport| {
                    self.keyboard.grid.map(|grid| {
                        (viewport.rect.height() / grid.stride().y.max(1.0))
                            .floor()
                            .max(1.0) as usize
                    })
                })
                .unwrap_or_else(|| self.visible_rows.len().div_ceil(columns).max(1));
            let page = visible.saturating_sub(1).max(1).saturating_mul(columns);
            if let Some(row) = navigation_target(key, self.focused, len, columns, page) {
                self.move_keyboard_focus(row, ctrl, modifiers.shift);
                ctx.request_repaint();
            }
        }
        None
    }

    fn keyboard_len(&self) -> usize {
        self.view_len().min(u32::MAX as usize + 1)
    }

    fn initial_keyboard_row(&self) -> usize {
        self.selected
            .iter()
            .next()
            .map_or(0, |row| row as usize)
            .min(self.keyboard_len().saturating_sub(1))
    }

    pub(super) fn clamp_keyboard_state(&mut self) {
        let len = self.keyboard_len();
        self.selected.clamp(len);
        if len == 0 {
            self.focused = None;
            self.anchor = None;
            self.keyboard.reveal = None;
            self.keyboard.cancel_range();
            return;
        }
        let last = len - 1;
        let focused = self.focused.map(|row| row.min(last));
        let anchor = self.anchor.map(|row| row.min(last));
        if self.focused != focused || self.anchor != anchor {
            self.keyboard.cancel_range();
        }
        self.focused = focused;
        self.anchor = anchor;
        self.keyboard.reveal = self.keyboard.reveal.map(|row| row.min(last));
    }

    fn move_keyboard_focus(&mut self, row: usize, ctrl: bool, shift: bool) {
        let previous = self.focused;
        if shift {
            let anchor = self.anchor.or(previous).unwrap_or(row);
            let continuing = self.keyboard.range.as_ref().is_some_and(|range| {
                range.anchor == anchor
                    && Some(range.focus) == previous
                    && range.additive == ctrl
                    && range.selection_revision == self.selected.revision()
            });
            if !continuing {
                self.keyboard.range = Some(KeyboardRange {
                    anchor,
                    focus: previous.unwrap_or(row),
                    additive: ctrl,
                    selection_revision: self.selected.revision(),
                    base: if ctrl {
                        self.selected.clone()
                    } else {
                        Default::default()
                    },
                });
            }
            let range = self.keyboard.range.as_mut().unwrap();
            self.selected = range.base.clone();
            self.selected.select(anchor as u32, row as u32);
            range.focus = row;
            range.selection_revision = self.selected.revision();
            self.anchor = Some(anchor);
        } else if ctrl {
            self.keyboard.cancel_range();
            self.anchor = self.anchor.or(previous).or(Some(row));
        } else {
            self.keyboard.cancel_range();
            self.selected.clear();
            self.selected.insert(row as u32);
            self.anchor = Some(row);
        }
        self.focused = Some(row);
        if shift || !ctrl {
            self.select_all_pending = false;
            self.invert_pending = false;
            self.sync_selection();
            self.last_selection = self.primary_selected_path();
        }
        self.reveal_keyboard_row(row);
    }

    fn reveal_keyboard_row(&mut self, row: usize) {
        self.keyboard.reveal = Some(row);
        self.ensure_window(row..row.saturating_add(1));
    }

    fn keyboard_context_position(&self, ctx: &egui::Context, row: Option<usize>) -> Pos2 {
        if let Some(viewport) = self
            .keyboard
            .viewport
            .filter(|viewport| viewport.rect.is_positive())
        {
            let rect = viewport.rect;
            let target = row.and_then(|row| {
                self.keyboard.grid.map(|grid| {
                    grid.rect_for(row)
                        .translate(rect.min.to_vec2() - vec2(0.0, viewport.offset))
                })
            });
            let position = target.map_or(rect.min + vec2(4.0, 4.0), |target| {
                pos2(target.left() + 4.0, target.bottom() - 1.0)
            });
            return pos2(
                position.x.clamp(rect.left(), rect.right()),
                position.y.clamp(rect.top(), rect.bottom()),
            );
        }
        self.focus_visual
            .filter(|rect| rect.is_positive())
            .map_or_else(|| ctx.content_rect().center(), |rect| rect.left_bottom())
    }

    pub(crate) fn listing_focus_surface(&mut self, ui: &Ui, body: Rect) -> bool {
        self.clamp_keyboard_state();
        let id = self.listing_focus_id();
        crate::keyboard::composite(
            ui,
            crate::keyboard::Region::Listing(self.id),
            id,
            body,
            "Files",
        );
        let owns = self.listing_owns_keyboard(ui.ctx()) && foldex_ui::components::can_focus(ui);
        if owns && self.keyboard_len() > 0 {
            let entering = !self.keyboard.was_owned || self.focused.is_none();
            if self.focused.is_none() {
                self.focused = Some(self.initial_keyboard_row());
            }
            if entering {
                self.keyboard.reveal = self.focused;
            }
        } else if !owns {
            self.keyboard.cancel_range();
        }
        self.keyboard.was_owned = owns;
        owns
    }

    pub(crate) fn focus_listing_from_pointer(&mut self, ui: &Ui) {
        if foldex_ui::components::can_focus(ui) {
            crate::keyboard::request_id(ui.ctx(), self.listing_focus_id());
            self.lock_listing_arrows(ui);
        }
    }

    fn lock_listing_arrows(&self, ui: &Ui) {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                self.listing_focus_id(),
                egui::EventFilter {
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });
    }

    /// Compact and full panes share the layout actually painted, independently
    /// of the full pane's remembered zoom and animated transition geometry.
    pub(crate) fn remember_listing_layout(&mut self, grid: GridMetrics) {
        if self.keyboard.was_owned
            && self.keyboard.grid.is_some_and(|previous| {
                previous.columns != grid.columns || previous.stride().y != grid.stride().y
            })
        {
            self.keyboard.reveal = self.focused;
        }
        self.grid_columns = grid.columns.max(1);
        self.keyboard.grid = Some(grid);
    }

    pub(crate) fn keyboard_scroll_offset(&mut self, grid: GridMetrics, height: f32) -> Option<f32> {
        self.remember_listing_layout(grid);
        let row = self.keyboard.reveal.take()?;
        let current = self
            .keyboard
            .viewport
            .map_or(0.0, |viewport| viewport.offset);
        Some(reveal_offset(grid, row, height, current))
    }

    pub(crate) fn remember_listing_viewport(&mut self, offset: f32, rect: Rect) {
        self.keyboard.viewport = Some(Viewport { offset, rect });
    }
}

fn owned_key(event: &egui::Event) -> bool {
    let egui::Event::Key {
        key,
        pressed: true,
        modifiers,
        ..
    } = event
    else {
        return false;
    };
    if modifiers.alt || modifiers.mac_cmd {
        return false;
    }
    match key {
        Key::ArrowUp
        | Key::ArrowDown
        | Key::ArrowLeft
        | Key::ArrowRight
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown => true,
        Key::Enter => !modifiers.shift,
        Key::Space => (modifiers.ctrl || modifiers.command) && !modifiers.shift,
        Key::F10 => modifiers.shift && !modifiers.ctrl && !modifiers.command,
        _ => false,
    }
}

fn navigation_target(
    key: Key,
    focused: Option<usize>,
    len: usize,
    columns: usize,
    page: usize,
) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let columns = columns.max(1);
    if columns == 1 && matches!(key, Key::ArrowLeft | Key::ArrowRight) {
        return None;
    }
    if key == Key::Home {
        return Some(0);
    }
    if key == Key::End {
        return Some(last);
    }
    let Some(row) = focused else {
        return Some(0);
    };
    let row = row.min(last);
    Some(match key {
        Key::ArrowUp => row.checked_sub(columns).unwrap_or(row),
        Key::ArrowDown if row / columns < last / columns => row.saturating_add(columns).min(last),
        Key::ArrowDown => row,
        Key::ArrowLeft => row.saturating_sub(1),
        Key::ArrowRight => row.saturating_add(1).min(last),
        Key::PageUp => row.saturating_sub(page),
        Key::PageDown => row.saturating_add(page).min(last),
        _ => return None,
    })
}

fn reveal_offset(grid: GridMetrics, row: usize, height: f32, current: f32) -> f32 {
    let target = grid.rect_for(row);
    if target.top() < current || target.height() > height {
        target.top().max(0.0)
    } else if target.bottom() > current + height {
        (target.bottom() - height).max(0.0)
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers};
    use foldex_core::{EngineOptions, Entry};

    fn fixture() -> (Engine, Pane) {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                native_watch: false,
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(17, &engine, PathBuf::from("keyboard-fixture"));
        let snapshot = Arc::new(Snapshot::from_entries(
            pane.path.clone(),
            (0..16)
                .map(|row| Entry::from_name(format!("file{row:03}"), row == 2, row, 1_792_454_400))
                .collect(),
        ));
        pane.indices = (0..snapshot.entries.len() as u32)
            .collect::<Vec<_>>()
            .into();
        pane.snapshot = Some(snapshot);
        (engine, pane)
    }

    fn press(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn dispatch(
        pane: &mut Pane,
        ctx: &egui::Context,
        owner: Id,
        events: Vec<Event>,
    ) -> (Option<Action>, Vec<Event>) {
        let mut action = None;
        let mut remaining = Vec::new();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 500.0))),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                crate::keyboard::composite(
                    ui,
                    crate::keyboard::Region::Listing(pane.id),
                    pane.listing_focus_id(),
                    ui.max_rect(),
                    "Files",
                );
                crate::keyboard::request_id(ui.ctx(), owner);
                action = pane.keyboard_navigation(ui.ctx());
                remaining = ui.input(|input| input.events.clone());
            },
        );
        output.textures_delta.clear();
        (action, remaining)
    }

    fn window(start: usize, total: usize, revision: u64) -> Arc<SearchWindow> {
        let mut snapshot = Snapshot::from_entries(
            PathBuf::from("keyboard-fixture"),
            (start..start.saturating_add(32).min(total))
                .map(|row| {
                    Entry::from_name(format!("file{row:07}"), false, row as u64, 1_792_454_400)
                })
                .collect(),
        );
        snapshot.recursive = true;
        Arc::new(SearchWindow {
            request: 1,
            start,
            snapshot: Arc::new(snapshot),
            total: Some(total),
            kind_counts: Some((0, total)),
            has_more: start.saturating_add(32) < total,
            elapsed: Duration::ZERO,
            status: foldex_index::IndexStatus {
                excluded_subtrees: Vec::new(),
                generation: "keyboard-fixture".into(),
                revision,
                scope: PathBuf::from("keyboard-fixture"),
                database_path: PathBuf::from("keyboard-fixture-index"),
                state: foldex_index::BuildState::Complete,
                complete: true,
                entries: total as u64,
                files: total as u64,
                folders: 0,
                skipped: 0,
                excluded_links: 0,
                excluded_cache: 0,
                started_unix_ms: 0,
                updated_unix_ms: 0,
                source_root_modified_secs: None,
                from_cache: false,
                synthetic: true,
                directory_only: false,
            },
        })
    }

    fn paint(pane: &mut Pane, ctx: &egui::Context, owner: Option<Id>, events: Vec<Event>) {
        let body = Rect::from_min_size(pos2(0.0, 40.0), vec2(600.0, 220.0));
        let theme = Theme::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 500.0))),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                if let Some(owner) = owner {
                    crate::keyboard::request_id(ui.ctx(), owner);
                }
                ui.interact(
                    Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 24.0)),
                    Id::new("before-files"),
                    Sense::click(),
                );
                pane.listing(
                    ui,
                    &theme,
                    true,
                    body,
                    &columns::DetailsLayout::new(
                        &columns::StoredColumns::default(),
                        body,
                        1.0,
                        0.5,
                    ),
                    None,
                    &mut Vec::new(),
                );
                ui.interact(
                    Rect::from_min_size(pos2(0.0, 300.0), vec2(100.0, 24.0)),
                    Id::new("after-files"),
                    Sense::click(),
                );
            },
        );
        output.textures_delta.clear();
    }

    #[test]
    fn typing_handoff_delivers_the_opening_text_to_the_existing_search_in_one_frame() {
        let (_engine, mut pane) = fixture();
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let id = Id::new((pane.id, "filter"));
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 500.0))),
                events: vec![press(Key::G, Modifiers::NONE), Event::Text("g".into())],
                ..Default::default()
            },
            |ui| {
                pane.focus_search_from_typing(ui.ctx(), false);
                assert!(pane.filter.is_empty(), "the real editor inserts early text");
                pane.footer(
                    ui,
                    &theme,
                    Rect::from_min_size(pos2(0.0, 450.0), vec2(800.0, 31.0)),
                );
                assert_eq!(pane.filter, "g");
                assert_eq!(ui.memory(|memory| memory.focused()), Some(id));
            },
        )
        .drop_without_applying_deltas();
    }

    #[test]
    fn late_typing_is_bounded_in_characters_and_cannot_be_replayed_on_a_discard_pass() {
        let (_engine, mut pane) = fixture();
        let ctx = egui::Context::default();
        pane.filter = "a".repeat(1022);
        let id = Id::new((pane.id, "filter"));
        let mut passes = 0;
        let output = ctx.run_ui(
            egui::RawInput {
                events: vec![Event::Text("键🙂extra".into())],
                ..Default::default()
            },
            |ui| {
                passes += 1;
                if ui.ctx().current_pass_index() == 0 {
                    pane.focus_search_from_typing(ui.ctx(), true);
                    assert!(ui.input(|input| input.events.is_empty()));
                }
                ui.interact(ui.max_rect(), id, Sense::focusable_noninteractive());
                assert_eq!(pane.filter.chars().count(), 1024);
                assert!(pane.filter.ends_with("键🙂"));
            },
        );
        assert!(passes > 1);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(id));
        let state = egui::TextEdit::load_state(&ctx, id).unwrap();
        assert_eq!(state.cursor.char_range().unwrap().primary.index.0, 1024);
        output.drop_without_applying_deltas();
    }

    #[test]
    fn grid_and_list_navigation_follow_the_rendered_row_major_layout() {
        assert_eq!(navigation_target(Key::ArrowLeft, Some(5), 16, 1, 5), None);
        assert_eq!(navigation_target(Key::ArrowRight, Some(5), 16, 1, 5), None);
        assert_eq!(
            navigation_target(Key::ArrowDown, Some(5), 16, 1, 5),
            Some(6)
        );
        assert_eq!(
            navigation_target(Key::ArrowDown, Some(5), 16, 4, 8),
            Some(9)
        );
        assert_eq!(
            navigation_target(Key::ArrowRight, Some(3), 16, 4, 8),
            Some(4)
        );
        assert_eq!(navigation_target(Key::ArrowUp, Some(2), 16, 4, 8), Some(2));
        assert_eq!(
            navigation_target(Key::ArrowDown, Some(10), 14, 4, 8),
            Some(13)
        );
        assert_eq!(
            navigation_target(Key::ArrowDown, Some(12), 14, 4, 8),
            Some(12)
        );
        assert_eq!(
            navigation_target(Key::PageDown, Some(5), 100, 4, 12),
            Some(17)
        );
        assert_eq!(navigation_target(Key::PageUp, Some(5), 100, 4, 12), Some(0));
        assert_eq!(navigation_target(Key::End, None, 100, 4, 12), Some(99));
        assert_eq!(navigation_target(Key::Home, Some(99), 100, 4, 12), Some(0));
        assert_eq!(navigation_target(Key::ArrowDown, None, 100, 4, 12), Some(0));
        assert_eq!(navigation_target(Key::End, None, 0, 1, 1), None);
    }

    #[test]
    fn shift_range_shrinks_and_crosses_its_fixed_anchor() {
        let (_engine, mut pane) = fixture();
        pane.move_keyboard_focus(5, false, false);
        pane.move_keyboard_focus(9, false, true);
        assert_eq!(pane.selected.intervals(), vec![(5, 9)]);
        pane.move_keyboard_focus(7, false, true);
        assert_eq!(pane.selected.intervals(), vec![(5, 7)]);
        pane.move_keyboard_focus(3, false, true);
        assert_eq!(pane.selected.intervals(), vec![(3, 5)]);
        assert_eq!(pane.anchor, Some(5));
    }

    #[test]
    fn ctrl_shift_keeps_disjoint_base_selection_while_its_range_shrinks() {
        let (_engine, mut pane) = fixture();
        pane.selected.insert(1);
        pane.selected.insert(12);
        pane.focused = Some(1);
        pane.anchor = Some(1);
        pane.move_keyboard_focus(6, true, true);
        assert_eq!(pane.selected.intervals(), vec![(1, 6), (12, 12)]);
        pane.move_keyboard_focus(3, true, true);
        assert_eq!(pane.selected.intervals(), vec![(1, 3), (12, 12)]);
        pane.move_keyboard_focus(0, true, true);
        assert_eq!(pane.selected.intervals(), vec![(0, 1), (12, 12)]);
    }

    #[test]
    fn external_selection_changes_cannot_reuse_an_old_keyboard_range_base() {
        let (_engine, mut pane) = fixture();
        pane.move_keyboard_focus(2, false, false);
        pane.move_keyboard_focus(6, true, true);
        pane.select_all();
        pane.move_keyboard_focus(3, true, true);
        assert_eq!(pane.selected.intervals(), vec![(0, 15)]);
    }

    #[test]
    fn ctrl_arrows_keep_selection_and_ctrl_space_toggles_the_focused_row() {
        let (_engine, mut pane) = fixture();
        let ctx = egui::Context::default();
        pane.move_keyboard_focus(4, false, false);
        let id = pane.listing_focus_id();
        let (action, remaining) = dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::ArrowDown, Modifiers::CTRL)],
        );
        assert!(action.is_none());
        assert!(remaining.is_empty());
        assert_eq!(pane.focused, Some(5));
        assert_eq!(pane.anchor, Some(4));
        assert_eq!(pane.selected.intervals(), vec![(4, 4)]);
        dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::Space, Modifiers::CTRL)],
        );
        assert_eq!(pane.selected.intervals(), vec![(4, 5)]);
        dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::Space, Modifiers::CTRL)],
        );
        assert_eq!(pane.selected.intervals(), vec![(4, 4)]);
        assert_eq!(pane.focused, Some(5));
    }

    #[test]
    fn activation_uses_focus_even_when_a_different_row_is_selected() {
        let (_engine, mut pane) = fixture();
        let ctx = egui::Context::default();
        let id = pane.listing_focus_id();
        pane.selected.insert(0);
        pane.focused = Some(2);
        let (action, _) = dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::Enter, Modifiers::CTRL)],
        );
        assert!(matches!(action, Some(Action::NewTab(path)) if path.ends_with("file002")));
        let (action, _) = dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::Enter, Modifiers::NONE)],
        );
        assert!(matches!(action, Some(Action::Navigate(path)) if path.ends_with("file002")));
        pane.focused = Some(7);
        let (action, _) = dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::Enter, Modifiers::NONE)],
        );
        assert!(matches!(action, Some(Action::Open(path)) if path.ends_with("file007")));
    }

    #[test]
    fn text_focus_and_unowned_modifiers_leave_navigation_events_untouched() {
        let (_engine, mut pane) = fixture();
        let ctx = egui::Context::default();
        pane.move_keyboard_focus(4, false, false);
        let events = vec![
            press(Key::Home, Modifiers::CTRL),
            press(Key::Enter, Modifiers::NONE),
        ];
        let (action, remaining) = dispatch(&mut pane, &ctx, Id::new("filter-editor"), events);
        assert!(action.is_none());
        assert_eq!(remaining.len(), 2);
        assert_eq!(pane.focused, Some(4));
        let id = pane.listing_focus_id();
        let events = vec![
            press(Key::ArrowDown, Modifiers::ALT),
            press(Key::ArrowDown, Modifiers::CTRL | Modifiers::ALT),
            press(Key::Space, Modifiers::NONE),
            press(Key::Tab, Modifiers::NONE),
            press(Key::Escape, Modifiers::NONE),
            Event::Text("typing".into()),
            Event::Ime(egui::ImeEvent::Preedit {
                text: "に".into(),
                active_range_chars: None,
            }),
            Event::Ime(egui::ImeEvent::Commit("日本".into())),
        ];
        let (action, remaining) = dispatch(&mut pane, &ctx, id, events);
        assert!(action.is_none());
        assert_eq!(remaining.len(), 8);
        assert_eq!(pane.selected.intervals(), vec![(4, 4)]);
    }

    #[test]
    fn a_pending_query_cannot_activate_or_move_a_stale_result_row() {
        let (_engine, mut pane) = fixture();
        let ctx = egui::Context::default();
        pane.focused = Some(2);
        pane.query.text = "changed query".into();
        let id = pane.listing_focus_id();
        let (action, remaining) = dispatch(
            &mut pane,
            &ctx,
            id,
            vec![
                press(Key::Enter, Modifiers::NONE),
                press(Key::ArrowDown, Modifiers::NONE),
            ],
        );
        assert!(action.is_none());
        assert!(remaining.is_empty());
        assert_eq!(pane.focused, Some(2));
    }

    #[test]
    fn scrolling_reveals_whole_cells_without_jumping_visible_rows_to_the_top() {
        let grid = GridMetrics::new(300.0, 12.0, 26.0, 0.0, 0.0);
        assert_eq!(reveal_offset(grid, 3, 104.0, 0.0), 0.0);
        assert_eq!(reveal_offset(grid, 4, 104.0, 0.0), 26.0);
        assert_eq!(reveal_offset(grid, 0, 104.0, 26.0), 0.0);
        let grid = GridMetrics::new(805.0, 12.0, 26.0, 8.0, 0.3);
        let row = grid.columns * 3;
        let expected = grid.rect_for(row).bottom() - 200.0;
        assert_eq!(reveal_offset(grid, row, 200.0, 0.0), expected);
    }

    #[test]
    fn compact_details_replace_the_full_panes_grid_before_paging() {
        let (_engine, mut pane) = fixture();
        pane.remember_listing_layout(GridMetrics::new(805.0, 12.0, 26.0, 8.0, 0.3));
        pane.remember_listing_viewport(0.0, Rect::from_min_size(Pos2::ZERO, vec2(600.0, 260.0)));
        pane.focused = Some(5);
        let _ = pane.keyboard_scroll_offset(GridMetrics::new(600.0, 12.0, 26.0, 0.0, 0.0), 260.0);
        let ctx = egui::Context::default();
        let id = pane.listing_focus_id();
        dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::PageDown, Modifiers::NONE)],
        );
        assert_eq!(pane.grid_columns, 1);
        assert_eq!(pane.focused, Some(14));
    }

    #[test]
    fn a_shorter_or_empty_model_clamps_selection_focus_and_anchor_together() {
        let (_engine, mut pane) = fixture();
        pane.selected.select(10, 999_999);
        pane.focused = Some(999_999);
        pane.anchor = Some(500_000);
        pane.clamp_keyboard_state();
        assert_eq!(pane.selected.intervals(), vec![(10, 15)]);
        assert_eq!(pane.focused, Some(15));
        assert_eq!(pane.anchor, Some(15));
        pane.indices = Arc::from([]);
        pane.clamp_keyboard_state();
        assert!(pane.selected.is_empty());
        assert_eq!(pane.focused, None);
        assert_eq!(pane.anchor, None);
    }

    #[test]
    fn a_million_results_keep_navigation_and_range_selection_as_positions_and_intervals() {
        let (_engine, mut pane) = fixture();
        pane.recursive = true;
        pane.accept_window(window(999_000, 1_000_123, 1));
        let ctx = egui::Context::default();
        let id = pane.listing_focus_id();
        dispatch(&mut pane, &ctx, id, vec![press(Key::End, Modifiers::CTRL)]);
        assert_eq!(pane.focused, Some(1_000_122));
        assert!(pane.selected.is_empty());
        dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::Home, Modifiers::SHIFT)],
        );
        assert_eq!(pane.selected.intervals(), vec![(0, 1_000_122)]);
        assert_eq!(pane.snapshot.as_ref().unwrap().entries.len(), 32);
        assert!(pane.entry_at(0).is_none());
    }

    #[test]
    fn additive_range_shrinking_survives_a_page_change_within_the_same_prefix() {
        let (_engine, mut pane) = fixture();
        pane.recursive = true;
        pane.accept_window(window(100, 1_000, 1));
        pane.selected.insert(100);
        pane.focused = Some(100);
        pane.anchor = Some(100);
        pane.move_keyboard_focus(110, true, true);
        pane.accept_window(window(105, 1_000, 1));
        pane.move_keyboard_focus(107, true, true);
        assert_eq!(pane.selected.intervals(), vec![(100, 107)]);
        assert_eq!(pane.focused, Some(107));
        assert!(pane.entry_at(100).is_none());
        assert!(pane.path_at(107).unwrap().ends_with("file0000107"));
    }

    #[test]
    fn an_unloaded_focus_never_opens_the_selected_path_or_defers_activation() {
        let (_engine, mut pane) = fixture();
        pane.recursive = true;
        pane.accept_window(window(100, 1_000, 1));
        pane.selected.insert(101);
        pane.focused = Some(600);
        let ctx = egui::Context::default();
        let id = pane.listing_focus_id();
        let (action, _) = dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::Enter, Modifiers::NONE)],
        );
        assert!(action.is_none());
        assert!(pane.pending_action.is_none());
        assert!(pane.drain_action().is_none());
        assert_eq!(pane.focused, Some(600));
        assert_eq!(pane.snapshot.as_ref().unwrap().entries.len(), 32);
    }

    #[test]
    fn a_replaced_expanded_prefix_cannot_reinterpret_an_unselected_focus_as_a_path() {
        let (_engine, mut pane) = fixture();
        pane.recursive = true;
        pane.accept_window(window(0, 1_000, 1));
        pane.focused = Some(5);
        pane.anchor = Some(3);
        pane.accept_window(window(0, 1_000, 2));
        assert_eq!(pane.focused, None);
        assert_eq!(pane.anchor, None);
        let ctx = egui::Context::default();
        let id = pane.listing_focus_id();
        let (action, _) = dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::Enter, Modifiers::NONE)],
        );
        assert!(action.is_none());
    }

    #[test]
    fn the_list_has_one_tab_stop_and_its_focus_visual_does_not_follow_selection() {
        let (_engine, mut pane) = fixture();
        let ctx = egui::Context::default();
        pane.selected.insert(4);
        pane.focused = Some(5);
        let id = pane.listing_focus_id();
        paint(&mut pane, &ctx, Some(id), Vec::new());
        assert!(pane.focus_visual.is_some());
        assert_eq!(pane.selected.intervals(), vec![(4, 4)]);
        assert!(ctx.read_response(id).unwrap().sense.is_focusable());
        let snapshot = pane.snapshot.as_ref().unwrap();
        let entry = pane.entry_at(5).unwrap();
        let row = Id::new((
            pane.id,
            &snapshot.path,
            &entry.raw_name,
            &entry.relative_path,
        ))
        .with("hit");
        assert!(!ctx.read_response(row).unwrap().sense.is_focusable());
        paint(
            &mut pane,
            &ctx,
            None,
            vec![press(Key::Tab, Modifiers::NONE)],
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("after-files"))
        );
        paint(&mut pane, &ctx, Some(Id::new("filter-editor")), Vec::new());
        assert!(pane.focus_visual.is_none());
        assert_eq!(pane.focused, Some(5));
        assert_eq!(pane.selected.intervals(), vec![(4, 4)]);
    }

    #[test]
    fn unloaded_rows_are_pointer_only_and_shift_tab_leaves_the_composite() {
        let (_engine, mut pane) = fixture();
        pane.recursive = true;
        pane.accept_window(window(100, 1_000, 1));
        let ctx = egui::Context::default();
        let id = pane.listing_focus_id();
        paint(&mut pane, &ctx, Some(id), Vec::new());
        assert!(pane.focus_visual.is_some());
        let row = Id::new((pane.id, "pending-row", 0_usize));
        assert!(!ctx.read_response(row).unwrap().sense.is_focusable());
        paint(
            &mut pane,
            &ctx,
            None,
            vec![press(Key::Tab, Modifiers::SHIFT)],
        );
        // egui intentionally applies backwards focus traversal next frame.
        paint(&mut pane, &ctx, None, Vec::new());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("before-files"))
        );
    }

    #[test]
    fn shift_f10_uses_the_focused_item_and_its_viewport_geometry() {
        let (_engine, mut pane) = fixture();
        pane.selected.insert(0);
        pane.focused = Some(2);
        let viewport = Rect::from_min_size(pos2(10.0, 40.0), vec2(600.0, 220.0));
        pane.remember_listing_layout(GridMetrics::new(600.0, 12.0, 26.0, 0.0, 0.0));
        pane.remember_listing_viewport(0.0, viewport);
        let ctx = egui::Context::default();
        let id = pane.listing_focus_id();
        let (action, _) = dispatch(&mut pane, &ctx, id, vec![press(Key::F10, Modifiers::SHIFT)]);
        assert!(
            matches!(action, Some(Action::Context { paths, background: false, position })
            if paths.len() == 1 && paths[0].ends_with("file002")
                && position == pos2(14.0, 117.0))
        );
        assert_eq!(pane.selected.intervals(), vec![(2, 2)]);
        pane.selected.insert(5);
        let (action, _) = dispatch(&mut pane, &ctx, id, vec![press(Key::F10, Modifiers::SHIFT)]);
        assert!(
            matches!(action, Some(Action::Context { paths, background: false, .. }) if paths.len() == 2)
        );
        let (_, remaining) = dispatch(
            &mut pane,
            &ctx,
            id,
            vec![press(Key::F10, Modifiers::CTRL | Modifiers::SHIFT)],
        );
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn shift_f10_in_an_empty_view_uses_the_folder_background() {
        let (_engine, mut pane) = fixture();
        pane.indices = Arc::from([]);
        let ctx = egui::Context::default();
        let id = pane.listing_focus_id();
        let (action, _) = dispatch(&mut pane, &ctx, id, vec![press(Key::F10, Modifiers::SHIFT)]);
        assert!(
            matches!(action, Some(Action::Context { paths, background: true, .. })
            if paths == vec![PathBuf::from("keyboard-fixture")])
        );
    }
}
