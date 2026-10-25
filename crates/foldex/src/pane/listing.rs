use super::*;
use foldex_ui::{
    motion::{MotionGains, MotionProfile},
    view::{GridMetrics, ViewMode},
};

const RETAINED_ROWS: usize = 512;

#[derive(Default)]
pub(super) struct ListingClicks {
    previous: Option<(Id, Pos2, f64)>,
    // Reading the same response twice in a frame must neither turn one release
    // into two clicks nor seed a new pair after an activation.
    frame: Option<(u64, Id, bool)>,
}

impl ListingClicks {
    pub(super) fn clear(&mut self) {
        self.previous = None;
    }

    fn double_clicked(&mut self, response: &egui::Response) -> bool {
        if !response.clicked_by(egui::PointerButton::Primary) {
            return false;
        }
        let ctx = &response.ctx;
        let frame = ctx.cumulative_frame_nr();
        if let Some((seen, identity, double)) = self.frame
            && seen == frame
        {
            return identity == response.id && double;
        }
        let Some(position) = response.interact_pointer_pos() else {
            self.clear();
            return false;
        };
        let time = ctx.input(|input| input.time);
        let options = ctx.options(|options| options.input_options);
        let previous = self.previous.take();
        let double = previous.is_some_and(|(identity, point, at)| {
            identity == response.id
                && (0.0..options.max_double_click_delay).contains(&(time - at))
                && point.distance_sq(position) < options.max_click_dist * options.max_click_dist
        }) || ctx.input(|input| {
            // A busy frame can contain both complete clicks. Preserve that
            // activation, but require both presses/releases to hit this item;
            // the global double/triple-click classification cannot prove it.
            let mut press = None;
            let mut first = None;
            for event in &input.events {
                match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        ..
                    } => {
                        press = response.interact_rect.contains(*pos).then_some(*pos);
                    }
                    egui::Event::PointerMoved(pos) => {
                        if press.is_some_and(|point: Pos2| {
                            point.distance_sq(*pos)
                                >= options.max_click_dist * options.max_click_dist
                        }) {
                            press = None;
                        }
                    }
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        ..
                    } => {
                        let clicked = press.take().is_some_and(|point| {
                            response.interact_rect.contains(*pos)
                                && point.distance_sq(*pos)
                                    < options.max_click_dist * options.max_click_dist
                        });
                        if clicked {
                            if first.is_some_and(|point: Pos2| {
                                point.distance_sq(*pos)
                                    < options.max_click_dist * options.max_click_dist
                            }) {
                                return true;
                            }
                            first = Some(*pos);
                        } else {
                            first = None;
                        }
                    }
                    egui::Event::PointerButton { .. } => {
                        press = None;
                        first = None;
                    }
                    _ => {}
                }
            }
            false
        });
        if !double {
            self.previous = Some((response.id, position, time));
        }
        self.frame = Some((frame, response.id, double));
        double
    }
}

#[derive(Default)]
pub(super) struct RowText {
    parent: Option<String>,
    size: Option<String>,
    cut: Option<(crate::cut_state::Version, bool)>,
    seen: u64,
}

