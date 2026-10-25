//! Native, bounded command palette. Execution belongs to the application shell.
use egui::{
    Align2, Color32, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, UiBuilder, pos2,
    vec2,
};
use foldex_match::Matcher;
use foldex_ui::{
    Icon, Theme, icon,
    motion::{LayoutMotion, MotionGains, MotionProfile},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

const MAX_COMMANDS: usize = 128;
const ROW: f32 = 42.0;
const GROUP: f32 = 24.0;

/// Popup shortcuts must not eat modified editing/navigation keys. egui's
/// `consume_key` deliberately ignores extra Shift/Alt modifiers.
pub(crate) fn take_key(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> bool {
    let taken = ctx.input_mut(|input| {
        let mut taken = false;
        input.events.retain(|event| {
            let matches = matches!(event, egui::Event::Key {
                key: pressed_key, pressed: true, modifiers: pressed_modifiers, ..
            } if *pressed_key == key && pressed_modifiers.matches_exact(modifiers));
            taken |= matches;
            !matches
        });
        taken
    });
    if taken
        && matches!(
            key,
            Key::Tab | Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown
        )
    {
        ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
    }
    taken
}

/// A bounded focus scope for the custom search surfaces. Child buttons belong
/// to the scope too, so closing from a button restores the invoking control.
#[derive(Default)]
pub(crate) struct SurfaceFocus {
    active: bool,
    previous: Option<Id>,
    return_to: Option<Id>,
    composing: bool,
    ime_frame: Option<u64>,
    ime_input: bool,
    member_ids: Vec<Id>,
}
impl SurfaceFocus {
    /// Keep the invoker deferred while the closing surface still disables the
    /// underlying editor. An explicitly released surface must stay released.
    pub(crate) fn update_animated(
        &mut self,
        ctx: &egui::Context,
        initial: Id,
        members: &[Id],
        closing: bool,
        finished: bool,
    ) {
        let active = !finished && (!closing || self.active);
        self.update(ctx, initial, members, active);
    }

    pub(crate) fn update(
        &mut self,
        ctx: &egui::Context,
        initial: Id,
        members: &[Id],
        active: bool,
    ) {
        if active && !self.active {
            self.previous = self.return_to.take().or_else(|| {
                ctx.memory(|memory| memory.focused())
                    .filter(|id| !members.contains(id))
            });
            ctx.memory_mut(|memory| memory.request_focus(initial));
            self.active = true;
        } else if !active && self.active {
            if ctx.memory(|memory| {
                memory
                    .focused()
                    .is_none_or(|id| members.contains(&id) || self.member_ids.contains(&id))
            }) {
                if let Some(previous) = self.previous.take() {
                    // This helper schedules restoration across the closing
                    // modal layer. Call it outside the egui memory lock.
                    foldex_ui::components::return_focus(ctx, previous);
                } else {
                    ctx.memory_mut(|memory| {
                        if let Some(focused) = memory.focused() {
                            memory.surrender_focus(focused);
                        }
                    });
                }
            }
            self.active = false;
            self.composing = false;
        }
        if active {
            if ctx.memory(|memory| memory.focused().is_none()) && !self.composing {
                ctx.memory_mut(|memory| memory.request_focus(initial));
            }
            if ctx.memory(|memory| {
                memory
                    .focused()
                    .is_some_and(|id| self.member_ids.contains(&id) && !members.contains(&id))
            }) {
                ctx.memory_mut(|memory| memory.request_focus(initial));
            }
            self.member_ids.clear();
            self.member_ids.extend_from_slice(members);
            for &id in members {
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
        }
        let frame = ctx.cumulative_frame_nr();
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
    }
    pub(crate) fn accepts_keys(&self, ctx: &egui::Context, members: &[Id]) -> bool {
        self.active
            && !self.ime_input
            && ctx.memory(|memory| memory.focused().is_some_and(|id| members.contains(&id)))
    }
    pub(crate) fn set_return_focus(&mut self, id: Id) {
        if self.active {
            self.previous = Some(id);
        } else {
            self.return_to = Some(id);
        }
    }
    pub(crate) fn redirect_text(&self, ctx: &egui::Context, list: Id, input: Id) {
        let text_input = ctx.input(|input| {
            let activating_space = input.events.iter().any(|event| {
                matches!(event,
                egui::Event::Key { key: Key::Space, pressed: true, modifiers, .. }
                if modifiers.matches_exact(Modifiers::NONE))
            });
            input.events.iter().any(|event| match event {
                egui::Event::Text(text) => !activating_space || text != " ",
                egui::Event::Paste(_) | egui::Event::Ime(_) => true,
                _ => false,
            })
        });
        if ctx.memory(|memory| memory.has_focus(list)) && text_input {
            ctx.memory_mut(|memory| memory.request_focus(input));
        }
    }
    pub(crate) fn tab(&self, ctx: &egui::Context, members: &[Id]) -> bool {
        if !self.accepts_keys(ctx, members) || members.is_empty() {
            return false;
        }
        let backwards = take_key(ctx, Modifiers::SHIFT, Key::Tab);
        if backwards || take_key(ctx, Modifiers::NONE, Key::Tab) {
            let at = ctx
                .memory(|memory| memory.focused())
                .and_then(|id| members.iter().position(|member| *member == id))
                .unwrap_or(0);
            let next = if backwards {
                (at + members.len() - 1) % members.len()
            } else {
                (at + 1) % members.len()
            };
            ctx.memory_mut(|memory| memory.request_focus(members[next]));
            return true;
        }
        false
    }
}

pub(crate) fn result_surface(ui: &egui::Ui, rect: Rect, id: Id, label: &str) {
    foldex_ui::components::PopupFocus::surface(ui, rect, id, label);
    ui.ctx().accesskit_node_builder(id, |node| {
        node.set_role(egui::accesskit::Role::ListBox);
    });
}

pub(crate) fn result_input(ctx: &egui::Context, input: Id, list: Id, expanded: bool) {
    ctx.accesskit_node_builder(input, |node| {
        node.set_role(egui::accesskit::Role::EditableComboBox);
        node.set_expanded(expanded);
        node.set_controls(vec![list.accesskit_id()]);
    });
}

pub(crate) fn result_row(
    ui: &egui::Ui,
    response: &egui::Response,
    list: Id,
    input: Id,
    label: &str,
    selected: bool,
    enabled: bool,
) {
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled() && enabled,
            selected,
            label,
        )
    });
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::ListBoxOption);
    });
    if selected && response.rect.intersects(ui.clip_rect()) {
        for id in [list, input] {
            ui.ctx().accesskit_node_builder(id, |node| {
                node.set_active_descendant(response.id.accesskit_id())
            });
        }
    }
}

