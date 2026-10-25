use super::*;
use crate::{
    assets::{AssetManager, AssetStamp, ViewportSignature, VisibleAsset},
    home_history::{Access, Validated},
    navigation::{Navigation, Route},
};
use foldex_ui::view::{GridMetrics, ViewMode};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
struct HomeItem {
    path: PathBuf,
    card: bool,
}

#[derive(Clone, Default)]
struct HomeKeyboard {
    focused: Option<HomeItem>,
    was_focused: bool,
    reveal: bool,
    view: Option<Arc<HomeView>>,
    layout: Option<[u32; 5]>,
    scroll_id: Option<Id>,
}

struct HomeLabel {
    name: String,
    parent: String,
    search_path: String,
    reason: String,
    icon: Icon,
    cut_key: Option<crate::cut_state::PathKey>,
}
struct HomeLabels {
    revision: u64,
    rows: Vec<HomeLabel>,
}
impl HomeLabels {
    fn get(ctx: &egui::Context, history: &crate::home_history::Service) -> Arc<Self> {
        let id = Id::new("home-history-labels");
        if let Some(cached) = ctx.data(|data| data.get_temp::<Arc<Self>>(id))
            && cached.revision == history.revision
            && cached.rows.len() == history.rows.len()
        {
            return cached;
        }
        let labels = Arc::new(Self {
            revision: history.revision,
            rows: history
                .rows
                .iter()
                .map(|row| HomeLabel {
                    name: crate::pane::path_label(&row.entry.path),
                    parent: parent_label(row),
                    search_path: row.entry.path.to_string_lossy().to_lowercase(),
                    reason: row.reason(),
                    icon: foldex_ui::file_icon(
                        &row.entry
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                        row.is_dir,
                    ),
                    cut_key: crate::cut_state::PathKey::new(&row.entry.path),
                })
                .collect(),
        });
        ctx.data_mut(|data| data.insert_temp(id, labels.clone()));
        labels
    }
}

/// Only indices are cached per pane. The shared label cache and validated
/// history own the text/paths, so navigation frames never clone all 512 paths.
struct HomeView {
    revision: u64,
    row_count: usize,
    filter: String,
    query: String,
    kind: foldex_core::KindFilter,
    columns: usize,
    ranked_at: i64,
    rank_until: i64,
    rows: Vec<usize>,
    cards: Vec<usize>,
    counts: (usize, usize),
}
impl HomeView {
    fn matches(
        &self,
        history: &crate::home_history::Service,
        filter: &str,
        kind: foldex_core::KindFilter,
        columns: usize,
        at: i64,
    ) -> bool {
        self.revision == history.revision
            && self.row_count == history.rows.len()
            && self.filter == filter
            && self.kind == kind
            && self.columns == columns
            && (!self.query.is_empty() || at >= self.ranked_at && at < self.rank_until)
    }
    fn new(
        history: &crate::home_history::Service,
        labels: &HomeLabels,
        filter: &str,
        kind: foldex_core::KindFilter,
        columns: usize,
        at: i64,
    ) -> Self {
        let query = filter.to_lowercase();
        let mut rows: Vec<_> = labels
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, label)| {
                (query.is_empty() || label.search_path.contains(&query)).then_some(index)
            })
            .collect();
        let folders = rows
            .iter()
            .filter(|&&index| history.rows[index].is_dir)
            .count();
        let counts = (folders, rows.len() - folders);
        rows.retain(|&index| kind.includes(history.rows[index].is_dir));
        let mut cards = if query.is_empty() {
            crate::home_history::recommended_at(&history.rows, at)
        } else {
            Vec::new()
        };
        cards.retain(|&index| kind.includes(history.rows[index].is_dir));
        cards.truncate(columns);
        Self {
            revision: history.revision,
            row_count: history.rows.len(),
            filter: filter.to_owned(),
            query,
            kind,
            columns,
            ranked_at: at,
            rank_until: crate::home_history::recommendation_deadline(&history.rows, at),
            rows,
            cards,
            counts,
        }
    }
}

fn home_item_matches(item: Option<&HomeItem>, row: &Validated, card: bool) -> bool {
    item.is_some_and(|item| item.card == card && item.path == row.entry.path)
}

fn home_step(
    current: usize,
    cards: usize,
    rows: usize,
    columns: usize,
    key: Key,
    page: usize,
) -> usize {
    let last = (cards + rows).saturating_sub(1);
    let columns = columns.max(1);
    match key {
        Key::Home => 0,
        Key::End => last,
        Key::ArrowLeft if current < cards => current.saturating_sub(1),
        Key::ArrowRight if current < cards => (current + 1).min(cards.saturating_sub(1)),
        Key::ArrowDown if current < cards => (cards + current.min(columns - 1)).min(last),
        Key::ArrowUp if current < cards => current,
        Key::ArrowUp if current < cards + columns && cards > 0 => {
            if columns == 1 {
                cards - 1
            } else {
                (current - cards).min(cards - 1)
            }
        }
        Key::ArrowDown => current.saturating_add(columns).min(last),
        Key::ArrowUp => current.saturating_sub(columns).max(cards),
        Key::ArrowLeft if columns > 1 => current.saturating_sub(1).max(cards),
        Key::ArrowRight if columns > 1 => (current + 1).min(last),
        Key::PageDown => current.saturating_add(page.max(1)).min(last),
        Key::PageUp => current.saturating_sub(page.max(1)),
        _ => current,
    }
}

fn home_reveal_offset(current: f32, height: f32, content_height: f32, target: Rect) -> f32 {
    let height = height.max(1.0);
    let offset = if target.height() >= height || target.top() < current {
        target.top()
    } else if target.bottom() > current + height {
        target.bottom() - height
    } else {
        current
    };
    offset.clamp(0.0, (content_height - height).max(0.0))
}

#[derive(Clone)]
pub(super) struct HomeProbe {
    pub recommended: usize,
    pub recent: usize,
    pub cards: Vec<Rect>,
    pub rows: Vec<Rect>,
    pub address: Rect,
    pub body: Rect,
    pub footer: Rect,
    pub mode: ViewMode,
    pub columns: usize,
    pub asset_requests: usize,
    pub scroll_id: Option<Id>,
    pub selected_visible: bool,
    pub focused_visible: bool,
}
impl Default for HomeProbe {
    fn default() -> Self {
        Self {
            recommended: 0,
            recent: 0,
            cards: vec![],
            rows: vec![],
            address: Rect::NOTHING,
            body: Rect::NOTHING,
            footer: Rect::NOTHING,
            mode: ViewMode::Details,
            columns: 1,
            asset_requests: 0,
            scroll_id: None,
            selected_visible: false,
            focused_visible: false,
        }
    }
}

impl Foldex {
    pub(crate) fn navigate_home(&mut self) {
        if !self.pane().is_home() {
            self.pane_mut().navigate_home(true);
        }
        self.home.invalidate();
        self.session_dirty = true;
    }
    pub(super) fn new_current_route_tab(&mut self) {
        if self.pane().is_home() {
            self.new_home_tab(None);
        } else if self.pane().is_recycle_bin() {
            self.new_recycle_tab(None);
        } else {
            self.new_tab(self.pane().path.clone());
        }
    }
    fn new_home_tab(&mut self, split: Option<bool>) {
        let mut pane = Pane::dormant(self.next_id, &self.engine, self.pane().path.clone());
        pane.attach_search(self.search.client());
        pane.restore_navigation(Navigation::new(Route::Home));
        let id = pane.id;
        self.next_id += 1;
        self.tabs.push(Tab {
            left: pane,
            right: None,
            vertical: false,
            pending_axis: None,
            split: 0.5,
            reveal: 0.0,
            closing: false,
        });
        if let Some(vertical) = split {
            self.desk.split(id, vertical);
        } else {
            self.desk.add(id);
        }
        self.active = self.tabs.len() - 1;
        self.active_pane = 0;
        self.home.invalidate();
        self.session_dirty = true;
    }
    pub(super) fn split_current_route(&mut self, vertical: bool) {
        if self.pane().is_home() {
            self.new_home_tab(Some(vertical));
        } else if self.pane().is_recycle_bin() {
            self.new_recycle_tab(Some(vertical));
        } else {
            self.split_workspace(self.pane().path.clone(), vertical);
        }
    }
    pub(crate) fn record_home_usage(&mut self, path: PathBuf, access: Access) {
        self.home_history
            .record(path, access, crate::home_history::now());
        self.home_usage_revision = self.home_usage_revision.wrapping_add(1);
        self.home.invalidate();
        self.session_dirty = true;
    }
    pub(super) fn retained_asset_views(&self) -> Vec<u64> {
        let mut views = if self.explorer_visible {
            self.desk.visible()
        } else {
            vec![]
        };
        if self.sidebar && self.explorer_visible {
            views.push(crate::sidebar::ICON_VIEW);
        }
        if self.launcher_open {
            views.push(crate::launcher::Launcher::icon_view_id());
        }
        views
    }
    pub(super) fn service_home(&mut self) {
        let visible = self.explorer_visible
            && self.desk.visible().iter().any(|id| {
                self.tabs.iter().any(|tab| {
                    tab.left.id == *id && tab.left.is_home()
                        || tab
                            .right
                            .as_ref()
                            .is_some_and(|pane| pane.id == *id && pane.is_home())
                })
            });
        self.home.service(&self.home_history, visible);
        if self
            .home_history
            .merge_windows_recent(self.home.take_imported())
        {
            self.home_usage_revision = self.home_usage_revision.wrapping_add(1);
            self.session_dirty = true;
        }
    }
}