impl Pane {
    pub(crate) fn benchmark_listing_state(&self) -> (bool, usize, usize, bool) {
        (
            self.listing_primed,
            self.listing_appearing,
            self.presented_entries.len(),
            self.listing_motion.is_moving(),
        )
    }
    /// Compare the real retained visible names with fresh layouts after the
    /// benchmark's atlas resets. No work is added to ordinary rendering.
    pub(crate) fn benchmark_name_galleys(&self, ctx: &egui::Context, t: &Theme) -> (usize, usize) {
        let Some(snapshot) = &self.snapshot else {
            return (0, 0);
        };
        let painter = ctx.debug_painter();
        let mut checked = 0;
        let mut mismatches = 0;
        for (&index, (width, cached, _)) in self.name_cache.iter().take(64) {
            let Some(entry) = snapshot.entries.get(index as usize) else {
                continue;
            };
            let color = if entry.hidden {
                t.fg().gamma_multiply(0.48)
            } else {
                t.fg()
            };
            let fresh = name_galley(
                &painter,
                t,
                &entry.name,
                &self.match_paint,
                color,
                f32::from_bits(*width),
            );
            checked += 1;
            mismatches += usize::from(cached.as_ref() != fresh.as_ref());
        }
        (checked, mismatches)
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        let next = if zoom.is_finite() {
            zoom.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if next != self.zoom {
            self.zoom_from = self.last_grid;
            self.zoom = next;
        }
        if let Some(row) = self.focused {
            self.scroll_to = Some(row);
        }
    }

    pub(super) fn listing(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        active: bool,
        body: Rect,
        columns: &columns::DetailsLayout,
        mut assets: Option<&mut crate::assets::AssetManager>,
        actions: &mut Vec<Action>,
    ) {
        let recycle_bin = self.is_recycle_bin();
        self.focus_visual = None;
        let keyboard_cues = foldex_ui::focus::keyboard_visible(ui.ctx());
        let restore_reveal = !keyboard_cues && self.focused.is_none();
        let reveal = self.keyboard.reveal;
        let mut listing_focused = self.listing_focus_surface(ui, body);
        if restore_reveal {
            // A fresh model can invalidate the logical cursor while the pointer
            // is dragging. Restoring the list's focus must not reveal row zero
            // over that viewport; keep only an explicit reveal already pending.
            self.keyboard.reveal = reveal;
        }
        let gains = MotionGains::new(ui.input(|i| i.stable_dt), t.duration());
        let asset_epoch = self.render_epoch.wrapping_add(self.columns.order_epoch);
        let probe_layout = ui.ctx().data(|data| {
            data.get_temp::<bool>(Id::new("benchmark-probe-layout"))
                .unwrap_or(false)
        });
        let rendered_zoom = self.zoom;
        let metrics = t.metrics();
        let caption_font = egui::FontId::proportional((t.font_size - 1.0).max(10.0));
        let caption_height = ui.fonts_mut(|fonts| fonts.row_height(&caption_font));
        self.listing_motion
            .set_pixels_per_point(ui.ctx().pixels_per_point());
        self.listing_motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(ui.ctx(), t.duration()));
        let style = (
            t.font_size.to_bits(),
            t.text,
            t.accent,
            ui.ctx().pixels_per_point().to_bits(),
            foldex_ui::fonts::revision(ui.ctx()),
        );
        if self.name_style != style {
            self.name_style = style;
            self.name_cache.clear();
        }
        let cache_frame = ui.ctx().cumulative_frame_nr();
        let snapshot = self.snapshot.clone();
        self.listing_snapshot = snapshot.as_ref().map(|s| s.id);
        // Keep the previous viewport for publication comparisons and refill the
        // other bounded map without allocating a new table on every frame.
        let mut presented = std::mem::take(&mut self.presented_scratch);
        presented.clear();
        let pending = columns::PendingPaint::new(ui.ctx(), t);
        let mut pending_numeric = false;
        let indices = self.indices.clone();
        let recursive = self.recursive;
        let window_start = self.window_start;
        let view_len = self.view_len();
        let mut clicked = None;
        let mut double = None;
        let mut row_pointer_clicked = false;
        let mut context = None;
        let mut selection_context = None;
        if snapshot.is_none() {
            if let Some(manager) = assets.as_deref_mut() {
                let signature = crate::assets::ViewportSignature {
                    epoch: asset_epoch,
                    revision: 0,
                    first: 0,
                    end: 0,
                    size: 16,
                    thumbnails: false,
                };
                let _ = manager.set_requests(self.id, signature, Vec::new);
            }
        }
        let blank = ui.interact(
            body,
            Id::new((self.id, "blank")),
            Sense::CLICK | Sense::DRAG,
        );
        if blank.clicked() {
            self.focus_listing_from_pointer(ui);
            listing_focused = true;
            self.clear_selection();
        }
        if blank.secondary_clicked() {
            self.focus_listing_from_pointer(ui);
            listing_focused = true;
            context = Some((
                vec![self.path.clone()],
                true,
                blank.interact_pointer_pos().unwrap_or(body.min),
            ));
        }
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            ui.set_clip_rect(body.intersect(ui.clip_rect()));
            ui.spacing_mut().item_spacing.y = 0.0;
            // Reserve the scrollbar's width before computing columns, avoiding a one-frame reflow.
            let width = (ui.available_width() - ui.spacing().scroll.allocated_width()).max(1.0);
            let mut grid = GridMetrics::new(
                width,
                t.font_size,
                t.file_row_height(),
                if self.zoom < 0.3 { 0.0 } else { t.gap },
                self.zoom,
            );
            if grid.mode == ViewMode::Columns {
                grid.gap.y = 0.0;
            }
            self.remember_listing_layout(grid);
            let name_rect = columns.name_rect();
            let name_bounds = [
                name_rect.left() - body.left(),
                name_rect.right() - body.left(),
            ];
            self.last_grid = Some((grid, name_bounds));
            let previous_view = self
                .zoom_from
                .filter(|_| !foldex_ui::motion::direct_layout(ui.ctx()));
            let stride = grid.stride().y;
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt((self.id, "files"))
                .auto_shrink([false, false]);
            if let Some(row) = self.scroll_to.take() {
                self.keyboard.reveal = None;
                scroll = scroll.vertical_scroll_offset((row / grid.columns) as f32 * stride);
            } else if let Some(offset) = self.keyboard_scroll_offset(grid, body.height()) {
                scroll = scroll.vertical_scroll_offset(offset);
            }
            let out = scroll.show_rows(ui, stride, grid.count_rows(view_len), |ui, range| {
                self.visible_rows = range.start.saturating_mul(grid.columns)
                    ..range.end.saturating_mul(grid.columns).min(view_len);
                if grid.mode == ViewMode::Details {
                    self.request_column_values(
                        ui.ctx(),
                        columns,
                        self.visible_rows.clone(),
                        active,
                    );
                } else {
                    self.columns.cancel(self.id);
                }
                if let Some(manager) = assets.as_deref_mut() {
                    let first = if recursive {
                        self.visible_rows.start.max(window_start)
                    } else {
                        self.visible_rows.start
                    };
                    let loaded_end = snapshot.as_ref().map_or(0, |s| {
                        if recursive {
                            window_start.saturating_add(s.entries.len())
                        } else {
                            indices.len()
                        }
                    });
                    let end = self
                        .visible_rows
                        .end
                        .min(first.saturating_add(128))
                        .min(loaded_end)
                        .max(first);
                    let physical = grid.icon_size * ui.ctx().pixels_per_point();
                    let size = [16, 48, 96, 152, 256]
                        .into_iter()
                        .find(|v| *v as f32 >= physical)
                        .unwrap_or(256);
                    let thumbnails = !recycle_bin && grid.mode.is_grid();
                    let signature = crate::assets::ViewportSignature {
                        epoch: asset_epoch,
                        revision: snapshot.as_ref().map_or(0, |s| s.id),
                        first,
                        end,
                        size,
                        thumbnails,
                    };
                    let _ = manager.set_requests(self.id, signature, || {
                        (first..end)
                            .filter_map(|row| {
                                let s = snapshot.as_ref()?;
                                let index = if recursive {
                                    row.checked_sub(window_start)?
                                } else {
                                    *indices.get(row)? as usize
                                };
                                let e = s.entries.get(index)?;
                                Some(crate::assets::VisibleAsset {
                                    key: if recycle_bin {
                                        crate::recycle_bin::icon_key(e, size)
                                    } else {
                                        foldex_assets::AssetKey {
                                            path: e.path(s),
                                            size,
                                            thumbnail: thumbnails,
                                        }
                                    },
                                    stamp: if recycle_bin {
                                        crate::assets::AssetStamp::UNOBSERVED
                                    } else {
                                        crate::assets::AssetStamp {
                                            bytes: e.size,
                                            modified_secs: e.modified_secs,
                                            is_dir: e.is_dir,
                                        }
                                    },
                                })
                            })
                            .collect()
                    });
                }
                for grid_row in range {
                    let (row_rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), stride), Sense::hover());
                    let origin = row_rect.min.to_vec2() - vec2(0.0, grid_row as f32 * stride);
                    for column in 0..grid.columns {
                        let row = grid_row * grid.columns + column;
                        if row >= view_len {
                            break;
                        }
                        let index = if recursive {
                            row.checked_sub(window_start)
                        } else {
                            indices.get(row).map(|index| *index as usize)
                        };
                        let loaded = snapshot.as_ref().and_then(|s| {
                            index.and_then(|index| {
                                s.entries.get(index).map(|entry| (s, index as u32, entry))
                            })
                        });
                        let Some((s, idx, e)) = loaded else {
                            let rect = grid.rect_for(row).translate(origin);
                            let response = ui.interact(
                                rect,
                                Id::new((self.id, "pending-row", row)),
                                Sense::CLICK,
                            );
                            if response.clicked() || response.secondary_clicked() {
                                self.focus_listing_from_pointer(ui);
                                listing_focused = true;
                            }
                            let selected = self.selected.contains(&(row as u32));
                            if selected {
                                ui.painter().rect_filled(
                                    rect.shrink2(vec2(1.0, 1.0)),
                                    t.radius,
                                    Theme::color(t.selection),
                                );
                            }
                            if self.focused == Some(row) && active && listing_focused {
                                let focused = rect.shrink2(vec2(1.0, 1.0));
                                self.focus_visual = Some(focused.intersect(body));
                                if keyboard_cues {
                                    ui.painter().rect_stroke(
                                        focused,
                                        t.radius,
                                        Stroke::new(1.0, t.accent()),
                                        StrokeKind::Inside,
                                    );
                                }
                            }
                            let center = if grid.mode.is_grid() {
                                rect.center()
                            } else {
                                pos2(rect.left() + 72.0, rect.center().y)
                            };
                            ui.painter().rect_filled(
                                Rect::from_center_size(
                                    center,
                                    vec2(88.0_f32.min(rect.width() * 0.65), 3.0),
                                ),
                                2,
                                t.line(),
                            );
                            if response.clicked() {
                                let m = ui.input(|i| i.modifiers);
                                clicked = Some((row, m.ctrl || m.command, m.shift));
                            }
                            if response.secondary_clicked() {
                                self.keyboard.cancel_range();
                                if !selected {
                                    self.selected.clear();
                                    self.selected.insert(row as u32);
                                    self.focused = Some(row);
                                    self.anchor = Some(row);
                                }
                                selection_context =
                                    Some(response.interact_pointer_pos().unwrap_or(rect.min));
                            }
                            continue;
                        };
                        // Raw identity survives reordering, refresh and lossy Unicode display names.
                        let identity = if recycle_bin {
                            self.recycle_model
                                .as_ref()
                                .and_then(|model| model.items.get(idx as usize))
                                .map_or_else(
                                    || Id::new((self.id, "recycle-row", idx)),
                                    |item| Id::new((self.id, "recycle-row", &item.id)),
                                )
                        } else {
                            Id::new((self.id, &s.path, &e.raw_name, &e.relative_path))
                        };
                        let revision = columns::EntryRevision::of(e);
                        if presented.len() < RETAINED_ROWS {
                            presented.insert(identity, revision);
                        }
                        let motion_id = identity.with(revision);
                        let target = grid.rect_for(row);
                        if let Some((previous, name_bounds)) = previous_view {
                            let previous_item = previous.rect_for(row);
                            let (icon, name) = item_parts(
                                previous,
                                previous_item,
                                metrics.scale,
                                t.font_size,
                                name_bounds,
                            );
                            self.listing_motion.seed(motion_id, previous_item);
                            self.listing_motion.seed(motion_id.with("icon"), icon);
                            self.listing_motion.seed(motion_id.with("name"), name);
                        }
                        // Arriving data paints at its destination immediately.
                        // Existing identities still animate reflow and reordering.
                        let placement = self.listing_motion.place(motion_id, target, false);
                        let rect = placement.rect.translate(origin);
                        let response =
                            ui.interact(rect, identity.with("hit"), Sense::CLICK | Sense::DRAG);
                        if response.clicked()
                            || response.secondary_clicked()
                            || response.middle_clicked()
                            || response.drag_started()
                        {
                            self.focus_listing_from_pointer(ui);
                            listing_focused = true;
                        }
                        let selected = self.selected.contains(&(row as u32));
                        let focus = self.focused == Some(row) && active && listing_focused;
                        if focus {
                            self.focus_visual = Some(rect.intersect(body));
                        }
                        let states = self.listing_states.entry(identity).or_insert([0.0; 4]);
                        for (value, on) in
                            states.iter_mut().zip([response.hovered(), selected, focus])
                        {
                            if gains.step_normalized(
                                value,
                                if on { 1.0 } else { 0.0 },
                                MotionProfile::Fast,
                            ) {
                                ui.ctx().request_repaint();
                            }
                        }
                        let [hover, selection, focus, _] = *states;
                        let mut p = ui.painter().with_clip_rect(body.intersect(ui.clip_rect()));
                        p.multiply_opacity(placement.opacity);
                        p.rect_filled(rect, 0, t.bg());
                        let filled = rect.shrink2(vec2(1.0, 1.0));
                        if hover > 0.0 {
                            p.rect_filled(
                                filled,
                                t.radius,
                                Theme::color(t.hover).gamma_multiply(hover * (1.0 - selection)),
                            );
                        }
                        if selection > 0.0 {
                            p.rect_filled(
                                filled,
                                t.radius,
                                Theme::color(t.selection).gamma_multiply(selection),
                            );
                        }
                        if focus > 0.0
                            && self.focused == Some(row)
                            && active
                            && listing_focused
                            && keyboard_cues
                        {
                            p.rect_stroke(
                                filled,
                                t.radius,
                                Stroke::new(1.0, t.accent().gamma_multiply(focus)),
                                StrokeKind::Inside,
                            );
                        }

                        // Selection and keyboard focus remain fully readable.
                        // Content opacity also dims highlighted text/numeric
                        // motion without changing their retained layout caches.
                        let cut = !recycle_bin
                            && assets.as_deref().is_some_and(|manager| {
                                let text = self.row_cache.entry(idx).or_default();
                                text.seen = cache_frame;
                                let version = manager.cut_version();
                                if text.cut.is_none_or(|(old, _)| old != version) {
                                    text.cut = Some((
                                        version,
                                        manager.is_cut_row(self.id, asset_epoch, row, || e.path(s)),
                                    ));
                                }
                                text.cut.is_some_and(|(_, cut)| cut)
                            });
                        p.multiply_opacity(crate::cut_state::content_opacity(cut));
                        let (icon_target, name_target) =
                            item_parts(grid, target, metrics.scale, t.font_size, name_bounds);
                        let icon_rect = self
                            .listing_motion
                            .place(motion_id.with("icon"), icon_target, false)
                            .rect
                            .translate(origin);
                        let image = assets
                            .as_deref_mut()
                            .and_then(|a| a.image(self.id, asset_epoch, row));
                        let fallback = image.as_ref().map_or(1.0, |a| {
                            (1.0 - a.opacity) * (1.0 - a.previous.map_or(0.0, |old| old.opacity))
                        });
                        if fallback > 0.0 {
                            icon(
                                &p,
                                icon_rect.center(),
                                foldex_ui::file_icon(&e.name, e.is_dir),
                                (if e.is_dir {
                                    Theme::color(t.folder)
                                } else {
                                    t.dim()
                                })
                                .gamma_multiply(fallback * if e.hidden { 0.48 } else { 1.0 }),
                                icon_rect.width().min(icon_rect.height()),
                            );
                        }
                        if let Some(image) = image {
                            let draw = |texture, size: egui::Vec2, opacity: f32| {
                                let scale =
                                    (icon_rect.width() / size.x).min(icon_rect.height() / size.y);
                                let scale = if recycle_bin {
                                    scale.min(1.0 / ui.ctx().pixels_per_point())
                                } else {
                                    scale
                                };
                                p.image(
                                    texture,
                                    Rect::from_center_size(icon_rect.center(), size * scale),
                                    Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                                    Color32::WHITE.gamma_multiply(
                                        opacity * if e.hidden { 0.48 } else { 1.0 },
                                    ),
                                );
                            };
                            if let Some(previous) = image.previous {
                                draw(
                                    previous.texture,
                                    previous.size,
                                    previous.opacity * (1.0 - image.opacity),
                                );
                            }
                            draw(image.texture, image.size, image.opacity);
                        }

                        let nr = self
                            .listing_motion
                            .place(motion_id.with("name"), name_target, false)
                            .rect
                            .translate(origin);
                        let color = if e.hidden {
                            t.fg().gamma_multiply(0.48)
                        } else {
                            t.fg()
                        };
                        let mut name_clip = nr.intersect(body);
                        if grid.mode == ViewMode::Details {
                            name_clip = name_clip.intersect(Rect::from_min_max(
                                pos2(columns.name_rect().left(), body.top()),
                                pos2(
                                    (columns.name_rect().right() - 8.0 * metrics.scale)
                                        .max(columns.name_rect().left()),
                                    body.bottom(),
                                ),
                            ));
                        }
                        let parent = (self.recursive && !grid.mode.is_grid())
                            .then(|| e.relative_path.as_deref().and_then(Path::parent))
                            .flatten()
                            .filter(|parent| !parent.as_os_str().is_empty());
                        let width = if grid.mode == ViewMode::Details {
                            nr.width().min(name_clip.right() - nr.left())
                        } else {
                            nr.width()
                        }
                        .max(1.0);
                        // Keep the basename readable first. Narrow rows give it
                        // the entire column; wider rows reserve a modest path tail.
                        let width = foldex_ui::components::file_name_width(
                            width,
                            metrics.scale,
                            parent.is_some(),
                        )
                        // epaint rounds finite wrap widths before its cache key.
                        // Match it before deciding to rebuild a highlighted job.
                        .round();
                        let cached = self.name_cache.entry(idx).or_insert_with(|| {
                            (
                                width.to_bits(),
                                name_galley(&p, t, &e.name, &self.match_paint, color, width),
                                cache_frame,
                            )
                        });
                        if cached.0 != width.to_bits() && !name_fits_width(&cached.1, width) {
                            *cached = (
                                width.to_bits(),
                                name_galley(&p, t, &e.name, &self.match_paint, color, width),
                                cache_frame,
                            );
                        }
                        cached.2 = cache_frame;
                        let gal = &cached.1;
                        let x = if grid.mode.is_grid() {
                            nr.center().x - gal.size().x.min(nr.width()) * 0.5
                        } else {
                            nr.left()
                        };
                        let text_position = pos2(x, nr.center().y - gal.size().y * 0.5);
                        if probe_layout && row == self.visible_rows.start {
                            crate::performance::record_layout_probe(
                                ui.ctx(),
                                crate::performance::LayoutProbe {
                                    pane: self.id,
                                    row,
                                    text: [text_position.x, text_position.y],
                                    icon: [
                                        icon_rect.left(),
                                        icon_rect.top(),
                                        icon_rect.width(),
                                        icon_rect.height(),
                                    ],
                                },
                            );
                        }
                        p.with_clip_rect(name_clip)
                            .galley(text_position, gal.clone(), color);
                        if let Some(parent) = parent {
                            let text = self.row_cache.entry(idx).or_default();
                            text.seen = cache_frame;
                            let parent = text
                                .parent
                                .get_or_insert_with(|| parent.to_string_lossy().into_owned());
                            let path_color = t
                                .dim()
                                .lerp_to_gamma(t.fg(), selection * 0.25 + hover * 0.12)
                                .gamma_multiply(if e.hidden { 0.48 } else { 1.0 });
                            foldex_ui::components::file_parent_label(
                                &p,
                                Rect::from_min_max(
                                    pos2(text_position.x, nr.top()),
                                    pos2(name_clip.right(), nr.bottom()),
                                ),
                                t,
                                gal.size().x,
                                parent,
                                path_color,
                            );
                        }
                        let caption_fits = grid.mode.is_grid()
                            && nr.bottom() + 2.0 + caption_height <= rect.bottom() + 0.25;
                        let caption = &mut self.listing_states.get_mut(&identity).unwrap()[3];
                        if gains.step_normalized(
                            caption,
                            if caption_fits { 1.0 } else { 0.0 },
                            MotionProfile::Fast,
                        ) {
                            ui.ctx().request_repaint();
                        }
                        let caption_opacity = *caption;
                        if grid.mode == ViewMode::Details {
                            for column in &columns.cells {
                                if column.column == columns::Column::Name {
                                    continue;
                                }
                                let value = self.column_value(column.column, e, idx);
                                let cell = Rect::from_min_max(
                                    pos2(
                                        rect.left() + column.rect.left() - body.left()
                                            + 8.0 * metrics.scale,
                                        nr.top(),
                                    ),
                                    pos2(
                                        rect.left() + column.rect.right()
                                            - body.left()
                                            - 8.0 * metrics.scale,
                                        nr.bottom(),
                                    ),
                                )
                                .intersect(body)
                                .intersect(p.clip_rect());
                                if cell.width() > 0.0 && cell.height() > 0.0 {
                                    let text_cell = cell;
                                    let numeric_pending = value.busy && column.column.numeric();
                                    pending_numeric |= numeric_pending;
                                    if numeric_pending && value.text.is_empty() {
                                        pending.skeleton(&p, text_cell, t);
                                    }
                                    let color = t.dim().gamma_multiply(
                                        (self.header_height / metrics.columns).clamp(0.0, 1.0)
                                            * if numeric_pending {
                                                pending.opacity
                                            } else {
                                                1.0
                                            },
                                    );
                                    if column.column.numeric() {
                                        self.numeric_motion.paint(
                                            &p,
                                            identity.with(("numeric-value", column.column)),
                                            text_cell,
                                            &value.text,
                                            t.font(),
                                            color,
                                            foldex_ui::rolling::RollingOptions::from_theme(t),
                                        );
                                    } else {
                                        foldex_ui::components::elided_label(
                                            &p,
                                            text_cell,
                                            t,
                                            &value.text,
                                            color,
                                            false,
                                        );
                                    }
                                    if value.partial && !value.text.is_empty() {
                                        let underline = Rect::from_min_max(
                                            pos2(
                                                text_cell.left(),
                                                text_cell.bottom() - 3.0 * metrics.scale,
                                            ),
                                            text_cell.right_bottom(),
                                        );
                                        p.line_segment(
                                            [underline.left_top(), underline.right_top()],
                                            Stroke::new(0.5, t.dim().gamma_multiply(0.45)),
                                        );
                                    }
                                    ui.interact(
                                        cell,
                                        identity.with(("column-value", column.column)),
                                        Sense::hover(),
                                    )
                                    .on_hover_text(value.tooltip);
                                }
                            }
                        } else if caption_fits && caption_opacity > 0.0 {
                            let caption: &str = if e.is_dir {
                                "Folder"
                            } else {
                                let text = self.row_cache.entry(idx).or_default();
                                text.seen = cache_frame;
                                text.size
                                    .get_or_insert_with(|| foldex_ui::format_size(e.size))
                            };
                            p.with_clip_rect(rect.intersect(body)).text(
                                pos2(nr.center().x, nr.bottom() + 2.0),
                                Align2::CENTER_TOP,
                                caption,
                                caption_font.clone(),
                                t.dim().gamma_multiply(caption_opacity),
                            );
                        }
                        if !foldex_ui::components::tooltips_blocked(ui.ctx())
                            && !ui.input(|input| input.pointer.any_pressed())
                        {
                            response.clone().on_hover_ui(|ui| {
                                if recycle_bin {
                                    if let Some(item) = self
                                        .recycle_model
                                        .as_ref()
                                        .and_then(|model| model.items.get(idx as usize))
                                    {
                                        ui.label(item.original_path.to_string_lossy());
                                    }
                                } else {
                                    ui.label(e.path(s).to_string_lossy());
                                }
                            });
                        }
                        if response.clicked() {
                            let m = ui.input(|i| i.modifiers);
                            clicked = Some((row, m.ctrl || m.command, m.shift));
                        }
                        // egui counts clicks across all widgets. After opening
                        // a folder, rapid clicks on its child can both be
                        // classified as triple-clicks. Pair actual clicks on
                        // this item's raw identity instead, consuming each pair.
                        row_pointer_clicked |= response.clicked_by(egui::PointerButton::Primary);
                        if self.listing_clicks.double_clicked(&response) {
                            if recycle_bin {
                                actions.push(Action::RecycleBin(
                                    crate::recycle_bin::Intent::PropertiesSelected,
                                ));
                            } else {
                                double = Some((e.path(s), e.is_dir));
                            }
                        }
                        if !recycle_bin && response.middle_clicked() && e.is_dir {
                            actions.push(Action::NewTab(e.path(s)));
                        }
                        if response.secondary_clicked() {
                            self.keyboard.cancel_range();
                            if !selected {
                                self.selected.clear();
                                self.selected.insert(row as u32);
                                self.focused = Some(row);
                                self.anchor = Some(row);
                            }
                            selection_context =
                                Some(response.interact_pointer_pos().unwrap_or(rect.min));
                        }
                    }
                }
            });
            self.remember_listing_viewport(out.state.offset.y, out.inner_rect);
            let pointer = ui.input(|i| i.pointer.interact_pos());
            let origin = out.inner_rect.min.to_vec2() - out.state.offset;
            // A fast drag can deliver its press, motion and release in one frame.
            // Anchor to the press event, rather than the pointer's final position.
            let press = ui.input(|i| {
                i.events.iter().find_map(|event| match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers,
                    } => Some((*pos, *modifiers)),
                    _ => None,
                })
            });
            if let Some((point, modifiers)) = press.filter(|(point, _)| {
                out.inner_rect.contains(*point)
                    && ui.ctx().layer_id_at(*point) == Some(ui.layer_id())
                    // Floating bars overlap inner_rect. Use egui's full hit target,
                    // including the track and its animated hover width.
                    && !ui
                        .ctx()
                        .read_response(out.id.with(1_usize))
                        .is_some_and(|bar| bar.rect.contains(*point))
            }) {
                self.focus_listing_from_pointer(ui);
                self.marquee = Some(Marquee {
                    anchor: point - origin,
                    press: point,
                    base: self.selected.clone(),
                    additive: modifiers.ctrl || modifiers.shift,
                    active: false,
                });
            }
            if let Some(mut drag) = self.marquee.take() {
                let down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
                let released =
                    ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
                if let Some(point) = pointer {
                    if point.distance(drag.press) > 4.0 {
                        drag.active = true;
                    }
                    if drag.active {
                        self.keyboard.cancel_range();
                        self.select_all_pending = false;
                        self.invert_pending = false;
                        let area = Rect::from_two_pos(drag.anchor, point - origin);
                        self.selected = if drag.additive {
                            drag.base.clone()
                        } else {
                            Default::default()
                        };
                        let columns = grid.columns.max(1);
                        let rows = grid.count_rows(view_len);
                        if rows > 0 {
                            let first = (area.top() / stride).floor().max(0.0) as usize;
                            let last =
                                ((area.bottom() / stride).floor().max(0.0) as usize).min(rows - 1);
                            let step_x = grid.stride().x.max(1.0);
                            let first_col =
                                ((area.left() / step_x).floor().max(0.0) as usize).min(columns - 1);
                            let last_col = ((area.right() / step_x).floor().max(0.0) as usize)
                                .min(columns - 1);
                            if first <= last {
                                if first_col == 0 && last_col == columns - 1 && area.right() >= 0.0
                                {
                                    self.selected.select(
                                        (first * columns) as u32,
                                        ((last + 1) * columns).min(view_len).saturating_sub(1)
                                            as u32,
                                    );
                                } else {
                                    for row in first..=last.min(first.saturating_add(4095)) {
                                        let mut range = None;
                                        for col in first_col..=last_col {
                                            let index = row * columns + col;
                                            if index < view_len
                                                && grid.rect_for(index).intersects(area)
                                            {
                                                range =
                                                    Some((range.map_or(index, |(a, _)| a), index));
                                            }
                                        }
                                        if let Some((first, last)) = range {
                                            self.selected.select(first as u32, last as u32);
                                        }
                                    }
                                }
                            }
                        }
                        let visual = area.translate(origin).intersect(out.inner_rect);
                        let p = ui.painter().with_clip_rect(out.inner_rect);
                        p.rect_filled(visual, 1, t.accent().gamma_multiply(0.15));
                        p.rect_stroke(visual, 1, Stroke::new(1.0, t.accent()), StrokeKind::Inside);
                        clicked = None;
                        double = None;
                        self.listing_clicks.clear();
                        self.sync_selection();
                        ui.ctx().request_repaint();
                    }
                }
                if down && !released {
                    self.marquee = Some(drag);
                }
            }
            if out
                .inner_rect
                .contains(ui.input(|i| i.pointer.hover_pos().unwrap_or(Pos2::ZERO)))
                && ui.input(|i| i.modifiers.ctrl)
            {
                let detents: f32 = ui.input(|i| {
                    i.raw
                        .events
                        .iter()
                        .filter_map(|e| match e {
                            egui::Event::MouseWheel { unit, delta, .. } => Some(
                                delta.y
                                    / if *unit == egui::MouseWheelUnit::Point {
                                        50.0
                                    } else {
                                        1.0
                                    },
                            ),
                            _ => None,
                        })
                        .sum()
                });
                if detents != 0.0 {
                    self.set_zoom(self.zoom + detents * 0.055);
                }
            }
        });
        if !row_pointer_clicked && ui.input(|input| input.pointer.any_released()) {
            // Blank space, another control, a pending row or a drag interrupts
            // the pair; none may supply the first click for a loaded item.
            self.listing_clicks.clear();
        }
        if self.listing_motion.is_moving() {
            ui.ctx().request_repaint();
        } else if self.zoom == rendered_zoom {
            self.zoom_from = None;
        }
        if let Some((row, toggle, range)) = clicked {
            self.keyboard.cancel_range();
            self.select_all_pending = false;
            self.invert_pending = false;
            if let Some(selection) = ui
                .ctx()
                .plugin_opt::<egui::text_selection::LabelSelectionState>()
            {
                selection.lock().clear_selection();
            }
            if range {
                let a = self.anchor.unwrap_or(row);
                if !toggle {
                    self.selected.clear();
                }
                self.selected.select(a.min(row) as u32, a.max(row) as u32);
            } else if toggle {
                if !self.selected.remove(&(row as u32)) {
                    self.selected.insert(row as u32);
                }
                self.anchor = Some(row);
            } else {
                self.selected.clear();
                self.selected.insert(row as u32);
                self.anchor = Some(row);
            }
            self.focused = Some(row);
            self.last_selection = self.primary_selected_path();
            ui.ctx().request_repaint();
        }
        self.sync_selection();
        if listing_focused
            && active
            && view_len == 0
            && keyboard_cues
            && !self.loading
            && !self.resolving
            && !self.scanning
            && self.displayed_view_is_current()
        {
            ui.painter().rect_stroke(
                body.shrink(1.0),
                t.radius,
                Stroke::new(1.0, t.accent()),
                StrokeKind::Inside,
            );
        }
        // Drop old viewport entries at the limit, keeping current row animation
        // state and layouts intact as a long scroll crosses the cache boundary.
        if self.name_cache.len() > RETAINED_ROWS {
            self.name_cache
                .retain(|_, (_, _, seen)| *seen == cache_frame);
        }
        if self.row_cache.len() > RETAINED_ROWS {
            self.row_cache.retain(|_, text| text.seen == cache_frame);
        }
        if self.listing_states.len() > RETAINED_ROWS {
            self.listing_states
                .retain(|identity, _| presented.contains_key(identity));
        }
        self.presented_scratch = std::mem::replace(&mut self.presented_entries, presented);
        self.listing_appearing = 0;
        pending.finish(ui.ctx(), pending_numeric);
        self.ensure_window(self.visible_rows.clone());
        if let Some((path, directory)) = double {
            actions.push(if directory {
                Action::Navigate(path)
            } else {
                Action::Open(path)
            });
        }
        if let Some((paths, background, position)) = context {
            if recycle_bin {
                self.clear_selection();
                actions.push(Action::RecycleBin(crate::recycle_bin::Intent::Context(
                    position,
                )));
            } else {
                actions.push(Action::Context {
                    paths,
                    background,
                    position,
                });
            }
        }
        if let Some(position) = selection_context {
            if let Some(action) = self.request_selection(SelectionCommand::Context(position)) {
                actions.push(action);
            }
        }
    }
}

