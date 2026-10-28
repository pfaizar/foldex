//! Details columns share one geometry for headers, hit targets, icons and rows.
use super::*;
use foldex_core::{Entry, TypeFilter};
use foldex_folder_stats::State;
use serde::{Deserialize, Serialize};

mod worker;
use worker::{ColumnService, Submission, ViewInput, ViewOutput};

/// A data revision, independent of model publication and layout/query epochs.
/// Access time is deliberately excluded: reading metadata/statistics must not
/// invalidate the very folder values being presented. Explicit refresh renews it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct EntryRevision {
    availability: foldex_fs::capabilities::Availability,
    is_dir: bool,
    symlink: bool,
    hidden: bool,
    metadata_known: bool,
    size: u64,
    modified_secs: i64,
    created_secs: Option<i64>,
}
impl EntryRevision {
    pub(super) fn of(entry: &Entry) -> Self {
        Self {
            availability: entry.availability,
            is_dir: entry.is_dir,
            symlink: entry.symlink,
            hidden: entry.hidden,
            metadata_known: entry.metadata_known,
            size: entry.size,
            modified_secs: entry.modified_secs,
            created_secs: entry.created_secs,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ValueKey {
    path: PathBuf,
    revision: EntryRevision,
}
impl ValueKey {
    fn of(snapshot: &Snapshot, entry: &Entry) -> Self {
        Self {
            path: entry.path(snapshot),
            revision: EntryRevision::of(entry),
        }
    }
}

/// At most the previous viewport (128 rows) is retained. Match exact raw paths
/// and metadata revisions, never row positions or lossy display names.
fn remap_rows(rows: &mut HashMap<u32, worker::RowValues>, snapshot: &Snapshot, visible: &[u32]) {
    let mut previous = std::mem::take(rows)
        .into_values()
        .filter_map(|mut values| values.key.take().map(|key| (key, values)))
        .collect::<HashMap<_, _>>();
    for &index in visible.iter().take(128) {
        let Some(entry) = snapshot.entries.get(index as usize) else {
            continue;
        };
        let key = ValueKey::of(snapshot, entry);
        if let Some(mut values) = previous.remove(&key) {
            values.key = Some(key);
            rows.insert(index, values);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Column {
    Name,
    Type,
    Modified,
    Folders,
    Files,
    Size,
    TotalItems,
    Created,
    Accessed,
    VolumeUsage,
    FolderUsage,
    OriginalLocation,
    DateDeleted,
}
impl Column {
    pub(crate) const ALL: [Self; 13] = [
        Self::Name,
        Self::Type,
        Self::Size,
        Self::TotalItems,
        Self::Folders,
        Self::Files,
        Self::Created,
        Self::Modified,
        Self::Accessed,
        Self::VolumeUsage,
        Self::FolderUsage,
        Self::OriginalLocation,
        Self::DateDeleted,
    ];
    pub(crate) fn supported(self, recycle: bool) -> bool {
        if recycle {
            matches!(
                self,
                Self::Name | Self::Type | Self::Size | Self::OriginalLocation | Self::DateDeleted
            )
        } else {
            !matches!(self, Self::OriginalLocation | Self::DateDeleted)
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Type => "Type",
            Self::Size => "Size",
            Self::Modified => "Date Modified",
            Self::Created => "Date Created",
            Self::Accessed => "Date Accessed",
            Self::TotalItems => "Total Items",
            Self::Folders => "Folders",
            Self::Files => "Files",
            Self::VolumeUsage => "Volume Usage",
            Self::FolderUsage => "Folder Usage",
            Self::OriginalLocation => "Original Location",
            Self::DateDeleted => "Date Deleted",
        }
    }
    fn min_width(self) -> f32 {
        match self {
            Self::Name => 132.0,
            Self::Type => 86.0,
            Self::Created | Self::Modified | Self::Accessed | Self::DateDeleted => 126.0,
            Self::OriginalLocation => 136.0,
            Self::Folders | Self::Files => 62.0,
            Self::Size => 82.0,
            Self::TotalItems => 86.0,
            Self::VolumeUsage | Self::FolderUsage => 104.0,
        }
    }
    fn default_width(self) -> f32 {
        match self {
            Self::Name => 320.0,
            Self::Type => 128.0,
            Self::Created | Self::Modified | Self::Accessed | Self::DateDeleted => 154.0,
            Self::OriginalLocation => 220.0,
            Self::Folders | Self::Files => 68.0,
            Self::Size => 94.0,
            _ => 116.0,
        }
    }
    pub(super) fn numeric(self) -> bool {
        matches!(
            self,
            Self::Size
                | Self::Folders
                | Self::Files
                | Self::TotalItems
                | Self::VolumeUsage
                | Self::FolderUsage
        )
    }
    fn core_sort(self) -> Option<SortColumn> {
        match self {
            Self::Name => Some(SortColumn::Name),
            Self::Type => Some(SortColumn::Kind),
            Self::Modified => Some(SortColumn::Modified),
            Self::Created => Some(SortColumn::Created),
            Self::Accessed => Some(SortColumn::Accessed),
            _ => None,
        }
    }
    pub(super) fn needs_folders(self) -> bool {
        self.numeric()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ColumnSetting {
    pub column: Column,
    /// Logical pixels, independent of the window's DPI and UI scale.
    pub width: f32,
    pub visible: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredColumns {
    pub columns: Vec<ColumnSetting>,
    pub sort: Option<(Column, bool)>,
    #[serde(default = "default_name_auto")]
    pub name_auto: bool,
}
fn default_name_auto() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredTypeFilter {
    pub include_folders: bool,
    #[serde(default)]
    pub exclude: bool,
    pub extensions: Vec<String>,
}
impl StoredTypeFilter {
    pub(crate) fn capture(filter: &TypeFilter) -> Self {
        Self {
            include_folders: filter.include_folders,
            exclude: filter.exclude,
            extensions: filter.extensions.clone(),
        }
    }
    pub(crate) fn restore(&self) -> TypeFilter {
        let mut extensions = self
            .extensions
            .iter()
            .filter(|value| value.len() <= 1024)
            .take(256)
            .map(|value| value.to_lowercase())
            .collect::<Vec<_>>();
        extensions.sort();
        extensions.dedup();
        TypeFilter {
            include_folders: self.include_folders,
            exclude: self.exclude,
            extensions,
        }
    }
}
impl Default for StoredColumns {
    fn default() -> Self {
        let defaults = [
            Column::Name,
            Column::Type,
            Column::Modified,
            Column::Folders,
            Column::Files,
            Column::Size,
        ];
        Self {
            columns: defaults
                .into_iter()
                .chain(Column::ALL.into_iter().filter(|c| !defaults.contains(c)))
                .map(|column| ColumnSetting {
                    column,
                    width: column.default_width(),
                    visible: defaults.contains(&column),
                })
                .collect(),
            sort: None,
            name_auto: true,
        }
    }
}
impl StoredColumns {
    pub(crate) fn recycle() -> Self {
        let visible = [
            Column::Name,
            Column::OriginalLocation,
            Column::DateDeleted,
            Column::Type,
            Column::Size,
        ];
        Self {
            columns: visible
                .into_iter()
                .chain(
                    Column::ALL
                        .into_iter()
                        .filter(|column| !visible.contains(column)),
                )
                .map(|column| ColumnSetting {
                    column,
                    width: column.default_width(),
                    visible: visible.contains(&column),
                })
                .collect(),
            sort: Some((Column::DateDeleted, true)),
            name_auto: true,
        }
    }
    pub(crate) fn sanitize(&mut self) {
        let mut seen = Vec::new();
        self.columns.retain(|setting| {
            if seen.contains(&setting.column) {
                false
            } else {
                seen.push(setting.column);
                true
            }
        });
        for column in Column::ALL {
            if !seen.contains(&column) {
                self.columns.push(ColumnSetting {
                    column,
                    width: column.default_width(),
                    visible: column == Column::Name,
                });
            }
        }
        for setting in &mut self.columns {
            setting.width = if setting.width.is_finite() {
                setting.width.clamp(setting.column.min_width(), 1200.0)
            } else {
                setting.column.default_width()
            };
            if setting.column == Column::Name {
                setting.visible = true;
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ColumnCell {
    pub column: Column,
    pub rect: Rect,
}
#[derive(Clone, Debug)]
pub(super) struct DetailsLayout {
    pub cells: Vec<ColumnCell>,
    name: Rect,
}

/// A single fixed status slot serves the sort arrow or calculation spinner.
/// Busy/sorted state never changes the label's position or available width.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HeaderContent {
    hit: Rect,
    label: Rect,
    status: Pos2,
}
impl HeaderContent {
    fn new(column: Column, rect: Rect, scale: f32) -> Self {
        let filter = if column == Column::Type {
            25.0 * scale
        } else {
            0.0
        };
        let right = (rect.right() - filter - 3.0 * scale).max(rect.left());
        let hit = Rect::from_min_max(
            pos2(
                rect.left()
                    + if column == Column::Name {
                        28.0 * scale
                    } else {
                        0.0
                    },
                rect.top(),
            ),
            pos2(right, rect.bottom()),
        );
        let left = (hit.left() + foldex_ui::components::INSET * scale).min(right);
        Self {
            hit,
            label: Rect::from_min_max(
                pos2(left, rect.top()),
                pos2((right - 12.0 * scale).max(left), rect.bottom()),
            ),
            status: pos2(right - 5.0 * scale, rect.center().y),
        }
    }
    fn paint(
        &self,
        p: &egui::Painter,
        t: &Theme,
        column: Column,
        sorted: bool,
        descending: bool,
        calculating: bool,
    ) {
        foldex_ui::components::elided_label(
            p,
            self.label,
            t,
            column.label(),
            if sorted { t.fg() } else { t.dim() },
            false,
        );
        if calculating && column.needs_folders() {
            paint_spinner(
                p,
                self.status,
                10.0 * t.metrics().scale,
                t.dim(),
                t.duration() > 0.0,
            );
        } else if sorted {
            icon(
                p,
                self.status,
                if descending { Icon::Down } else { Icon::Up },
                t.dim(),
                9.0 * t.metrics().scale,
            );
        }
    }
}
impl DetailsLayout {
    pub(super) fn name_rect(&self) -> Rect {
        self.name
    }
    fn contains(&self, column: Column) -> bool {
        self.cells.iter().any(|cell| cell.column == column)
    }
    pub(super) fn new(
        settings: &StoredColumns,
        header: Rect,
        scale: f32,
        name_fraction: f32,
    ) -> Self {
        let available = header.width().max(0.0) / scale;
        let mut visible = settings
            .columns
            .iter()
            .filter(|c| c.visible || c.column == Column::Name)
            .collect::<Vec<_>>();
        // Details stay legible in narrow splits. The checklist retains the user's
        // choices and these columns return as soon as their minimums fit again.
        for column in [
            Column::FolderUsage,
            Column::VolumeUsage,
            Column::Accessed,
            Column::Created,
            Column::TotalItems,
            Column::Type,
            Column::Folders,
            Column::Files,
            Column::Modified,
            Column::Size,
            Column::OriginalLocation,
            Column::DateDeleted,
        ] {
            if visible.iter().map(|c| c.column.min_width()).sum::<f32>() <= available {
                break;
            }
            visible.retain(|c| c.column != column);
        }
        let mut widths = visible
            .iter()
            .map(|c| c.width.max(c.column.min_width()))
            .collect::<Vec<_>>();
        if let Some(name) = visible.iter().position(|c| c.column == Column::Name) {
            let others = widths
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != name)
                .map(|(_, width)| *width)
                .sum::<f32>();
            if settings.name_auto {
                widths[name] = (available - others)
                    .max((available * name_fraction.clamp(0.15, 0.8)).min(320.0))
                    .max(Column::Name.min_width());
            }
            let excess = (widths.iter().sum::<f32>() - available).max(0.0);
            let shrinkable = widths
                .iter()
                .zip(&visible)
                .map(|(w, c)| (w - c.column.min_width()).max(0.0))
                .sum::<f32>();
            if excess > 0.0 && shrinkable > 0.0 {
                for (width, setting) in widths.iter_mut().zip(&visible) {
                    *width -= excess.min(shrinkable)
                        * (*width - setting.column.min_width()).max(0.0)
                        / shrinkable;
                }
            }
            let used = widths.iter().sum::<f32>();
            if settings.name_auto {
                widths[name] = (widths[name] + available - used).max(0.0);
            }
        }
        let mut x = header.left();
        let mut cells = Vec::new();
        let mut name = header;
        for (setting, width) in visible.into_iter().zip(widths) {
            let right = (x + width * scale).min(header.right()).max(x);
            let rect = Rect::from_min_max(pos2(x, header.top()), pos2(right, header.bottom()));
            if setting.column == Column::Name {
                name = rect;
            }
            cells.push(ColumnCell {
                column: setting.column,
                rect,
            });
            x = right;
        }
        Self { cells, name }
    }
}

pub(super) struct ColumnState {
    settings: StoredColumns,
    folder_settings: Option<StoredColumns>,
    menu: foldex_ui::components::MenuState,
    menu_column: Column,
    menu_anchor: Rect,
    types: foldex_ui::components::MenuState,
    types_anchor: Rect,
    service: Option<Arc<ColumnService>>,
    output: Arc<ViewOutput>,
    epoch: u64,
    pub(super) source: Option<(PathBuf, u64)>,
    revision: u64,
    base_indices: Arc<[u32]>,
    applied: Option<Arc<[u32]>>,
    pub(super) order_epoch: u64,
    retry_cancel: bool,
    view_id: Option<u64>,
    pub(super) refresh_pending: bool,
    controlled_pending: bool,
}
impl Default for ColumnState {
    fn default() -> Self {
        Self {
            settings: Default::default(),
            folder_settings: None,
            menu: Default::default(),
            menu_column: Column::Name,
            menu_anchor: Rect::ZERO,
            types: Default::default(),
            types_anchor: Rect::ZERO,
            service: None,
            output: Arc::default(),
            epoch: 1,
            source: None,
            revision: 0,
            base_indices: Arc::from([]),
            applied: None,
            order_epoch: 0,
            retry_cancel: false,
            view_id: None,
            refresh_pending: false,
            controlled_pending: false,
        }
    }
}
impl Drop for ColumnState {
    fn drop(&mut self) {
        if let (Some(service), Some(id)) = (&self.service, self.view_id) {
            service.cancel(id);
        }
    }
}
impl ColumnState {
    pub(super) fn enter_recycle(&mut self) {
        if self.folder_settings.is_some() {
            return;
        }
        let restored = self.settings.columns.iter().any(|setting| {
            setting.visible
                && matches!(
                    setting.column,
                    Column::OriginalLocation | Column::DateDeleted
                )
        });
        self.folder_settings = Some(if restored {
            StoredColumns::default()
        } else {
            self.settings.clone()
        });
        if !restored {
            self.settings = StoredColumns::recycle();
        }
    }
    pub(super) fn leave_recycle(&mut self) {
        if let Some(settings) = self.folder_settings.take() {
            self.settings = settings;
        }
    }
    pub(super) fn recycle_sort(&self) -> Option<(Column, bool)> {
        self.settings.sort
    }
    pub(super) fn same_presentation(&self, other: &Self) -> bool {
        self.settings.sort == other.settings.sort
    }
    pub(super) fn prime_from(
        &mut self,
        existing: &Self,
        path: PathBuf,
        navigation: u64,
        revision: u64,
        indices: Arc<[u32]>,
    ) {
        self.service = existing.service.clone();
        self.source = Some((path, navigation));
        self.epoch = existing.epoch;
        self.revision = revision;
        self.base_indices = indices;
        let mut output = (*existing.output).clone();
        output.epoch = self.epoch;
        output.revision = revision;
        output.data_revision = 0;
        output.order = None;
        output.remap = None;
        output.sort = self.settings.sort;
        self.output = Arc::new(output);
    }

    pub(super) fn enter_recursive(&mut self, query: &mut Query) {
        if let Some((column, descending)) = self.settings.sort.take() {
            query.sort = if matches!(
                column,
                Column::Size | Column::VolumeUsage | Column::FolderUsage
            ) {
                SortColumn::Size
            } else {
                SortColumn::Name
            };
            query.descending = descending && query.sort == SortColumn::Size;
        }
    }
    pub(super) fn has_popup(&self) -> bool {
        self.menu.active() || self.types.active()
    }
    pub(super) fn cancel(&mut self, id: u64) {
        self.retry_cancel = self
            .service
            .as_ref()
            .is_some_and(|service| !service.cancel(id));
    }
}

impl Pane {
    pub(crate) fn stored_columns(&self) -> StoredColumns {
        self.columns.settings.clone()
    }
    pub(crate) fn restore_columns(&mut self, mut settings: StoredColumns) {
        settings.sanitize();
        self.columns.settings = settings;
    }
    pub(crate) fn hash_columns(&self, h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        for column in &self.columns.settings.columns {
            column.column.hash(h);
            column.visible.hash(h);
            column.width.to_bits().hash(h);
        }
        self.columns.settings.sort.hash(h);
        self.columns.settings.name_auto.hash(h);
        if let Some(filter) = &self.query.type_filter {
            filter.include_folders.hash(h);
            filter.exclude.hash(h);
            filter.extensions.hash(h);
        }
        self.query.type_filter.is_some().hash(h);
    }

    pub(super) fn details_header(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        header: Rect,
        active: bool,
    ) -> DetailsLayout {
        let scale = t.metrics().scale;
        if !self.is_recycle_bin() {
            self.poll_column_output(ui.ctx());
        }
        let layout = DetailsLayout::new(&self.columns.settings, header, scale, self.column_name);
        if self.header_height <= 1.0 {
            return layout;
        }
        let opacity = (self.header_height / t.metrics().columns).clamp(0.0, 1.0);
        let content = Rect::from_min_size(header.min, vec2(header.width(), t.metrics().columns));
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt((self.id, "columns"))
                .max_rect(content),
        );
        child.set_clip_rect(header.intersect(ui.clip_rect()));
        child.multiply_opacity(opacity);
        if self.zoom >= 0.1 {
            foldex_ui::components::disable_interaction(&mut child);
        }
        for (index, cell) in layout.cells.iter().enumerate() {
            let column = cell.column;
            let r = Rect::from_min_max(cell.rect.min, pos2(cell.rect.right(), content.bottom()));
            let sorted = self.sorted_column() == column;
            let filter_width = if column == Column::Type {
                25.0 * scale
            } else {
                0.0
            };
            let heading = HeaderContent::new(column, r, scale);
            let response = foldex_ui::components::button_response(
                &child,
                heading.hit,
                Id::new((self.id, "column", column)),
            );
            crate::keyboard::register(
                &child,
                crate::keyboard::Region::Toolbar(self.id),
                response.id,
                heading.hit,
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    child.is_enabled(),
                    format!(
                        "Sort by {}{}",
                        column.label(),
                        if sorted {
                            if self.sort_descending() {
                                ", descending"
                            } else {
                                ", ascending"
                            }
                        } else {
                            ""
                        }
                    ),
                )
            });
            let hover = foldex_ui::animated_state(
                child.ctx(),
                response.id.with("hover"),
                response.hovered(),
                t,
                foldex_ui::motion::MotionProfile::Fast,
            );
            child.painter().rect_filled(
                r.shrink2(vec2(1.0, 2.0 * scale)),
                t.radius,
                Theme::color(t.hover).gamma_multiply(hover),
            );
            heading.paint(
                child.painter(),
                t,
                column,
                sorted,
                self.sort_descending(),
                self.columns.output.busy || self.columns.controlled_pending,
            );
            foldex_ui::components::focus_ring(
                &child,
                heading.hit,
                response.id,
                t,
                response.has_focus(),
            );
            if column == Column::Name {
                let check = Rect::from_center_size(
                    pos2(r.left() + 13.0 * scale, r.center().y),
                    vec2(24.0 * scale, r.height()),
                );
                let state = if self.selected.is_empty() {
                    0
                } else if self.selected.len() == self.view_len() {
                    1
                } else {
                    -1
                };
                let selection = foldex_ui::components::selection_checkbox(
                    &child,
                    check,
                    Id::new((self.id, "select-all")),
                    t,
                    state,
                )
                .on_hover_text(if state == 1 {
                    "Clear selection"
                } else {
                    "Select all files · Ctrl+A"
                });
                crate::keyboard::register(
                    &child,
                    crate::keyboard::Region::Toolbar(self.id),
                    selection.id,
                    check,
                );
                if selection.clicked() {
                    if state == 1 {
                        self.clear_selection();
                    } else {
                        self.select_all();
                    }
                }
            }
            if column == Column::Type {
                let funnel = Rect::from_min_max(pos2(r.right() - filter_width, r.top()), r.max);
                let response = foldex_ui::components::button_response(
                    &child,
                    funnel,
                    Id::new((self.id, "type-funnel")),
                )
                .on_hover_text("Filter file types");
                crate::keyboard::register(
                    &child,
                    crate::keyboard::Region::Toolbar(self.id),
                    response.id,
                    funnel,
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        child.is_enabled(),
                        "Filter file types",
                    )
                });
                let selected = self.query.type_filter.is_some();
                if response.hovered() || selected {
                    child.painter().rect_filled(
                        funnel.shrink2(vec2(1.0, 3.0)),
                        t.radius,
                        Theme::color(if selected { t.selection } else { t.hover }),
                    );
                }
                paint_funnel(
                    child.painter(),
                    funnel.center(),
                    11.0 * scale,
                    if selected { t.accent() } else { t.dim() },
                );
                foldex_ui::components::focus_ring(
                    &child,
                    funnel,
                    response.id,
                    t,
                    response.has_focus(),
                );
                let keyboard_open = response.has_focus()
                    && (Self::consume_navigation_key(
                        child.ctx(),
                        egui::Modifiers::NONE,
                        Key::ArrowDown,
                    ) || Self::consume_navigation_key(
                        child.ctx(),
                        egui::Modifiers::SHIFT,
                        Key::F10,
                    ));
                if response.clicked() || keyboard_open {
                    crate::keyboard::request_id(child.ctx(), response.id);
                    self.columns.menu.close();
                    self.columns.types_anchor = funnel;
                    self.columns.types.toggle(child.ctx());
                }
            }
            let keyboard_menu = response.has_focus()
                && (Self::consume_navigation_key(child.ctx(), egui::Modifiers::SHIFT, Key::F10)
                    || Self::consume_navigation_key(
                        child.ctx(),
                        egui::Modifiers::NONE,
                        Key::ArrowDown,
                    ));
            if response.secondary_clicked() || keyboard_menu {
                crate::keyboard::request_id(child.ctx(), response.id);
                self.columns.types.close();
                self.columns.menu_column = column;
                self.columns.menu_anchor = Rect::from_min_size(
                    (if response.secondary_clicked() {
                        response.interact_pointer_pos()
                    } else {
                        None
                    })
                    .unwrap_or(r.right_bottom()),
                    vec2(260.0 * scale, 0.0),
                );
                self.columns.menu.toggle(child.ctx());
            }
            if response.middle_clicked() && column != Column::Name {
                self.set_column_visible(column, false);
            }
            if response.clicked() {
                if self.recursive && column.numeric() && column != Column::Size {
                    response.on_hover_text(
                        "This calculated column can be sorted in an ordinary folder view",
                    );
                } else {
                    self.sort_details_column(column);
                }
            }
            if index > 0 {
                child.painter().line_segment(
                    [
                        r.left_top() + vec2(0.0, 7.0 * scale),
                        r.left_bottom() - vec2(0.0, 7.0 * scale),
                    ],
                    Stroke::new(1.0, t.line()),
                );
            }
        }
        for cell in &layout.cells {
            let divider = Rect::from_center_size(
                pos2(cell.rect.right(), content.center().y),
                vec2(7.0 * scale, content.height()),
            );
            let response = child
                .interact(
                    divider,
                    Id::new((self.id, "column-resize", cell.column)),
                    Sense::CLICK | Sense::DRAG,
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            let delta = foldex_ui::components::divider_delta(&child, divider, &response);
            if delta.x != 0.0 {
                if let Some(setting) = self
                    .columns
                    .settings
                    .columns
                    .iter_mut()
                    .find(|c| c.column == cell.column)
                {
                    setting.width = (cell.rect.width() / scale + delta.x / scale)
                        .clamp(cell.column.min_width(), 1200.0);
                    if cell.column == Column::Name {
                        self.column_name =
                            (setting.width * scale / header.width().max(1.0)).clamp(0.15, 0.8);
                        self.columns.settings.name_auto = false;
                    }
                }
            }
            if response.double_clicked() {
                self.fit_column(&child, t, cell.column, header.width());
            }
            foldex_ui::components::resize_divider(&child, divider, &response, t, true);
        }
        self.column_menu(ui, t, header.width(), active);
        self.type_menu(ui, t);
        ui.painter().line_segment(
            [header.left_bottom(), header.right_bottom()],
            Stroke::new(1.0, t.line().gamma_multiply(opacity)),
        );
        layout
    }

    fn sorted_column(&self) -> Column {
        self.columns
            .settings
            .sort
            .map(|v| v.0)
            .unwrap_or(match self.query.sort {
                SortColumn::Name => Column::Name,
                SortColumn::Kind => Column::Type,
                SortColumn::Size => Column::Size,
                SortColumn::Modified => Column::Modified,
                SortColumn::Created => Column::Created,
                SortColumn::Accessed => Column::Accessed,
            })
    }
    fn sort_descending(&self) -> bool {
        self.columns
            .settings
            .sort
            .map_or(self.query.descending, |v| v.1)
    }
    fn sort_details_column(&mut self, column: Column) {
        let descending = if self.sorted_column() == column {
            !self.sort_descending()
        } else {
            column.numeric()
                || matches!(
                    column,
                    Column::Created | Column::Modified | Column::Accessed | Column::DateDeleted
                )
        };
        self.columns.applied = None;
        if let Some(sort) = column.core_sort() {
            self.columns.settings.sort = None;
            self.query.sort = sort;
            self.query.descending = descending;
        } else if self.recursive {
            self.columns.settings.sort = None;
            self.query.sort = SortColumn::Size;
            self.query.descending = descending;
        } else {
            self.columns.settings.sort = Some((column, descending));
            self.query.sort = SortColumn::Name;
            self.query.descending = false;
        }
        self.refilter();
    }
    fn set_column_visible(&mut self, column: Column, visible: bool) {
        if column == Column::Name {
            return;
        }
        if let Some(setting) = self
            .columns
            .settings
            .columns
            .iter_mut()
            .find(|c| c.column == column)
        {
            setting.visible = visible;
        }
    }
    fn column_menu(&mut self, ui: &mut Ui, t: &Theme, available: f32, active: bool) {
        if !self.columns.menu.active() {
            return;
        }
        use foldex_ui::components::MenuItem;
        let column = self.columns.menu_column;
        let visible = self
            .columns
            .settings
            .columns
            .iter()
            .filter(|c| c.visible)
            .map(|c| c.column)
            .collect::<Vec<_>>();
        let at = visible.iter().position(|c| *c == column).unwrap_or(0);
        let mut items = vec![
            MenuItem {
                id: 0,
                label: "Shift Column Left",
                shortcut: "",
                glyph: Icon::Back,
                checked: None,
                enabled: at > 0,
            },
            MenuItem {
                id: 1,
                label: "Shift Column Right",
                shortcut: "",
                glyph: Icon::Forward,
                checked: None,
                enabled: at + 1 < visible.len(),
            },
            MenuItem {
                id: 2,
                label: "Size Column To Fit",
                shortcut: "",
                glyph: Icon::Expand,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 3,
                label: "Close Column",
                shortcut: "MouseMiddle",
                glyph: Icon::Close,
                checked: None,
                enabled: column != Column::Name,
            },
            MenuItem {
                id: 4,
                label: "Size All Columns To Fit",
                shortcut: "Ctrl+=",
                glyph: Icon::Expand,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 5,
                label: "Restore Default Columns",
                shortcut: "",
                glyph: Icon::Refresh,
                checked: None,
                enabled: true,
            },
        ];
        for (n, column) in Column::ALL
            .into_iter()
            .enumerate()
            .filter(|(_, column)| column.supported(self.is_recycle_bin()))
        {
            items.push(MenuItem {
                id: 100 + n as u32,
                label: column.label(),
                shortcut: "",
                glyph: Icon::Empty,
                checked: Some(visible.contains(&column)),
                enabled: column != Column::Name,
            });
        }
        if let Some(id) = self.columns.menu.show_width(
            ui.ctx(),
            Id::new((self.id, "column-menu")),
            t,
            self.columns.menu_anchor,
            &items,
            282.0 * t.metrics().scale,
        ) {
            match id {
                0 | 1 => {
                    let other = visible
                        .get(if id == 0 {
                            at.saturating_sub(1)
                        } else {
                            at + 1
                        })
                        .copied();
                    if let Some(other) = other {
                        let a = self
                            .columns
                            .settings
                            .columns
                            .iter()
                            .position(|c| c.column == column)
                            .unwrap();
                        let b = self
                            .columns
                            .settings
                            .columns
                            .iter()
                            .position(|c| c.column == other)
                            .unwrap();
                        self.columns.settings.columns.swap(a, b);
                    }
                }
                2 => self.fit_column(ui, t, column, available),
                3 => self.set_column_visible(column, false),
                4 => self.fit_all_columns(ui, t, available),
                5 => {
                    self.columns.settings = if self.is_recycle_bin() {
                        StoredColumns::recycle()
                    } else {
                        StoredColumns::default()
                    };
                    self.column_name = 0.45;
                }
                id if id >= 100 => {
                    if let Some(column) = Column::ALL.get((id - 100) as usize) {
                        self.set_column_visible(*column, !visible.contains(column));
                    }
                }
                _ => {}
            }
        }
        if active
            && self.zoom < 0.1
            && !ui.ctx().egui_wants_keyboard_input()
            && (crate::keyboard::owns(ui.ctx(), crate::keyboard::Region::Toolbar(self.id))
                || crate::keyboard::listing_owns(ui.ctx(), self.id))
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, Key::Equals))
        {
            self.fit_all_columns(ui, t, available);
        }
    }
    fn fit_all_columns(&mut self, ui: &Ui, t: &Theme, available: f32) {
        for column in Column::ALL {
            self.fit_column(ui, t, column, available);
        }
    }
    fn fit_column(&mut self, ui: &Ui, t: &Theme, column: Column, available: f32) {
        let scale = t.metrics().scale;
        let mut width = ui
            .painter()
            .layout_no_wrap(column.label().into(), t.font(), t.dim())
            .size()
            .x
            / scale
            + 42.0;
        // Fitting measures the loaded viewport, not every entry in a huge directory.
        for row in self.visible_rows.clone().take(128) {
            let Some(entry) = self.entry_at(row) else {
                continue;
            };
            let index = if self.recursive {
                row.saturating_sub(self.window_start) as u32
            } else {
                *self.indices.get(row).unwrap_or(&0)
            };
            let text = if column == Column::Name {
                entry.name.clone()
            } else {
                self.column_value(column, entry, index).text
            };
            width = width.max(
                ui.painter().layout_no_wrap(text, t.font(), t.fg()).size().x / scale
                    + if column == Column::Name { 42.0 } else { 20.0 },
            );
        }
        width = width.clamp(
            column.min_width(),
            (available / scale * 0.7)
                .max(column.min_width())
                .min(1200.0),
        );
        if let Some(setting) = self
            .columns
            .settings
            .columns
            .iter_mut()
            .find(|c| c.column == column)
        {
            setting.width = width;
        }
        if column == Column::Name {
            self.column_name = (width * scale / available.max(1.0)).clamp(0.15, 0.8);
            self.columns.settings.name_auto = false;
        }
    }
    fn type_menu(&mut self, ui: &Ui, t: &Theme) {
        if !self.columns.types.active() {
            return;
        }
        use foldex_ui::components::MenuItem;
        let mut types = self.columns.output.types.to_vec();
        if let Some(filter) = &self.query.type_filter {
            for extension in &filter.extensions {
                if !types.iter().any(|(value, _)| value == extension) {
                    types.push((extension.clone(), 0));
                }
            }
        }
        types.sort_by(|a, b| a.0.cmp(&b.0));
        let labels = types
            .iter()
            .map(|(extension, count)| {
                if extension.is_empty() {
                    format!("Files without extension · {count}")
                } else {
                    format!(".{} files · {count}", extension)
                }
            })
            .collect::<Vec<_>>();
        let filter = self.query.type_filter.as_ref();
        let mut items = vec![
            MenuItem {
                id: 0,
                label: "Toggle All",
                shortcut: "",
                glyph: Icon::Check,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 1,
                label: "Untoggle All",
                shortcut: "",
                glyph: Icon::Minus,
                checked: None,
                enabled: true,
            },
            MenuItem {
                id: 2,
                label: "File folders",
                shortcut: "",
                glyph: Icon::Folder,
                checked: Some(filter.is_none_or(|f| f.include_folders)),
                enabled: true,
            },
        ];
        for (n, ((extension, _), label)) in types.iter().zip(&labels).enumerate() {
            items.push(MenuItem {
                id: 100 + n as u32,
                label,
                shortcut: "",
                glyph: Icon::File,
                checked: Some(filter.is_none_or(|f| f.extensions.contains(extension) != f.exclude)),
                enabled: true,
            });
        }
        if let Some(id) = self.columns.types.show_width(
            ui.ctx(),
            Id::new((self.id, "type-menu")),
            t,
            self.columns.types_anchor,
            &items,
            282.0 * t.metrics().scale,
        ) {
            match id {
                0 => self.query.type_filter = None,
                1 => self.query.type_filter = Some(TypeFilter::default()),
                2 => {
                    let filter = self.query.type_filter.get_or_insert_with(|| TypeFilter {
                        include_folders: true,
                        exclude: true,
                        extensions: Vec::new(),
                    });
                    filter.include_folders = !filter.include_folders;
                }
                id if id >= 100 => {
                    if let Some((extension, _)) = types.get((id - 100) as usize) {
                        let filter = self.query.type_filter.get_or_insert_with(|| TypeFilter {
                            include_folders: true,
                            exclude: true,
                            extensions: Vec::new(),
                        });
                        if let Some(at) = filter
                            .extensions
                            .iter()
                            .position(|value| value == extension)
                        {
                            filter.extensions.remove(at);
                        } else if filter.extensions.len() < 256 {
                            filter.extensions.push(extension.clone());
                        }
                    }
                }
                _ => {}
            }
            self.refilter();
            // Checklists stay open for successive type changes and remain keyboard navigable.
            self.columns.types.toggle(ui.ctx());
            self.columns.types.select(if id >= 100 {
                (id - 100) as usize + 3
            } else {
                id as usize
            });
        }
    }

    fn poll_column_output(&mut self, ctx: &egui::Context) {
        if self.columns.service.is_none() {
            let key = Id::new("foldex-details-service");
            let service = ctx.data_mut(|data| {
                if let Some(service) = data
                    .get_temp::<std::sync::Weak<ColumnService>>(key)
                    .and_then(|service| service.upgrade())
                {
                    service
                } else {
                    let service = Arc::new(ColumnService::new(ctx.clone()));
                    data.insert_temp(key, Arc::downgrade(&service));
                    service
                }
            });
            self.columns.service = Some(service);
        }
        if let Some(output) = self
            .columns
            .service
            .as_ref()
            .and_then(|service| service.output(self.id))
        {
            if output.epoch == self.columns.epoch
                && output.revision == self.columns.revision
                && output.revision == self.render_epoch
                && self
                    .columns
                    .source
                    .as_ref()
                    .is_some_and(|(path, navigation)| {
                        path == &self.path && *navigation == self.navigation_epoch()
                    })
            {
                if let Some(order) = output.order.as_ref().filter(|order| {
                    !self.recursive
                        && self.columns.settings.sort == output.sort
                        && self
                            .columns
                            .applied
                            .as_ref()
                            .is_none_or(|old| !Arc::ptr_eq(old, order))
                }) {
                    if let Some(remap) = output.remap.as_ref().filter(|remap| {
                        Arc::ptr_eq(&remap.source, &self.indices)
                            && remap.selected == self.selected.intervals()
                            && remap.focused == self.focused
                    }) {
                        self.selected.replace_intervals(remap.mapped.clone());
                        self.focused = remap.mapped_focus;
                        self.anchor = self.focused;
                        self.indices = order.clone();
                        self.columns.applied = Some(order.clone());
                        self.columns.order_epoch = self.columns.order_epoch.wrapping_add(1);
                    } else {
                        ctx.request_repaint();
                    }
                }
                self.columns.output = output;
                self.columns.refresh_pending = false;
            }
        }
    }
    pub(super) fn request_column_values(
        &mut self,
        ctx: &egui::Context,
        layout: &DetailsLayout,
        visible: std::ops::Range<usize>,
        active: bool,
    ) {
        if self.is_recycle_bin() {
            self.columns.cancel(self.id);
            return;
        }
        self.columns.view_id = Some(self.id);
        let Some(snapshot) = self.snapshot.clone() else {
            self.columns.cancel(self.id);
            return;
        };
        // Snapshot IDs change for harmless watcher reconciliations and streamed
        // result windows. Only navigation/explicit refresh creates a new scope.
        let source = (self.path.clone(), self.navigation_epoch());
        if self.columns.source.as_ref() != Some(&source) {
            self.columns.source = Some(source);
            self.columns.epoch = self.columns.epoch.wrapping_add(1);
            self.columns.output = Arc::default();
        }
        let visible = visible
            .take(128)
            .filter_map(|row| {
                if self.recursive {
                    row.checked_sub(self.window_start).map(|i| i as u32)
                } else {
                    self.indices.get(row).copied()
                }
            })
            .filter(|index| (*index as usize) < snapshot.entries.len())
            .collect::<Vec<_>>();
        if self.columns.revision != self.render_epoch {
            self.columns.revision = self.render_epoch;
            self.columns.base_indices = self.indices.clone();
            self.columns.applied = None;
            let output = Arc::make_mut(&mut self.columns.output);
            remap_rows(&mut output.rows, &snapshot, &visible);
            output.order = None;
            output.remap = None;
            output.busy = (layout.cells.iter().any(|cell| cell.column.needs_folders())
                && visible.iter().any(|index| {
                    let entry = &snapshot.entries[*index as usize];
                    (entry.is_dir
                        && output
                            .rows
                            .get(index)
                            .and_then(|values| values.folder.as_ref())
                            .is_none_or(|stats| stats.state == State::Scanning))
                        || (!entry.metadata_known
                            && output
                                .rows
                                .get(index)
                                .is_none_or(|values| !values.metadata_done))
                }))
                || self.columns.settings.sort.is_some()
                || (layout.contains(Column::VolumeUsage) && !output.volume_done)
                || (layout.contains(Column::FolderUsage)
                    && output
                        .parent
                        .as_ref()
                        .is_none_or(|stats| stats.state == State::Scanning));
        }
        let data_revision = if self.recursive {
            0
        } else {
            self.browser.data_revision()
        };
        let changed_entries = if data_revision != self.columns.output.data_revision {
            let Some((revision, names)) = self
                .browser
                .filesystem_changes_since(self.columns.output.data_revision)
            else {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            };
            // A later notification is included on the next frame. Never label
            // names from a newer mailbox revision with an older atomic value.
            if revision != data_revision {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            names.map(Arc::from)
        } else {
            Some(Arc::from([]))
        };
        let request = ViewInput {
            priority: active,
            epoch: self.columns.epoch,
            revision: self.render_epoch,
            refresh: self.columns.refresh_pending,
            presented: self.columns.output.clone(),
            data_revision,
            changed_entries,
            snapshot,
            indices: if self.recursive {
                Arc::from([])
            } else {
                self.columns.base_indices.clone()
            },
            visible,
            folders: layout.cells.iter().any(|cell| cell.column.needs_folders()),
            dates: layout.contains(Column::Created) || layout.contains(Column::Accessed),
            volume: layout.contains(Column::VolumeUsage),
            parent: layout.contains(Column::FolderUsage),
            sort: if self.recursive {
                None
            } else {
                self.columns.settings.sort
            },
            selection_order: self.indices.clone(),
            selection: self.selected.intervals(),
            focused: self.focused,
        };
        if let Some(service) = &self.columns.service {
            match service.submit(self.id, request) {
                Submission::Submitted => {}
                Submission::Busy => ctx.request_repaint_after(Duration::from_millis(16)),
                Submission::AtCapacity => {
                    let output = Arc::make_mut(&mut self.columns.output);
                    output.error = Some("The 32-view statistics subscription limit has been reached. Close an unused panel to calculate more values.".into());
                    output.busy = false;
                }
            }
        }
    }

    pub(super) fn column_value(&self, column: Column, entry: &Entry, index: u32) -> CellValue {
        if self.is_recycle_bin() {
            return self.recycle_column_value(column, index);
        }
        let values = self.columns.output.rows.get(&index);
        let stats = values.and_then(|value| value.folder.as_deref());
        let mut busy = false;
        let mut partial = false;
        let text = match column {
            Column::Name => entry.name.clone(),
            Column::Type => {
                if entry.is_dir {
                    "File folder".into()
                } else {
                    file_kind(&entry.name)
                }
            }
            Column::Modified => {
                if entry.metadata_known {
                    format_date(entry.modified_secs)
                } else {
                    busy = values.is_none_or(|v| !v.metadata_done);
                    values
                        .and_then(|v| v.modified)
                        .map(format_date)
                        .unwrap_or_else(|| if busy { String::new() } else { "—".into() })
                }
            }
            Column::Created | Column::Accessed => {
                let timestamp = if column == Column::Created {
                    entry
                        .created_secs
                        .or_else(|| values.and_then(|v| v.created))
                } else {
                    entry
                        .accessed_secs
                        .or_else(|| values.and_then(|v| v.accessed))
                };
                busy = timestamp.is_none() && values.is_none_or(|v| !v.metadata_done);
                timestamp
                    .map(format_date)
                    .unwrap_or_else(|| if busy { String::new() } else { "—".into() })
            }
            column => {
                let bytes = if entry.is_dir {
                    busy = stats.is_none_or(|s| s.state == State::Scanning);
                    partial = stats.is_some_and(|s| s.state == State::Partial);
                    stats
                        .filter(|s| s.state != State::Unavailable)
                        .map(|s| s.bytes)
                } else {
                    if entry.metadata_known {
                        Some(entry.size)
                    } else {
                        busy = values.is_none_or(|v| !v.metadata_done);
                        values.and_then(|v| v.size)
                    }
                };
                match column {
                    Column::Size => bytes
                        .map(foldex_ui::format_size)
                        .unwrap_or_else(|| if busy { String::new() } else { "—".into() }),
                    Column::Folders | Column::Files | Column::TotalItems => {
                        if !entry.is_dir {
                            busy = false;
                            "—".into()
                        } else {
                            stats
                                .filter(|s| s.state != State::Unavailable)
                                .map(|s| {
                                    format_count(match column {
                                        Column::Folders => s.folders,
                                        Column::Files => s.files,
                                        _ => s.folders.saturating_add(s.files),
                                    })
                                })
                                .unwrap_or_else(|| if busy { String::new() } else { "—".into() })
                        }
                    }
                    Column::VolumeUsage => {
                        busy |= !self.columns.output.volume_done;
                        match (bytes, self.columns.output.volume_bytes) {
                            (Some(bytes), Some(total)) if total > 0 => {
                                format!("{:.1}%", bytes as f64 / total as f64 * 100.0)
                            }
                            _ => {
                                if busy {
                                    String::new()
                                } else {
                                    "—".into()
                                }
                            }
                        }
                    }
                    Column::FolderUsage => {
                        let parent = self.columns.output.parent.as_deref();
                        busy |= parent.is_none_or(|s| s.state == State::Scanning);
                        match (bytes, parent) {
                            (Some(bytes), Some(parent))
                                if parent.state == State::Complete && !partial =>
                            {
                                format!(
                                    "{:.1}%",
                                    if parent.bytes == 0 {
                                        0.0
                                    } else {
                                        bytes as f64 / parent.bytes as f64 * 100.0
                                    }
                                )
                            }
                            _ => {
                                if busy {
                                    String::new()
                                } else {
                                    "—".into()
                                }
                            }
                        }
                    }
                    _ => String::new(),
                }
            }
        };
        if self.columns.output.error.is_some() {
            busy = false;
        }
        if self.columns.controlled_pending && column.numeric() {
            busy = true;
        }
        let text = if !busy && text.is_empty() {
            "—".into()
        } else if partial && !text.is_empty() && text != "—" {
            format!("≥ {text}")
        } else {
            text
        };
        let tooltip = if let Some(error) = &self.columns.output.error {
            crate::friendly::present("Couldn't calculate this value", error)
        } else if let Some(stats) =
            stats.filter(|s| matches!(s.state, State::Partial | State::Unavailable))
        {
            match stats.error.as_deref() {
                Some(error) => crate::friendly::present("Couldn't calculate this value", error),
                None => crate::friendly::with_detail(
                    "Some items couldn't be read or counted, so this number may be lower than the real one.",
                    &format!("Incomplete recursive coverage: {} unreadable, {} links, {} offline, {} depth limits, {} entry limits. Shown counts and bytes are lower bounds.",
                        stats.issues.unreadable, stats.issues.skipped_reparse, stats.issues.skipped_offline,
                        stats.issues.depth_limited, stats.issues.entry_limited),
                ),
            }
        } else if busy {
            "Calculating in the background…".into()
        } else {
            match column {
                Column::Folders => "All descendant folders; excludes this folder".into(),
                Column::Files => "All descendant ordinary files".into(),
                Column::TotalItems => "All descendant folders and files".into(),
                Column::Size => "Logical file bytes; folder sizes include descendants".into(),
                Column::VolumeUsage => "Logical item bytes as a share of volume capacity".into(),
                Column::FolderUsage => {
                    "Logical item bytes as a share of the current folder's recursive bytes".into()
                }
                _ => text.clone(),
            }
        };
        CellValue {
            text,
            busy,
            partial,
            tooltip,
        }
    }

    pub(crate) fn benchmark_column_menu(
        &mut self,
        column: Column,
        anchor: Rect,
        ctx: &egui::Context,
    ) {
        self.columns.menu_column = column;
        self.columns.menu_anchor = anchor;
        if !self.columns.menu.open {
            self.columns.menu.toggle(ctx);
        }
    }
    pub(crate) fn benchmark_type_filter(&mut self, anchor: Rect, ctx: &egui::Context) {
        self.columns.types_anchor = anchor;
        if !self.columns.types.open {
            self.columns.types.toggle(ctx);
        }
    }
    pub(crate) fn benchmark_set_column(&mut self, column: Column, visible: bool) {
        self.set_column_visible(column, visible);
    }
    pub(crate) fn benchmark_column_state(&self) -> (StoredColumns, Option<TypeFilter>, bool) {
        (
            self.stored_columns(),
            self.query.type_filter.clone(),
            self.columns.output.busy,
        )
    }

    /// Cumulative service counters let the native harness distinguish ordinary
    /// selection submissions from actual recursive walks and invalidations.
    pub(crate) fn benchmark_column_diagnostics(&self) -> ColumnDiagnostics {
        let published = self
            .columns
            .service
            .as_ref()
            .and_then(|service| service.output(self.id));
        let (submissions, invalidations, walks_started, publications) = self
            .columns
            .service
            .as_ref()
            .map_or((0, 0, 0, 0), |service| service.metrics());
        ColumnDiagnostics {
            submissions,
            invalidations,
            walks_started,
            publications,
            epoch: self.columns.epoch,
            revision: self.columns.revision,
            output_revision: self.columns.output.revision,
            data_revision: self.browser.data_revision(),
            output_data_revision: self.columns.output.data_revision,
            retained_rows: self.columns.output.rows.len(),
            pending_rows: self
                .columns
                .output
                .rows
                .values()
                .filter(|values| {
                    values
                        .folder
                        .as_ref()
                        .is_some_and(|stats| stats.state == State::Scanning)
                })
                .count(),
            busy: self.columns.output.busy,
            refresh_pending: self.columns.refresh_pending,
            controlled_pending: self.columns.controlled_pending,
            configured_sort: self.columns.settings.sort,
            published_epoch: published.as_ref().map(|output| output.epoch),
            published_revision: published.as_ref().map(|output| output.revision),
            published_busy: published.as_ref().map(|output| output.busy),
            published_rows: published.as_ref().map(|output| output.rows.len()),
            source_current: self
                .columns
                .source
                .as_ref()
                .is_some_and(|(path, navigation)| {
                    path == &self.path && *navigation == self.navigation_epoch()
                }),
        }
    }
    /// A visual-only pending scene for the native screenshot harness. It keeps
    /// real identities/values and does not stop, invalidate or fake worker data.
    pub(crate) fn benchmark_pending_columns(&mut self, pending: bool) {
        self.columns.controlled_pending = pending;
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(crate) struct ColumnDiagnostics {
    pub submissions: u64,
    pub invalidations: u64,
    pub walks_started: u64,
    pub publications: u64,
    pub epoch: u64,
    pub revision: u64,
    pub output_revision: u64,
    pub data_revision: u64,
    pub output_data_revision: u64,
    pub retained_rows: usize,
    pub pending_rows: usize,
    pub busy: bool,
    pub refresh_pending: bool,
    pub controlled_pending: bool,
    pub configured_sort: Option<(Column, bool)>,
    pub published_epoch: Option<u64>,
    pub published_revision: Option<u64>,
    pub published_busy: Option<bool>,
    pub published_rows: Option<usize>,
    pub source_current: bool,
}

pub(super) struct CellValue {
    pub text: String,
    pub busy: bool,
    pub partial: bool,
    pub tooltip: String,
}
pub(super) fn format_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 10_000 {
        format!("{:.1}K", value as f64 / 1000.0)
    } else {
        value.to_string()
    }
}
const PENDING_PAINT_INTERVAL: Duration = Duration::from_millis(50);

/// One quantized clock for the visible numeric cells, with no retained per-row
/// timer, texture, blur or layout animation. Finished/off-screen values schedule
/// no frames, and reduced motion or loss of focus leaves a static placeholder.
pub(super) struct PendingPaint {
    phase: f32,
    pub(super) opacity: f32,
    animate: bool,
}
impl PendingPaint {
    pub(super) fn new(ctx: &egui::Context, t: &Theme) -> Self {
        let (time, focused) = ctx.input(|input| (input.time, input.focused));
        let animate = t.duration() > 0.0 && focused;
        let phase = ((time * 20.0).floor() / 36.0).rem_euclid(1.0) as f32;
        Self {
            phase,
            opacity: if animate {
                0.76 + 0.10 * (phase * std::f32::consts::TAU).sin()
            } else {
                0.76
            },
            animate,
        }
    }
    pub(super) fn skeleton(&self, p: &egui::Painter, cell: Rect, t: &Theme) {
        let width = (t.font_size * 2.8).min(cell.width()).max(0.0);
        let rect = Rect::from_center_size(
            pos2(cell.right() - width * 0.5, cell.center().y),
            vec2(width, (t.font_size * 0.24).max(2.0)),
        );
        let p = p.with_clip_rect(cell.intersect(p.clip_rect()));
        p.rect_filled(rect, 2.0, t.dim().gamma_multiply(0.18));
        if self.animate {
            let band = width * 0.45;
            let left = rect.left() - band + self.phase * (width + band);
            let highlight = Rect::from_min_size(pos2(left, rect.top()), vec2(band, rect.height()));
            p.with_clip_rect(rect.intersect(p.clip_rect())).rect_filled(
                highlight,
                2.0,
                t.dim().gamma_multiply(0.10),
            );
        }
    }
    pub(super) fn finish(&self, ctx: &egui::Context, visible_pending: bool) {
        if self.animate && visible_pending {
            ctx.request_repaint_after(PENDING_PAINT_INTERVAL);
        }
    }
}

pub(super) fn paint_spinner(
    p: &egui::Painter,
    center: Pos2,
    size: f32,
    color: Color32,
    motion: bool,
) {
    let (time, focused) = p.ctx().input(|input| (input.time, input.focused));
    let animate = motion && focused;
    let now = if animate {
        ((time * 20.0).floor() / 20.0) as f32
    } else {
        0.0
    };
    for spoke in 0..10 {
        let angle = (spoke as f32 / 10.0 + now * 0.8) * std::f32::consts::TAU;
        let direction = vec2(angle.cos(), angle.sin());
        p.line_segment(
            [
                center + direction * size * 0.28,
                center + direction * size * 0.5,
            ],
            Stroke::new(
                (size * 0.11).max(1.0),
                color.gamma_multiply(0.15 + spoke as f32 * 0.085),
            ),
        );
    }
    if animate {
        p.ctx().request_repaint_after(PENDING_PAINT_INTERVAL);
    }
}
fn paint_funnel(p: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let points = [
        (-0.5, -0.4),
        (0.5, -0.4),
        (0.12, 0.02),
        (0.12, 0.4),
        (-0.12, 0.5),
        (-0.12, 0.02),
        (-0.5, -0.4),
    ]
    .map(|(x, y)| center + vec2(x, y) * size);
    p.add(egui::Shape::line(points.to_vec(), Stroke::new(1.1, color)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw_keyboard_header(
        pane: &mut Pane,
        ctx: &egui::Context,
        t: &Theme,
        frame: u64,
        events: Vec<egui::Event>,
    ) {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 600.0))),
                time: Some(frame as f64 / 60.0),
                events,
                ..Default::default()
            },
            |ui| {
                pane.details_header(
                    ui,
                    t,
                    Rect::from_min_size(pos2(10.0, 50.0), vec2(1080.0, 25.0)),
                    true,
                );
            },
        )
        .drop_without_applying_deltas();
    }

    #[test]
    fn modified_enter_does_not_activate_sort_or_type_filter_buttons() {
        for target in [
            Id::new((722_u64, "column", Column::Name)),
            Id::new((722_u64, "type-funnel")),
        ] {
            for modifiers in [egui::Modifiers::ALT, egui::Modifiers::SHIFT] {
                let engine = Engine::with_options(
                    Arc::new(|| {}),
                    foldex_core::EngineOptions {
                        poll_interval: None,
                        ..Default::default()
                    },
                )
                .unwrap();
                let mut pane =
                    Pane::dormant(722, &engine, PathBuf::from("virtual-columns-fixture"));
                let ctx = egui::Context::default();
                let t = Theme {
                    reduced_motion: true,
                    ..Default::default()
                };
                t.apply(&ctx);
                draw_keyboard_header(&mut pane, &ctx, &t, 0, vec![]);
                crate::keyboard::request_id(&ctx, target);
                draw_keyboard_header(&mut pane, &ctx, &t, 1, vec![]);
                let query = pane.query.clone();
                let sort = pane.columns.settings.sort;
                draw_keyboard_header(
                    &mut pane,
                    &ctx,
                    &t,
                    2,
                    vec![egui::Event::Key {
                        key: Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers,
                    }],
                );
                assert_eq!(pane.query, query);
                assert_eq!(pane.columns.settings.sort, sort);
                assert!(!pane.columns.menu.open);
                assert!(!pane.columns.types.open);
                assert!(ctx.input(|input| input.key_pressed(Key::Enter)));
            }
        }
    }

    #[test]
    fn type_filter_enter_and_space_open_once_without_activating_a_menu_choice() {
        for opening in [Key::Enter, Key::Space] {
            let engine = Engine::with_options(
                Arc::new(|| {}),
                foldex_core::EngineOptions {
                    poll_interval: None,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut pane = Pane::dormant(723, &engine, PathBuf::from("virtual-columns-fixture"));
            let ctx = egui::Context::default();
            let t = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            t.apply(&ctx);
            draw_keyboard_header(&mut pane, &ctx, &t, 0, vec![]);
            crate::keyboard::request_id(&ctx, Id::new((pane.id, "type-funnel")));
            draw_keyboard_header(&mut pane, &ctx, &t, 1, vec![]);
            let query = pane.query.clone();
            let mut events = vec![egui::Event::Key {
                key: opening,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }];
            if opening == Key::Space {
                events.push(egui::Event::Text(" ".into()));
            }
            draw_keyboard_header(&mut pane, &ctx, &t, 2, events);
            assert!(pane.columns.types.open);
            assert!(!pane.columns.menu.open);
            assert_eq!(pane.query, query);
            assert!(!ctx.input(|input| input.key_pressed(opening)));
            assert!(!ctx.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Text(text) if text == " "))
            }));
            assert_eq!(
                ctx.memory(|memory| memory.focused()),
                Some(Id::new((pane.id, "type-menu")).with("keyboard"))
            );
            draw_keyboard_header(&mut pane, &ctx, &t, 3, vec![]);
            assert!(pane.columns.types.open);
            assert_eq!(pane.query, query);
        }
    }

    #[test]
    fn column_header_and_type_filter_open_from_their_keyboard_focus() {
        for (target, key, modifiers, types) in [
            (
                Id::new((721_u64, "column", Column::Name)),
                Key::F10,
                egui::Modifiers::SHIFT,
                false,
            ),
            (
                Id::new((721_u64, "type-funnel")),
                Key::Enter,
                egui::Modifiers::NONE,
                true,
            ),
        ] {
            let engine = Engine::with_options(
                Arc::new(|| {}),
                foldex_core::EngineOptions {
                    poll_interval: None,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut pane = Pane::dormant(721, &engine, PathBuf::from("virtual-columns-fixture"));
            let ctx = egui::Context::default();
            let t = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            t.apply(&ctx);
            let draw = |pane: &mut Pane, frame: u64, events: Vec<egui::Event>| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 600.0))),
                        time: Some(frame as f64 / 60.0),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        pane.details_header(
                            ui,
                            &t,
                            Rect::from_min_size(pos2(10.0, 50.0), vec2(1080.0, 25.0)),
                            true,
                        );
                    },
                )
                .drop_without_applying_deltas();
            };
            draw(&mut pane, 0, vec![]);
            crate::keyboard::request_id(&ctx, target);
            draw(&mut pane, 1, vec![]);
            draw(
                &mut pane,
                2,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
            );
            assert!(if types {
                pane.columns.types.open
            } else {
                pane.columns.menu.open
            });
            assert_eq!(pane.query.type_filter, None);
        }
    }
    struct PaintedHeading {
        label: Rect,
        text_position: Pos2,
        elided: bool,
        natural_text_width: f32,
        lines: Vec<[Pos2; 2]>,
    }
    fn paint_heading(
        ctx: &egui::Context,
        theme: &Theme,
        column: Column,
        sorted: bool,
        calculating: bool,
    ) -> PaintedHeading {
        let scale = theme.metrics().scale;
        let geometry = HeaderContent::new(
            column,
            Rect::from_min_size(
                pos2(20.0, 12.0),
                vec2(column.default_width() * scale, 25.0 * scale),
            ),
            scale,
        );
        let mut natural_text_width = 0.0;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 200.0))),
                time: Some(1.0),
                ..Default::default()
            },
            |ui| {
                natural_text_width = ui
                    .painter()
                    .layout_no_wrap(column.label().to_owned(), theme.font(), theme.dim())
                    .size()
                    .x;
                geometry.paint(ui.painter(), theme, column, sorted, false, calculating);
            },
        );
        let text = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == column.label() => Some(text),
                _ => None,
            })
            .expect("header label must be painted");
        let painted = PaintedHeading {
            label: geometry.label,
            text_position: text.pos,
            elided: text.galley.elided,
            natural_text_width,
            lines: output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::LineSegment { points, .. } => Some(*points),
                    _ => None,
                })
                .collect(),
        };
        output.drop_without_applying_deltas();
        painted
    }
    #[test]
    fn headings_use_the_same_left_padding_and_alignment_without_moving_status_slots() {
        for font_size in [12.0, 14.0, 17.0] {
            let ctx = egui::Context::default();
            let theme = Theme {
                font_size,
                ..Default::default()
            };
            theme.apply(&ctx);
            let scale = theme.metrics().scale;
            for column in Column::ALL {
                let cell = Rect::from_min_size(
                    pos2(20.0, 10.0),
                    vec2(column.default_width() * scale, 25.0 * scale),
                );
                let geometry = HeaderContent::new(column, cell, scale);
                let checkbox = if column == Column::Name {
                    28.0 * scale
                } else {
                    0.0
                };
                assert!(
                    (geometry.label.left()
                        - cell.left()
                        - checkbox
                        - foldex_ui::components::INSET * scale)
                        .abs()
                        < 0.001
                );
                let painted = paint_heading(&ctx, &theme, column, false, false);
                assert!((painted.text_position.x - painted.label.left()).abs() < 0.001);
                assert_eq!(geometry.status.y, cell.center().y);
            }
        }
    }

    #[test]
    fn default_folder_header_label_is_complete_and_stationary_during_calculation() {
        for font_size in [12.0, 14.0, 17.0] {
            let ctx = egui::Context::default();
            let theme = Theme {
                font_size,
                ..Default::default()
            };
            theme.apply(&ctx);
            let idle = paint_heading(&ctx, &theme, Column::Folders, false, false);
            let pending = paint_heading(&ctx, &theme, Column::Folders, false, true);
            assert!(
                !idle.elided && !pending.elided,
                "the default 68px Folders header must fit at {font_size}px text: available={}, natural={}",
                idle.label.width(),
                idle.natural_text_width,
            );
            assert_eq!(idle.label, pending.label);
            assert_eq!(idle.text_position, pending.text_position);
            assert!(idle.lines.is_empty());
            assert_eq!(pending.lines.len(), 10);
        }
    }
    #[test]
    fn folder_work_keeps_name_sort_arrow_and_calculated_sort_uses_one_status_slot() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        theme.apply(&ctx);
        let name_idle = paint_heading(&ctx, &theme, Column::Name, true, false);
        let name_pending = paint_heading(&ctx, &theme, Column::Name, true, true);
        assert_eq!(name_idle.label, name_pending.label);
        assert_eq!(name_idle.text_position, name_pending.text_position);
        assert_eq!(name_idle.lines.len(), 3);
        assert_eq!(
            name_idle.lines, name_pending.lines,
            "folder work must preserve Name's ordinary sort arrow"
        );
        let sorted_idle = paint_heading(&ctx, &theme, Column::Folders, true, false);
        let sorted_pending = paint_heading(&ctx, &theme, Column::Folders, true, true);
        assert_eq!(sorted_idle.label, sorted_pending.label);
        assert_eq!(sorted_idle.text_position, sorted_pending.text_position);
        assert!(!sorted_idle.elided && !sorted_pending.elided);
        assert_eq!(sorted_idle.lines.len(), 3);
        assert_eq!(
            sorted_pending.lines.len(),
            10,
            "a busy calculated sort replaces its arrow with one spinner"
        );
    }
    #[test]
    fn responsive_columns_keep_name_and_do_not_overlap() {
        let mut settings = StoredColumns::default();
        for c in &mut settings.columns {
            c.visible = true;
        }
        for width in [50.0, 180.0, 360.0, 640.0, 1100.0] {
            let layout = DetailsLayout::new(
                &settings,
                Rect::from_min_size(pos2(20.0, 10.0), vec2(width, 26.0)),
                1.0,
                0.45,
            );
            assert!(layout.contains(Column::Name));
            assert!(layout.cells.iter().all(|cell| cell.rect.width() >= 0.0));
            assert!(
                layout
                    .cells
                    .windows(2)
                    .all(|cells| cells[0].rect.right() <= cells[1].rect.left())
            );
            assert!(layout.cells.last().unwrap().rect.right() <= 20.0 + width + 0.01);
        }
    }
    #[test]
    fn malformed_saved_layout_cannot_hide_name_or_keep_nan_widths() {
        let mut settings = StoredColumns {
            columns: vec![ColumnSetting {
                column: Column::Size,
                width: f32::NAN,
                visible: true,
            }],
            sort: None,
            name_auto: true,
        };
        settings.sanitize();
        assert_eq!(settings.columns.len(), Column::ALL.len());
        assert!(
            settings
                .columns
                .iter()
                .find(|c| c.column == Column::Name)
                .unwrap()
                .visible
        );
        assert!(settings.columns.iter().all(|c| c.width.is_finite()));
    }
}