/// Home shares the pane footer while keeping its history and asset work independent
/// of the filesystem listing and column worker.
pub(super) fn pane_ui(
    ui: &mut Ui,
    t: &Theme,
    pane: &mut Pane,
    history: &crate::home_history::Service,
    assets: &mut AssetManager,
    active: bool,
    inspector: bool,
) -> Vec<Action> {
    history.schedule_recheck(ui.ctx());
    let mut actions = Vec::new();
    let all = ui.max_rect();
    let m = t.metrics();
    let scale = m.scale;
    let listing_id = crate::keyboard::listing_id(pane.id);
    let filter_id = Id::new((pane.id, "filter"));
    let state_id = listing_id.with("home-keyboard");
    let mut keyboard = ui
        .ctx()
        .data(|data| data.get_temp::<HomeKeyboard>(state_id))
        .unwrap_or_default();
    if ui.memory(|memory| memory.has_focus(listing_id))
        && Pane::consume_navigation_key(ui.ctx(), egui::Modifiers::SHIFT, Key::Tab)
    {
        pane.focus_filter = true;
        crate::keyboard::request_id(ui.ctx(), filter_id);
    }
    let chrome = pane.chrome(ui, t, all, active, inspector, &mut actions);
    let toolbar = chrome.toolbar;
    let footer = chrome.footer;
    let address = chrome.breadcrumbs;
    let toolbar_painter = ui
        .painter()
        .with_clip_rect(toolbar.intersect(ui.clip_rect()));
    if !pane.is_home() {
        return actions;
    }

    let body_top = (toolbar.bottom() + 3.0 * scale).min(footer.top());
    let body = Rect::from_min_max(
        pos2(all.left(), body_top),
        pos2(all.right(), (footer.top() - 2.0 * scale).max(body_top)),
    );
    let padding = (20.0 * scale).min(body.width() * 0.06);
    let scroll_width = ui.spacing().scroll.allocated_width();
    let width = (body.width() - 2.0 * padding - scroll_width).max(1.0);
    let grid = GridMetrics::new(
        width,
        t.font_size,
        t.file_row_height(),
        if pane.zoom < 0.3 { 0.0 } else { t.gap },
        pane.zoom,
    );
    let card_icon = if grid.mode.is_grid() {
        grid.icon_size
    } else {
        58.0 * scale
    };
    let columns = ((width + 12.0 * scale) / (190.0 * scale).max(card_icon + 36.0 * scale))
        .floor()
        .clamp(1.0, 4.0) as usize;
    let labels = HomeLabels::get(ui.ctx(), history);
    let at = crate::home_history::now();
    let view = keyboard
        .view
        .take()
        .filter(|view| view.matches(history, &pane.filter, pane.query.kind, columns, at))
        .unwrap_or_else(|| {
            Arc::new(HomeView::new(
                history,
                &labels,
                &pane.filter,
                pane.query.kind,
                columns,
                at,
            ))
        });
    keyboard.view = Some(view.clone());
    let query = &view.query;
    let rows = &view.rows;
    let cards = &view.cards;
    let gap = 8.0 * scale;
    let card_width = ((width - gap * columns.saturating_sub(1) as f32) / columns as f32).max(1.0);
    let card_icon = card_icon.min((card_width - 24.0 * scale).max(0.0));
    let card_height = (card_icon + 92.0 * scale).max(150.0 * scale);
    let layout = [
        grid.columns as u32,
        grid.item_size.x.to_bits(),
        grid.stride().y.to_bits(),
        columns as u32,
        card_height.to_bits(),
    ];
    if keyboard.layout != Some(layout) {
        keyboard.reveal = true;
        keyboard.layout = Some(layout);
    }
    crate::keyboard::composite(
        ui,
        crate::keyboard::Region::Listing(pane.id),
        listing_id,
        body,
        "Home recommended and recent items",
    );
    foldex_ui::components::register_focus_target(ui, body, listing_id);
    let items = || {
        cards
            .iter()
            .map(|&index| (&history.rows[index], true))
            .chain(rows.iter().map(|&index| (&history.rows[index], false)))
    };
    let item_count = cards.len() + rows.len();
    if !items().any(|(row, card)| home_item_matches(keyboard.focused.as_ref(), row, card)) {
        keyboard.focused = items()
            .find(|(row, _)| Some(&row.entry.path) == pane.home_selected.as_ref())
            .or_else(|| items().next())
            .map(|(row, card)| HomeItem {
                path: row.entry.path.clone(),
                card,
            });
        keyboard.reveal = true;
    }
    let owned = ui.memory(|memory| memory.has_focus(listing_id));
    foldex_ui::components::focus_ring(
        ui,
        body,
        listing_id.with("empty-home-ring"),
        t,
        owned && item_count == 0,
    );
    let mut context_menu = false;
    if active && owned && !pane.has_popup() && foldex_ui::components::can_focus(ui) {
        keyboard.reveal |= !keyboard.was_focused;
        for _ in 0..32 {
            let Some(key) = Pane::take_composite_key(
                ui.ctx(),
                &[
                    Key::ArrowDown,
                    Key::ArrowUp,
                    Key::ArrowLeft,
                    Key::ArrowRight,
                    Key::Home,
                    Key::End,
                    Key::PageDown,
                    Key::PageUp,
                    Key::Escape,
                    Key::Enter,
                    Key::Space,
                    Key::F10,
                ],
            ) else {
                break;
            };
            if key == Key::Escape {
                assets.cancel_cut();
                if !pane.filter.is_empty() {
                    pane.filter.clear();
                } else {
                    pane.home_selected = None;
                }
                ui.ctx().request_repaint();
                break;
            }
            if item_count == 0 {
                continue;
            }
            let at = items()
                .position(|(row, card)| home_item_matches(keyboard.focused.as_ref(), row, card))
                .unwrap_or(0);
            if key == Key::F10 {
                context_menu = true;
                break;
            }
            if matches!(key, Key::Enter | Key::Space) {
                if let Some(item) = &keyboard.focused {
                    pane.home_selected = Some(item.path.clone());
                    if key == Key::Enter
                        && let Some(row) =
                            history.rows.iter().find(|row| row.entry.path == item.path)
                    {
                        actions.push(if row.is_dir {
                            Action::Navigate(item.path.clone())
                        } else {
                            Action::Open(item.path.clone())
                        });
                    }
                }
                if key == Key::Space {
                    continue;
                }
                break;
            }
            let page = (body.height() / grid.stride().y).floor().max(1.0) as usize * grid.columns;
            keyboard.focused = items()
                .nth(home_step(
                    at,
                    cards.len(),
                    rows.len(),
                    grid.columns,
                    key,
                    page,
                ))
                .map(|(row, card)| HomeItem {
                    path: row.entry.path.clone(),
                    card,
                });
            keyboard.reveal = true;
        }
    }
    keyboard.was_focused = owned;
    let scroll_selection = keyboard
        .reveal
        .then(|| {
            items().position(|(row, card)| home_item_matches(keyboard.focused.as_ref(), row, card))
        })
        .flatten();
    let card_top = 40.0 * scale;
    let card_rows = cards.len().div_ceil(columns);
    let recent_top = card_top
        + card_rows as f32 * (card_height + gap)
        + if card_rows > 0 {
            16.0 * scale
        } else {
            40.0 * scale
        };
    let list_top = recent_top + 40.0 * scale;
    let total_height =
        (list_top + grid.total_height(rows.len()).max(t.file_row_height()) + padding)
            .max(body.height() - 2.0 * padding);
    let benchmark_probe = ui
        .ctx()
        .data(|data| data.get_temp::<bool>(Id::new("benchmark-probe-layout")))
        .unwrap_or(false);
    let mut probe = benchmark_probe.then(|| HomeProbe {
        recommended: cards.len(),
        recent: rows.len(),
        address: chrome.breadcrumbs,
        body,
        footer,
        mode: grid.mode,
        columns: grid.columns,
        ..Default::default()
    });
    ui.scope_builder(
        UiBuilder::new()
            .id_salt((pane.id, "home-body"))
            .max_rect(body),
        |ui| {
            ui.set_clip_rect(body.intersect(ui.clip_rect()));
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt((pane.id, "home-scroll"))
                .auto_shrink([false, false])
                .min_scrolled_height(1.0);
            if let Some(slot) = scroll_selection {
                let target = if slot < cards.len() {
                    Rect::from_min_size(
                        pos2(padding, padding)
                            + vec2((slot % columns) as f32 * (card_width + gap), card_top),
                        vec2(card_width, card_height),
                    )
                } else {
                    grid.rect_for(slot - cards.len())
                        .translate(vec2(padding, padding + list_top))
                };
                let current = keyboard
                    .scroll_id
                    .and_then(|id| egui::scroll_area::State::load(ui.ctx(), id))
                    .map_or(0.0, |state| state.offset.y);
                // Set the offset before egui constructs the content origin and
                // viewport. A queued scroll_to_rect target is applied afterwards,
                // so rapid zoom reflows can otherwise paint a stale row window.
                scroll = scroll.vertical_scroll_offset(home_reveal_offset(
                    current,
                    body.height(),
                    total_height + padding * 2.0,
                    target,
                ));
            }
            let scroll_output = scroll.show_viewport(ui, |ui, viewport| {
                let (content, _) = ui.allocate_exact_size(
                    vec2(
                        (body.width() - scroll_width).max(0.0),
                        total_height + padding * 2.0,
                    ),
                    Sense::hover(),
                );
                let origin = content.min + vec2(padding, padding);
                let p = ui.painter().with_clip_rect(body.intersect(ui.clip_rect()));
                let first = (((viewport.top() - padding - list_top) / grid.stride().y)
                    .floor()
                    .max(0.0) as usize)
                    .saturating_mul(grid.columns)
                    .min(rows.len());
                let end = (((viewport.bottom() - padding - list_top) / grid.stride().y)
                    .ceil()
                    .max(0.0) as usize)
                    .saturating_mul(grid.columns)
                    .min(rows.len());
                let mut visible_cards = Vec::new();
                for (slot, index) in cards.iter().copied().enumerate() {
                    let rect = Rect::from_min_size(
                        origin
                            + vec2(
                                (slot % columns) as f32 * (card_width + gap),
                                card_top + (slot / columns) as f32 * (card_height + gap),
                            ),
                        vec2(card_width, card_height),
                    );
                    if rect.intersects(p.clip_rect()) {
                        visible_cards.push((index, rect));
                    }
                }
                // One bounded asset lane per visible Home pane; cards and virtual rows
                // share a single request submission and the existing native cache.
                let physical_icon = if visible_cards.is_empty() {
                    grid.icon_size
                } else {
                    grid.icon_size.max(card_icon)
                } * ui.ctx().pixels_per_point();
                let size = [16, 48, 96, 152, 256]
                    .into_iter()
                    .find(|size| *size as f32 >= physical_icon)
                    .unwrap_or(256);
                let mut requested: Vec<(usize, bool)> = visible_cards
                    .iter()
                    .map(|(index, _)| (*index, true))
                    .collect();
                requested.extend(
                    rows[first..end]
                        .iter()
                        .take(foldex_assets::MAX_REQUESTS_PER_VIEW - 1 - visible_cards.len())
                        .map(|index| (*index, grid.mode.is_grid())),
                );
                if let Some(probe) = &mut probe {
                    probe.asset_requests = requested.len() + 1;
                }
                // Include the real Home icon in the same native lane.
                let mut signature = std::collections::hash_map::DefaultHasher::new();
                requested.hash(&mut signature);
                history.revision.hash(&mut signature);
                query.hash(&mut signature);
                let epoch = pane.navigation_epoch().wrapping_add(signature.finish());
                let _ = assets.set_requests(
                    pane.id,
                    ViewportSignature {
                        epoch,
                        revision: history.revision,
                        first: 0,
                        end: requested.len() + 1,
                        size,
                        thumbnails: true,
                    },
                    || {
                        let mut requests = vec![VisibleAsset {
                            key: foldex_assets::AssetKey::known_folder_icon(
                                foldex_assets::KnownFolderIcon::Home,
                                size,
                            ),
                            stamp: AssetStamp::UNOBSERVED,
                        }];
                        requests.extend(requested.iter().map(|(index, thumbnail)| {
                            let row = &history.rows[*index];
                            VisibleAsset {
                                key: foldex_assets::AssetKey {
                                    path: row.entry.path.clone(),
                                    size,
                                    thumbnail: *thumbnail && !row.is_dir,
                                },
                                stamp: AssetStamp {
                                    bytes: row.bytes,
                                    modified_secs: row.modified,
                                    is_dir: row.is_dir,
                                },
                            }
                        }));
                        requests
                    },
                );
                if let Some(image) = assets.image(pane.id, epoch, 0) {
                    paint_image(
                        &toolbar_painter,
                        Rect::from_center_size(
                            pos2(address.left() + 12.0 * scale, address.center().y),
                            vec2(m.icon, m.icon),
                        ),
                        image,
                    );
                }
                section_heading(&p, origin, width, t, "Recommended");
                if cards.is_empty() {
                    text(
                        &p,
                        origin + vec2(0.0, 44.0 * scale),
                        t,
                        if history.pending() {
                            "Checking your recent items…"
                        } else {
                            "Your frequently used and recently opened items appear here."
                        },
                        t.dim(),
                        false,
                    );
                }
                for (asset, (index, rect)) in visible_cards.iter().enumerate() {
                    let row = &history.rows[*index];
                    let label_text = &labels.rows[*index];
                    let response = item_response(
                        ui,
                        t,
                        pane,
                        *rect,
                        row,
                        label_text,
                        &mut actions,
                        "card",
                        &mut keyboard,
                        context_menu,
                    );
                    p.rect_filled(
                        *rect,
                        t.radius.saturating_add(2),
                        if response.hovered() {
                            Theme::color(t.hover)
                        } else {
                            Theme::color(t.surface)
                        },
                    );
                    p.rect_stroke(
                        *rect,
                        t.radius.saturating_add(2),
                        Stroke::new(
                            1.0,
                            if pane.home_selected.as_ref() == Some(&row.entry.path) {
                                t.accent()
                            } else {
                                t.line()
                            },
                        ),
                        StrokeKind::Inside,
                    );
                    let mut p = p.clone();
                    p.multiply_opacity(crate::cut_state::content_opacity(
                        assets.is_cut_key(label_text.cut_key.as_ref()),
                    ));
                    let reason = Rect::from_min_size(
                        rect.min + vec2(12.0 * scale, 5.0 * scale),
                        vec2(rect.width() - 24.0 * scale, 27.0 * scale),
                    );
                    foldex_ui::components::elided_label(
                        &p,
                        reason,
                        t,
                        &label_text.reason,
                        t.dim(),
                        false,
                    );
                    let image_rect = Rect::from_center_size(
                        pos2(rect.center().x, rect.top() + 35.0 * scale + card_icon * 0.5),
                        vec2(card_icon, card_icon),
                    );
                    paint_file(
                        &p,
                        t,
                        image_rect,
                        row,
                        label_text.icon,
                        assets.image(pane.id, epoch, asset + 1),
                    );
                    label(
                        &p,
                        Rect::from_min_max(
                            pos2(rect.left() + 12.0 * scale, rect.bottom() - 47.0 * scale),
                            pos2(rect.right() - 12.0 * scale, rect.bottom() - 25.0 * scale),
                        ),
                        t,
                        &label_text.name,
                        t.fg(),
                    );
                    label(
                        &p,
                        Rect::from_min_max(
                            pos2(rect.left() + 12.0 * scale, rect.bottom() - 27.0 * scale),
                            pos2(rect.right() - 12.0 * scale, rect.bottom() - 5.0 * scale),
                        ),
                        t,
                        &label_text.parent,
                        t.dim(),
                    );
                    home_focus(ui, t, pane.id, *rect, row, true, &keyboard);
                    if let Some(probe) = &mut probe {
                        probe.cards.push(*rect);
                        probe.focused_visible |= rect.intersects(p.clip_rect())
                            && keyboard
                                .focused
                                .as_ref()
                                .is_some_and(|item| item.card && item.path == row.entry.path)
                            && ui.memory(|memory| memory.has_focus(listing_id));
                    }
                }
                section_heading(&p, origin + vec2(0.0, recent_top), width, t, "Recent");
                for (offset, index) in rows[first..end].iter().copied().enumerate() {
                    let row = &history.rows[index];
                    let label_text = &labels.rows[index];
                    let rect = grid
                        .rect_for(first + offset)
                        .translate(origin.to_vec2() + vec2(0.0, list_top));
                    let response = item_response(
                        ui,
                        t,
                        pane,
                        rect,
                        row,
                        label_text,
                        &mut actions,
                        "recent",
                        &mut keyboard,
                        context_menu,
                    );
                    if response.hovered() || pane.home_selected.as_ref() == Some(&row.entry.path) {
                        p.rect_filled(
                            rect.shrink(1.0),
                            t.radius,
                            Theme::color(if pane.home_selected.as_ref() == Some(&row.entry.path) {
                                t.selection
                            } else {
                                t.hover
                            }),
                        );
                    }
                    let mut p = p.clone();
                    p.multiply_opacity(crate::cut_state::content_opacity(
                        assets.is_cut_key(label_text.cut_key.as_ref()),
                    ));
                    paint_recent(
                        &p,
                        t,
                        grid,
                        rect,
                        row,
                        label_text,
                        assets.image(pane.id, epoch, visible_cards.len() + offset + 1),
                    );
                    home_focus(ui, t, pane.id, rect, row, false, &keyboard);
                    if let Some(probe) = &mut probe {
                        probe.rows.push(rect);
                        probe.selected_visible |= rect.intersects(p.clip_rect())
                            && pane.home_selected.as_ref().is_some_and(|path| {
                                crate::bookmarks::path_eq(path, &row.entry.path)
                            });
                        probe.focused_visible |= rect.intersects(p.clip_rect())
                            && keyboard
                                .focused
                                .as_ref()
                                .is_some_and(|item| !item.card && item.path == row.entry.path)
                            && ui.memory(|memory| memory.has_focus(listing_id));
                    }
                }
                if rows.is_empty() {
                    text(
                        &p,
                        origin + vec2(0.0, list_top + 22.0 * scale),
                        t,
                        if history.pending() {
                            "Checking saved history…"
                        } else if pane.query.kind == foldex_core::KindFilter::None {
                            "Folders and files are hidden."
                        } else if query.is_empty() {
                            "Open files or visit folders in Foldex to build your recent history."
                        } else {
                            "No recent items match this filter."
                        },
                        t.dim(),
                        false,
                    );
                }
            });
            keyboard.scroll_id = Some(scroll_output.id);
            if let Some(probe) = &mut probe {
                probe.scroll_id = Some(scroll_output.id);
                probe.body = body.intersect(scroll_output.inner_rect);
            }
            if !pane.has_popup()
                && scroll_output.inner_rect.contains(
                    ui.input(|input| input.pointer.hover_pos().unwrap_or(egui::Pos2::ZERO)),
                )
                && ui.input(|input| input.modifiers.ctrl)
            {
                let detents: f32 = ui.input(|input| {
                    input
                        .raw
                        .events
                        .iter()
                        .filter_map(|event| match event {
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
                    pane.set_zoom(pane.zoom + detents * 0.055);
                }
            }
        },
    );
    let selected = rows
        .iter()
        .map(|&index| &history.rows[index])
        .find(|row| pane.home_selected.as_ref() == Some(&row.entry.path))
        .map_or((0, 0), |row| if row.is_dir { (1, 0) } else { (0, 1) });
    ui.painter().rect_filled(footer, 0, t.bg());
    ui.painter()
        .hline(footer.x_range(), footer.top(), Stroke::new(1.0, t.line()));
    ui.scope_builder(UiBuilder::new().max_rect(footer), |ui| {
        ui.set_clip_rect(footer.intersect(ui.clip_rect()));
        pane.home_footer(
            ui,
            t,
            footer,
            (!history.pending() || !history.rows.is_empty()).then_some(view.counts),
            selected,
        );
    });
    if let Some(probe) = probe {
        ui.ctx()
            .data_mut(|data| data.insert_temp(Id::new((pane.id, "home-probe")), probe));
    }
    keyboard.reveal = false;
    ui.ctx()
        .data_mut(|data| data.insert_temp(state_id, keyboard));
    actions
}

fn home_focus(
    ui: &Ui,
    t: &Theme,
    pane: u64,
    rect: Rect,
    row: &Validated,
    card: bool,
    keyboard: &HomeKeyboard,
) {
    if keyboard
        .focused
        .as_ref()
        .is_some_and(|item| item.card == card && item.path == row.entry.path)
        && ui.memory(|memory| memory.has_focus(crate::keyboard::listing_id(pane)))
    {
        foldex_ui::components::focus_ring_with_radius(
            ui,
            rect,
            crate::keyboard::listing_id(pane).with("home-item-ring"),
            t,
            true,
            if card {
                t.radius.saturating_add(2)
            } else {
                t.radius
            },
        );
    }
}

fn parent_label(row: &Validated) -> String {
    row.entry
        .path
        .parent()
        .map(crate::pane::friendly_path)
        .unwrap_or_default()
}
fn section_heading(p: &egui::Painter, at: egui::Pos2, width: f32, t: &Theme, value: &str) {
    let scale = t.metrics().scale;
    p.text(
        at,
        Align2::LEFT_TOP,
        value,
        egui::FontId::proportional(t.font_size + 2.0 * scale),
        t.fg(),
    );
    p.hline(
        at.x..=at.x + width,
        at.y + 28.0 * scale,
        Stroke::new(1.0, t.line().gamma_multiply(0.55)),
    );
}
fn text(
    p: &egui::Painter,
    at: egui::Pos2,
    t: &Theme,
    value: &str,
    color: egui::Color32,
    _heading: bool,
) {
    p.text(
        at,
        Align2::LEFT_TOP,
        value,
        egui::FontId::proportional(t.font_size),
        color,
    );
}
fn label(p: &egui::Painter, rect: Rect, t: &Theme, value: &str, color: egui::Color32) {
    foldex_ui::components::elided_label(p, rect, t, value, color, false);
}

fn paint_recent(
    p: &egui::Painter,
    t: &Theme,
    grid: GridMetrics,
    rect: Rect,
    row: &Validated,
    label: &HomeLabel,
    image: Option<crate::assets::AssetImage>,
) {
    let scale = t.metrics().scale;
    let image_rect = Rect::from_center_size(
        if grid.mode.is_grid() {
            pos2(
                rect.center().x,
                rect.top() + grid.icon_size * 0.5 + 9.0 * scale,
            )
        } else {
            pos2(
                rect.left() + (13.0 * scale).min(rect.width() * 0.5),
                rect.center().y,
            )
        },
        vec2(grid.icon_size, grid.icon_size),
    );
    paint_file(p, t, image_rect, row, label.icon, image);
    let name_rect = if grid.mode.is_grid() {
        Rect::from_min_max(
            pos2(rect.left() + 4.0 * scale, image_rect.bottom() + 8.0 * scale),
            pos2(
                rect.right() - 4.0 * scale,
                image_rect.bottom() + 12.0 * scale + t.font_size,
            ),
        )
    } else {
        Rect::from_min_max(
            pos2((rect.left() + 28.0 * scale).min(rect.right()), rect.top()),
            pos2((rect.right() - 8.0 * scale).max(rect.left()), rect.bottom()),
        )
    };
    if name_rect.width() <= 0.0 {
        return;
    }
    let has_parent = grid.mode == ViewMode::Details && !label.parent.is_empty();
    let width = foldex_ui::components::file_name_width(name_rect.width(), scale, has_parent);
    foldex_ui::fonts::request_text(p.ctx(), &label.name);
    let mut job = egui::text::LayoutJob::simple(label.name.clone(), t.font(), t.fg(), width);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = p.layout_job(job);
    let text_width = galley.size().x;
    let x = if grid.mode.is_grid() {
        name_rect.center().x - text_width.min(name_rect.width()) * 0.5
    } else {
        name_rect.left()
    };
    p.with_clip_rect(name_rect).galley(
        pos2(x, name_rect.center().y - galley.size().y * 0.5),
        galley,
        t.fg(),
    );
    if has_parent {
        foldex_ui::components::file_parent_label(
            p,
            name_rect,
            t,
            text_width,
            &label.parent,
            t.dim(),
        );
    } else if grid.mode.is_grid() {
        let parent_rect = Rect::from_min_max(
            pos2(name_rect.left(), name_rect.bottom() + 2.0 * scale),
            pos2(
                name_rect.right(),
                (name_rect.bottom() + t.font_size + 6.0 * scale).min(rect.bottom()),
            ),
        );
        if parent_rect.height() > 0.0 {
            foldex_ui::fonts::request_text(p.ctx(), &label.parent);
            let mut job = egui::text::LayoutJob::simple(
                label.parent.clone(),
                t.font(),
                t.dim(),
                parent_rect.width(),
            );
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = p.layout_job(job);
            p.with_clip_rect(parent_rect).galley(
                pos2(
                    parent_rect.center().x - galley.size().x.min(parent_rect.width()) * 0.5,
                    parent_rect.center().y - galley.size().y * 0.5,
                ),
                galley,
                t.dim(),
            );
        }
    }
}

fn item_response(
    ui: &mut Ui,
    t: &Theme,
    pane: &mut Pane,
    rect: Rect,
    row: &Validated,
    label: &HomeLabel,
    actions: &mut Vec<Action>,
    kind: &str,
    keyboard: &mut HomeKeyboard,
    context_menu: bool,
) -> egui::Response {
    let response = ui.interact(
        rect,
        Id::new((pane.id, "home-item", kind, &row.entry.path)),
        Sense::CLICK,
    );
    let card = kind == "card";
    if response.clicked() || response.secondary_clicked() {
        pane.home_selected = Some(row.entry.path.clone());
        keyboard.focused = Some(HomeItem {
            path: row.entry.path.clone(),
            card,
        });
        crate::keyboard::request_id(ui.ctx(), crate::keyboard::listing_id(pane.id));
    }
    if response.double_clicked() {
        actions.push(if row.is_dir {
            Action::Navigate(row.entry.path.clone())
        } else {
            Action::Open(row.entry.path.clone())
        });
    }
    if response.middle_clicked() {
        actions.push(if row.is_dir {
            Action::NewTab(row.entry.path.clone())
        } else {
            Action::Reveal(row.entry.path.clone())
        });
    }
    if response.secondary_clicked()
        || context_menu && home_item_matches(keyboard.focused.as_ref(), row, card)
    {
        actions.push(Action::Context {
            paths: vec![row.entry.path.clone()],
            background: false,
            position: (if response.secondary_clicked() {
                response.interact_pointer_pos()
            } else {
                None
            })
            .unwrap_or(rect.left_bottom()),
        });
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            pane.home_selected.as_ref() == Some(&row.entry.path),
            &label.name,
        )
    });
    let _ = t;
    response.on_hover_ui(|ui| {
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.label(format!(
            "{}\n{}\n{}",
            row.entry.path.display(),
            label.reason,
            row.entry.access.label()
        ));
    })
}
fn paint_file(
    p: &egui::Painter,
    t: &Theme,
    rect: Rect,
    row: &Validated,
    fallback_icon: Icon,
    image: Option<crate::assets::AssetImage>,
) {
    let fallback = image.as_ref().map_or(1.0, |image| 1.0 - image.opacity);
    icon(
        p,
        rect.center(),
        fallback_icon,
        (if row.is_dir {
            Theme::color(t.folder)
        } else {
            t.dim()
        })
        .gamma_multiply(fallback),
        rect.width().min(rect.height()),
    );
    if let Some(image) = image {
        paint_image(p, rect, image);
    }
}
fn paint_image(p: &egui::Painter, rect: Rect, image: crate::assets::AssetImage) {
    let draw = |texture, size: egui::Vec2, opacity: f32| {
        let ratio = (rect.width() / size.x.max(1.0)).min(rect.height() / size.y.max(1.0));
        p.image(
            texture,
            Rect::from_center_size(rect.center(), size * ratio),
            Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0)),
            egui::Color32::WHITE.gamma_multiply(opacity),
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

#[cfg(test)]
mod keyboard_tests {
    use super::*;

    fn key(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    struct Fixture {
        ctx: egui::Context,
        theme: Theme,
        _engine: foldex_core::Engine,
        pane: Pane,
        history: crate::home_history::Service,
        assets: AssetManager,
        frame: u64,
        size: egui::Vec2,
    }
    impl Fixture {
        fn new() -> Self {
            let ctx = egui::Context::default();
            let theme = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            theme.apply(&ctx);
            let engine = foldex_core::Engine::with_options(
                std::sync::Arc::new(|| {}),
                foldex_core::EngineOptions {
                    poll_interval: None,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut pane = Pane::dormant(701, &engine, PathBuf::from("virtual-home-fixture"));
            pane.navigate_home(true);
            let mut history =
                crate::home_history::Service::new(std::sync::Arc::new(|| {}), false).unwrap();
            history.rows = (0..80)
                .map(|index| Validated {
                    entry: crate::home_history::Entry {
                        path: PathBuf::from(format!("C:/home/recent_{index:02}.txt")),
                        access: Access::Opened,
                        last_used: 100 - index,
                        uses: 1,
                    },
                    is_dir: false,
                    bytes: 1,
                    modified: 0,
                })
                .collect();
            let assets = AssetManager::disabled(&ctx);
            Self {
                ctx,
                theme,
                _engine: engine,
                pane,
                history,
                assets,
                frame: 0,
                size: vec2(800.0, 500.0),
            }
        }
        fn draw(&mut self, events: Vec<egui::Event>) -> Vec<Action> {
            let (actions, output) = self.render(events);
            output.drop_without_applying_deltas();
            actions
        }
        fn render(&mut self, events: Vec<egui::Event>) -> (Vec<Action>, egui::FullOutput) {
            let mut actions = Vec::new();
            let output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, self.size)),
                    time: Some(self.frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    actions = pane_ui(
                        ui,
                        &self.theme,
                        &mut self.pane,
                        &self.history,
                        &mut self.assets,
                        true,
                        false,
                    );
                },
            );
            self.frame += 1;
            (actions, output)
        }
        fn cached_view(&self) -> Arc<HomeView> {
            self.ctx
                .data(|data| {
                    data.get_temp::<HomeKeyboard>(
                        crate::keyboard::listing_id(self.pane.id).with("home-keyboard"),
                    )
                })
                .unwrap()
                .view
                .unwrap()
        }
        fn probe(&self) -> HomeProbe {
            self.ctx
                .data(|data| data.get_temp::<HomeProbe>(Id::new((self.pane.id, "home-probe"))))
                .unwrap()
        }
        fn enable_probe(&self) {
            self.ctx
                .data_mut(|data| data.insert_temp(Id::new("benchmark-probe-layout"), true));
        }
        fn click(&mut self, at: egui::Pos2) {
            self.draw(vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            self.draw(vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
    }

    #[test]
    fn home_and_folder_render_the_same_header_and_footer_surfaces() {
        for (width, font_size, light) in [
            (360.0, 12.0, false),
            (1000.0, 12.0, false),
            (720.0, 18.0, true),
        ] {
            let mut home = Fixture::new();
            let mut folder = Fixture::new();
            for f in [&mut home, &mut folder] {
                f.size = vec2(width, 600.0);
                f.theme.font_size = font_size;
                if light {
                    f.theme.set_color_mode(foldex_ui::ColorMode::Light, None);
                }
                f.theme.apply(&f.ctx);
            }
            home.enable_probe();
            home.draw(vec![]);
            let (_, mut home_output) = home.render(vec![]);
            home_output.textures_delta.clear();
            folder.pane.back();
            assert!(!folder.pane.is_home());
            let mut folder_output = folder.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, folder.size)),
                    ..Default::default()
                },
                |ui| {
                    folder.pane.ui(ui, &folder.theme, true, false, None);
                },
            );
            let toolbar =
                Rect::from_min_size(egui::Pos2::ZERO, vec2(width, home.theme.metrics().toolbar));
            folder_output.textures_delta.clear();
            for output in [&home_output, &folder_output] {
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Rect(rect) if rect.rect == toolbar && rect.fill == home.theme.bg()
                )));
                assert!(
                    !output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Rect(rect) if rect.fill == Theme::color(home.theme.surface)
                            && rect.rect.intersect(toolbar).is_positive()
                    )),
                    "a separate address surface must not return on Home"
                );
            }
            for key in ["more", "inspector"] {
                assert_eq!(
                    home.ctx
                        .read_response(Id::new((home.pane.id, key)))
                        .unwrap()
                        .rect,
                    folder
                        .ctx
                        .read_response(Id::new((folder.pane.id, key)))
                        .unwrap()
                        .rect,
                );
            }
            assert_eq!(
                home.pane.benchmark_footer_view_rect(),
                folder.pane.benchmark_footer_view_rect()
            );
            let home_filter = home.pane.benchmark_footer_search_rect();
            let folder_filter = folder.pane.benchmark_footer_search_rect();
            assert_eq!(home_filter.left(), folder_filter.left());
            assert_eq!(home_filter.y_range(), folder_filter.y_range());
            let search_surface = |output: &egui::FullOutput, filter: Rect| {
                output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect)
                            if rect.fill == Theme::color(home.theme.surface)
                                && rect.rect.contains_rect(filter) =>
                        {
                            Some((rect.rect.y_range(), rect.corner_radius))
                        }
                        _ => None,
                    })
                    .unwrap()
            };
            assert_eq!(
                search_surface(&home_output, home_filter),
                search_surface(&folder_output, folder_filter)
            );
            home_output.drop_without_applying_deltas();
            folder_output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn home_section_spacing_keeps_recent_names_and_dimmed_paths_in_normal_file_rows() {
        let mut f = Fixture::new();
        f.pane.filter = "recent".into();
        f.ctx
            .data_mut(|data| data.insert_temp(Id::new("benchmark-probe-layout"), true));
        f.draw(vec![]);
        let (_, output) = f.render(vec![]);
        let probe = f
            .ctx
            .data(|data| data.get_temp::<HomeProbe>(Id::new((f.pane.id, "home-probe"))))
            .unwrap();
        let row = probe.rows[0];
        assert_eq!(row.height(), f.theme.file_row_height());
        assert!(row.left() - probe.body.left() >= 16.0 * f.theme.metrics().scale);
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        let name = text
            .iter()
            .find(|text| text.galley.text() == "recent_00.txt")
            .unwrap();
        let parent_label = parent_label(&f.history.rows[0]);
        let parent = text
            .iter()
            .find(|text| text.galley.text() == parent_label)
            .unwrap();
        assert!((name.pos.y - parent.pos.y).abs() < 0.51);
        assert!(parent.pos.x > name.pos.x + name.galley.size().x);
        assert_eq!(parent.fallback_color, f.theme.dim());
        for title in ["Recommended", "Recent"] {
            let heading = text
                .iter()
                .find(|text| text.galley.text() == title)
                .unwrap();
            assert!(heading.galley.job.sections[0].format.font_id.size > f.theme.font_size);
            assert!(output.shapes.iter().any(|shape| match &shape.shape {
                egui::Shape::LineSegment { points, stroke } => {
                    (points[0].x - heading.pos.x).abs() < 0.51
                        && points[0].y > heading.pos.y + heading.galley.size().y
                        && points[0].y < heading.pos.y + 36.0 * f.theme.metrics().scale
                        && (points[1].x - points[0].x - row.width()).abs() < 0.51
                        && stroke.color.a() < f.theme.line().a()
                }
                _ => false,
            }));
        }
        output.drop_without_applying_deltas();
    }

    #[test]
    fn home_shared_footer_is_pinned_with_no_toolbar_filter_or_expanded_search() {
        for (width, height, font_size, light) in [
            (180.0, 220.0, 14.0, false),
            (260.0, 300.0, 24.0, true),
            (360.0, 500.0, 24.0, true),
            (800.0, 500.0, 14.0, false),
        ] {
            let mut f = Fixture::new();
            f.size = vec2(width, height);
            f.theme.font_size = font_size;
            if light {
                f.theme.set_color_mode(foldex_ui::ColorMode::Light, None);
            }
            f.theme.apply(&f.ctx);
            f.enable_probe();
            f.pane.filter = "recent".into();
            f.draw(vec![]);
            let (_, output) = f.render(vec![]);
            let probe = f.probe();
            let filter = f.pane.benchmark_footer_search_rect();
            let view = f.pane.benchmark_footer_view_rect();
            assert!(probe.footer.contains_rect(filter), "{width}/{font_size}");
            assert!(probe.footer.contains_rect(view), "{width}/{font_size}");
            assert!(probe.body.bottom() <= probe.footer.top());
            assert!(probe.address.bottom() <= probe.body.top());
            assert!(filter.right() <= f.pane.benchmark_footer_kind_rect(true).left());
            assert!(!f.pane.benchmark_footer_expand_rect().is_positive());
            assert!(
                f.ctx
                    .read_response(Id::new((f.pane.id, "expand-filter")))
                    .is_none()
            );
            let editor = f.ctx.read_response(Id::new((f.pane.id, "filter"))).unwrap();
            assert!(probe.footer.contains_rect(editor.rect));
            assert!(output.shapes.iter().all(|shape| match &shape.shape {
                egui::Shape::Text(text) => text.galley.text() != "Filter recent…",
                _ => true,
            }));
            output.drop_without_applying_deltas();

            crate::keyboard::request_id(&f.ctx, crate::keyboard::listing_id(f.pane.id));
            f.draw(vec![key(Key::End, egui::Modifiers::NONE)]);
            for _ in 0..3 {
                f.draw(vec![]);
            }
            assert_eq!(f.probe().footer, probe.footer);
            assert_eq!(f.pane.benchmark_footer_search_rect(), filter);
        }
    }

    #[test]
    fn home_shared_clear_only_appears_for_a_query_and_keeps_selected_path() {
        let mut f = Fixture::new();
        f.enable_probe();
        let clear_id = Id::new((f.pane.id, "filter")).with("clear");
        f.draw(vec![]);
        f.draw(vec![]);
        assert!(f.ctx.read_response(clear_id).is_none());
        let selected = f.history.rows[4].entry.path.clone();
        f.pane.home_selected = Some(selected.clone());
        f.pane.filter = "recent".into();
        f.draw(vec![]);
        f.draw(vec![]);
        let clear = f.ctx.read_response(clear_id).unwrap();
        assert!(f.probe().footer.contains_rect(clear.rect));
        f.click(clear.rect.center());
        f.draw(vec![]);
        assert!(f.pane.filter.is_empty());
        assert_eq!(f.pane.home_selected, Some(selected));
        assert!(!f.cached_view().cards.is_empty());
        // read_response intentionally falls back to the previous pass. Wait
        // until both retained passes describe the now-empty query.
        f.draw(vec![]);
        assert!(f.ctx.read_response(clear_id).is_none());
        assert_eq!(
            f.ctx.memory(|memory| memory.focused()),
            Some(Id::new((f.pane.id, "filter")))
        );
    }

    #[test]
    fn home_percentage_hover_stays_closed_and_click_menu_returns_keyboard_focus() {
        let mut f = Fixture::new();
        f.draw(vec![]);
        let trigger = Id::new((f.pane.id, "view-button"));
        let center = f.pane.benchmark_footer_view_rect().center();
        f.draw(vec![egui::Event::PointerMoved(center)]);
        for _ in 0..4 {
            f.draw(vec![]);
        }
        assert!(!f.pane.benchmark_footer_view_open());
        f.click(center);
        assert!(f.pane.benchmark_footer_view_open());
        f.draw(vec![key(Key::Escape, egui::Modifiers::NONE)]);
        assert!(!f.pane.benchmark_footer_view_open());
        assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(trigger));
    }

    #[test]
    fn home_kind_toggles_keep_available_counts_and_filter_both_sections_without_folder_work() {
        let mut f = Fixture::new();
        f.enable_probe();
        for (index, row) in f.history.rows.iter_mut().enumerate() {
            row.is_dir = index % 2 == 0;
        }
        f.draw(vec![]);
        let epoch = f.pane.navigation_epoch();
        assert_eq!(f.cached_view().counts, (40, 40));
        f.click(f.pane.benchmark_footer_kind_rect(true).center());
        f.draw(vec![]);
        assert_eq!(f.pane.query.kind, foldex_core::KindFilter::Files);
        let files = f.cached_view();
        assert_eq!(files.counts, (40, 40));
        assert_eq!(files.rows.len(), 40);
        assert!(
            files
                .rows
                .iter()
                .chain(&files.cards)
                .all(|&index| !f.history.rows[index].is_dir)
        );
        f.click(f.pane.benchmark_footer_kind_rect(false).center());
        f.draw(vec![]);
        assert_eq!(f.pane.query.kind, foldex_core::KindFilter::None);
        assert!(f.cached_view().rows.is_empty());
        assert!(f.cached_view().cards.is_empty());
        assert_eq!(f.cached_view().counts, (40, 40));
        f.click(f.pane.benchmark_footer_kind_rect(true).center());
        f.draw(vec![]);
        assert_eq!(f.pane.query.kind, foldex_core::KindFilter::Folders);
        assert_eq!(f.cached_view().rows.len(), 40);
        assert_eq!(f.pane.navigation_epoch(), epoch);
        assert!(f.pane.snapshot.is_none());
        assert!(!f.pane.loading && !f.pane.scanning && !f.pane.recursive);
    }

    #[test]
    fn home_recent_zoom_modes_reflow_and_keyboard_reveal_uses_the_painted_grid() {
        let mut f = Fixture::new();
        f.enable_probe();
        f.size = vec2(1000.0, 600.0);
        f.pane.filter = "recent".into();
        let first = f.history.rows[0].entry.path.clone();
        f.pane.home_selected = Some(first.clone());
        for zoom in [0.0, 0.1, 0.2, 0.3, 0.5, 0.65, 0.8, 1.0] {
            f.pane.set_zoom(zoom);
            f.draw(vec![]);
            crate::keyboard::request_id(&f.ctx, crate::keyboard::listing_id(f.pane.id));
            f.draw(vec![key(Key::Home, egui::Modifiers::NONE)]);
            f.draw(vec![]);
            let probe = f.probe();
            assert_eq!(probe.mode, ViewMode::from_zoom(zoom));
            assert_eq!(probe.recent, f.history.rows.len());
            let row = probe.rows[0];
            if probe.mode.is_grid() {
                assert!(row.height() > f.theme.file_row_height() * 2.0);
            } else {
                assert_eq!(row.height(), f.theme.file_row_height());
            }
            if probe.columns > 1 {
                assert!(probe.rows[1].left() > row.left());
                assert_eq!(probe.rows[1].top(), row.top());
            }
            f.draw(vec![key(Key::ArrowDown, egui::Modifiers::NONE)]);
            let keyboard = f
                .ctx
                .data(|data| {
                    data.get_temp::<HomeKeyboard>(
                        crate::keyboard::listing_id(f.pane.id).with("home-keyboard"),
                    )
                })
                .unwrap();
            assert_eq!(
                keyboard.focused.unwrap().path,
                f.history.rows[probe.columns].entry.path
            );
            assert_eq!(f.pane.home_selected, Some(first.clone()));
            f.draw(vec![key(Key::End, egui::Modifiers::NONE)]);
            for _ in 0..4 {
                f.draw(vec![]);
            }
            let end = f.probe();
            let keyboard = f
                .ctx
                .data(|data| {
                    data.get_temp::<HomeKeyboard>(
                        crate::keyboard::listing_id(f.pane.id).with("home-keyboard"),
                    )
                })
                .unwrap();
            assert!(
                end.focused_visible,
                "zoom {zoom}, columns {}, rows {:?}, body {:?}, focused {:?}, owner {:?}, offset {:?}",
                end.columns,
                end.rows,
                end.body,
                keyboard.focused,
                f.ctx.memory(|memory| memory.focused()),
                end.scroll_id
                    .and_then(|id| egui::scroll_area::State::load(&f.ctx, id))
                    .map(|state| state.offset)
            );
            let actions = f.draw(vec![key(Key::F10, egui::Modifiers::SHIFT)]);
            assert!(matches!(actions.as_slice(), [Action::Context { paths, .. }]
                if paths == &[f.history.rows.last().unwrap().entry.path.clone()]));
        }
    }

    #[test]
    fn wide_home_grid_paints_every_visible_item_while_native_requests_remain_bounded() {
        let mut f = Fixture::new();
        f.enable_probe();
        f.size = vec2(4000.0, 2400.0);
        let template = f.history.rows[0].clone();
        f.history.rows = (0..512)
            .map(|index| {
                let mut row = template.clone();
                row.entry.path = PathBuf::from(format!("C:/home/recent_{index:03}.txt"));
                row
            })
            .collect();
        f.pane.filter = "recent".into();
        f.pane.set_zoom(0.3);
        f.draw(vec![]);
        f.draw(vec![]);
        let probe = f.probe();
        assert_eq!(probe.recent, 512);
        assert_eq!(probe.rows.len(), 512);
        assert_eq!(probe.asset_requests, foldex_assets::MAX_REQUESTS_PER_VIEW);
    }

    #[test]
    fn home_cut_snapshot_dims_names_in_cards_and_rows_but_preserves_selection() {
        let mut f = Fixture::new();
        f.history.rows.truncate(3);
        // Absolute native fixture paths on every platform, with no file access.
        for (index, row) in f.history.rows.iter_mut().enumerate() {
            row.entry.path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/home-cut-fixture")
                .join(format!("recent_{index:02}.txt"));
        }
        f.pane.home_selected = Some(f.history.rows[0].entry.path.clone());
        f.draw(vec![]);
        let (_, before) = f.render(vec![]);
        f.assets
            .benchmark_set_cut_paths(&[f.history.rows[0].entry.path.clone()])
            .unwrap();
        let (_, after) = f.render(vec![]);
        let texts = |output: &egui::FullOutput, name: &str| {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == name => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for (name, opacity) in [("recent_00.txt", 0.5), ("recent_01.txt", 1.0)] {
            let plain = texts(&before, name);
            let cut = texts(&after, name);
            assert!(!plain.is_empty());
            assert_eq!(plain.len(), cut.len());
            for (plain, cut) in plain.iter().zip(&cut) {
                assert_eq!(plain.pos, cut.pos);
                assert_eq!(
                    cut.fallback_color,
                    plain.fallback_color.gamma_multiply(opacity)
                );
                let plain_fallback = plain.fallback_color;
                let cut_fallback = cut.fallback_color;
                for (plain, cut) in plain.galley.rows.iter().zip(&cut.galley.rows) {
                    for (plain, cut) in plain
                        .visuals
                        .mesh
                        .vertices
                        .iter()
                        .zip(&cut.visuals.mesh.vertices)
                    {
                        // Uncolored galleys retain egui's PLACEHOLDER sentinel.
                        // Painter opacity dims their fallback, which tessellation
                        // substitutes for the sentinel when drawing each glyph.
                        let resolved = |color, fallback| {
                            if color == egui::Color32::PLACEHOLDER {
                                fallback
                            } else {
                                color
                            }
                        };
                        assert_eq!(
                            resolved(cut.color, cut_fallback),
                            resolved(plain.color, plain_fallback).gamma_multiply(opacity),
                        );
                    }
                }
            }
        }
        let selection = |output: &egui::FullOutput| {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.fill == Theme::color(f.theme.selection)
                            || rect.stroke.color == f.theme.accent() =>
                    {
                        Some(rect.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let plain = selection(&before);
        assert!(!plain.is_empty());
        assert_eq!(plain, selection(&after));
        before.drop_without_applying_deltas();
        after.drop_without_applying_deltas();
    }

    #[test]
    fn repeated_home_frames_reuse_labels_and_filter_results_until_their_sources_change() {
        let mut f = Fixture::new();
        let selected = f.history.rows[4].entry.path.clone();
        f.pane.home_selected = Some(selected.clone());
        f.draw(vec![]);
        let labels = HomeLabels::get(&f.ctx, &f.history);
        let initial_view = f.cached_view();
        f.draw(vec![]);
        assert!(Arc::ptr_eq(&initial_view, &f.cached_view()));
        assert!(Arc::ptr_eq(&labels, &HomeLabels::get(&f.ctx, &f.history)));

        f.pane.filter = "recent_7".into();
        f.draw(vec![]);
        let filtered = f.cached_view();
        assert_eq!(filtered.rows, (70..80).collect::<Vec<_>>());
        assert!(filtered.cards.is_empty());
        assert!(!Arc::ptr_eq(&initial_view, &filtered));
        assert!(Arc::ptr_eq(&labels, &HomeLabels::get(&f.ctx, &f.history)));
        assert_eq!(f.pane.home_selected, Some(selected));
        f.draw(vec![]);
        assert!(Arc::ptr_eq(&filtered, &f.cached_view()));

        f.history.rows.reverse();
        f.history.revision += 1;
        f.draw(vec![]);
        let updated = HomeLabels::get(&f.ctx, &f.history);
        assert!(!Arc::ptr_eq(&labels, &updated));
        assert_eq!(updated.rows[0].name, "recent_79.txt");
        assert_eq!(f.cached_view().rows, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn recommendation_cache_expires_at_the_exact_frequency_boundary_and_on_clock_reversal() {
        let mut f = Fixture::new();
        let at = 4_000_000;
        f.history.rows.truncate(2);
        f.history.rows[0].entry.last_used = at - 30 * 86400;
        f.history.rows[0].entry.uses = 3;
        f.history.rows[1].entry.last_used = at;
        let labels = HomeLabels::get(&f.ctx, &f.history);
        let all = foldex_core::KindFilter::All;
        let view = HomeView::new(&f.history, &labels, "", all, 4, at);
        assert_eq!(view.cards, [0, 1]);
        assert!(view.matches(&f.history, "", all, 4, at));
        assert!(!view.matches(&f.history, "", all, 4, at - 1));
        assert!(!view.matches(&f.history, "", all, 4, at + 1));
        assert!(!view.matches(&f.history, "", all, 2, at));
        let expired = HomeView::new(&f.history, &labels, "", all, 4, at + 1);
        assert_eq!(expired.cards, [1, 0]);
    }

    #[test]
    fn recommended_and_recent_navigation_has_bounded_edges_and_page_steps() {
        assert_eq!(home_step(1, 4, 80, 1, Key::ArrowRight, 10), 2);
        assert_eq!(home_step(3, 4, 80, 1, Key::ArrowRight, 10), 3);
        assert_eq!(home_step(2, 4, 80, 1, Key::ArrowDown, 10), 4);
        assert_eq!(home_step(4, 4, 80, 1, Key::ArrowUp, 10), 3);
        assert_eq!(home_step(10, 4, 80, 1, Key::PageDown, 10), 20);
        assert_eq!(home_step(80, 4, 80, 1, Key::End, 10), 83);
        assert_eq!(home_step(0, 0, 0, 1, Key::End, 10), 0);
        assert_eq!(home_step(5, 4, 80, 3, Key::ArrowDown, 9), 8);
        assert_eq!(home_step(8, 4, 80, 3, Key::ArrowUp, 9), 5);
        assert_eq!(home_step(5, 4, 80, 3, Key::ArrowUp, 9), 1);
        assert_eq!(home_step(4, 4, 80, 3, Key::ArrowLeft, 9), 4);
        assert_eq!(home_step(83, 4, 80, 3, Key::ArrowDown, 9), 83);
    }

    #[test]
    fn home_filter_tab_down_and_escape_preserve_query_and_selection() {
        for transfer in [Key::Tab, Key::ArrowDown, Key::Escape] {
            let mut f = Fixture::new();
            f.pane.filter = "recent".into();
            let selected = f.history.rows[4].entry.path.clone();
            f.pane.home_selected = Some(selected.clone());
            f.draw(vec![]);
            let filter = Id::new((f.pane.id, "filter"));
            crate::keyboard::request_id(&f.ctx, filter);
            f.draw(vec![]);
            f.draw(vec![key(transfer, egui::Modifiers::NONE)]);
            assert_eq!(f.pane.filter, "recent");
            assert_eq!(f.pane.home_selected, Some(selected.clone()));
            assert_eq!(
                f.ctx.memory(|memory| memory.focused()),
                Some(crate::keyboard::listing_id(f.pane.id))
            );
            f.draw(vec![key(Key::Tab, egui::Modifiers::SHIFT)]);
            assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(filter));
            assert_eq!(f.pane.filter, "recent");
        }
    }

    #[test]
    fn home_end_moves_focus_without_selection_and_enter_activates_that_item() {
        let mut f = Fixture::new();
        f.ctx
            .data_mut(|data| data.insert_temp(Id::new("benchmark-probe-layout"), true));
        f.pane.filter = "recent".into();
        let selected = f.history.rows[0].entry.path.clone();
        let last = f.history.rows.last().unwrap().entry.path.clone();
        f.pane.home_selected = Some(selected.clone());
        f.draw(vec![]);
        crate::keyboard::request_id(&f.ctx, crate::keyboard::listing_id(f.pane.id));
        f.draw(vec![key(Key::End, egui::Modifiers::NONE)]);
        assert_eq!(f.pane.home_selected, Some(selected));
        let probe_id = Id::new((f.pane.id, "home-probe"));
        // ScrollArea applies a reveal after its viewport callback. Allow its
        // stored offset and the virtual viewport to settle across UI passes.
        for _ in 0..4 {
            f.draw(vec![]);
            if f.ctx
                .data(|data| data.get_temp::<HomeProbe>(probe_id))
                .is_some_and(|probe| probe.focused_visible)
            {
                break;
            }
        }
        let probe = f
            .ctx
            .data(|data| data.get_temp::<HomeProbe>(probe_id))
            .unwrap();
        let keyboard = f
            .ctx
            .data(|data| {
                data.get_temp::<HomeKeyboard>(
                    crate::keyboard::listing_id(f.pane.id).with("home-keyboard"),
                )
            })
            .unwrap();
        assert_eq!(
            keyboard.focused.as_ref().map(|item| &item.path),
            Some(&last)
        );
        assert!(
            probe.focused_visible,
            "End must paint the focused last row inside the scroll viewport within four follow-up frames"
        );
        assert_eq!(
            f.ctx.memory(|memory| memory.focused()),
            Some(crate::keyboard::listing_id(f.pane.id))
        );
        let actions = f.draw(vec![key(Key::Enter, egui::Modifiers::NONE)]);
        assert!(matches!(actions.as_slice(), [Action::Open(path)] if path == &last));
    }

    #[test]
    fn home_filter_keeps_composition_and_modified_arrows_in_the_editor() {
        let mut f = Fixture::new();
        f.draw(vec![]);
        let filter = Id::new((f.pane.id, "filter"));
        crate::keyboard::request_id(&f.ctx, filter);
        f.draw(vec![]);
        f.draw(vec![
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "に".into(),
                active_range_chars: None,
            }),
            key(Key::ArrowDown, egui::Modifiers::NONE),
        ]);
        assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(filter));
        f.draw(vec![key(
            Key::ArrowDown,
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        )]);
        assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(filter));
        f.draw(vec![key(Key::Tab, egui::Modifiers::NONE)]);
        assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(filter));
    }

    #[test]
    fn home_filter_delivers_text_before_transferring_focus_in_the_same_frame() {
        let mut f = Fixture::new();
        f.draw(vec![]);
        let filter = Id::new((f.pane.id, "filter"));
        crate::keyboard::request_id(&f.ctx, filter);
        f.draw(vec![]);
        f.draw(vec![
            egui::Event::Text("recent".into()),
            key(Key::Tab, egui::Modifiers::NONE),
        ]);
        assert_eq!(f.pane.filter, "recent");
        assert_eq!(
            f.ctx.memory(|memory| memory.focused()),
            Some(crate::keyboard::listing_id(f.pane.id))
        );
    }

    #[test]
    fn home_navigation_batch_activates_the_item_reached_in_event_order() {
        let mut f = Fixture::new();
        f.pane.filter = "recent".into();
        f.draw(vec![]);
        crate::keyboard::request_id(&f.ctx, crate::keyboard::listing_id(f.pane.id));
        let expected = f.history.rows[f.history.rows.len() - 2].entry.path.clone();
        let actions = f.draw(vec![
            key(Key::End, egui::Modifiers::NONE),
            key(Key::ArrowUp, egui::Modifiers::NONE),
            key(Key::Enter, egui::Modifiers::NONE),
        ]);
        assert!(matches!(actions.as_slice(), [Action::Open(path)] if path == &expected));
    }

    #[test]
    fn home_shift_f10_targets_the_focused_item_without_changing_selection() {
        let mut f = Fixture::new();
        f.pane.filter = "recent".into();
        let selected = f.history.rows[0].entry.path.clone();
        let last = f.history.rows.last().unwrap().entry.path.clone();
        f.pane.home_selected = Some(selected.clone());
        f.draw(vec![]);
        crate::keyboard::request_id(&f.ctx, crate::keyboard::listing_id(f.pane.id));
        f.draw(vec![key(Key::End, egui::Modifiers::NONE)]);
        f.draw(vec![]);
        let actions = f.draw(vec![key(Key::F10, egui::Modifiers::SHIFT)]);
        assert!(
            matches!(actions.as_slice(), [Action::Context { paths, background: false, .. }] if paths == &[last])
        );
        assert_eq!(f.pane.home_selected, Some(selected));
    }
}
