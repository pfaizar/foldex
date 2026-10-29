//! Searchable native options surface reconstructed from the observed source popup.
//! Working appearance and Windows settings. Geometry at 1131×611 is
//! 720×465 at y=79, with 38px header, 42px rows and 28px footer.
use crate::commands::{control, highlight};
use egui::{
    Align2, Color32, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder,
    pos2, vec2,
};
use foldex_match::Matcher;
use foldex_ui::{
    Icon, Theme,
    motion::{LayoutMotion, MotionGains, MotionProfile},
};
use std::{collections::HashMap, sync::Arc};
mod launcher_options;
use launcher_options::{LauncherOptions, is_launcher_option};
mod theme_options;
use theme_options::{AppearanceOptions, is_theme_option};

const WIDTH: f32 = 720.0;
const HEADER: f32 = 38.0;
const FOOTER: f32 = 28.0;
const ROW: f32 = 42.0;
const MAX_BODY: f32 = 399.0;
const MAX_CACHE: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum OptionId {
    FontSize,
    InspectorFontSize,
    Spacing,
    Animations,
    RoundCorners,
    ColorMode,
    ColorScheme,
    ThemeName,
    SaveTheme,
    LoadThemes,
    ExportTheme,
    DefaultExplorer,
    Updates,
    LauncherBackground,
    LauncherClipboard,
    LauncherOpenFolder,
    LauncherFolders,
    LauncherHistory,
    SectionBookmarks,
    SectionStorage,
    SectionPlaces,
    SectionNetwork,
    SectionRecents,
    StorageUsage,
    PlaceUser,
    PlaceDesktop,
    PlaceDownloads,
    PlaceDocuments,
    PlacePictures,
    PlaceMusic,
    PlaceVideos,
    RowHeight,
    SidebarWidth,
    CornerRadius,
    MotionDuration,
    Background,
    Sidebar,
    Chrome,
    Surface,
    Accent,
    Selection,
    Text,
    Muted,
    Border,
    Folders,
    Hover,
    Danger,
    Reset,
    DeveloperMode,
    PerformanceDetails,
    ErrorLog,
}
/// Tools that exist only while Developer Mode is on. Add new ones here.
fn is_developer_option(option: OptionId) -> bool {
    matches!(option, OptionId::PerformanceDetails | OptionId::ErrorLog)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeveloperAction {
    PerformanceDetails,
    ErrorLog,
}
struct Definition {
    id: OptionId,
    title: &'static str,
    description: &'static str,
}
const OPTIONS: &[Definition] = &[
    Definition {
        id: OptionId::FontSize,
        title: "Font Size",
        description: "Size of the interface text, in points.",
    },
    Definition {
        id: OptionId::InspectorFontSize,
        title: "Inspector Font Size",
        description: "Base size of preview text. Inspector zoom remains independent.",
    },
    Definition {
        id: OptionId::Spacing,
        title: "Spacing",
        description: "Space between controls and items in icon views.",
    },
    Definition {
        id: OptionId::Animations,
        title: "Animations",
        description: "Smooth changes in layout, scrolling, hover and focus.",
    },
    Definition {
        id: OptionId::RoundCorners,
        title: "Round Corners",
        description: "Round the corners of controls, menus and selections.",
    },
    Definition {
        id: OptionId::ColorMode,
        title: "Color Mode",
        description: "Dark, Light or System. Color controls edit the active mode.",
    },
    Definition {
        id: OptionId::ColorScheme,
        title: "Color Scheme",
        description: "Default and your saved themes. Each includes dark and light colors.",
    },
    Definition {
        id: OptionId::ThemeName,
        title: "Theme Name",
        description: "Name for your saved theme. Both color modes are included.",
    },
    Definition {
        id: OptionId::SaveTheme,
        title: "Save Theme",
        description: "Save both palettes under the name above in your theme library.",
    },
    Definition {
        id: OptionId::LoadThemes,
        title: "Load Themes",
        description: "Import a theme file into your library and select its colors.",
    },
    Definition {
        id: OptionId::ExportTheme,
        title: "Export Theme",
        description: "Share both palettes as a theme file. Choose where to save it.",
    },
    Definition {
        id: OptionId::DefaultExplorer,
        title: "Default File Explorer",
        description: "Open folders, drives and Win+E in Foldex. Turn off to restore.",
    },
    Definition {
        id: OptionId::Updates,
        title: "Application Updates",
        description: "Check for new versions, download updates and choose automatic updates.",
    },
    Definition {
        id: OptionId::LauncherBackground,
        title: "Launcher System Tray",
        description: "Keep Foldex ready in the background after closing its window.",
    },
    Definition {
        id: OptionId::LauncherClipboard,
        title: "Launcher Clipboard History",
        description: "Remember copied text while Foldex runs. Stored only in memory.",
    },
    Definition {
        id: OptionId::LauncherOpenFolder,
        title: "Open Folder from Launcher",
        description: "Replace the current tab, or open a new tab or window.",
    },
    Definition {
        id: OptionId::LauncherFolders,
        title: "Launcher Search Folders",
        description: "Choose up to two folders, or use the default search locations.",
    },
    Definition {
        id: OptionId::LauncherHistory,
        title: "Launcher Text History",
        description: "Clear copied text stored in memory by the launcher.",
    },
    Definition {
        id: OptionId::SectionBookmarks,
        title: "Sidebar: Bookmarks",
        description: "Show the Bookmarks section in the sidebar.",
    },
    Definition {
        id: OptionId::SectionStorage,
        title: "Sidebar: Storage",
        description: "Show the Storage section in the sidebar.",
    },
    Definition {
        id: OptionId::SectionPlaces,
        title: "Sidebar: Places",
        description: "Show the Places section in the sidebar.",
    },
    Definition {
        id: OptionId::SectionNetwork,
        title: "Sidebar: Network",
        description: "Show the Network section. Hiding it stops connection discovery.",
    },
    Definition {
        id: OptionId::SectionRecents,
        title: "Sidebar: Recents",
        description: "Show the Recents section in the sidebar.",
    },
    Definition {
        id: OptionId::StorageUsage,
        title: "Storage Usage Bars",
        description: "Show used space beneath drives in the sidebar.",
    },
    Definition {
        id: OptionId::PlaceUser,
        title: "Places: User",
        description: "Show your user folder in the sidebar Places section.",
    },
    Definition {
        id: OptionId::PlaceDesktop,
        title: "Places: Desktop",
        description: "Show Desktop in the sidebar Places section.",
    },
    Definition {
        id: OptionId::PlaceDownloads,
        title: "Places: Downloads",
        description: "Show Downloads in the sidebar Places section.",
    },
    Definition {
        id: OptionId::PlaceDocuments,
        title: "Places: Documents",
        description: "Show Documents in the sidebar Places section.",
    },
    Definition {
        id: OptionId::PlacePictures,
        title: "Places: Pictures",
        description: "Show Pictures in the sidebar Places section.",
    },
    Definition {
        id: OptionId::PlaceMusic,
        title: "Places: Music",
        description: "Show Music in the sidebar Places section.",
    },
    Definition {
        id: OptionId::PlaceVideos,
        title: "Places: Videos",
        description: "Show Videos in the sidebar Places section.",
    },
    Definition {
        id: OptionId::RowHeight,
        title: "Row Height",
        description: "Height of each item in compact file views.",
    },
    Definition {
        id: OptionId::SidebarWidth,
        title: "Sidebar Width",
        description: "Width of Quick Access when the sidebar is shown.",
    },
    Definition {
        id: OptionId::CornerRadius,
        title: "Corner Radius",
        description: "Amount of rounding applied when corners are enabled.",
    },
    Definition {
        id: OptionId::MotionDuration,
        title: "Motion Response",
        description: "Animation response time. Zero settles immediately.",
    },
    Definition {
        id: OptionId::Background,
        title: "Background Color",
        description: "Main file-view background.",
    },
    Definition {
        id: OptionId::Sidebar,
        title: "Sidebar Color",
        description: "Background of Quick Access and the inspector.",
    },
    Definition {
        id: OptionId::Chrome,
        title: "Chrome Color",
        description: "Background of window and panel controls.",
    },
    Definition {
        id: OptionId::Surface,
        title: "Surface Color",
        description: "Background of menus and floating panels.",
    },
    Definition {
        id: OptionId::Accent,
        title: "Accent Color",
        description: "Active controls, focus outlines and matching text.",
    },
    Definition {
        id: OptionId::Selection,
        title: "Selection Color",
        description: "Background of selected items.",
    },
    Definition {
        id: OptionId::Text,
        title: "Text Color",
        description: "Primary interface and file-name text.",
    },
    Definition {
        id: OptionId::Muted,
        title: "Muted Text Color",
        description: "Descriptions, metadata and secondary text.",
    },
    Definition {
        id: OptionId::Border,
        title: "Border Color",
        description: "Separators and control outlines.",
    },
    Definition {
        id: OptionId::Folders,
        title: "Folder Color",
        description: "Tint of fallback folder icons.",
    },
    Definition {
        id: OptionId::Hover,
        title: "Hover Color",
        description: "Background shown while pointing at an item.",
    },
    Definition {
        id: OptionId::Danger,
        title: "Danger Color",
        description: "Destructive actions and error messages.",
    },
    Definition {
        id: OptionId::Reset,
        title: "Reset Appearance",
        description: "Restore the default colors, sizing and animation settings.",
    },
    Definition {
        id: OptionId::DeveloperMode,
        title: "Developer Mode",
        description: "Show technical error details and unlock developer tools.",
    },
    Definition {
        id: OptionId::PerformanceDetails,
        title: "Developer: Performance Details",
        description: "Live frame timing, memory and handles. Also Ctrl+Alt+P.",
    },
    Definition {
        id: OptionId::ErrorLog,
        title: "Developer: Error Log",
        description: "Show the file where technical error details are recorded.",
    },
];

#[derive(Clone, Copy, PartialEq)]
struct TextKey {
    fonts: usize,
    width: u32,
    font: u32,
    ppp: u32,
    foreground: Color32,
    muted: Color32,
    accent: Color32,
}
struct CachedRow {
    key: TextKey,
    title: Arc<egui::Galley>,
    description: Arc<egui::Galley>,
    hover: f32,
    selection: f32,
    seen: u64,
}

#[derive(Clone, Copy)]
struct ControlFocus {
    id: Id,
    rect: Rect,
    seen: u64,
}
#[derive(Default)]
pub(super) struct OptionsResponse {
    pub apply: bool,
    pub save: bool,
    pub closed: bool,
    pub integration: Option<IntegrationRequest>,
    pub sidebar_changed: bool,
    pub updates: bool,
    pub developer: Option<DeveloperAction>,
}

#[derive(Clone, Copy)]
pub(super) struct IntegrationRequest {
    pub id: u64,
    pub enabled: Option<bool>,
}

pub(super) struct OptionsPopup {
    updates_requested: bool,
    active: bool,
    closing: bool,
    query: String,
    matcher: Matcher,
    rows: Vec<(usize, i32)>,
    selected: Option<OptionId>,
    activate: Option<OptionId>,
    focus: bool,
    scroll_to: bool,
    height: f32,
    opacity: f32,
    opened_frame: u64,
    last_radius: u8,
    last_motion: f32,
    motion: LayoutMotion,
    cache: HashMap<OptionId, CachedRow>,
    scheme_menu: foldex_ui::components::MenuState,
    scheme_option: OptionId,
    scheme_anchor: Rect,
    scheme_seen: u64,
    color_picker: foldex_ui::color::ColorPicker,
    color_option: Option<OptionId>,
    integration_sequence: u64,
    integration_request: Option<IntegrationRequest>,
    integration_pending: bool,
    integration_target: Option<bool>,
    integration_status: Option<crate::integration::Status>,
    integration_error: Option<String>,
    developer_action: Option<DeveloperAction>,
    refilter_pending: bool,
    launcher_options: LauncherOptions,
    appearance: AppearanceOptions,
    sidebar_preferences: crate::sidebar::Preferences,
    sidebar_changed: bool,
    previous_focus: Option<Id>,
    capture_return_focus: bool,
    child_return_focus: Option<Id>,
    restore_child_focus: Option<Id>,
    focus_control: Option<OptionId>,
    controls: HashMap<OptionId, ControlFocus>,
    composing: bool,
    ime_frame: Option<u64>,
    ime_input: bool,
}
impl Default for OptionsPopup {
    fn default() -> Self {
        Self {
            updates_requested: false,
            active: false,
            closing: false,
            query: String::new(),
            matcher: Matcher::new(""),
            rows: Vec::with_capacity(OPTIONS.len()),
            selected: None,
            activate: None,
            focus: true,
            scroll_to: true,
            height: 0.0,
            opacity: 0.0,
            opened_frame: 0,
            last_radius: Theme::default().radius,
            last_motion: Theme::default().motion_ms,
            motion: LayoutMotion::new(MAX_CACHE),
            cache: HashMap::with_capacity(MAX_CACHE),
            scheme_menu: Default::default(),
            scheme_option: OptionId::ColorScheme,
            scheme_anchor: Rect::NOTHING,
            scheme_seen: 0,
            color_picker: Default::default(),
            color_option: None,
            integration_sequence: 0,
            integration_request: None,
            integration_pending: false,
            integration_target: None,
            integration_status: None,
            integration_error: None,
            developer_action: None,
            refilter_pending: false,
            launcher_options: LauncherOptions::default(),
            appearance: AppearanceOptions::default(),
            sidebar_preferences: Default::default(),
            sidebar_changed: false,
            previous_focus: None,
            capture_return_focus: true,
            child_return_focus: None,
            restore_child_focus: None,
            focus_control: None,
            controls: HashMap::with_capacity(OPTIONS.len()),
            composing: false,
            ime_frame: None,
            ime_input: false,
        }
    }
}
impl OptionsPopup {
    /// Opens the accent editor for the offscreen catalog.
    #[cfg(test)]
    pub(in crate::app) fn catalog_accent(&mut self, theme: &mut Theme) {
        if let Some(value) = color_token(theme, OptionId::Accent) {
            self.color_option = Some(OptionId::Accent);
            self.color_picker.open(*value);
        }
    }
    /// Opens the Open Folder from Launcher dropdown for the offscreen catalog.
    #[cfg(test)]
    pub(in crate::app) fn catalog_open_folder_menu(&mut self) {
        self.activate = Some(OptionId::LauncherOpenFolder);
    }
    /// Transfer focus before any covered editor is rendered. In particular,
    /// text delivered with the Options shortcut must reach only this search.
    pub(in crate::app) fn prepare_open(&mut self, ctx: &egui::Context) {
        let frame = ctx.cumulative_frame_nr();
        let area = Id::new("appearance-options-popup");
        if !self.active {
            self.open(frame);
        }
        if self.capture_return_focus {
            self.previous_focus = ctx.memory(|memory| memory.focused()).filter(|id| {
                *id != area.with("search")
                    && ctx
                        .read_response(*id)
                        .is_none_or(|response| response.layer_id.id != area)
            });
            self.opened_frame = frame;
        }
        self.capture_return_focus = false;
        self.focus = true;
        crate::keyboard::request_id(ctx, area.with("search"));
    }

    fn request_integration(&mut self, enabled: Option<bool>) {
        self.integration_sequence = self.integration_sequence.wrapping_add(1);
        self.integration_request = Some(IntegrationRequest {
            id: self.integration_sequence,
            enabled,
        });
        self.integration_pending = true;
        self.integration_target = enabled;
        self.integration_error = None;
    }
    pub(super) fn finish_integration(
        &mut self,
        id: u64,
        result: Result<crate::integration::Status, String>,
    ) {
        if id != self.integration_sequence {
            return;
        }
        self.integration_pending = false;
        self.integration_target = None;
        match result {
            Ok(status) => {
                self.integration_status = Some(status);
                self.integration_error = None;
            }
            Err(error) => {
                self.integration_error = Some(crate::friendly::present(
                    "Couldn't change the Windows Explorer setting",
                    &error,
                ));
            }
        }
    }
    fn open(&mut self, frame: u64) {
        if !self.integration_pending {
            self.request_integration(None);
        }
        self.scheme_menu = Default::default();
        self.color_picker = Default::default();
        self.color_option = None;
        self.active = true;
        self.updates_requested = false;
        self.closing = false;
        self.query.clear();
        self.focus = true;
        self.height = 0.0;
        self.opacity = 0.0;
        self.opened_frame = frame;
        self.selected = None;
        self.capture_return_focus = true;
        self.child_return_focus = None;
        self.restore_child_focus = None;
        self.focus_control = None;
        self.controls.clear();
        self.composing = false;
        self.refilter();
    }
    fn refilter(&mut self) {
        self.matcher = Matcher::new(&self.query);
        self.rows.clear();
        self.cache.clear();
        self.activate = None;
        self.refilter_pending = false;
        let developer = crate::friendly::developer_mode();
        for (index, option) in OPTIONS.iter().enumerate() {
            if is_developer_option(option.id) && !developer {
                continue;
            }
            let title = self.matcher.score(option.title);
            let detail = self.matcher.score(option.description);
            if title.is_some() || detail.is_some() {
                self.rows
                    .push((index, title.unwrap_or(0) + detail.unwrap_or(0) / 2));
            }
        }
        if !self.query.is_empty() {
            self.rows
                .sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        }
        if !self
            .rows
            .iter()
            .any(|(index, _)| Some(OPTIONS[*index].id) == self.selected)
        {
            self.selected = self.rows.first().map(|(index, _)| OPTIONS[*index].id);
        }
        self.scroll_to = true;
    }
    fn move_selection(&mut self, offset: isize) {
        if self.rows.is_empty() {
            self.selected = None;
            return;
        }
        let old = self
            .rows
            .iter()
            .position(|(index, _)| Some(OPTIONS[*index].id) == self.selected)
            .unwrap_or(0);
        let next = (old as isize + offset).clamp(0, self.rows.len() as isize - 1) as usize;
        self.selected = Some(OPTIONS[self.rows[next].0].id);
        self.scroll_to = true;
    }

    /// The Options modal and its child editors own all navigation while open.
    /// Root file actions must yield even when a non-text control has focus.
    pub(in crate::app) fn owns_navigation(&self, _ctx: &egui::Context) -> bool {
        self.active
    }

    fn control_focus(&mut self, ui: &Ui, option: OptionId, response: &egui::Response) {
        foldex_ui::components::register_focus_target(ui, response.rect, response.id);
        if let Some(definition) = OPTIONS.iter().find(|definition| definition.id == option) {
            foldex_ui::components::label_control(response, definition.title);
        }
        self.controls.insert(
            option,
            ControlFocus {
                id: response.id,
                rect: response.rect,
                seen: ui.ctx().cumulative_frame_nr(),
            },
        );
        if self.focus_control == Some(option) && foldex_ui::components::can_focus(ui) {
            response.request_focus();
            self.focus_control = None;
        }
        if response.has_focus() {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: false,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            self.selected = Some(option);
            ui.scroll_to_rect(response.rect.expand2(vec2(0.0, 7.0)), None);
        }
    }

    fn return_from_child(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.child_return_focus.take() {
            self.focus = false;
            let target = if ctx
                .read_response(id)
                .is_some_and(|response| response.rect.intersects(ctx.content_rect()))
            {
                id
            } else {
                Id::new("appearance-options-popup").with("results-focus")
            };
            crate::keyboard::request_id(ctx, target);
            self.restore_child_focus = Some(target);
        }
    }

    #[cfg(test)]
    pub(super) fn ui(&mut self, ctx: &egui::Context, theme: &mut Theme) -> OptionsResponse {
        self.ui_impl(ctx, theme, None)
    }

    fn ui_impl(
        &mut self,
        ctx: &egui::Context,
        theme: &mut Theme,
        mut launcher: Option<&mut crate::launcher::Launcher>,
    ) -> OptionsResponse {
        let frame = ctx.cumulative_frame_nr();
        self.appearance.library.start(ctx);
        let restore_child_focus = self.restore_child_focus.take();
        if !self.active {
            self.open(frame);
        }
        if self.refilter_pending {
            self.refilter();
        }
        if theme.radius > 0 {
            self.last_radius = theme.radius;
        }
        if theme.motion_ms > 0.0 {
            self.last_motion = theme.motion_ms;
        }
        let paint = theme.clone();
        let area_id = Id::new("appearance-options-popup");
        let search_id = area_id.with("search");
        let list_id = area_id.with("results-focus");
        if self.capture_return_focus {
            self.previous_focus = ctx.memory(|memory| memory.focused()).filter(|id| {
                ctx.read_response(*id)
                    .is_none_or(|response| response.layer_id.id != area_id)
            });
            self.capture_return_focus = false;
        }
        if self.ime_frame != Some(frame) {
            self.ime_frame = Some(frame);
            self.ime_input = self.composing;
            ctx.input(|input| {
                for event in &input.events {
                    if let egui::Event::Ime(event) = event {
                        self.ime_input = true;
                        match event {
                            egui::ImeEvent::Preedit { text, .. } => {
                                self.composing = !text.is_empty()
                            }
                            egui::ImeEvent::Commit(_) => self.composing = false,
                            _ => {}
                        }
                    }
                }
            });
        }
        let native_child = egui::Popup::is_any_open(ctx);
        let launcher_editor = self.launcher_options.editor_open();
        let child_open = native_child
            || self.scheme_menu.active()
            || self.color_picker.is_open()
            || launcher_editor;
        let numeric_editor = ctx.text_edit_focused()
            && ctx.memory(|memory| memory.focused()) != Some(search_id)
            && !launcher_editor;
        if !self.closing
            && !self.ime_input
            && !numeric_editor
            && !self.color_picker.is_open()
            && crate::commands::take_key(ctx, Modifiers::NONE, Key::Escape)
        {
            if launcher_editor {
                self.launcher_options.close_editor();
            } else if self.scheme_menu.open {
                self.scheme_menu.close();
            } else if native_child {
                egui::Popup::close_all(ctx);
            } else {
                self.closing = true;
            }
            ctx.request_repaint();
        }
        let mut use_selected = false;
        if !self.closing && !child_open && !self.ime_input {
            let focused = ctx.memory(|memory| memory.focused());
            let in_list = focused == Some(list_id);
            let search_focused = focused == Some(search_id);
            let navigate = in_list || search_focused;
            for (key, offset) in [
                (Key::ArrowDown, 1),
                (Key::ArrowUp, -1),
                (Key::PageDown, 9),
                (Key::PageUp, -9),
            ] {
                if navigate && crate::commands::take_key(ctx, Modifiers::NONE, key) {
                    ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
                    self.move_selection(offset);
                }
            }
            if in_list && crate::commands::take_key(ctx, Modifiers::NONE, Key::Home) {
                self.move_selection(-(self.rows.len() as isize));
            }
            if in_list && crate::commands::take_key(ctx, Modifiers::NONE, Key::End) {
                self.move_selection(self.rows.len() as isize);
            }
            if navigate {
                use_selected = crate::commands::take_key(ctx, Modifiers::NONE, Key::Enter);
            }
            let focused_control = focused.and_then(|id| {
                let response = ctx.read_response(id)?;
                self.controls
                    .iter()
                    .find(|(_, control)| control.id == id)
                    .or_else(|| {
                        self.controls.iter().find(|(_, control)| {
                            control.seen.saturating_add(1) >= frame
                                && response.layer_id.id == area_id
                                && control.rect.contains(response.rect.center())
                        })
                    })
                    .map(|(option, control)| (*option, *control, response.rect))
            });
            let backwards = ctx.input(|input| {
                input.events.iter().any(|event| {
                    matches!(event,
                egui::Event::Key { key: Key::Tab, pressed: true, modifiers, .. }
                    if modifiers.matches_exact(Modifiers::SHIFT))
                })
            });
            let inner_slider = focused_control.is_some_and(|(option, control, focus)| {
                matches!(
                    option,
                    OptionId::Spacing
                        | OptionId::RowHeight
                        | OptionId::SidebarWidth
                        | OptionId::CornerRadius
                        | OptionId::MotionDuration
                ) && if backwards {
                    focus.left() > control.rect.left() + 4.0
                } else {
                    focus.right() < control.rect.right() - 4.0
                }
            });
            if (in_list && !backwards || focused_control.is_some() && !inner_slider)
                && (crate::commands::take_key(ctx, Modifiers::SHIFT, Key::Tab)
                    || crate::commands::take_key(ctx, Modifiers::NONE, Key::Tab))
            {
                ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
                let mut target = if let Some((option, _, _)) = focused_control {
                    let at = self
                        .rows
                        .iter()
                        .position(|(at, _)| OPTIONS[*at].id == option);
                    at.and_then(|at| {
                        if backwards {
                            at.checked_sub(1)
                        } else {
                            Some(at + 1)
                        }
                    })
                    .and_then(|at| self.rows.get(at))
                    .map(|(at, _)| OPTIONS[*at].id)
                } else {
                    self.selected
                };
                if target == Some(OptionId::DefaultExplorer) && self.integration_pending {
                    let at = self
                        .rows
                        .iter()
                        .position(|(at, _)| OPTIONS[*at].id == OptionId::DefaultExplorer);
                    target = at
                        .and_then(|at| {
                            if backwards {
                                at.checked_sub(1)
                            } else {
                                Some(at + 1)
                            }
                        })
                        .and_then(|at| self.rows.get(at))
                        .map(|(at, _)| OPTIONS[*at].id);
                }
                if let Some(option) = target {
                    self.selected = Some(option);
                    self.focus_control = Some(option);
                    self.scroll_to = true;
                    self.focus = false;
                } else {
                    crate::keyboard::request_id(ctx, if backwards { list_id } else { search_id });
                }
            }
        }
        let screen = ctx.content_rect();
        let m = paint.metrics();
        let geometry = geometry(screen, self.rows.len(), paint.font_size);
        let target_height = if self.closing {
            0.0
        } else {
            geometry.outer.height()
        };
        let gains = MotionGains::new(ctx.input(|input| input.stable_dt), paint.duration());
        let mut moving = foldex_ui::motion::popup_gains(ctx, paint.duration()).step_pixels(
            &mut self.height,
            target_height,
            MotionProfile::Standard,
        );
        moving |= gains.step_normalized(
            &mut self.opacity,
            if self.closing { 0.0 } else { 1.0 },
            MotionProfile::Standard,
        );
        if self.closing && self.height <= 0.25 && self.opacity <= 0.01 {
            let focused = ctx.memory(|memory| memory.focused());
            if let Some(focused) = focused {
                let owned = focused == search_id
                    || ctx
                        .read_response(focused)
                        .is_some_and(|response| response.layer_id.id == area_id);
                if owned {
                    ctx.memory_mut(|memory| memory.surrender_focus(focused));
                }
            }
            egui::Popup::close_all(ctx);
            self.scheme_menu = Default::default();
            self.launcher_options.close_editor();
            self.launcher_options.open = false;
            self.active = false;
            self.cache.clear();
            self.motion.clear();
            if let Some(previous) = self.previous_focus.take() {
                crate::keyboard::request_id(ctx, previous);
            }
            return OptionsResponse {
                closed: true,
                updates: std::mem::take(&mut self.updates_requested),
                ..Default::default()
            };
        }
        self.motion.set_pixels_per_point(ctx.pixels_per_point());
        self.motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(ctx, paint.duration()));
        let outer = Rect::from_min_size(
            geometry.outer.min,
            vec2(geometry.outer.width(), self.height),
        );
        let mut result = OptionsResponse::default();
        foldex_ui::components::popup_surface(
            ctx,
            egui::Area::new(area_id)
                .order(egui::Order::Foreground)
                .fixed_pos(outer.min)
                .fade_in(false),
            !self.closing,
            |ui| {
                ui.set_min_size(outer.size());
                ui.set_max_size(outer.size());
                ui.set_clip_rect(screen);
                ui.set_opacity(self.opacity);
                ui.painter().add(
                    egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 18,
                        spread: 0,
                        color: Color32::from_black_alpha(85),
                    }
                    .as_shape(outer, paint.radius),
                );
                ui.painter()
                    .rect_filled(outer, paint.radius, Theme::color(paint.surface));
                ui.painter().rect_stroke(
                    outer,
                    paint.radius,
                    Stroke::new(1.0, paint.line()),
                    StrokeKind::Inside,
                );
                ui.set_clip_rect(outer.intersect(screen));
                if self.closing || child_open {
                    foldex_ui::components::disable_interaction(ui);
                }
                let header = Rect::from_min_size(outer.min, vec2(outer.width(), geometry.header));
                foldex_ui::components::popup_input_icon(ui, header, &paint, Icon::Search);
                let input = foldex_ui::components::popup_input_rect(header, 2);
                ui.scope_builder(UiBuilder::new().max_rect(input), |ui| {
                    let acquiring_focus = self.focus
                        && !self.closing
                        && !launcher_editor
                        && foldex_ui::components::can_focus(ui);
                    if acquiring_focus {
                        ui.memory_mut(|memory| memory.request_focus(search_id));
                    }
                    let input = foldex_ui::components::text_input_at(
                        ui,
                        input,
                        search_id,
                        &paint,
                        &mut self.query,
                        "Select an option…",
                        256,
                    );
                    if acquiring_focus {
                        // A cached Area can skip its sizing pass. The click that
                        // opens it may still clear focus during end_pass, so
                        // acquire it again on the next frame before settling.
                        self.focus = ui.is_sizing_pass()
                            || !input.response.has_focus()
                            || self.opened_frame == frame;
                        if self.focus {
                            ctx.request_repaint();
                        }
                    }
                    if input.response.changed() {
                        self.refilter();
                    }
                    if input.response.has_focus() {
                        ui.memory_mut(|memory| {
                            memory.set_focus_lock_filter(
                                search_id,
                                egui::EventFilter {
                                    tab: false,
                                    horizontal_arrows: true,
                                    vertical_arrows: true,
                                    escape: true,
                                },
                            )
                        });
                    }
                });
                if use_selected {
                    self.activate = self.selected;
                    self.scroll_to = true;
                }
                let save = foldex_ui::components::popup_action_rect(header, 0, 2);
                let save_hit = foldex_ui::components::icon_button_at(
                    ui,
                    save,
                    area_id.with("save"),
                    &paint,
                    Icon::Save,
                    false,
                )
                .on_hover_text("Save a named theme");
                if save_hit.clicked() {
                    self.focus_theme_name(theme);
                }
                let close = foldex_ui::components::popup_action_rect(header, 1, 2);
                if control(
                    ui,
                    &paint,
                    close,
                    area_id.with("close"),
                    Icon::Close,
                    false,
                    gains,
                    &mut moving,
                )
                .on_hover_text("Close · Escape")
                .clicked()
                {
                    self.closing = true;
                }
                ui.painter().line_segment(
                    [header.left_bottom(), header.right_bottom()],
                    Stroke::new(1.0, paint.line()),
                );
                let body_height =
                    (geometry.outer.height() - geometry.header - geometry.footer).max(0.0);
                let body = Rect::from_min_size(
                    pos2(outer.left() + 4.0, header.bottom()),
                    vec2((outer.width() - 8.0).max(1.0), body_height),
                );
                if body_height > 1.0 {
                    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
                        ui.set_clip_rect(body.intersect(outer));
                        let list = foldex_ui::components::PopupFocus::surface(
                            ui,
                            body.intersect(outer),
                            list_id,
                            "Options",
                        );
                        if list.has_focus() {
                            ui.memory_mut(|memory| {
                                memory.set_focus_lock_filter(
                                    list_id,
                                    egui::EventFilter {
                                        tab: false,
                                        horizontal_arrows: true,
                                        vertical_arrows: true,
                                        escape: true,
                                    },
                                )
                            });
                        }
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if self.rows.is_empty() {
                            ui.painter().text(
                                body.center(),
                                Align2::CENTER_CENTER,
                                "No matching options",
                                paint.font(),
                                paint.dim(),
                            );
                        }
                        let mut scroll = foldex_ui::components::menu_scroll_area_for_content(
                            ui,
                            &paint,
                            area_id.with("rows"),
                            body_height,
                            self.rows.len() as f32 * geometry.row,
                        );
                        if self.scroll_to && body_height < geometry.row {
                            scroll = scroll.vertical_scroll_offset(0.0);
                        } else if self.scroll_to
                            && (self.height - geometry.outer.height()).abs() < 0.5
                        {
                            let index = self
                                .rows
                                .iter()
                                .position(|(index, _)| Some(OPTIONS[*index].id) == self.selected)
                                .unwrap_or(0);
                            scroll = scroll.vertical_scroll_offset(
                                (index as f32 * geometry.row - body_height * 0.5
                                    + geometry.row * 0.5)
                                    .max(0.0),
                            );
                            self.scroll_to = false;
                        }
                        scroll.show_rows(ui, geometry.row, self.rows.len(), |ui, range| {
                            for row_index in range.take(MAX_CACHE) {
                                let option = &OPTIONS[self.rows[row_index].0];
                                let target = ui
                                    .allocate_exact_size(
                                        vec2(ui.available_width(), geometry.row),
                                        Sense::hover(),
                                    )
                                    .0;
                                let origin = pos2(
                                    target.left(),
                                    target.top() - row_index as f32 * geometry.row,
                                )
                                .to_vec2();
                                let placement = self.motion.place(
                                    area_id.with(("option", option.id)),
                                    Rect::from_min_size(
                                        pos2(0.0, row_index as f32 * geometry.row),
                                        target.size(),
                                    ),
                                    true,
                                );
                                let rect = placement.rect.translate(origin);
                                let control_width = geometry
                                    .control
                                    .min((rect.width() * 0.43).max(80.0))
                                    .min((rect.width() - 24.0).max(1.0));
                                let control_rect = Rect::from_min_max(
                                    pos2(
                                        rect.right() - control_width - 12.0 * m.scale,
                                        rect.center().y - m.control * 0.5,
                                    ),
                                    pos2(
                                        rect.right() - 12.0 * m.scale,
                                        rect.center().y + m.control * 0.5,
                                    ),
                                );
                                let text_width =
                                    (control_rect.left() - rect.left() - 25.0 * m.scale).max(1.0);
                                let hit = ui.interact(
                                    rect,
                                    area_id.with(("row-hit", option.id)),
                                    Sense::CLICK,
                                );
                                if hit.clicked() {
                                    self.selected = Some(option.id);
                                    list.request_focus();
                                }
                                let key = TextKey {
                                    fonts: foldex_ui::fonts::revision(ctx),
                                    width: text_width.to_bits(),
                                    font: paint.font_size.to_bits(),
                                    ppp: ctx.pixels_per_point().to_bits(),
                                    foreground: paint.fg(),
                                    muted: paint.dim(),
                                    accent: paint.accent(),
                                };
                                if self
                                    .cache
                                    .get(&option.id)
                                    .is_none_or(|entry| entry.key != key)
                                {
                                    let previous = self.cache.remove(&option.id);
                                    self.cache.insert(
                                        option.id,
                                        CachedRow {
                                            key,
                                            title: ui.painter().layout_job(highlight(
                                                option.title,
                                                &self
                                                    .matcher
                                                    .match_indices(option.title)
                                                    .unwrap_or_default(),
                                                paint.font(),
                                                paint.fg(),
                                                paint.accent(),
                                                text_width,
                                            )),
                                            description: ui.painter().layout_job(highlight(
                                                option.description,
                                                &self
                                                    .matcher
                                                    .match_indices(option.description)
                                                    .unwrap_or_default(),
                                                FontId::proportional(
                                                    (paint.font_size - 1.0).max(9.0),
                                                ),
                                                paint.dim(),
                                                paint.accent(),
                                                text_width,
                                            )),
                                            hover: previous
                                                .as_ref()
                                                .map_or(0.0, |entry| entry.hover),
                                            selection: previous
                                                .as_ref()
                                                .map_or(0.0, |entry| entry.selection),
                                            seen: frame,
                                        },
                                    );
                                }
                                let cached = self.cache.get_mut(&option.id).unwrap();
                                cached.seen = frame;
                                moving |= gains.step_normalized(
                                    &mut cached.hover,
                                    if hit.hovered() { 1.0 } else { 0.0 },
                                    MotionProfile::Fast,
                                );
                                moving |= gains.step_normalized(
                                    &mut cached.selection,
                                    if self.selected == Some(option.id) {
                                        1.0
                                    } else {
                                        0.0
                                    },
                                    MotionProfile::Standard,
                                );
                                let mut painter =
                                    ui.painter().with_clip_rect(body.intersect(outer));
                                painter.multiply_opacity(placement.opacity);
                                if cached.hover > 0.0 {
                                    painter.rect_filled(
                                        rect.shrink2(vec2(2.0, 0.5)),
                                        paint.radius.min(4),
                                        Theme::color(paint.hover).gamma_multiply(cached.hover),
                                    );
                                }
                                if cached.selection > 0.0 {
                                    painter.rect_filled(
                                        rect.shrink2(vec2(2.0, 0.5)),
                                        paint.radius.min(4),
                                        Theme::color(paint.selection)
                                            .gamma_multiply(cached.selection * 0.72),
                                    );
                                }
                                if self.selected == Some(option.id)
                                    && list.has_focus()
                                    && foldex_ui::focus::keyboard_visible(ui.ctx())
                                {
                                    painter.rect_stroke(
                                        rect.shrink2(vec2(2.0, 0.5)),
                                        paint.radius.min(4),
                                        Stroke::new(1.0, paint.accent()),
                                        StrokeKind::Inside,
                                    );
                                }
                                let text_y = rect.center().y
                                    - (cached.title.size().y + cached.description.size().y + 2.0)
                                        * 0.5;
                                painter.galley(
                                    pos2(rect.left() + 12.0 * m.scale, text_y),
                                    cached.title.clone(),
                                    paint.fg(),
                                );
                                let description_at = pos2(
                                    rect.left() + 12.0 * m.scale,
                                    text_y + cached.title.size().y + 2.0,
                                );
                                if let Some((detail, error)) = self
                                    .appearance
                                    .status(option.id)
                                    .or_else(|| self.launcher_options.status(option.id))
                                {
                                    let mut style = paint.clone();
                                    style.font_size = (paint.font_size - 1.0).max(9.0);
                                    foldex_ui::components::elided_label(
                                        &painter,
                                        Rect::from_min_size(
                                            description_at,
                                            vec2(text_width, cached.description.size().y),
                                        ),
                                        &style,
                                        detail,
                                        if error {
                                            Theme::color(paint.danger)
                                        } else {
                                            paint.dim()
                                        },
                                        false,
                                    );
                                } else {
                                    painter.galley(
                                        description_at,
                                        cached.description.clone(),
                                        paint.dim(),
                                    );
                                }
                                let activate = self.activate == Some(option.id);
                                if activate {
                                    self.activate = None;
                                }
                                let changed = if is_launcher_option(option.id) {
                                    if let Some(launcher) = launcher.as_deref_mut() {
                                        self.launcher_control(
                                            ui,
                                            &paint,
                                            control_rect,
                                            option.id,
                                            activate,
                                            placement.opacity,
                                            launcher,
                                        );
                                    }
                                    false
                                } else {
                                    self.control(
                                        ui,
                                        theme,
                                        &paint,
                                        control_rect,
                                        option.id,
                                        activate,
                                        placement.opacity,
                                    )
                                };
                                if changed {
                                    result.apply = true;
                                    self.selected = Some(option.id);
                                }
                            }
                        });
                    });
                }
                let footer = Rect::from_min_size(
                    pos2(outer.left(), outer.bottom() - geometry.footer),
                    vec2(outer.width(), geometry.footer),
                );
                ui.painter().line_segment(
                    [footer.left_top(), footer.right_top()],
                    Stroke::new(1.0, paint.line()),
                );
                foldex_ui::components::popup_hint(
                    ui.painter(),
                    footer,
                    &paint,
                    "Tab  controls     ↑ ↓  options     Enter  use     Esc  close",
                    "Tab  controls     Enter  use     Esc  close",
                );
            },
        );
        let visible_body = Rect::from_min_max(
            outer.min + vec2(0.0, geometry.header),
            outer.max - vec2(0.0, geometry.footer),
        );
        if let Some(launcher) = launcher.as_deref_mut() {
            self.launcher_editor_ui(
                ctx,
                &paint,
                area_id,
                visible_body.intersect(outer),
                launcher,
            );
        }
        if self.closing
            || self.scheme_seen != frame
            || !visible_body.contains(self.scheme_anchor.center())
        {
            self.scheme_menu.close();
        }
        if let Some(launcher) = launcher.as_deref_mut() {
            self.launcher_menu(ctx, &paint, area_id, launcher);
        }
        result.apply |= self.theme_menu(ctx, theme, &paint, area_id);
        if self.color_picker.is_open()
            && let Some(option) = self.color_option
        {
            let title = OPTIONS
                .iter()
                .find(|definition| definition.id == option)
                .map_or("Color", |definition| definition.title);
            let title = format!("{title} · {}", theme.resolved_mode.label());
            if let Some(value) = color_token(theme, option) {
                result.apply |=
                    self.color_picker
                        .ui(ctx, area_id.with("color-editor"), &paint, &title, value);
            }
            if !self.color_picker.is_open() {
                ctx.request_repaint();
            }
        }
        if !self.closing
            && !self.scheme_menu.active()
            && !self.color_picker.is_open()
            && !self.launcher_options.editor_open()
            && !egui::Popup::is_any_open(ctx)
        {
            self.return_from_child(ctx);
        }
        self.cache
            .retain(|_, entry| entry.seen.saturating_add(2) >= frame);
        moving |= self.motion.is_moving();
        if moving || result.apply || self.closing {
            ctx.request_repaint();
        }
        if !self.closing
            && !child_open
            && frame > self.opened_frame
            && ctx.input(|input| {
                input.pointer.any_pressed()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|point| !outer.contains(point))
            })
        {
            self.closing = true;
            ctx.request_repaint();
        }
        result.integration = self.integration_request.take();
        result.developer = self.developer_action.take();
        result.sidebar_changed = std::mem::take(&mut self.sidebar_changed);
        if result.sidebar_changed {
            ctx.request_repaint();
        }
        if result.apply {
            theme.remember_active_palette();
        }
        if let Some(id) = restore_child_focus
            && !self.closing
            && !self.launcher_options.editor_open()
            && !self.scheme_menu.active()
            && !self.color_picker.is_open()
            && ctx.memory(|memory| memory.focused().is_none_or(|focused| focused == id))
        {
            // Native popups retain their modal layer for one extra pass. A
            // single bounded retry lets a covered native control keep focus.
            crate::keyboard::request_id(ctx, id);
        }
        result
    }

    fn control(
        &mut self,
        ui: &mut Ui,
        theme: &mut Theme,
        paint: &Theme,
        rect: Rect,
        option: OptionId,
        activate: bool,
        opacity: f32,
    ) -> bool {
        let mut changed = false;
        {
            // The option row already owns layout space. A non-allocating child
            // keeps the control from moving the virtual list's row cursor.
            let mut child = ui.new_child(
                UiBuilder::new()
                    .id_salt(("option-control", option))
                    .max_rect(rect),
            );
            let ui = &mut child;
            ui.multiply_opacity(opacity);
            ui.spacing_mut().slider_width =
                (rect.width() - 52.0 * paint.metrics().scale).max(paint.metrics().control);
            ui.spacing_mut().interact_size.y = paint.metrics().control;
            ui.visuals_mut().widgets.inactive.bg_fill = Theme::color(paint.chrome);
            if is_theme_option(option) {
                return self.theme_control(ui, theme, paint, rect, option, activate);
            }
            let response = match option {
                OptionId::Updates => {
                    let response = foldex_ui::components::text_button_at(
                        ui,
                        rect,
                        ui.id().with("updates"),
                        paint,
                        "Open",
                        false,
                    );
                    self.control_focus(ui, option, &response);
                    if response.clicked() || activate {
                        self.updates_requested = true;
                        self.closing = true;
                    }
                    return false;
                }
                OptionId::DeveloperMode => {
                    let mut value = crate::friendly::developer_mode();
                    let response = foldex_ui::components::checkbox(ui, paint, &mut value, "");
                    self.control_focus(ui, option, &response);
                    if activate {
                        value = !value;
                    }
                    if response.changed() || activate {
                        // The saved session picks the flag up on its next signature check.
                        crate::friendly::set_developer_mode(value);
                        self.refilter_pending = true;
                        self.selected = Some(option);
                        ui.ctx().request_repaint();
                    }
                    return false;
                }
                OptionId::PerformanceDetails | OptionId::ErrorLog => {
                    let response = foldex_ui::components::text_button_at(
                        ui,
                        rect,
                        ui.id().with("developer"),
                        paint,
                        if option == OptionId::ErrorLog {
                            "Show"
                        } else {
                            "Open"
                        },
                        false,
                    );
                    self.control_focus(ui, option, &response);
                    if response.clicked() || activate {
                        self.developer_action = Some(if option == OptionId::ErrorLog {
                            DeveloperAction::ErrorLog
                        } else {
                            DeveloperAction::PerformanceDetails
                        });
                        self.selected = Some(option);
                    }
                    return false;
                }
                OptionId::SectionBookmarks
                | OptionId::SectionStorage
                | OptionId::SectionPlaces
                | OptionId::SectionNetwork
                | OptionId::SectionRecents => {
                    let section = option_section(option).unwrap();
                    let mut value = self.sidebar_preferences.sections.visible(section);
                    let response = foldex_ui::components::checkbox(ui, paint, &mut value, "");
                    self.control_focus(ui, option, &response);
                    if response.changed() || activate {
                        self.sidebar_preferences.sections.toggle(section);
                        self.sidebar_changed = true;
                        self.selected = Some(option);
                    }
                    return false;
                }
                OptionId::StorageUsage => {
                    let mut value = self.sidebar_preferences.storage_usage;
                    let response = foldex_ui::components::checkbox(ui, paint, &mut value, "");
                    self.control_focus(ui, option, &response);
                    if activate {
                        value = !value;
                    }
                    if response.changed() || activate {
                        self.sidebar_preferences.storage_usage = value;
                        self.sidebar_changed = true;
                        self.selected = Some(option);
                    }
                    return false;
                }
                OptionId::PlaceUser
                | OptionId::PlaceDesktop
                | OptionId::PlaceDownloads
                | OptionId::PlaceDocuments
                | OptionId::PlacePictures
                | OptionId::PlaceMusic
                | OptionId::PlaceVideos => {
                    let kind = option_place(option).unwrap();
                    let mut value = self.sidebar_preferences.places.visible(kind);
                    let response = foldex_ui::components::checkbox(ui, paint, &mut value, "");
                    self.control_focus(ui, option, &response);
                    if response.changed() || activate {
                        self.sidebar_preferences.places.toggle(kind);
                        self.sidebar_changed = true;
                        self.selected = Some(option);
                    }
                    return false;
                }
                OptionId::LauncherBackground
                | OptionId::LauncherClipboard
                | OptionId::LauncherOpenFolder
                | OptionId::LauncherFolders
                | OptionId::LauncherHistory => {
                    // Live launcher controls are supplied by ui_with_services.
                    return false;
                }
                OptionId::DefaultExplorer => {
                    if self.integration_pending {
                        let mut enabled = self.integration_target.unwrap_or_else(|| {
                            self.integration_status
                                .is_some_and(|s| s.enabled || s.partial)
                        });
                        ui.disable();
                        foldex_ui::components::checkbox(
                            ui,
                            paint,
                            &mut enabled,
                            if self.integration_target.is_some() {
                                "Updating…"
                            } else {
                                "Checking…"
                            },
                        );
                        return false;
                    }
                    if let Some(error) = &self.integration_error {
                        let response = foldex_ui::components::text_button_at(
                            ui,
                            rect,
                            ui.id().with("retry"),
                            paint,
                            "Retry",
                            false,
                        )
                        .on_hover_text(error.as_str());
                        self.control_focus(ui, option, &response);
                        if response.clicked() || activate {
                            self.request_integration(None);
                        }
                        return false;
                    }
                    let status = self.integration_status.unwrap_or_default();
                    let mut enabled = status.enabled || status.partial;
                    let label = if status.partial {
                        "Partial"
                    } else if enabled {
                        "On"
                    } else {
                        "Off"
                    };
                    let response = foldex_ui::components::checkbox(ui, paint, &mut enabled, label);
                    self.control_focus(ui, option, &response);
                    if activate {
                        enabled = !enabled;
                    }
                    if response.changed() || activate {
                        self.request_integration(Some(enabled));
                        self.selected = Some(option);
                    }
                    if status.partial {
                        response.on_hover_text("Some Windows registrations changed. Turn off to restore Foldex's remaining registrations.");
                    }
                    // This changes the Windows registration, not Theme preferences.
                    return false;
                }
                OptionId::FontSize | OptionId::InspectorFontSize => {
                    let value = if option == OptionId::FontSize {
                        &mut theme.font_size
                    } else {
                        &mut theme.inspector_font_size
                    };
                    let mut points = *value * 0.75;
                    let range = if option == OptionId::FontSize {
                        7.5..=18.0
                    } else {
                        6.0..=21.0
                    };
                    let response = ui.add_sized(
                        rect.size(),
                        egui::DragValue::new(&mut points)
                            .speed(0.25)
                            .range(range)
                            .max_decimals(2)
                            .suffix(" pt"),
                    );
                    if response.changed() {
                        *value = points / 0.75;
                        changed = true;
                    }
                    foldex_ui::components::focus_ring(
                        ui,
                        response.rect,
                        response.id,
                        paint,
                        response.has_focus(),
                    );
                    response
                }
                OptionId::Spacing => foldex_ui::components::slider(
                    ui,
                    paint,
                    "Spacing",
                    &mut theme.gap,
                    0.0..=32.0,
                    1.0,
                ),
                OptionId::RowHeight => foldex_ui::components::slider(
                    ui,
                    paint,
                    "Row Height",
                    &mut theme.row_height,
                    18.0..=64.0,
                    1.0,
                ),
                OptionId::SidebarWidth => foldex_ui::components::slider(
                    ui,
                    paint,
                    "Sidebar Width",
                    &mut theme.sidebar_width,
                    160.0..=500.0,
                    1.0,
                ),
                OptionId::CornerRadius => foldex_ui::components::slider(
                    ui,
                    paint,
                    "Corner Radius",
                    &mut theme.radius,
                    0..=24,
                    1.0,
                ),
                OptionId::MotionDuration => foldex_ui::components::slider(
                    ui,
                    paint,
                    "Motion Response",
                    &mut theme.motion_ms,
                    0.0..=1000.0,
                    5.0,
                ),
                OptionId::Animations => {
                    let mut enabled = !theme.reduced_motion && theme.motion_ms > 0.0;
                    if activate {
                        enabled = !enabled;
                    }
                    let response = foldex_ui::components::checkbox(ui, paint, &mut enabled, "");
                    if response.changed() || activate {
                        theme.reduced_motion = !enabled;
                        if enabled && theme.motion_ms <= 0.0 {
                            theme.motion_ms = self.last_motion.max(1.0);
                        }
                        changed = true;
                    }
                    response
                }
                OptionId::RoundCorners => {
                    let mut enabled = theme.radius > 0;
                    if activate {
                        enabled = !enabled;
                    }
                    let response = foldex_ui::components::checkbox(ui, paint, &mut enabled, "");
                    if response.changed() || activate {
                        theme.radius = if enabled { self.last_radius.max(1) } else { 0 };
                        changed = true;
                    }
                    response
                }
                OptionId::Reset => {
                    let response = foldex_ui::components::text_button_at(
                        ui,
                        rect,
                        ui.id().with("reset"),
                        paint,
                        "Reset",
                        false,
                    );
                    if response.clicked() || activate {
                        *theme = Theme::default();
                        theme.resolve_system(ui.ctx().system_theme());
                        changed = true;
                    }
                    response
                }
                color => {
                    let Some(value) = color_token(theme, color) else {
                        return false;
                    };
                    let response = foldex_ui::color::color_button(
                        ui,
                        rect,
                        ui.id().with("color"),
                        paint,
                        *value,
                        activate || self.color_picker.is_open() && self.color_option == Some(color),
                    );
                    if response.clicked() || activate {
                        self.color_option = Some(color);
                        self.child_return_focus = Some(response.id);
                        response.request_focus();
                        self.color_picker.open(*value);
                        ui.ctx().request_repaint();
                    }
                    response
                }
            };
            self.control_focus(ui, option, &response);
            if activate
                && !matches!(
                    option,
                    OptionId::Animations
                        | OptionId::RoundCorners
                        | OptionId::Reset
                        | OptionId::ColorScheme
                )
            {
                response.request_focus();
            }
            if response.has_focus() {
                self.selected = Some(option);
                ui.scroll_to_rect(rect.expand2(vec2(0.0, 7.0 * paint.metrics().scale)), None);
            }
            changed |= response.changed();
        }
        changed
    }
}