/// The shared choice-row appearance with mouse-only hits. Keyboard focus stays
/// on the virtualized results composite rather than on transient row widgets.
#[allow(clippy::too_many_arguments)]
pub(crate) fn result_choice_row(
    ui: &egui::Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    label: &str,
    glyph: Icon,
    selected: bool,
    trailing: Option<Icon>,
) -> egui::Response {
    let response = ui.interact(rect.intersect(ui.clip_rect()), id, Sense::CLICK);
    let hover = foldex_ui::animated_state(
        ui.ctx(),
        id.with("hover"),
        response.hovered(),
        t,
        MotionProfile::Fast,
    );
    let selection = foldex_ui::animated_state(
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
    foldex_ui::components::elided_label(
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
    response
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Back,
    Forward,
    Parent,
    GoTo,
    Commands,
    EditAddress,
    Filter,
    ExpandedSearch,
    ToggleInspector,
    ToggleSidebar,
    SplitRight,
    SplitBelow,
    ClosePanel,
    NextPanel,
    PreviousPanel,
    NewTab,
    DuplicateTab,
    NewWindow,
    MoveTabToNewWindow,
    CloseTab,
    ReopenTab,
    SearchTabs,
    Refresh,
    ToggleHidden,
    ViewDetails,
    ViewList,
    ViewColumns,
    ViewMedium,
    ViewLarge,
    ViewExtraLarge,
    Options,
    Launcher,
    LauncherSettings,
    ReloadTheme,
    OpenExplorer,
    OpenConsole,
    Fullscreen,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    Rename,
    Recycle,
    DeletePermanent,
    NewFolder,
    SelectAll,
    CopyPath,
    SaveLayout,
    LoadLayout,
    SaveFolderView,
    InspectorDetails,
    NewFile,
    InvertSelection,
    Hotkeys,
    #[serde(rename = "next-focus-region")]
    NextFocusRegion,
    #[serde(rename = "previous-focus-region")]
    PreviousFocusRegion,
    #[serde(rename = "keyboard-navigation-help")]
    KeyboardHelp,
    Properties,
}

#[derive(Clone, Copy, Debug)]
pub struct CommandEntry {
    pub command: Command,
    pub title: &'static str,
    pub description: &'static str,
    pub shortcut: &'static str,
    pub keywords: &'static str,
    pub enabled: bool,
}

macro_rules! entry {
    ($command:ident, $title:literal, $description:literal, $shortcut:literal, $keywords:literal, $enabled:literal) => {
        CommandEntry {
            command: Command::$command,
            title: $title,
            description: $description,
            shortcut: $shortcut,
            keywords: $keywords,
            enabled: $enabled,
        }
    };
}

/// An application can override availability or register replacement descriptions.
/// Optional source features are visible but disabled until their execution exists.
pub const COMMANDS: &[CommandEntry] = &[
    entry!(
        Commands,
        "Commands",
        "Find an action by name, shortcut or description",
        "Ctrl+Shift+P",
        "command palette actions",
        true
    ),
    entry!(
        GoTo,
        "GoTo",
        "Search common locations and recently opened paths",
        "Ctrl+P",
        "navigate jump folder location",
        true
    ),
    entry!(
        EditAddress,
        "Open Address Bar",
        "Edit the current folder path in GoTo",
        "Ctrl+L",
        "address path location",
        true
    ),
    entry!(
        Back,
        "Go Back",
        "Return to the previous folder",
        "Alt+Left",
        "history previous",
        true
    ),
    entry!(
        Forward,
        "Go Forward",
        "Visit the next folder in history",
        "Alt+Right",
        "history next",
        true
    ),
    entry!(
        Parent,
        "Go Up",
        "Open the parent folder",
        "Alt+Up",
        "parent directory",
        true
    ),
    entry!(
        Filter,
        "Filter",
        "Find items in the current view",
        "Ctrl+F",
        "search find local",
        true
    ),
    entry!(
        ExpandedSearch,
        "Expanded Search",
        "Search files and folders through subfolders",
        "Ctrl+E",
        "recursive expand flatten",
        true
    ),
    entry!(
        ToggleInspector,
        "Inspector",
        "Show or hide the selection preview",
        "Space",
        "preview image text metadata",
        true
    ),
    entry!(
        ToggleSidebar,
        "Quick Access",
        "Show or hide the navigation sidebar",
        "Ctrl+B",
        "sidebar bookmarks favorites",
        true
    ),
    entry!(
        SplitRight,
        "Split Right",
        "Open another panel to the right",
        "Ctrl+Shift+S",
        "pane split horizontal",
        true
    ),
    entry!(
        SplitBelow,
        "Split Below",
        "Open another panel below",
        "Ctrl+S",
        "pane split vertical bottom",
        true
    ),
    entry!(
        ClosePanel,
        "Close Panel",
        "Close the secondary panel",
        "",
        "pane split remove",
        true
    ),
    entry!(
        NextPanel,
        "Next Panel",
        "Move focus to the next panel",
        "Ctrl+Alt+Right",
        "pane focus right",
        true
    ),
    entry!(
        PreviousPanel,
        "Previous Panel",
        "Move focus to the previous panel",
        "Ctrl+Alt+Left",
        "pane focus left",
        true
    ),
    entry!(
        NewTab,
        "New Tab",
        "Open the current folder in another tab",
        "Ctrl+T",
        "create tab",
        true
    ),
    entry!(
        DuplicateTab,
        "Duplicate Tab",
        "Duplicate the active tab's location",
        "",
        "copy tab",
        true
    ),
    entry!(
        NewWindow,
        "Duplicate Tab To New Window",
        "Open the current folder in a separate window",
        "Ctrl+N",
        "new window duplicate tab",
        true
    ),
    entry!(
        MoveTabToNewWindow,
        "Move Tab To New Window",
        "Move this tab and its view to a separate window",
        "",
        "detach tab window",
        true
    ),
    entry!(
        CloseTab,
        "Close Tab",
        "Close the active tab",
        "Ctrl+W",
        "remove tab",
        true
    ),
    entry!(
        ReopenTab,
        "Reopen Closed Tab",
        "Restore the most recently closed tab",
        "Ctrl+Shift+T",
        "undo restore tab",
        false
    ),
    entry!(
        SearchTabs,
        "Search Tabs",
        "Find a tab by its folder name or path",
        "Ctrl+Shift+A",
        "switch tab",
        false
    ),
    entry!(
        Refresh,
        "Refresh",
        "Reload the current folder",
        "F5",
        "reload update rescan",
        true
    ),
    entry!(
        ToggleHidden,
        "Show Hidden Items",
        "Toggle hidden files and folders",
        "Ctrl+H",
        "visibility hidden system",
        true
    ),
    entry!(
        ViewDetails,
        "Details",
        "Show aligned rows with file information columns",
        "Ctrl+Shift+1",
        "view size table",
        true
    ),
    entry!(
        ViewList,
        "List",
        "Show a compact list of files and folders",
        "Ctrl+Shift+2",
        "view size compact",
        true
    ),
    entry!(
        ViewColumns,
        "Columns",
        "Arrange compact rows across columns",
        "Ctrl+Shift+3",
        "view size grid",
        true
    ),
    entry!(
        ViewMedium,
        "M Icons",
        "Use medium icons",
        "Ctrl+Shift+4",
        "view size thumbnail zoom",
        true
    ),
    entry!(
        ViewLarge,
        "L Icons",
        "Use large icons",
        "Ctrl+Shift+5",
        "view size thumbnail zoom",
        true
    ),
    entry!(
        ViewExtraLarge,
        "XL Icons",
        "Use extra large icons",
        "Ctrl+Shift+6",
        "view size thumbnail zoom",
        true
    ),
    entry!(
        Copy,
        "Copy",
        "Copy the selected files and folders",
        "Ctrl+C",
        "clipboard duplicate",
        true
    ),
    entry!(
        Undo,
        "Undo",
        "Undo the latest supported file operation",
        "Ctrl+Z",
        "restore move copy rename recycle new history",
        false
    ),
    entry!(
        Redo,
        "Redo",
        "Redo an undone file operation",
        "Ctrl+Y",
        "repeat restore move copy rename recycle new history",
        false
    ),
    entry!(
        Cut,
        "Cut",
        "Move the selected items through the clipboard",
        "Ctrl+X",
        "clipboard move",
        true
    ),
    entry!(
        Paste,
        "Paste",
        "Paste clipboard items into the current folder",
        "Ctrl+V",
        "clipboard copy move",
        true
    ),
    entry!(
        CopyPath,
        "Copy Paths",
        "Copy the selected items' full paths",
        "Ctrl+Shift+C",
        "clipboard quoted address",
        true
    ),
    entry!(
        Rename,
        "Rename",
        "Change the selected item's name",
        "F2",
        "edit filename",
        true
    ),
    entry!(
        Properties,
        "Properties",
        "Show native Windows properties for the selected items",
        "Alt+Enter",
        "details information attributes",
        true
    ),
    entry!(
        Recycle,
        "Move to Recycle Bin",
        "Send selected items to the Recycle Bin",
        "Delete",
        "trash delete remove",
        true
    ),
    entry!(
        DeletePermanent,
        "Delete permanently",
        "Confirm permanent deletion of selected items; this cannot be undone",
        "Shift+Delete",
        "delete permanently remove erase",
        true
    ),
    entry!(
        NewFolder,
        "New Folder",
        "Create a folder at the current location",
        "Ctrl+Shift+N",
        "create directory",
        true
    ),
    entry!(
        SelectAll,
        "Select All",
        "Select every item in the current view",
        "Ctrl+A",
        "selection files folders",
        true
    ),
    entry!(
        Options,
        "Options",
        "Adjust appearance, motion and application settings",
        "Ctrl+,",
        "settings preferences theme colors design",
        true
    ),
    entry!(
        ReloadTheme,
        "Reload Theme",
        "Reload Foldex's saved colors, typography and component settings",
        "Ctrl+Shift+R",
        "customization theme config design reload refresh",
        true
    ),
    entry!(
        Launcher,
        "Launcher",
        "Search apps, files and windows, calculate, or open clipboard history",
        "",
        "raycast apps files programs calculator clipboard switcher tray",
        true
    ),
    entry!(
        LauncherSettings,
        "Launcher settings",
        "Configure the global shortcut, tray mode and clipboard history",
        "",
        "background shortcut hotkey tray clipboard",
        true
    ),
    entry!(
        OpenExplorer,
        "Open File Explorer",
        "Open the current folder in Windows File Explorer",
        "",
        "windows explorer native shell",
        true
    ),
    entry!(
        OpenConsole,
        "Open Console",
        "Open a console in the current folder",
        "",
        "terminal shell cmd powershell",
        true
    ),
    entry!(
        Fullscreen,
        "Fullscreen",
        "Toggle the full screen window",
        "F11",
        "window maximize",
        true
    ),
    entry!(
        SaveLayout,
        "Save Layout",
        "Save window dimensions, panels and sidebar",
        "",
        "workspace session",
        false
    ),
    entry!(
        LoadLayout,
        "Load Layout",
        "Restore a saved window layout",
        "",
        "workspace session",
        false
    ),
    entry!(
        SaveFolderView,
        "Save Folder View",
        "Remember this folder's view, sort and columns",
        "",
        "layout icons sorting",
        false
    ),
    entry!(
        InspectorDetails,
        "Inspector Extra Details",
        "Show additional selection information",
        "Ctrl+I",
        "preview metadata properties",
        false
    ),
    entry!(
        NewFile,
        "New File",
        "Create a file at the current location",
        "",
        "create document",
        false
    ),
    entry!(
        InvertSelection,
        "Invert Selection",
        "Select items that are currently unselected",
        "",
        "selection toggle",
        false
    ),
    entry!(
        Hotkeys,
        "Hotkeys",
        "View and customize command shortcuts",
        "",
        "keyboard bindings assignment",
        false
    ),
    entry!(
        NextFocusRegion,
        "Focus Next Region",
        "Move keyboard focus to the next navigation or content region",
        "F6",
        "keyboard focus region navigation pane sidebar toolbar",
        true
    ),
    entry!(
        PreviousFocusRegion,
        "Focus Previous Region",
        "Move keyboard focus to the previous navigation or content region",
        "Shift+F6",
        "keyboard focus region navigation pane sidebar toolbar",
        true
    ),
    entry!(
        KeyboardHelp,
        "Keyboard Navigation",
        "Read the keyboard-only guide to focus, selection, menus and forms",
        "",
        "help accessibility keyboard shortcuts tab arrows focus selection menus guide",
        true
    ),
];

#[derive(Clone, Copy, PartialEq)]
struct TextKey {
    fonts: usize,
    font: u32,
    width: u32,
    ppp: u32,
    foreground: Color32,
    muted: Color32,
    accent: Color32,
}
struct RowText {
    key: TextKey,
    title: Arc<egui::Galley>,
    description: Arc<egui::Galley>,
}
struct Row {
    entry: usize,
    score: i32,
    heading: Option<&'static str>,
    y: f32,
    title_indices: Vec<usize>,
    description_indices: Vec<usize>,
    text: Option<RowText>,
}
#[derive(Default)]
struct RowState {
    hover: f32,
    selected: f32,
    star: f32,
}

pub struct Palette {
    entries: Vec<CommandEntry>,
    rows: Vec<Row>,
    bookmarks: HashSet<Command>,
    query: String,
    matcher: Matcher,
    quick_only: bool,
    selected: Option<Command>,
    focus: bool,
    closing: bool,
    height: f32,
    opacity: f32,
    content_height: f32,
    row_height: f32,
    group_height: f32,
    opened_frame: Option<u64>,
    scroll_to: bool,
    motion: LayoutMotion,
    states: HashMap<Command, RowState>,
    shortcut_displays: HashMap<Command, String>,
    focus_scope: SurfaceFocus,
}

impl Default for Palette {
    fn default() -> Self {
        Self::new()
    }
}
impl Palette {
    /// Starts open. Reuse this value across openings to retain bookmarks.
    pub fn new() -> Self {
        let mut this = Self {
            entries: COMMANDS.to_vec(),
            rows: Vec::with_capacity(MAX_COMMANDS),
            bookmarks: HashSet::from([
                Command::GoTo,
                Command::NewTab,
                Command::SplitRight,
                Command::ToggleInspector,
                Command::Options,
            ]),
            query: String::new(),
            matcher: Matcher::new(""),
            quick_only: false,
            selected: None,
            focus: true,
            closing: false,
            height: 0.0,
            opacity: 0.0,
            content_height: 0.0,
            row_height: ROW,
            group_height: GROUP,
            opened_frame: None,
            scroll_to: false,
            motion: LayoutMotion::new(MAX_COMMANDS),
            states: HashMap::with_capacity(MAX_COMMANDS),
            shortcut_displays: HashMap::new(),
            focus_scope: Default::default(),
        };
        this.refilter();
        this
    }
    pub fn open(&mut self) {
        self.closing = false;
        self.focus = true;
        self.opened_frame = None;
        self.query.clear();
        self.refilter();
    }
    pub fn close(&mut self) {
        self.closing = true;
    }
    pub fn set_return_focus(&mut self, id: Id) {
        self.focus_scope.set_return_focus(id);
    }
    pub fn release_focus(&mut self, ctx: &egui::Context) {
        self.focus_scope.update(
            ctx,
            Id::new("command-palette-input"),
            &[
                Id::new("command-palette-input"),
                Id::new("commands-quick"),
                Id::new("commands-close"),
                Id::new("command-palette-results-focus"),
            ],
            false,
        );
    }
    pub fn is_open(&self) -> bool {
        !self.closing
    }
    pub fn finished(&self) -> bool {
        self.closing && self.height <= 0.25 && self.opacity <= 0.01
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.refilter();
    }
    pub fn entries(&self) -> &[CommandEntry] {
        &self.entries
    }
    /// Update visible/searchable shortcut labels in one batch after key edits.
    pub fn set_shortcut_displays<'a>(
        &mut self,
        values: impl IntoIterator<Item = (Command, &'a str)>,
    ) {
        let mut changed = false;
        for (command, label) in values {
            if self.shortcut_displays.get(&command).map(String::as_str) != Some(label) {
                self.shortcut_displays.insert(command, label.to_owned());
                changed = true;
            }
        }
        if changed {
            self.refilter();
        }
    }
    pub fn set_enabled(&mut self, command: Command, enabled: bool) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.command == command)
        {
            if entry.enabled != enabled {
                entry.enabled = enabled;
                self.refilter();
            }
        }
    }
    /// Replace or append a descriptor; all caches remain bounded to128 commands.
    pub fn register(&mut self, entry: CommandEntry) -> bool {
        if entry.title.len() > 256 || entry.description.len() > 512 || entry.keywords.len() > 512 {
            return false;
        }
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|existing| existing.command == entry.command)
        {
            *existing = entry;
        } else if self.entries.len() < MAX_COMMANDS {
            self.entries.push(entry);
        } else {
            return false;
        }
        self.refilter();
        true
    }
    pub fn bookmarks(&self) -> Vec<Command> {
        self.entries
            .iter()
            .filter(|entry| self.bookmarks.contains(&entry.command))
            .map(|entry| entry.command)
            .collect()
    }
    pub fn set_bookmarks(&mut self, commands: impl IntoIterator<Item = Command>) {
        self.bookmarks.clear();
        for command in commands.into_iter().take(MAX_COMMANDS) {
            if self.entries.iter().any(|entry| entry.command == command) {
                self.bookmarks.insert(command);
            }
        }
        self.refilter();
    }
    pub fn toggle_bookmark(&mut self, command: Command) {
        if !self.entries.iter().any(|entry| entry.command == command) {
            return;
        }
        if !self.bookmarks.remove(&command) {
            self.bookmarks.insert(command);
        }
        self.refilter();
    }
    fn refilter(&mut self) {
        self.matcher = Matcher::new(&self.query);
        self.rows.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            if self.quick_only && !self.bookmarks.contains(&entry.command) {
                continue;
            }
            let primary = self.matcher.score(entry.title);
            let shortcut = self
                .shortcut_displays
                .get(&entry.command)
                .map(String::as_str)
                .unwrap_or(entry.shortcut);
            let secondary = [entry.description, shortcut, entry.keywords]
                .into_iter()
                .filter_map(|text| self.matcher.score(text))
                .max();
            if primary.is_none() && secondary.is_none() {
                continue;
            }
            let score =
                primary.unwrap_or(0) + (secondary.unwrap_or(0) as f32 * 0.75).round() as i32;
            self.rows.push(Row {
                entry: index,
                score,
                heading: None,
                y: 0.0,
                title_indices: self.matcher.match_indices(entry.title).unwrap_or_default(),
                description_indices: self
                    .matcher
                    .match_indices(entry.description)
                    .unwrap_or_default(),
                text: None,
            });
        }
        if self.query.is_empty() {
            self.rows.sort_by_key(|row| {
                (
                    !self.bookmarks.contains(&self.entries[row.entry].command),
                    row.entry,
                )
            });
        } else {
            self.rows
                .sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.entry.cmp(&b.entry)));
        }
        let mut y = 0.0;
        let mut previous_group = None;
        for row in &mut self.rows {
            let quick = self.bookmarks.contains(&self.entries[row.entry].command);
            if self.query.is_empty() && previous_group != Some(quick) {
                row.heading = Some(if quick { "Quick" } else { "All commands" });
                y += self.group_height;
                previous_group = Some(quick);
            }
            row.y = y;
            y += self.row_height;
        }
        self.content_height = y.max(self.row_height);
        if !self.rows.iter().any(|row| {
            self.selected == Some(self.entries[row.entry].command)
                && self.entries[row.entry].enabled
        }) {
            self.selected = self
                .rows
                .iter()
                .find(|row| self.entries[row.entry].enabled)
                .map(|row| self.entries[row.entry].command);
        }
        self.scroll_to = true;
    }
    fn move_selection(&mut self, step: isize) {
        if self.rows.is_empty() {
            return;
        }
        let current = self
            .rows
            .iter()
            .position(|row| self.selected == Some(self.entries[row.entry].command));
        let mut index = current.map_or(
            if step > 0 {
                -1
            } else {
                self.rows.len() as isize
            },
            |index| index as isize,
        );
        let direction = step.signum();
        index = (index + step).clamp(0, self.rows.len() as isize - 1);
        for _ in 0..self.rows.len() {
            let entry = &self.entries[self.rows[index as usize].entry];
            if entry.enabled {
                self.selected = Some(entry.command);
                self.scroll_to = true;
                return;
            }
            let next = index + direction;
            if next < 0 || next >= self.rows.len() as isize {
                return;
            }
            index = next;
        }
    }
    fn select_edge(&mut self, last: bool) {
        let row = if last {
            self.rows
                .iter()
                .rev()
                .find(|row| self.entries[row.entry].enabled)
        } else {
            self.rows.iter().find(|row| self.entries[row.entry].enabled)
        };
        self.selected = row.map(|row| self.entries[row.entry].command);
        self.scroll_to = true;
    }

    /// Call while open or closing; remove only after `finished()` becomes true.
    pub fn ui(&mut self, ctx: &egui::Context, t: &Theme) -> Option<Command> {
        let m = t.metrics();
        let scale = m.scale;
        if self.row_height != ROW * scale {
            self.row_height = ROW * scale;
            self.group_height = GROUP * scale;
            self.refilter();
        }
        self.opened_frame.get_or_insert(ctx.cumulative_frame_nr());
        let input_id = Id::new("command-palette-input");
        let list_id = Id::new("command-palette-results-focus");
        let members = [
            input_id,
            Id::new("commands-quick"),
            Id::new("commands-close"),
            list_id,
        ];
        self.focus_scope
            .update_animated(ctx, input_id, &members, self.closing, self.finished());
        if !self.closing {
            self.focus_scope.redirect_text(ctx, list_id, input_id);
            if self.focus_scope.tab(ctx, &members) {
                self.focus = false;
            }
        }
        let mut picked = None;
        let mut execute = false;
        let mut bookmark = false;
        if !self.closing && self.focus_scope.accepts_keys(ctx, &members) {
            let results_focus = ctx.memory(|memory| memory.has_focus(list_id));
            let search_focus = ctx.memory(|memory| memory.has_focus(input_id));
            if results_focus || search_focus {
                let page = ((ctx.content_rect().height() - 175.0 * scale)
                    .clamp(self.row_height, 420.0 * scale)
                    / self.row_height)
                    .floor()
                    .max(1.0) as isize;
                for (key, step) in [
                    (Key::ArrowDown, 1),
                    (Key::ArrowUp, -1),
                    (Key::PageDown, page),
                    (Key::PageUp, -page),
                ] {
                    if take_key(ctx, Modifiers::NONE, key) {
                        self.move_selection(step);
                    }
                }
                if (results_focus || self.query.is_empty())
                    && take_key(ctx, Modifiers::NONE, Key::Home)
                {
                    self.select_edge(false);
                }
                if (results_focus || self.query.is_empty())
                    && take_key(ctx, Modifiers::NONE, Key::End)
                {
                    self.select_edge(true);
                }
                if take_key(ctx, Modifiers::NONE, Key::Enter) {
                    execute = true;
                }
                bookmark = results_focus && take_key(ctx, Modifiers::NONE, Key::Space);
            }
            if take_key(ctx, Modifiers::NONE, Key::Escape) {
                self.close();
            }
        }
        let screen = ctx.content_rect();
        let width = (screen.width() - 40.0 * scale).max(1.0).min(720.0 * scale);
        let body_height = self
            .content_height
            .min((screen.height() - 175.0 * scale).clamp(self.row_height, 420.0 * scale));
        let target = if self.closing {
            0.0
        } else {
            m.popup_header + body_height + 29.0 * scale
        };
        let gains = MotionGains::new(ctx.input(|input| input.stable_dt), t.duration());
        let mut moving = foldex_ui::motion::popup_gains(ctx, t.duration()).step_pixels(
            &mut self.height,
            target,
            MotionProfile::Standard,
        );
        moving |= gains.step_normalized(
            &mut self.opacity,
            if self.closing { 0.0 } else { 1.0 },
            MotionProfile::Standard,
        );
        if self.finished() {
            self.focus_scope.update(ctx, input_id, &members, false);
            return None;
        }
        self.motion.set_pixels_per_point(ctx.pixels_per_point());
        self.motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(ctx, t.duration()));
        let top = (screen.top() + 32.0 + 47.0 * scale)
            .min(screen.bottom() - target.min(screen.height() - 30.0) - 15.0)
            .max(screen.top() + 15.0);
        let outer = Rect::from_min_size(
            pos2(screen.center().x - width / 2.0, top),
            vec2(width, self.height),
        );
        let mut changed = false;
        let mut toggle = bookmark.then_some(self.selected).flatten();
        let mut quick_toggle = false;
        foldex_ui::components::popup_surface(
            ctx,
            egui::Area::new(Id::new("command-palette"))
                .order(egui::Order::Foreground)
                .fixed_pos(outer.min)
                .fade_in(false),
            !self.closing,
            |ui| {
                ui.set_min_size(outer.size());
                ui.set_max_size(outer.size());
                ui.set_clip_rect(outer.intersect(screen));
                ui.set_opacity(self.opacity);
                ui.painter().rect_filled(
                    outer,
                    t.radius.saturating_add(5),
                    Theme::color(t.surface),
                );
                ui.painter().rect_stroke(
                    outer,
                    t.radius.saturating_add(5),
                    Stroke::new(1.0, t.line()),
                    StrokeKind::Inside,
                );
                let layout =
                    foldex_ui::components::PopupLayout::with_body_height(outer, t, body_height);
                let header = layout.header;
                foldex_ui::components::popup_input_icon(ui, header, t, Icon::Search);
                let input = foldex_ui::components::popup_input_rect(header, 2);
                ui.scope_builder(UiBuilder::new().max_rect(input), |ui| {
                    let acquire_focus = self.focus && !self.closing;
                    if acquire_focus && !ui.memory(|memory| memory.has_focus(input_id)) {
                        ui.memory_mut(|memory| memory.request_focus(input_id));
                    }
                    let response = foldex_ui::components::text_input_at(
                        ui,
                        input,
                        input_id,
                        t,
                        &mut self.query,
                        "Select a command…",
                        1024,
                    );
                    if acquire_focus {
                        self.focus = ui.is_sizing_pass() || !response.response.has_focus();
                    }
                    changed = response.response.changed();
                    result_input(ctx, input_id, list_id, !self.closing);
                });
                if changed {
                    self.refilter();
                }
                let quick = foldex_ui::components::popup_action_rect(header, 0, 2);
                if control(
                    ui,
                    t,
                    quick,
                    Id::new("commands-quick"),
                    Icon::Bookmark,
                    self.quick_only,
                    gains,
                    &mut moving,
                )
                .on_hover_text("Show bookmarked commands")
                .clicked()
                {
                    quick_toggle = true;
                }
                let close = foldex_ui::components::popup_action_rect(header, 1, 2);
                if control(
                    ui,
                    t,
                    close,
                    Id::new("commands-close"),
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
                    Stroke::new(1.0, t.line()),
                );
                let body = layout.body;
                let body_height = body.height();
                result_surface(ui, body.intersect(outer), list_id, "Commands");
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(body)
                        .accessibility_parent(list_id),
                    |ui| {
                        ui.set_clip_rect(body.intersect(outer));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let mut scroll = foldex_ui::components::menu_scroll_area_for_content(
                            ui,
                            t,
                            "command-palette-scroll",
                            body_height,
                            self.content_height,
                        );
                        if self.scroll_to && body_height >= self.row_height {
                            let y = self
                                .rows
                                .iter()
                                .find(|row| self.selected == Some(self.entries[row.entry].command))
                                .map_or(0.0, |row| row.y);
                            scroll = scroll.vertical_scroll_offset(
                                (y - body_height / 2.0 + self.row_height / 2.0).max(0.0),
                            );
                            self.scroll_to = false;
                        }
                        scroll.show(ui, |ui| {
                            let content = ui
                                .allocate_exact_size(
                                    vec2(ui.available_width(), self.content_height),
                                    Sense::hover(),
                                )
                                .0;
                            let clip = ui.clip_rect();
                            for row in &mut self.rows {
                                let entry = &self.entries[row.entry];
                                if let Some(heading) = row.heading {
                                    let at = pos2(
                                        content.left() + 10.0 * scale,
                                        content.top() + row.y - self.group_height / 2.0,
                                    );
                                    ui.painter().text(
                                        at,
                                        Align2::LEFT_CENTER,
                                        heading,
                                        FontId::proportional((t.font_size - 1.5).max(10.0)),
                                        t.dim(),
                                    );
                                }
                                let target = Rect::from_min_size(
                                    pos2(0.0, row.y),
                                    vec2(content.width(), self.row_height),
                                );
                                let placement = self.motion.place_in_view(
                                    Id::new(("palette-row", entry.command)),
                                    target,
                                    true,
                                    clip.translate(-content.min.to_vec2()),
                                );
                                let rect = placement.rect.translate(content.min.to_vec2());
                                if !rect.intersects(clip) {
                                    continue;
                                }
                                let response = ui.interact(
                                    rect,
                                    Id::new(("palette-command", entry.command)),
                                    Sense::CLICK,
                                );
                                result_row(
                                    ui,
                                    &response,
                                    list_id,
                                    input_id,
                                    entry.title,
                                    self.selected == Some(entry.command),
                                    entry.enabled,
                                );
                                let state = self.states.entry(entry.command).or_default();
                                moving |= gains.step_normalized(
                                    &mut state.hover,
                                    if entry.enabled && response.hovered() {
                                        1.0
                                    } else {
                                        0.0
                                    },
                                    MotionProfile::Standard,
                                );
                                moving |= gains.step_normalized(
                                    &mut state.selected,
                                    if self.selected == Some(entry.command) {
                                        1.0
                                    } else {
                                        0.0
                                    },
                                    MotionProfile::Standard,
                                );
                                let mut painter = ui.painter().with_clip_rect(rect.intersect(clip));
                                painter.multiply_opacity(
                                    placement.opacity * if entry.enabled { 1.0 } else { 0.45 },
                                );
                                painter.rect_filled(
                                    rect,
                                    t.radius,
                                    Theme::color(t.hover)
                                        .gamma_multiply(state.hover * (1.0 - state.selected)),
                                );
                                painter.rect_filled(
                                    rect,
                                    t.radius,
                                    Theme::color(t.selection).gamma_multiply(state.selected),
                                );
                                foldex_ui::components::focus_ring(
                                    ui,
                                    rect,
                                    list_id.with(entry.command),
                                    t,
                                    self.selected == Some(entry.command)
                                        && ctx.memory(|memory| memory.has_focus(list_id)),
                                );
                                let key = TextKey {
                                    fonts: foldex_ui::fonts::revision(ctx),
                                    font: t.font_size.to_bits(),
                                    width: rect.width().to_bits(),
                                    ppp: ctx.pixels_per_point().to_bits(),
                                    foreground: t.fg(),
                                    muted: t.dim(),
                                    accent: t.accent(),
                                };
                                if row.text.as_ref().is_none_or(|text| text.key != key) {
                                    row.text = Some(RowText {
                                        key,
                                        title: ui.painter().layout_job(highlight(
                                            entry.title,
                                            &row.title_indices,
                                            t.font(),
                                            t.fg(),
                                            t.accent(),
                                            (rect.width() - 180.0 * scale).max(40.0),
                                        )),
                                        description: ui.painter().layout_job(highlight(
                                            entry.description,
                                            &row.description_indices,
                                            FontId::proportional((t.font_size - 2.0).max(10.0)),
                                            t.dim(),
                                            t.accent(),
                                            (rect.width() - 52.0 * scale).max(40.0),
                                        )),
                                    });
                                }
                                let text = row.text.as_ref().unwrap();
                                let text_y = rect.center().y
                                    - (text.title.size().y + text.description.size().y + 2.0) * 0.5;
                                painter.galley(
                                    pos2(rect.left() + 10.0 * scale, text_y),
                                    Arc::clone(&text.title),
                                    t.fg(),
                                );
                                painter.galley(
                                    pos2(
                                        rect.left() + 10.0 * scale,
                                        text_y + text.title.size().y + 2.0,
                                    ),
                                    Arc::clone(&text.description),
                                    t.dim(),
                                );
                                let shortcut = self
                                    .shortcut_displays
                                    .get(&entry.command)
                                    .map(String::as_str)
                                    .unwrap_or(entry.shortcut);
                                if !shortcut.is_empty() {
                                    painter.text(
                                        pos2(
                                            rect.right() - 40.0 * scale,
                                            text_y + text.title.size().y * 0.5,
                                        ),
                                        Align2::RIGHT_CENTER,
                                        shortcut,
                                        FontId::proportional((t.font_size - 1.5).max(10.0)),
                                        t.dim(),
                                    );
                                }
                                let star_rect = Rect::from_center_size(
                                    pos2(rect.right() - 18.0 * scale, rect.center().y),
                                    vec2(m.control, m.control),
                                );
                                let star = ui.interact(
                                    star_rect,
                                    Id::new(("palette-star", entry.command)),
                                    Sense::CLICK,
                                );
                                moving |= gains.step_normalized(
                                    &mut state.star,
                                    if star.hovered() { 1.0 } else { 0.0 },
                                    MotionProfile::Standard,
                                );
                                if state.star > 0.0 {
                                    painter.rect_filled(
                                        star_rect,
                                        t.radius,
                                        Theme::color(t.hover).gamma_multiply(state.star),
                                    );
                                }
                                let bookmarked = self.bookmarks.contains(&entry.command);
                                icon(
                                    &painter,
                                    star_rect.center(),
                                    if bookmarked {
                                        Icon::Star
                                    } else {
                                        Icon::StarOutline
                                    },
                                    if bookmarked {
                                        t.accent()
                                    } else {
                                        t.dim().gamma_multiply(0.45 + 0.55 * state.hover)
                                    },
                                    m.icon,
                                );
                                if star.clicked() {
                                    toggle = Some(entry.command);
                                    self.selected = Some(entry.command);
                                    ui.memory_mut(|memory| memory.request_focus(list_id));
                                }
                                star.on_hover_text(if bookmarked {
                                    "Remove from Quick commands"
                                } else {
                                    "Add to Quick commands"
                                });
                                if response.clicked()
                                    && toggle != Some(entry.command)
                                    && entry.enabled
                                    && !self.closing
                                {
                                    picked = Some(entry.command);
                                }
                                if response.hovered() && !entry.enabled {
                                    response.on_hover_text(match entry.command {
                                        Command::ClosePanel
                                        | Command::NextPanel
                                        | Command::PreviousPanel => "Open a split pane first",
                                        Command::Back => "No previous folder",
                                        Command::Forward => "No next folder",
                                        Command::Parent => "Already at the root",
                                        Command::ReopenTab => "No closed tabs",
                                        Command::ToggleInspector | Command::InspectorDetails => {
                                            "Widen the window to show the inspector"
                                        }
                                        _ => "Not available yet",
                                    });
                                }
                            }
                            if self.rows.is_empty() {
                                foldex_ui::components::message_at(
                                    ui,
                                    content,
                                    Id::new("commands-empty"),
                                    t,
                                    if self.quick_only {
                                        "No matching Quick commands"
                                    } else {
                                        "No matching commands"
                                    },
                                    t.dim(),
                                );
                            }
                        });
                    },
                );
                let footer = layout.footer;
                ui.painter().line_segment(
                    [footer.left_top(), footer.right_top()],
                    Stroke::new(1.0, t.line()),
                );
                let runnable = self.selected.is_some_and(|selected| {
                    self.entries
                        .iter()
                        .any(|entry| entry.command == selected && entry.enabled)
                });
                let (full, compact) = if runnable {
                    (
                        "↑ ↓  select     Enter  run     Space  bookmark in results     Esc  close",
                        "Enter  run     Esc  close",
                    )
                } else {
                    ("↑ ↓  select     Esc  close", "Esc  close")
                };
                foldex_ui::components::popup_hint(ui.painter(), footer, t, full, compact);
            },
        );
        if quick_toggle {
            self.quick_only = !self.quick_only;
            self.refilter();
        }
        if let Some(command) = toggle {
            self.toggle_bookmark(command);
        }
        if execute && !self.closing {
            picked = self.selected.filter(|command| {
                self.entries
                    .iter()
                    .any(|entry| entry.command == *command && entry.enabled)
            });
        }
        if !self.closing
            && ctx.cumulative_frame_nr() > self.opened_frame.unwrap_or_default()
            && ctx.input(|input| {
                input.pointer.any_pressed()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|position| !outer.contains(position))
            })
        {
            self.close();
        }
        if picked.is_some() {
            self.close();
        }
        self.focus_scope
            .update_animated(ctx, input_id, &members, self.closing, self.finished());
        if moving
            || self.motion.is_moving()
            || changed
            || quick_toggle
            || toggle.is_some()
            || (self.closing && !self.finished())
        {
            ctx.request_repaint();
        }
        picked
    }
}