fn item_parts(
    grid: GridMetrics,
    target: Rect,
    scale: f32,
    font_size: f32,
    name_bounds: [f32; 2],
) -> (Rect, Rect) {
    let icon = Rect::from_center_size(
        if grid.mode.is_grid() {
            pos2(target.center().x, target.top() + grid.icon_size * 0.5 + 9.0)
        } else {
            pos2(
                target.left()
                    + if grid.mode == ViewMode::Details {
                        name_bounds[0]
                    } else {
                        0.0
                    }
                    + 13.0 * scale,
                target.center().y,
            )
        },
        vec2(grid.icon_size, grid.icon_size),
    );
    let name = if grid.mode.is_grid() {
        Rect::from_min_max(
            pos2(target.left() + 4.0, icon.bottom() + 8.0),
            pos2(target.right() - 4.0, icon.bottom() + 8.0 + font_size + 4.0),
        )
    } else {
        Rect::from_min_max(
            pos2(
                target.left()
                    + if grid.mode == ViewMode::Details {
                        name_bounds[0]
                    } else {
                        0.0
                    }
                    + 28.0 * scale,
                target.top(),
            ),
            pos2(
                if grid.mode == ViewMode::Details {
                    target.left() + name_bounds[1] - 8.0 * scale
                } else {
                    target.right() - 8.0 * scale
                },
                target.bottom(),
            ),
        )
    };
    (icon, name)
}