fn option_section(option: OptionId) -> Option<crate::sidebar::Section> {
    use crate::sidebar::Section;
    Some(match option {
        OptionId::SectionBookmarks => Section::Bookmarks,
        OptionId::SectionStorage => Section::Storage,
        OptionId::SectionPlaces => Section::Places,
        OptionId::SectionNetwork => Section::Network,
        OptionId::SectionRecents => Section::Recents,
        _ => return None,
    })
}

fn option_place(option: OptionId) -> Option<crate::sidebar::PlaceKind> {
    use crate::sidebar::PlaceKind;
    Some(match option {
        OptionId::PlaceUser => PlaceKind::User,
        OptionId::PlaceDesktop => PlaceKind::Desktop,
        OptionId::PlaceDownloads => PlaceKind::Downloads,
        OptionId::PlaceDocuments => PlaceKind::Documents,
        OptionId::PlacePictures => PlaceKind::Pictures,
        OptionId::PlaceMusic => PlaceKind::Music,
        OptionId::PlaceVideos => PlaceKind::Videos,
        _ => return None,
    })
}

struct Geometry {
    outer: Rect,
    header: f32,
    footer: f32,
    row: f32,
    control: f32,
}
fn geometry(screen: Rect, count: usize, font_size: f32) -> Geometry {
    let scale = foldex_ui::components::Metrics::new(font_size).scale;
    let width = (WIDTH * scale).min((screen.width() - 30.0 * scale).max(1.0));
    let header = HEADER * scale;
    let footer = FOOTER * scale;
    let row = ROW * scale;
    let top = (screen.top() + 32.0 + 47.0 * scale)
        .min(screen.bottom() - (header + footer + row).min(screen.height()) - 12.0)
        .max(screen.top());
    let available = (screen.bottom() - top - 12.0).max(1.0);
    let body = (count.max(1) as f32 * row)
        .min(MAX_BODY * scale)
        .min((available - header - footer).max(0.0));
    let height = (header + body + footer).min(available);
    Geometry {
        outer: Rect::from_min_size(
            pos2(screen.center().x - width * 0.5, top),
            vec2(width, height),
        ),
        header,
        footer,
        row,
        control: 160.0 * scale,
    }
}