pub(crate) fn control(
    ui: &egui::Ui,
    t: &Theme,
    rect: Rect,
    id: Id,
    glyph: Icon,
    active: bool,
    _gains: MotionGains,
    _moving: &mut bool,
) -> egui::Response {
    foldex_ui::components::icon_button_at(ui, rect, id, t, glyph, active)
}
pub(crate) fn highlight(
    text: &str,
    indices: &[usize],
    font: FontId,
    normal: Color32,
    matched: Color32,
    width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('…');
    let mut start = 0;
    let mut previous = false;
    for (offset, _) in text.char_indices() {
        let selected = indices.binary_search(&offset).is_ok();
        if offset > 0 && selected != previous {
            job.append(
                &text[start..offset],
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color: if previous { matched } else { normal },
                    ..Default::default()
                },
            );
            start = offset;
        }
        previous = selected;
    }
    job.append(
        &text[start..],
        0.0,
        egui::TextFormat {
            font_id: font,
            color: if previous { matched } else { normal },
            ..Default::default()
        },
    );
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn keyboard_frame(
        ctx: &egui::Context,
        palette: &mut Palette,
        frame: u32,
        events: Vec<egui::Event>,
    ) -> Option<Command> {
        keyboard_frame_with_header(ctx, palette, frame, events).0
    }

    fn keyboard_frame_with_header(
        ctx: &egui::Context,
        palette: &mut Palette,
        frame: u32,
        events: Vec<egui::Event>,
    ) -> (Option<Command>, Option<bool>) {
        let mut picked = None;
        let mut header_enabled = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(frame as f64 / 60.0),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1100.0, 700.0))),
                events,
                ..Default::default()
            },
            |ui| {
                foldex_ui::components::PopupFocus::surface(
                    ui,
                    Rect::from_min_size(pos2(5.0, 5.0), vec2(100.0, 24.0)),
                    Id::new("palette-invoker"),
                    "Files",
                );
                picked = palette
                    .ui(
                        ui.ctx(),
                        &Theme {
                            reduced_motion: true,
                            ..Default::default()
                        },
                    )
                    .or(picked);
                // Inspect this paint's response before end_pass swaps the widget
                // buffers; outside run_ui, read_response may find the sizing pass.
                header_enabled = ui
                    .ctx()
                    .read_response(Id::new("commands-quick"))
                    .map(|response| response.enabled());
            },
        );
        output.textures_delta.clear();
        (picked, header_enabled)
    }

    #[test]
    fn palette_tab_walks_controls_and_one_results_stop_and_close_restores_invoker() {
        let ctx = egui::Context::default();
        let mut palette = Palette::new();
        palette.set_return_focus(Id::new("palette-invoker"));
        keyboard_frame(&ctx, &mut palette, 0, vec![]);
        for (frame, expected) in [
            (1, Id::new("commands-quick")),
            (2, Id::new("commands-close")),
            (3, Id::new("command-palette-results-focus")),
            (4, Id::new("command-palette-input")),
        ] {
            assert_eq!(
                keyboard_frame(
                    &ctx,
                    &mut palette,
                    frame,
                    vec![key(Key::Tab, Modifiers::NONE)]
                ),
                None
            );
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(expected));
        }
        keyboard_frame(&ctx, &mut palette, 5, vec![key(Key::Tab, Modifiers::SHIFT)]);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("command-palette-results-focus"))
        );
        ctx.memory_mut(|memory| memory.request_focus(Id::new("commands-close")));
        assert_eq!(
            keyboard_frame(
                &ctx,
                &mut palette,
                6,
                vec![key(Key::Enter, Modifiers::NONE)]
            ),
            None
        );
        assert!(!palette.is_open());
        keyboard_frame(&ctx, &mut palette, 7, vec![]);
        assert!(palette.finished());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("palette-invoker"))
        );
    }

    #[test]
    fn palette_header_enter_does_not_run_a_result_and_results_space_bookmarks() {
        let ctx = egui::Context::default();
        let mut palette = Palette::new();
        keyboard_frame(&ctx, &mut palette, 0, vec![]);
        // A new Area starts with an invisible sizing pass. Finish initial input
        // acquisition, then reach the live header with the user's Tab path.
        let (_, header_enabled) = keyboard_frame_with_header(&ctx, &mut palette, 1, vec![]);
        assert_eq!(header_enabled, Some(true));
        assert_eq!(
            keyboard_frame(&ctx, &mut palette, 2, vec![key(Key::Tab, Modifiers::NONE)]),
            None
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("commands-quick"))
        );
        assert_eq!(
            keyboard_frame(
                &ctx,
                &mut palette,
                3,
                vec![key(Key::Enter, Modifiers::NONE)]
            ),
            None
        );
        assert!(palette.quick_only && palette.is_open());
        let selected = palette.selected.unwrap();
        let bookmarked = palette.bookmarks.contains(&selected);
        keyboard_frame(&ctx, &mut palette, 4, vec![key(Key::Tab, Modifiers::NONE)]);
        keyboard_frame(&ctx, &mut palette, 5, vec![key(Key::Tab, Modifiers::NONE)]);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("command-palette-results-focus"))
        );
        assert_eq!(
            keyboard_frame(
                &ctx,
                &mut palette,
                6,
                vec![
                    key(Key::Space, Modifiers::NONE),
                    egui::Event::Text(" ".into())
                ]
            ),
            None
        );
        assert_eq!(palette.bookmarks.contains(&selected), !bookmarked);
        assert_eq!(palette.query(), "");
    }

    #[test]
    fn palette_end_skips_disabled_rows_and_modified_or_ime_keys_do_not_activate() {
        let ctx = egui::Context::default();
        let mut palette = Palette::new();
        palette.set_enabled(Command::KeyboardHelp, false);
        keyboard_frame(&ctx, &mut palette, 0, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(Id::new("command-palette-results-focus")));
        keyboard_frame(&ctx, &mut palette, 1, vec![key(Key::End, Modifiers::NONE)]);
        let last = palette
            .rows
            .iter()
            .rev()
            .find(|row| palette.entries[row.entry].enabled)
            .unwrap();
        assert_eq!(palette.selected, Some(palette.entries[last.entry].command));
        let selected = palette.selected;
        assert_eq!(
            keyboard_frame(
                &ctx,
                &mut palette,
                2,
                vec![
                    key(Key::ArrowUp, Modifiers::ALT),
                    key(Key::Enter, Modifiers::SHIFT)
                ]
            ),
            None
        );
        assert_eq!(palette.selected, selected);
        ctx.memory_mut(|memory| memory.request_focus(Id::new("command-palette-input")));
        assert_eq!(
            keyboard_frame(
                &ctx,
                &mut palette,
                3,
                vec![
                    egui::Event::Ime(egui::ImeEvent::Preedit {
                        text: "に".into(),
                        active_range_chars: None
                    }),
                    key(Key::Enter, Modifiers::NONE)
                ]
            ),
            None
        );
        assert_eq!(
            keyboard_frame(
                &ctx,
                &mut palette,
                4,
                vec![key(Key::Enter, Modifiers::NONE)]
            ),
            None
        );
        assert!(palette.is_open());
    }

    #[test]
    fn new_focus_command_identifiers_are_stable() {
        for (command, id) in [
            (Command::NextFocusRegion, "next-focus-region"),
            (Command::PreviousFocusRegion, "previous-focus-region"),
            (Command::KeyboardHelp, "keyboard-navigation-help"),
        ] {
            assert_eq!(
                serde_json::to_value(command).unwrap(),
                serde_json::Value::String(id.into())
            );
            assert_eq!(
                serde_json::from_value::<Command>(serde_json::Value::String(id.into())).unwrap(),
                command
            );
        }
        assert_eq!(
            serde_json::to_value(Command::NextPanel).unwrap(),
            "next_panel"
        );
    }

    #[test]
    fn bookmarks_and_availability_preserve_a_valid_selection() {
        let mut palette = Palette::new();
        palette.set_bookmarks([Command::Options]);
        assert_eq!(
            palette.entries[palette.rows[0].entry].command,
            Command::Options
        );
        assert_eq!(palette.rows[0].heading, Some("Quick"));
        palette.set_query("save layout");
        assert!(
            palette
                .rows
                .iter()
                .any(|row| palette.entries[row.entry].command == Command::SaveLayout)
        );
        assert_ne!(palette.selected, Some(Command::SaveLayout));
        palette.set_enabled(Command::SaveLayout, true);
        palette.selected = Some(Command::SaveLayout);
        palette.refilter();
        assert_eq!(palette.selected, Some(Command::SaveLayout));
        palette.set_enabled(Command::SaveLayout, false);
        assert_ne!(palette.selected, Some(Command::SaveLayout));
    }

    #[test]
    fn source_fuzzy_scores_rank_titles_and_cache_highlight_offsets() {
        let mut palette = Palette::new();
        palette.entries.clear();
        palette.bookmarks.clear();
        for (command, title) in [
            (Command::Back, "Design"),
            (Command::Forward, "Documents"),
            (Command::Parent, "Downloads"),
            (Command::GoTo, "Readme.md"),
        ] {
            assert!(palette.register(CommandEntry {
                command,
                title,
                description: "",
                shortcut: "",
                keywords: "",
                enabled: true
            }));
        }
        palette.set_query("dsg");
        assert_eq!(palette.rows.len(), 1);
        assert_eq!(palette.rows[0].score, 141);
        assert_eq!(palette.rows[0].title_indices, [0, 2, 4]);
        palette.set_query("d");
        assert_eq!(palette.entries[palette.rows[0].entry].title, "Design");
        assert_eq!(
            palette.rows.iter().map(|row| row.score).collect::<Vec<_>>(),
            [135, 126, 126, 73]
        );
    }

    #[test]
    fn palette_motion_settles_and_closes_without_continuous_idle_frames() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let mut palette = Palette::new();
        let mut delay = std::time::Duration::ZERO;
        let mut idle_frames = 0;
        for frame in 0..120 {
            let input = egui::RawInput {
                time: Some(frame as f64 / 60.0),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                assert_eq!(palette.ui(ui.ctx(), &theme), None);
            });
            delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            if frame >= 60 && delay > std::time::Duration::from_millis(100) {
                idle_frames += 1;
            }
            output.textures_delta.clear();
        }
        assert!(!palette.motion.is_moving());
        assert_eq!(palette.opacity, 1.0);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("command-palette-input"))
        );
        assert!(
            idle_frames >= 40,
            "idle frames{idle_frames}, last delay{delay:?}; causes{:?}",
            ctx.repaint_causes()
        );
        palette.close();
        for frame in 120..240 {
            let input = egui::RawInput {
                time: Some(frame as f64 / 60.0),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                palette.ui(ui.ctx(), &theme);
            });
            output.textures_delta.clear();
        }
        assert!(palette.finished());
    }

    #[test]
    fn newly_opened_palette_accepts_same_frame_query_text() {
        let ctx = egui::Context::default();
        let mut palette = Palette::new();
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(0.0),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0))),
                events: vec![egui::Event::Text("goto".into())],
                ..Default::default()
            },
            |ui| {
                palette.ui(ui.ctx(), &Theme::default());
            },
        );
        output.textures_delta.clear();
        assert_eq!(palette.query(), "goto");
    }
}