/// A complete, left-aligned name has the same glyphs at every width that fits it.
/// Keep the original job width in the cache so atlas checks can recreate that job.
fn name_fits_width(galley: &egui::Galley, width: f32) -> bool {
    // Keep a strict margin at the GUI-rounded boundary instead of scanning glyphs
    // on cache hits. Rebuilding at that boundary preserves exact elision behavior.
    !galley.elided && galley.size().x.max(galley.intrinsic_size().x) < width
}

fn name_galley(
    p: &egui::Painter,
    t: &Theme,
    name: &str,
    matcher: &foldex_match::Matcher,
    color: Color32,
    width: f32,
) -> Arc<egui::Galley> {
    foldex_ui::fonts::request_text(p.ctx(), name);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    if let Some(indices) = matcher.match_indices(name).filter(|_| !matcher.is_empty()) {
        let mut at = 0;
        for byte in indices {
            if byte < at || !name.is_char_boundary(byte) {
                continue;
            }
            if at < byte {
                job.append(
                    &name[at..byte],
                    0.0,
                    egui::TextFormat {
                        font_id: t.font(),
                        color,
                        ..Default::default()
                    },
                );
            }
            let end = byte + name[byte..].chars().next().map_or(0, char::len_utf8);
            job.append(
                &name[byte..end],
                0.0,
                egui::TextFormat {
                    font_id: t.font(),
                    color: t.accent(),
                    ..Default::default()
                },
            );
            at = end;
        }
        if at < name.len() {
            job.append(
                &name[at..],
                0.0,
                egui::TextFormat {
                    font_id: t.font(),
                    color,
                    ..Default::default()
                },
            );
        }
    } else {
        job.append(
            name,
            0.0,
            egui::TextFormat {
                font_id: t.font(),
                color,
                ..Default::default()
            },
        );
    }
    p.layout_job(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ClickDirectories(PathBuf);

    impl ClickDirectories {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "foldex-nested-click-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn nested(&self, branch: &str, names: [&str; 3]) -> Vec<PathBuf> {
            let mut path = self.0.join(branch);
            let mut folders = vec![path.clone()];
            for name in names {
                path.push(name);
                std::fs::create_dir_all(&path).unwrap();
                folders.push(path.clone());
            }
            folders
        }
    }

    impl Drop for ClickDirectories {
        fn drop(&mut self) {
            let root = self.0.canonicalize().unwrap();
            let base = std::env::temp_dir().canonicalize().unwrap();
            assert!(root.starts_with(&base) && root != base);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    fn publish_generated_directory(pane: &mut Pane) {
        let entries = std::fs::read_dir(&pane.path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                foldex_core::Entry::from_name(
                    entry.file_name(),
                    entry.file_type().unwrap().is_dir(),
                    0,
                    0,
                )
            })
            .collect();
        pane.accept(Arc::new(Snapshot::from_entries(pane.path.clone(), entries)));
        pane.loading = false;
    }

    struct ClickHarness {
        ctx: egui::Context,
        theme: Theme,
        frame: u64,
        discard: bool,
    }

    impl ClickHarness {
        fn new(reduced_motion: bool) -> Self {
            let ctx = egui::Context::default();
            let theme = Theme {
                reduced_motion,
                ..Default::default()
            };
            theme.apply(&ctx);
            Self {
                ctx,
                theme,
                frame: 0,
                discard: false,
            }
        }

        fn draw(&mut self, pane: &mut Pane, events: Vec<egui::Event>) -> (Vec<Action>, u8) {
            let body = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 500.0));
            let mut actions = Vec::new();
            let output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(body),
                    time: Some(self.frame as f64 * 0.01),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| {
                    pane.listing(
                        ui,
                        &self.theme,
                        true,
                        body,
                        &columns::DetailsLayout::new(
                            &columns::StoredColumns::default(),
                            body,
                            1.0,
                            0.5,
                        ),
                        None,
                        &mut actions,
                    );
                    if self.discard && ui.ctx().current_pass_index() == 0 {
                        ui.ctx()
                            .request_discard("Replay the listing's pointer input");
                    }
                },
            );
            if self.discard {
                assert_eq!(output.platform_output.num_completed_passes, 2);
            }
            output.drop_without_applying_deltas();
            self.frame += 1;
            let count = self.ctx.input(|input| {
                if input
                    .pointer
                    .button_triple_clicked(egui::PointerButton::Primary)
                {
                    3
                } else if input
                    .pointer
                    .button_double_clicked(egui::PointerButton::Primary)
                {
                    2
                } else {
                    u8::from(input.pointer.button_clicked(egui::PointerButton::Primary))
                }
            });
            (actions, count)
        }

        fn item_point(&self, pane: &Pane, row: usize) -> Pos2 {
            let snapshot = pane.snapshot.as_ref().unwrap();
            let entry = pane.entry_at(row).unwrap();
            let identity = Id::new((
                pane.id,
                &snapshot.path,
                &entry.raw_name,
                &entry.relative_path,
            ));
            self.ctx
                .read_response(identity.with("hit"))
                .expect("the item must have a painted hit target")
                .rect
                .center()
        }

        fn click(&mut self, pane: &mut Pane, point: Pos2) -> (Vec<Action>, u8) {
            let button = |pressed| egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            assert!(
                self.draw(pane, vec![egui::Event::PointerMoved(point), button(true)])
                    .0
                    .is_empty(),
                "a press alone must never open an item"
            );
            self.draw(pane, vec![button(false)])
        }
    }

    #[test]
    fn rapid_double_clicks_navigate_three_nested_folders_at_the_same_position() {
        let directories = ClickDirectories::new();
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        for (same_name, names) in [(false, ["first", "second", "third"]), (true, ["same"; 3])] {
            let folders = directories.nested(if same_name { "same" } else { "different" }, names);
            for reduced_motion in [false, true] {
                for zoom in [0.0, 1.0] {
                    let mut pane = Pane::dormant(945, &engine, folders[0].clone());
                    pane.set_active(true);
                    pane.zoom = zoom;
                    publish_generated_directory(&mut pane);
                    let mut input = ClickHarness::new(reduced_motion);
                    input.draw(&mut pane, Vec::new());
                    let point = input.item_point(&pane, 0);
                    let mut counts = Vec::new();
                    for target in folders.iter().skip(1) {
                        assert_eq!(input.item_point(&pane, 0), point);
                        let (actions, count) = input.click(&mut pane, point);
                        counts.push(count);
                        assert!(
                            actions.is_empty(),
                            "the new item's first click must only select"
                        );
                        assert_eq!(pane.focused, Some(0));
                        assert_eq!(pane.selected.intervals(), vec![(0, 0)]);
                        let (mut actions, count) = input.click(&mut pane, point);
                        counts.push(count);
                        assert_eq!(actions.len(), 1, "the second click must open {target:?}");
                        assert!(
                            matches!(actions.pop(), Some(Action::Navigate(path)) if path == *target)
                        );
                        pane.navigate(target.clone(), true);
                        assert!(pane.selected.is_empty());
                        publish_generated_directory(&mut pane);
                        input.draw(&mut pane, Vec::new());
                        assert_eq!(pane.listing_appearing, 0);
                        assert!(!pane.listing_motion.is_moving());
                    }
                    assert_eq!(pane.path, folders[3]);
                    assert_eq!(
                        counts,
                        [1, 2, 3, 3, 3, 3],
                        "exercise egui's global triple-click history"
                    );
                }
            }
        }
    }

    #[test]
    fn a_discarded_pass_cannot_turn_one_pointer_click_into_an_open() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let mut pane = Pane::dormant(946, &engine, PathBuf::from("click-discard-fixture"));
        pane.accept(Arc::new(Snapshot::from_entries(
            pane.path.clone(),
            vec![foldex_core::Entry::from_name("item.txt", false, 0, 0)],
        )));
        let mut input = ClickHarness::new(true);
        input.draw(&mut pane, Vec::new());
        input.discard = true;
        let point = input.item_point(&pane, 0);
        assert!(input.click(&mut pane, point).0.is_empty());
        assert!(
            matches!(input.click(&mut pane, point).0.as_slice(), [Action::Open(path)] if path.ends_with("item.txt"))
        );
        assert!(
            input.click(&mut pane, point).0.is_empty(),
            "the preceding pair was consumed"
        );
    }

    #[test]
    fn two_complete_clicks_in_one_frame_still_open_the_item() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let mut pane = Pane::dormant(947, &engine, PathBuf::from("click-batch-fixture"));
        pane.accept(Arc::new(Snapshot::from_entries(
            pane.path.clone(),
            vec![foldex_core::Entry::from_name("child", true, 0, 0)],
        )));
        let mut input = ClickHarness::new(true);
        input.draw(&mut pane, Vec::new());
        let point = input.item_point(&pane, 0);
        for _ in 0..2 {
            let events = [true, false, true, false]
                .into_iter()
                .map(|pressed| egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                })
                .collect();
            assert!(matches!(
                input.draw(&mut pane, events).0.as_slice(),
                [Action::Navigate(path)] if path.ends_with("child")
            ));
        }
    }

    #[test]
    fn pointer_pairs_respect_time_distance_and_intervening_blank_clicks() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let mut pane = Pane::dormant(948, &engine, PathBuf::from("click-limits-fixture"));
        pane.accept(Arc::new(Snapshot::from_entries(
            pane.path.clone(),
            vec![foldex_core::Entry::from_name("item.txt", false, 0, 0)],
        )));
        let mut input = ClickHarness::new(true);
        input.ctx.options_mut(|options| {
            options.input_options.max_double_click_delay = 0.08;
            options.input_options.max_click_dist = 4.0;
        });
        input.draw(&mut pane, Vec::new());
        let point = input.item_point(&pane, 0);
        assert!(input.click(&mut pane, point).0.is_empty());
        input.frame += 10;
        assert!(
            input.click(&mut pane, point).0.is_empty(),
            "the old click expired"
        );
        let moved = point + vec2(5.0, 0.0);
        assert!(
            input.click(&mut pane, moved).0.is_empty(),
            "the clicks are too far apart"
        );
        assert!(matches!(
            input.click(&mut pane, moved).0.as_slice(),
            [Action::Open(_)]
        ));
        assert!(input.click(&mut pane, moved).0.is_empty());
        assert!(
            input
                .click(&mut pane, moved + vec2(0.0, 100.0))
                .0
                .is_empty()
        );
        assert!(
            input.click(&mut pane, moved).0.is_empty(),
            "blank space interrupted the pair"
        );
        assert!(matches!(
            input.click(&mut pane, moved).0.as_slice(),
            [Action::Open(_)]
        ));
    }

    #[test]
    fn cut_names_and_icons_dim_in_every_view_without_fading_selection_or_focus() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let names = ["a-cut", "b-cut", "c-hidden", "d-normal"];
        for zoom in foldex_ui::view::ZOOM_STOPS {
            let path =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/cut-paint-fixture");
            let mut pane = Pane::dormant(931, &engine, path.clone());
            pane.query.show_hidden = true;
            pane.accept(Arc::new(Snapshot::from_entries(
                path.clone(),
                names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        let mut entry = foldex_core::Entry::from_name(name, true, 0, 0);
                        entry.hidden = index == 2;
                        entry
                    })
                    .collect(),
            )));
            pane.loading = false;
            pane.zoom = zoom;
            pane.selected.insert(0);
            pane.focused = Some(0);
            pane.match_paint = foldex_match::Matcher::new("cut");
            let ctx = egui::Context::default();
            let theme = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            theme.apply(&ctx);
            let body = Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 700.0));
            let mut stored = columns::StoredColumns::default();
            for setting in &mut stored.columns {
                setting.visible = setting.column == columns::Column::Name;
            }
            let layout = columns::DetailsLayout::new(&stored, body, 1.0, 0.5);
            let mut assets = crate::assets::AssetManager::disabled(&ctx);
            let render = |pane: &mut Pane, assets: &mut crate::assets::AssetManager, frame| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(body),
                        time: Some(frame as f64 / 60.0),
                        focused: true,
                        events: vec![egui::Event::Key {
                            key: Key::ArrowDown,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        }],
                        ..Default::default()
                    },
                    |ui| {
                        crate::keyboard::request_id(ui.ctx(), pane.listing_focus_id());
                        assets.begin_frame(ui.ctx());
                        pane.listing(
                            ui,
                            &theme,
                            true,
                            body,
                            &layout,
                            Some(assets),
                            &mut Vec::new(),
                        );
                    },
                )
            };
            render(&mut pane, &mut assets, 0).drop_without_applying_deltas();
            let before = render(&mut pane, &mut assets, 1);
            let retained_name = pane.name_cache[&0].1.clone();
            assets
                .benchmark_set_cut_paths(
                    &names[..3]
                        .iter()
                        .map(|name| path.join(name))
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            let after = render(&mut pane, &mut assets, 2);
            assert!(
                Arc::ptr_eq(&retained_name, &pane.name_cache[&0].1),
                "cut must reuse the retained name layout"
            );
            for (index, name) in names.iter().enumerate() {
                let text = |output: &egui::FullOutput| {
                    output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.text() == *name => {
                                Some(text.clone())
                            }
                            _ => None,
                        })
                        .unwrap()
                };
                let plain = text(&before);
                let cut = text(&after);
                let factor = if index < 3 {
                    crate::cut_state::CUT_OPACITY
                } else {
                    1.0
                };
                assert_eq!(
                    cut.fallback_color,
                    plain.fallback_color.gamma_multiply(factor),
                    "{zoom}: {name}"
                );
                for (before, after) in plain.galley.rows.iter().zip(&cut.galley.rows) {
                    for (before, after) in before
                        .visuals
                        .mesh
                        .vertices
                        .iter()
                        .zip(&after.visuals.mesh.vertices)
                    {
                        assert_eq!(
                            after.color,
                            before.color.gamma_multiply(factor),
                            "highlighted text must dim too"
                        );
                    }
                }
            }
            // Folder fallback bodies prove icon dimming before native images
            // arrive. Native texture layers use this same content painter.
            let folder = Theme::color(theme.folder);
            let bodies = |output: &egui::FullOutput| {
                output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect)
                            if rect.rect.width() < 300.0
                                && rect.fill.a() > 0
                                && (rect.fill == folder
                                    || rect.fill == folder.gamma_multiply(0.48)
                                    || rect.fill == folder.gamma_multiply(0.5)
                                    || rect.fill
                                        == folder.gamma_multiply(0.48).gamma_multiply(0.5)) =>
                        {
                            Some(rect.clone())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            let plain = bodies(&before);
            let cut = bodies(&after);
            assert_eq!(
                plain.len(),
                8,
                "two fallback folder bodies per item at zoom {zoom}"
            );
            assert_eq!(plain.len(), cut.len());
            for (index, (plain, cut)) in plain.iter().zip(&cut).enumerate() {
                assert_eq!(plain.rect, cut.rect);
                assert_eq!(
                    cut.fill,
                    plain.fill.gamma_multiply(if index < 6 { 0.5 } else { 1.0 })
                );
            }
            for color in [Theme::color(theme.selection), theme.accent()] {
                let decorations = |output: &egui::FullOutput| {
                    output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Rect(rect)
                                if rect.fill == color || rect.stroke.color == color =>
                            {
                                Some(rect.clone())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                };
                let plain = decorations(&before);
                assert!(!plain.is_empty());
                assert_eq!(
                    plain,
                    decorations(&after),
                    "selection/focus decoration must remain opaque"
                );
            }
            before.drop_without_applying_deltas();
            after.drop_without_applying_deltas();
        }
    }

    fn paint(pane: &mut Pane, ctx: &egui::Context, theme: &Theme, frame: u64) {
        paint_at_width(
            pane,
            ctx,
            theme,
            frame,
            1000.0,
            &columns::StoredColumns::default(),
        );
    }

    fn paint_at_width(
        pane: &mut Pane,
        ctx: &egui::Context,
        theme: &Theme,
        frame: u64,
        width: f32,
        columns: &columns::StoredColumns,
    ) {
        let body = Rect::from_min_size(Pos2::ZERO, vec2(width, 650.0));
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(body),
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            },
            |ui| {
                pane.listing(
                    ui,
                    theme,
                    true,
                    body,
                    &columns::DetailsLayout::new(columns, body, theme.metrics().scale, 0.5),
                    None,
                    &mut Vec::new(),
                );
            },
        )
        .drop_without_applying_deltas();
    }

    #[test]
    fn fitting_names_reuse_layouts_through_resize_and_rebuild_elision() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("name-resize-fixture"));
        let parent = Path::new("archive").join("review");
        let mut long = foldex_core::Entry::from_name(
            "b-quarterly-report-with-supporting-release-documents.txt",
            false,
            0,
            0,
        );
        long.relative_path = Some(parent.join(&long.name));
        pane.accept(Arc::new(Snapshot::from_entries(
            pane.path.clone(),
            vec![foldex_core::Entry::from_name("a.txt", false, 0, 0), long],
        )));
        pane.recursive = true;
        pane.loading = false;
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        theme.apply(&ctx);
        foldex_ui::motion::set_direct_layout(&ctx, true);
        let mut columns = columns::StoredColumns::default();
        for column in &mut columns.columns {
            column.visible = column.column == columns::Column::Name;
        }
        paint_at_width(&mut pane, &ctx, &theme, 0, 1200.0, &columns);
        let short = pane.name_cache[&0].1.clone();
        let full = pane.name_cache[&1].1.clone();
        let original_width = pane.name_cache[&1].0;
        assert!(!short.elided && !full.elided);

        paint_at_width(&mut pane, &ctx, &theme, 1, 1050.0, &columns);
        assert!(Arc::ptr_eq(&full, &pane.name_cache[&1].1));
        assert_eq!(pane.name_cache[&1].0, original_width);
        assert_eq!(pane.benchmark_name_galleys(&ctx, &theme), (2, 0));

        paint_at_width(&mut pane, &ctx, &theme, 2, 240.0, &columns);
        let truncated = pane.name_cache[&1].1.clone();
        assert!(truncated.elided);
        assert!(!Arc::ptr_eq(&full, &truncated));
        paint_at_width(&mut pane, &ctx, &theme, 3, 180.0, &columns);
        assert!(pane.name_cache[&1].1.elided);
        assert!(!Arc::ptr_eq(&truncated, &pane.name_cache[&1].1));
        assert_eq!(pane.benchmark_name_galleys(&ctx, &theme), (2, 0));

        paint_at_width(&mut pane, &ctx, &theme, 4, 1200.0, &columns);
        assert!(!pane.name_cache[&1].1.elided);
        assert!(Arc::ptr_eq(&short, &pane.name_cache[&0].1));
        assert_eq!(
            pane.row_cache[&1].parent.as_deref(),
            Some(parent.to_string_lossy().as_ref())
        );
        assert_eq!(pane.benchmark_name_galleys(&ctx, &theme), (2, 0));
    }

    #[test]
    fn name_reuse_preserves_selection_query_theme_font_zoom_and_dpi_changes() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("name-style-fixture"));
        pane.accept(Arc::new(Snapshot::from_entries(
            pane.path.clone(),
            vec![foldex_core::Entry::from_name(
                "Design-report.txt",
                false,
                0,
                0,
            )],
        )));
        pane.loading = false;
        let ctx = egui::Context::default();
        let mut theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        theme.apply(&ctx);
        foldex_ui::motion::set_direct_layout(&ctx, true);
        let mut columns = columns::StoredColumns::default();
        for column in &mut columns.columns {
            column.visible = column.column == columns::Column::Name;
        }
        paint_at_width(&mut pane, &ctx, &theme, 0, 1200.0, &columns);
        let initial = pane.name_cache[&0].1.clone();
        pane.selected.insert(0);
        pane.focused = Some(0);
        paint_at_width(&mut pane, &ctx, &theme, 1, 1100.0, &columns);
        assert!(Arc::ptr_eq(&initial, &pane.name_cache[&0].1));

        theme.text = [83, 94, 105];
        theme.accent = [200, 80, 40];
        theme.apply(&ctx);
        paint_at_width(&mut pane, &ctx, &theme, 2, 1050.0, &columns);
        let recolored = pane.name_cache[&0].1.clone();
        assert!(!Arc::ptr_eq(&initial, &recolored));
        assert!(
            recolored
                .job
                .sections
                .iter()
                .all(|section| section.format.color == theme.fg())
        );
        theme.font_size += 2.0;
        theme.apply(&ctx);
        paint_at_width(&mut pane, &ctx, &theme, 3, 1000.0, &columns);
        assert!(!Arc::ptr_eq(&recolored, &pane.name_cache[&0].1));
        assert!(
            pane.name_cache[&0]
                .1
                .job
                .sections
                .iter()
                .all(|section| section.format.font_id == theme.font())
        );

        pane.filter = "Design".into();
        pane.refilter();
        let deadline = Instant::now() + Duration::from_secs(3);
        while pane.rendered_query != pane.query && Instant::now() < deadline {
            pane.poll();
            std::thread::yield_now();
        }
        assert_eq!(pane.rendered_query, pane.query);
        assert!(pane.name_cache.is_empty());
        paint_at_width(&mut pane, &ctx, &theme, 4, 950.0, &columns);
        let highlighted = pane.name_cache[&0].1.clone();
        let matched: String = highlighted
            .job
            .sections
            .iter()
            .filter(|section| section.format.color == theme.accent())
            .map(|section| {
                &highlighted.job.text[section.byte_range.start.0..section.byte_range.end.0]
            })
            .collect();
        assert_eq!(matched, "Design");
        paint_at_width(&mut pane, &ctx, &theme, 5, 900.0, &columns);
        assert!(Arc::ptr_eq(&highlighted, &pane.name_cache[&0].1));

        ctx.set_pixels_per_point(1.5);
        paint_at_width(&mut pane, &ctx, &theme, 6, 850.0, &columns);
        assert!(!Arc::ptr_eq(&highlighted, &pane.name_cache[&0].1));
        assert_eq!(
            pane.name_cache[&0].1.pixels_per_point,
            ctx.pixels_per_point()
        );
        for (frame, zoom) in foldex_ui::view::ZOOM_STOPS.iter().enumerate() {
            pane.set_zoom(*zoom);
            paint_at_width(&mut pane, &ctx, &theme, frame as u64 + 7, 800.0, &columns);
            assert_eq!(pane.benchmark_name_galleys(&ctx, &theme), (1, 0));
        }
    }

    #[test]
    fn empty_list_keyboard_outline_waits_for_a_current_finished_folder() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("empty-listing-fixture"));
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        theme.apply(&ctx);
        let body = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 500.0));
        for frame in 0..7 {
            pane.loading = matches!(frame, 0 | 1);
            pane.resolving = frame == 3;
            pane.scanning = frame == 4;
            if frame == 1 {
                pane.accept(Arc::new(Snapshot::from_entries(
                    pane.path.clone(),
                    Vec::new(),
                )));
            }
            if frame == 5 {
                pane.path.push("another-folder");
            }
            if frame == 6 {
                pane.accept(Arc::new(Snapshot::from_entries(
                    pane.path.clone(),
                    Vec::new(),
                )));
            }
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(body),
                    time: Some(frame as f64 / 60.0),
                    focused: true,
                    events: vec![egui::Event::Key {
                        key: Key::ArrowDown,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    crate::keyboard::request_id(ui.ctx(), pane.listing_focus_id());
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
                    assert!(
                        pane.listing_owns_keyboard(ui.ctx()),
                        "loading lost logical focus"
                    );
                },
            );
            let outlines = output
                .shapes
                .iter()
                .filter(|shape| {
                    matches!(&shape.shape, egui::Shape::Rect(rect)
                    if rect.rect == body.shrink(1.0)
                        && rect.stroke.width == 1.0 && rect.stroke.color == theme.accent())
                })
                .count();
            assert_eq!(
                outlines,
                usize::from(matches!(frame, 2 | 6)),
                "frame {frame}"
            );
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn opening_another_folder_has_no_entry_arrival_motion() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("listing-motion-first"));
        let ctx = egui::Context::default();
        let theme = Theme::default();
        theme.apply(&ctx);
        pane.accept(Arc::new(Snapshot::from_entries(
            pane.path.clone(),
            vec![foldex_core::Entry::from_name("first.txt", false, 0, 0)],
        )));
        paint(&mut pane, &ctx, &theme, 0);
        assert_eq!(pane.listing_appearing, 0);
        assert!(!pane.listing_motion.is_moving());
        pane.navigate(PathBuf::from("listing-motion-second"), false);
        pane.accept(Arc::new(Snapshot::from_entries(
            pane.path.clone(),
            vec![foldex_core::Entry::from_name("second.txt", false, 0, 0)],
        )));
        pane.loading = false;
        paint(&mut pane, &ctx, &theme, 1);
        assert_eq!(pane.listing_appearing, 0);
        assert!(!pane.listing_motion.is_moving());
        pane.set_zoom(1.0);
        paint(&mut pane, &ctx, &theme, 2);
        assert!(
            pane.listing_motion.is_moving(),
            "layout reflow lost its motion"
        );
    }

    #[test]
    fn retained_parent_and_caption_text_follow_snapshot_replacements() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let path = PathBuf::from("virtual-listing-text");
        let mut pane = Pane::dormant(1, &engine, path.clone());
        let snapshot = |parent: &str, bytes| {
            let mut entry = foldex_core::Entry::from_name("report.txt", false, bytes, 0);
            entry.relative_path = Some(Path::new(parent).join("report.txt"));
            Arc::new(Snapshot::from_entries(path.clone(), vec![entry]))
        };
        pane.accept(snapshot("東京", 1024));
        pane.recursive = true;
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        theme.apply(&ctx);
        paint(&mut pane, &ctx, &theme, 0);
        assert_eq!(pane.row_cache[&0].parent.as_deref(), Some("東京"));
        assert!(pane.row_cache[&0].size.is_none());
        pane.zoom = 1.0;
        paint(&mut pane, &ctx, &theme, 1);
        assert_eq!(
            pane.row_cache[&0].size.as_deref(),
            Some(foldex_ui::format_size(1024).as_str())
        );
        pane.accept(snapshot("New parent", 8192));
        assert!(pane.row_cache.is_empty());
        pane.zoom = 0.0;
        paint(&mut pane, &ctx, &theme, 2);
        assert_eq!(pane.row_cache[&0].parent.as_deref(), Some("New parent"));
        pane.zoom = 1.0;
        paint(&mut pane, &ctx, &theme, 3);
        assert_eq!(
            pane.row_cache[&0].size.as_deref(),
            Some(foldex_ui::format_size(8192).as_str())
        );
    }

    #[test]
    fn fuzzy_highlights_keep_separated_letters_and_unicode_byte_boundaries() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        theme.apply(&ctx);
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for (name, query, expected) in
                [("Design", "dsg", "Dsg"), ("東京の本.txt", "東本", "東本")]
            {
                let galley = name_galley(
                    ui.painter(),
                    &theme,
                    name,
                    &foldex_match::Matcher::new(query),
                    theme.fg(),
                    f32::INFINITY,
                );
                let highlighted: String = galley
                    .job
                    .sections
                    .iter()
                    .filter(|section| section.format.color == theme.accent())
                    .map(|section| &name[section.byte_range.start.0..section.byte_range.end.0])
                    .collect();
                assert_eq!(highlighted, expected);
            }
        });
        output.drop_without_applying_deltas();
    }
}