fn color_token(theme: &mut Theme, option: OptionId) -> Option<&mut [u8; 3]> {
    Some(match option {
        OptionId::Background => &mut theme.background,
        OptionId::Sidebar => &mut theme.sidebar,
        OptionId::Chrome => &mut theme.chrome,
        OptionId::Surface => &mut theme.surface,
        OptionId::Accent => &mut theme.accent,
        OptionId::Selection => &mut theme.selection,
        OptionId::Text => &mut theme.text,
        OptionId::Muted => &mut theme.muted,
        OptionId::Border => &mut theme.border,
        OptionId::Folders => &mut theme.folder,
        OptionId::Hover => &mut theme.hover,
        OptionId::Danger => &mut theme.danger,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn pump(
        popup: &mut OptionsPopup,
        theme: &mut Theme,
        ctx: &egui::Context,
        frame: usize,
        events: Vec<egui::Event>,
    ) -> (OptionsResponse, egui::FullOutput) {
        let mut response = OptionsResponse::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0))),
                time: Some(frame as f64 / 60.0),
                events,
                ..Default::default()
            },
            |ui| {
                if !response.closed {
                    let next = popup.ui(ui.ctx(), theme);
                    response.apply |= next.apply;
                    response.save |= next.save;
                    response.sidebar_changed |= next.sidebar_changed;
                    response.closed |= next.closed;
                    response.updates |= next.updates;
                    if next.integration.is_some() {
                        response.integration = next.integration;
                    }
                }
            },
        );
        output.textures_delta.clear(); // Headless layout/input test has no renderer.
        (response, output)
    }
    fn key(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn opening_updates_releases_options_navigation_after_its_close_animation() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let _ = pump(&mut popup, &mut theme, &ctx, 0, vec![]);
        popup.query = "Application Updates".into();
        popup.refilter();
        for frame in 1..5 {
            let _ = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        popup.activate = Some(OptionId::Updates);
        let mut requested = false;
        for frame in 5..30 {
            let (response, _) = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
            if response.updates {
                assert!(response.closed);
                assert!(!popup.owns_navigation(&ctx));
                requested = true;
                break;
            }
        }
        assert!(requested, "the update dialog was never requested");
    }

    #[test]
    fn options_slider_retains_its_arrow_keys_and_current_selection() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let _ = pump(&mut popup, &mut theme, &ctx, 0, vec![]);
        popup.query = "spacing".into();
        popup.refilter();
        for frame in 1..5 {
            let _ = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        popup.activate = Some(OptionId::Spacing);
        let _ = pump(&mut popup, &mut theme, &ctx, 5, vec![]);
        let _ = pump(&mut popup, &mut theme, &ctx, 6, vec![]);
        let before = theme.gap;
        let _ = pump(&mut popup, &mut theme, &ctx, 7, vec![key(Key::ArrowRight)]);
        assert!(
            theme.gap > before,
            "the slider must receive its own arrow key"
        );
        assert_eq!(popup.selected, Some(OptionId::Spacing));
        assert!(!popup.focus);
    }

    #[test]
    fn options_tab_reaches_an_offscreen_selected_control_without_activation() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        for frame in 0..5 {
            let _ = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        popup.selected = Some(OptionId::Border);
        popup.scroll_to = true;
        popup.focus = false;
        ctx.memory_mut(|memory| {
            memory.request_focus(Id::new("appearance-options-popup").with("results-focus"))
        });
        let before = theme.border;
        let _ = pump(&mut popup, &mut theme, &ctx, 5, vec![key(Key::Tab)]);
        for frame in 6..9 {
            let _ = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        assert_eq!(popup.selected, Some(OptionId::Border));
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(popup.controls[&OptionId::Border].id)
        );
        assert!(!popup.color_picker.is_open());
        assert_eq!(theme.border, before);
    }

    #[test]
    fn options_ime_and_modified_navigation_do_not_activate_or_close_the_popup() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        for frame in 0..5 {
            let _ = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        let selected = popup.selected;
        let mut modified = key(Key::ArrowDown);
        if let egui::Event::Key { modifiers, .. } = &mut modified {
            *modifiers = Modifiers::ALT;
        }
        let _ = pump(&mut popup, &mut theme, &ctx, 5, vec![modified]);
        assert_eq!(popup.selected, selected);
        let (response, _) = pump(
            &mut popup,
            &mut theme,
            &ctx,
            6,
            vec![
                egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: "に".into(),
                    active_range_chars: None,
                }),
                key(Key::Enter),
                key(Key::Escape),
            ],
        );
        assert!(!response.apply && !response.closed);
        assert!(!popup.closing);
    }

    #[test]
    fn prepared_options_take_same_frame_text_before_the_covered_editor_is_drawn() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let previous = Id::new("options-covered-editor");
        let mut old_text = "seed".to_owned();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.add(egui::TextEdit::singleline(&mut old_text).id(previous))
                .request_focus();
        });
        output.textures_delta.clear();
        let mut shortcut = key(Key::Comma);
        if let egui::Event::Key { modifiers, .. } = &mut shortcut {
            *modifiers = Modifiers::CTRL;
        }
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0))),
                time: Some(0.1),
                events: vec![shortcut, egui::Event::Text("typed".into())],
                ..Default::default()
            },
            |ui| {
                popup.prepare_open(ui.ctx());
                ui.add(egui::TextEdit::singleline(&mut old_text).id(previous));
                popup.ui(ui.ctx(), &mut theme);
            },
        );
        output.textures_delta.clear();
        assert_eq!(old_text, "seed");
        assert_eq!(popup.query, "typed");
        assert_eq!(popup.previous_focus, Some(previous));
        assert!(!popup.capture_return_focus);
        let integration = popup.integration_sequence;
        popup.prepare_open(&ctx);
        assert_eq!(
            popup.query, "typed",
            "an active surface must not reset its search"
        );
        assert_eq!(popup.integration_sequence, integration);
        assert_eq!(popup.previous_focus, Some(previous));
    }

    #[test]
    fn default_explorer_switch_requests_background_changes_without_saving_theme() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let original = theme.write().unwrap();
        let (opened, _) = pump(&mut popup, &mut theme, &ctx, 0, vec![]);
        let inspect = opened
            .integration
            .expect("opening Options must inspect the live state");
        assert_eq!(inspect.enabled, None);
        popup.finish_integration(inspect.id, Ok(crate::integration::Status::default()));
        popup.query = "default file explorer".into();
        popup.refilter();
        for frame in 1..6 {
            pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        let (enabled, _) = pump(&mut popup, &mut theme, &ctx, 6, vec![key(Key::Enter)]);
        let request = enabled
            .integration
            .expect("Enter must activate the visible switch");
        assert_eq!(request.enabled, Some(true));
        assert!(!enabled.apply && !enabled.save);
        let (busy, _) = pump(&mut popup, &mut theme, &ctx, 7, vec![key(Key::Enter)]);
        assert!(
            busy.integration.is_none(),
            "pending changes must not pile up"
        );
        popup.finish_integration(
            request.id,
            Ok(crate::integration::Status {
                enabled: true,
                partial: false,
            }),
        );
        let (disabled, _) = pump(&mut popup, &mut theme, &ctx, 8, vec![key(Key::Enter)]);
        let off = disabled.integration.unwrap();
        assert_eq!(off.enabled, Some(false));
        assert!(!disabled.apply && !disabled.save);
        popup.finish_integration(
            request.id,
            Ok(crate::integration::Status {
                enabled: true,
                partial: false,
            }),
        );
        assert!(
            popup.integration_pending,
            "stale replies must not finish a newer change"
        );
        popup.finish_integration(off.id, Ok(crate::integration::Status::default()));
        assert!(!popup.integration_pending);
        assert_eq!(theme.write().unwrap(), original);
    }

    #[test]
    fn source_geometry_has_exact_default_rows_and_stays_inside_small_windows() {
        let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0));
        let layout = geometry(screen, OPTIONS.len(), 12.0);
        assert_eq!(
            layout.outer,
            Rect::from_min_size(pos2(205.5, 79.0), vec2(720.0, 465.0))
        );
        assert_eq!(
            (layout.header, layout.row, layout.footer, layout.control),
            (38.0, 42.0, 28.0, 160.0)
        );
        for size in [vec2(160.0, 100.0), vec2(480.0, 260.0), vec2(1200.0, 760.0)] {
            let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
            let layout = geometry(screen, OPTIONS.len(), 24.0);
            assert!(layout.outer.is_finite() && layout.outer.is_positive());
            assert!(screen.contains_rect(layout.outer));
        }
    }

    #[test]
    fn searchable_options_keep_only_working_ids_and_valid_selection() {
        let mut popup = OptionsPopup::default();
        popup.open(0);
        let public = OPTIONS
            .iter()
            .filter(|option| !is_developer_option(option.id))
            .count();
        assert_eq!(popup.rows.len(), public);
        assert_eq!(popup.selected, Some(OptionId::FontSize));
        popup.selected = Some(OptionId::Reset);
        popup.query = "inspector font size".into();
        popup.refilter();
        assert!(
            popup
                .rows
                .iter()
                .any(|(index, _)| OPTIONS[*index].id == OptionId::InspectorFontSize)
        );
        assert_ne!(popup.selected, Some(OptionId::Reset));
        popup.query = "qzqzqz".into();
        popup.refilter();
        assert!(popup.rows.is_empty());
        assert_eq!(popup.selected, None);
        popup.move_selection(1);
        assert_eq!(popup.selected, None);
    }

    #[test]
    fn developer_tools_appear_in_settings_only_while_developer_mode_is_on() {
        crate::friendly::set_developer_mode(false);
        let mut popup = OptionsPopup::default();
        popup.open(0);
        let listed = |popup: &OptionsPopup, id| {
            popup
                .rows
                .iter()
                .any(|(index, _)| OPTIONS[*index].id == id)
        };
        assert!(listed(&popup, OptionId::DeveloperMode));
        assert!(!listed(&popup, OptionId::PerformanceDetails));
        assert!(!listed(&popup, OptionId::ErrorLog));
        popup.query = "developer".into();
        popup.refilter();
        assert!(!listed(&popup, OptionId::PerformanceDetails));
        crate::friendly::set_developer_mode(true);
        popup.refilter();
        assert!(listed(&popup, OptionId::PerformanceDetails));
        assert!(listed(&popup, OptionId::ErrorLog));
        crate::friendly::set_developer_mode(false);
    }

    #[test]
    fn sidebar_options_change_saved_preferences_without_requesting_a_theme_change() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme::default();
        let paint = theme.clone();
        popup.sidebar_preferences.network_expanded = true;
        popup.sidebar_preferences.order.rotate_left(1);
        let before = popup.sidebar_preferences.clone();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(180.0, 28.0));
            assert!(!popup.control(
                ui,
                &mut theme,
                &paint,
                rect,
                OptionId::StorageUsage,
                true,
                1.0
            ));
            assert!(!popup.control(
                ui,
                &mut theme,
                &paint,
                rect,
                OptionId::PlaceMusic,
                true,
                1.0
            ));
            for option in [
                OptionId::SectionBookmarks,
                OptionId::SectionStorage,
                OptionId::SectionPlaces,
                OptionId::SectionNetwork,
                OptionId::SectionRecents,
            ] {
                assert!(!popup.control(ui, &mut theme, &paint, rect, option, true, 1.0));
            }
        });
        output.textures_delta.clear(); // Headless test intentionally has no renderer.
        assert_eq!(
            popup.sidebar_preferences.storage_usage,
            !before.storage_usage
        );
        assert_eq!(popup.sidebar_preferences.places.music, !before.places.music);
        assert_eq!(
            popup.sidebar_preferences.places.documents,
            before.places.documents
        );
        assert!(
            crate::sidebar::Section::DEFAULT_ORDER
                .into_iter()
                .all(|section| !popup.sidebar_preferences.sections.visible(section))
        );
        assert_eq!(popup.sidebar_preferences.order, before.order);
        assert_eq!(
            popup.sidebar_preferences.network_expanded,
            before.network_expanded
        );
        assert!(popup.sidebar_changed);
        assert_eq!(theme.write().unwrap(), paint.write().unwrap());
    }

    #[test]
    fn sidebar_section_visibility_is_searchable_and_keyboard_operated() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let original = theme.write().unwrap();
        popup.sidebar_preferences.network_expanded = true;
        for frame in 0..3 {
            pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        popup.query = "sidebar network".into();
        popup.refilter();
        assert!(
            popup
                .rows
                .iter()
                .any(|(index, _)| OPTIONS[*index].id == OptionId::SectionNetwork)
        );
        popup.selected = Some(OptionId::SectionNetwork);
        popup.scroll_to = true;
        for frame in 3..7 {
            pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        let (hidden, _) = pump(&mut popup, &mut theme, &ctx, 7, vec![key(Key::Enter)]);
        assert!(hidden.sidebar_changed);
        assert!(!hidden.apply && !hidden.save);
        assert!(!popup.sidebar_preferences.sections.network);
        assert!(popup.sidebar_preferences.network_expanded);
        let (shown, _) = pump(&mut popup, &mut theme, &ctx, 8, vec![key(Key::Enter)]);
        assert!(shown.sidebar_changed);
        assert!(popup.sidebar_preferences.sections.network);
        assert_eq!(theme.write().unwrap(), original);
    }

    #[test]
    fn schemes_change_colors_without_resetting_metrics_or_motion_preferences() {
        let mut theme = Theme {
            font_size: 17.0,
            inspector_font_size: 19.0,
            row_height: 33.0,
            sidebar_width: 320.0,
            radius: 9,
            gap: 11.0,
            motion_ms: 200.0,
            reduced_motion: true,
            ..Theme::default()
        };
        theme.set_scheme(Theme::midnight().scheme);
        assert_eq!(theme.name, "Midnight");
        assert_eq!(theme.background, Theme::midnight().background);
        assert_eq!(
            (
                theme.font_size,
                theme.inspector_font_size,
                theme.row_height,
                theme.sidebar_width
            ),
            (17.0, 19.0, 33.0, 320.0)
        );
        assert_eq!(
            (
                theme.radius,
                theme.gap,
                theme.motion_ms,
                theme.reduced_motion
            ),
            (9, 11.0, 200.0, true)
        );
    }

    #[test]
    fn search_focus_survives_area_sizing_and_rows_remain_viewport_bounded() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme::default();
        for frame in 0..40 {
            let _ = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
            assert!(popup.cache.len() <= MAX_CACHE);
            assert!(popup.motion.retained_items() <= MAX_CACHE);
        }
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("appearance-options-popup").with("search"))
        );
        assert!(!popup.focus);
        assert!(popup.cache.len() < OPTIONS.len());
        assert!(!popup.motion.is_moving());
        let _ = pump(
            &mut popup,
            &mut theme,
            &ctx,
            40,
            vec![egui::Event::Text("inspector".into())],
        );
        assert_eq!(popup.query, "inspector");
        assert!(popup.rows.len() < OPTIONS.len());

        // Reopening an already-sized Area by clicking the options button must
        // keep focus after the opening pointer event has finished routing.
        theme.reduced_motion = true;
        let (response, _) = pump(&mut popup, &mut theme, &ctx, 41, vec![key(Key::Escape)]);
        assert!(response.closed);
        let pointer = pos2(100.0, 20.0);
        let _ = pump(
            &mut popup,
            &mut theme,
            &ctx,
            42,
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        let _ = pump(
            &mut popup,
            &mut theme,
            &ctx,
            43,
            vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        for frame in 44..48 {
            let _ = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("appearance-options-popup").with("search"))
        );
        assert!(!popup.focus);
        let _ = pump(
            &mut popup,
            &mut theme,
            &ctx,
            48,
            vec![egui::Event::Text("spacing".into())],
        );
        assert_eq!(popup.query, "spacing");
    }

    #[test]
    fn keyboard_navigation_uses_real_boolean_control_and_escape_settles() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme::default();
        for frame in 0..20 {
            let _ = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        for frame in 20..23 {
            let _ = pump(
                &mut popup,
                &mut theme,
                &ctx,
                frame,
                vec![key(Key::ArrowDown)],
            );
        }
        assert_eq!(popup.selected, Some(OptionId::Animations));
        let (response, _) = pump(&mut popup, &mut theme, &ctx, 23, vec![key(Key::Enter)]);
        assert!(response.apply);
        assert!(theme.reduced_motion);
        let (response, _) = pump(&mut popup, &mut theme, &ctx, 24, vec![key(Key::Escape)]);
        assert!(response.closed); // Reduced motion closes in the same frame.
        assert!(!popup.active);
    }

    #[test]
    fn animated_close_releases_focus_and_has_no_idle_repaint_loop() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme::default();
        for frame in 0..30 {
            let _ = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
        }
        let _ = pump(&mut popup, &mut theme, &ctx, 30, vec![key(Key::Escape)]);
        for frame in 31..100 {
            let (response, _) = pump(&mut popup, &mut theme, &ctx, frame, vec![]);
            if response.closed {
                assert!(!popup.active);
                assert!(popup.cache.is_empty());
                for tail in 0..5 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            time: Some((frame + tail + 1) as f64 / 60.0),
                            ..Default::default()
                        },
                        |_| {},
                    );
                    output.textures_delta.clear();
                    if !ctx.has_requested_repaint() {
                        assert_eq!(
                            output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
                            Duration::MAX
                        );
                        return;
                    }
                }
                panic!("closed options popup kept requesting frames");
            }
        }
        panic!("options close animation never settled");
    }
}
