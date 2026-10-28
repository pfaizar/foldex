//! Native keyboard workflow driven through owned egui RawInput, including the
//! immediate launcher viewport. No global input or result/file command executes.
use super::*;
use crate::{
    keyboard::{self, Region},
    launcher::{Mode, native},
    performance::{
        KeyboardUiRun,
        keyboard::{
            self as workload, CAPTURE_TIMEOUT, InputPlugin, MAX_CAPTURES, MAX_SAMPLES,
            PHASE_TIMEOUT, PendingCapture, Probe, Viewport, geometry_visible,
        },
    },
};
use std::sync::OnceLock;

const CTRL: u8 = 1;
const SHIFT: u8 = 2;
const ALT: u8 = 4;
const STEP_DELAY: Duration = Duration::from_millis(140);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Listing,
    Sidebar,
    Search,
    Inspector,
    FilesToggle,
    OptionsSearch,
    OptionsResults,
    OptionsControl,
    OptionsEditor,
    PaletteSearch,
    GoToSearch,
    HelpClose,
    HelpScroll,
    LauncherQuery,
    LauncherResults,
    HotkeysSearch,
    HotkeysRemove,
}
impl Target {
    fn viewport(self) -> Viewport {
        match self {
            Self::LauncherQuery | Self::LauncherResults => Viewport::Launcher,
            _ => Viewport::Root,
        }
    }
}

#[derive(Clone, Copy)]
enum Owner {
    Browser,
    RootRegion,
    Context,
    Options,
    Help,
    Launcher,
    Menu,
    Hotkeys,
    Focus(Target),
}
#[derive(Clone, Copy)]
enum Remember {
    Invoker,
    MenuParent,
    Selection,
    Options,
    Editor,
    LauncherSelection,
}
#[derive(Clone, Copy)]
enum Appearance {
    Wide,
    Compact,
    Paper,
    LargerText,
}
#[derive(Clone, Copy)]
enum Composition {
    Preedit,
    Commit,
}

#[derive(Clone, Copy)]
enum Check {
    BrowserReady,
    SearchMatches,
    ImeCommitted,
    NoFileAction,
    Focus(Target),
    KeyboardCues(Viewport, bool),
    Selection(&'static [u32]),
    FocusedRow(usize),
    FocusedLast,
    FocusedVisible,
    UnselectedFocus,
    SelectionPreserved,
    TabLeftListing,
    FoldersOnly,
    AllKinds,
    MenuOpen,
    MenuPanels(usize),
    MenuParentRestored,
    MenuClosed,
    InvokerRestored,
    SidebarDidNotNavigate,
    HomeReady,
    HomeSelected,
    HomeLayout(f32),
    HomeFilter,
    SidebarNetwork(bool),
    HotkeysGeometry,
    HotkeysConfirmation,
    RecycleLayout(f32, usize),
    RecycleConfirmation,
    OptionsOpen,
    OptionsClosed,
    CornersChanged,
    CornersRestored,
    EditorCancelled,
    PaletteOpen,
    PaletteClosed,
    GoToOpen,
    GoToClosed,
    RenameOpen,
    RenameCancelled,
    HelpOpen,
    HelpClosed,
    HelpScrolledEnd,
    GeneratedFolderSelected,
    InspectorReady,
    InspectorClosed,
    LauncherReady,
    LauncherMoved,
    LauncherLast,
    LauncherExactResult,
    LauncherCalculation,
    CalculatorAnswer(&'static str),
    LauncherRevealed,
    ThemeSaved,
    NoPopup,
    DeletePreview,
    BuiltinReady(foldex_shell::BuiltinKind),
    LauncherClosed,
    LauncherRowVisible,
    LauncherGeometry,
    Size(f32, f32),
    AssetsReady,
}
#[derive(Clone, Copy)]
enum Operation {
    Prepare,
    Press(Owner, Key, u8),
    OpenText(Key, u8, &'static str),
    Text(Target, &'static str),
    HoverFocus(Viewport),
    LeavePointer(Viewport),
    Compose(Composition),
    Seek(Target, Key, u8, u8),
    Observe(Check),
    Remember(Remember),
    Capture(Viewport, &'static str),
    Circuit(bool),
    Appearance(Appearance),
    HomeZoom(f32),
    IconSweep,
    HotkeysPreview,
    RecyclePreview,
    RecycleConfirm(bool),
    OpenLauncher,
    Calculator,
    NetworkPreview,
    MenuPreview(&'static str),
    BuiltinPreview(foldex_shell::BuiltinKind),
    PermanentPreview,
    CutPreview(bool),
    WatchLauncherRow,
    SettleHelp,
    Finish,
}
#[derive(Clone, Copy)]
struct Step {
    name: &'static str,
    operation: Operation,
}
impl Step {
    const fn new(name: &'static str, operation: Operation) -> Self {
        Self { name, operation }
    }
}
fn press(name: &'static str, target: Target, key: Key, modifiers: u8) -> Step {
    Step::new(name, Operation::Press(Owner::Focus(target), key, modifiers))
}
fn observe(name: &'static str, check: Check) -> Step {
    Step::new(name, Operation::Observe(check))
}
fn seek(name: &'static str, target: Target, key: Key, modifiers: u8, limit: u8) -> Step {
    Step::new(name, Operation::Seek(target, key, modifiers, limit))
}

fn home_layout_workflow() -> Vec<Step> {
    use Target::*;
    let mut steps = vec![
        Step::new(
            "Ctrl+F focuses shared Home footer",
            Operation::Press(Owner::Browser, Key::F, CTRL),
        ),
        observe("Home footer search owns focus", Check::Focus(Search)),
        Step::new(
            "filter Home from the footer",
            Operation::Text(Search, "Keyboard 07"),
        ),
        observe(
            "Home footer filters Recent and exposes clear",
            Check::HomeFilter,
        ),
        Step::new(
            "capture Home footer filter",
            Operation::Capture(Viewport::Root, "kbd-home-footer-filter"),
        ),
        press("select Home footer query", Search, Key::A, CTRL),
        press("clear Home footer query", Search, Key::Backspace, 0),
        observe("Home query clears without hiding footer", Check::HomeReady),
    ];
    for (zoom, label) in [
        (0.0, "kbd-home-details"),
        (0.1, "kbd-home-list"),
        (0.2, "kbd-home-columns"),
        (0.3, "kbd-home-medium-icons"),
        (0.65, "kbd-home-large-icons"),
        (1.0, "kbd-home-extra-large-icons"),
    ] {
        steps.extend([
            Step::new(
                "change Home view through shared zoom model",
                Operation::HomeZoom(zoom),
            ),
            observe(
                "Home view and footer fit the viewport",
                Check::HomeLayout(zoom),
            ),
            Step::new(
                "capture Home view geometry",
                Operation::Capture(Viewport::Root, label),
            ),
        ]);
    }
    for (appearance, zoom, label, width, height) in [
        (
            Appearance::Compact,
            1.0,
            "kbd-home-compact-icons",
            640.0,
            540.0,
        ),
        (
            Appearance::Compact,
            0.0,
            "kbd-home-compact-details",
            640.0,
            540.0,
        ),
        (
            Appearance::Paper,
            0.3,
            "kbd-home-paper-icons",
            1200.0,
            800.0,
        ),
        (
            Appearance::LargerText,
            1.0,
            "kbd-home-larger-text-icons",
            1200.0,
            800.0,
        ),
    ] {
        steps.extend([
            Step::new(
                "apply Home appearance fixture",
                Operation::Appearance(appearance),
            ),
            observe("Home root size settles", Check::Size(width, height)),
            Step::new("apply Home appearance zoom", Operation::HomeZoom(zoom)),
            observe("Home appearance remains bounded", Check::HomeLayout(zoom)),
            Step::new(
                "capture Home appearance",
                Operation::Capture(Viewport::Root, label),
            ),
        ]);
    }
    steps.extend([
        Step::new(
            "restore wide Home appearance",
            Operation::Appearance(Appearance::Wide),
        ),
        observe("wide root size restores", Check::Size(1200.0, 800.0)),
        Step::new(
            "restore Details for browser workflow",
            Operation::HomeZoom(0.0),
        ),
        observe("restored Home Details fits", Check::HomeLayout(0.0)),
    ]);
    steps
}

fn recycle_layout_workflow() -> Vec<Step> {
    use Target::*;
    vec![
        Step::new("seed inert Recycle Bin page", Operation::RecyclePreview),
        Step::new("show Recycle Bin detail columns", Operation::HomeZoom(0.0)),
        observe(
            "Recycle Bin detail rows and toolbar fit",
            Check::RecycleLayout(0.0, 36),
        ),
        Step::new(
            "capture Recycle Bin Details",
            Operation::Capture(Viewport::Root, "kbd-recycle-details"),
        ),
        Step::new(
            "focus Recycle Bin filter",
            Operation::Press(Owner::Browser, Key::F, CTRL),
        ),
        observe(
            "Recycle Bin uses shared search editor",
            Check::Focus(Search),
        ),
        Step::new(
            "filter duplicate native Recycle Bin names",
            Operation::Text(Search, "Quarterly"),
        ),
        observe(
            "duplicate original names stay distinct",
            Check::RecycleLayout(0.0, 2),
        ),
        Step::new(
            "capture Recycle Bin filter and clear",
            Operation::Capture(Viewport::Root, "kbd-recycle-filtered"),
        ),
        press("select Recycle Bin query", Search, Key::A, CTRL),
        press("clear Recycle Bin query", Search, Key::Backspace, 0),
        observe(
            "clearing filter restores all bin rows",
            Check::RecycleLayout(0.0, 36),
        ),
        Step::new(
            "request Empty Bin confirmation only",
            Operation::RecycleConfirm(true),
        ),
        observe(
            "Empty Bin confirmation starts on Cancel",
            Check::RecycleConfirmation,
        ),
        Step::new(
            "capture Empty Bin confirmation",
            Operation::Capture(Viewport::Root, "kbd-recycle-empty-confirmation"),
        ),
        Step::new(
            "Escape cancels Empty Bin",
            Operation::Press(Owner::Menu, Key::Escape, 0),
        ),
        observe("Empty Bin cancels without native work", Check::NoPopup),
        Step::new(
            "request Restore All confirmation only",
            Operation::RecycleConfirm(false),
        ),
        observe(
            "Restore All confirmation starts on Cancel",
            Check::RecycleConfirmation,
        ),
        Step::new(
            "capture Restore All confirmation",
            Operation::Capture(Viewport::Root, "kbd-recycle-restore-confirmation"),
        ),
        Step::new(
            "Escape cancels Restore All",
            Operation::Press(Owner::Menu, Key::Escape, 0),
        ),
        observe("Restore All cancels without native work", Check::NoPopup),
        Step::new("enlarge recycled item icons", Operation::HomeZoom(1.0)),
        observe(
            "Recycle Bin shares large icon layout",
            Check::RecycleLayout(1.0, 36),
        ),
        Step::new(
            "capture Recycle Bin icon view",
            Operation::Capture(Viewport::Root, "kbd-recycle-icons"),
        ),
        Step::new(
            "apply compact Recycle Bin appearance",
            Operation::Appearance(Appearance::Compact),
        ),
        observe("compact bin root settles", Check::Size(640.0, 540.0)),
        Step::new("restore compact Bin Details", Operation::HomeZoom(0.0)),
        observe(
            "compact Bin toolbar and footer fit",
            Check::RecycleLayout(0.0, 36),
        ),
        Step::new(
            "capture compact Recycle Bin",
            Operation::Capture(Viewport::Root, "kbd-recycle-compact"),
        ),
        Step::new(
            "apply light Recycle Bin appearance",
            Operation::Appearance(Appearance::Paper),
        ),
        observe("light bin root settles", Check::Size(1200.0, 800.0)),
        observe(
            "light Bin toolbar and footer fit",
            Check::RecycleLayout(0.0, 36),
        ),
        Step::new(
            "capture light Recycle Bin",
            Operation::Capture(Viewport::Root, "kbd-recycle-paper"),
        ),
    ]
}

fn workflow() -> &'static [Step] {
    static PLAN: OnceLock<Vec<Step>> = OnceLock::new();
    PLAN.get_or_init(|| {
        use Target::*;
        if std::env::args_os().any(|argument| argument == "--benchmark-keyboard-fluidity") {
            let mut steps = vec![
                Step::new("prepare isolated fluidity model", Operation::Prepare),
                observe("fluidity generated directory is ready", Check::BrowserReady),
                seek("focus fluidity listing", Listing, Key::F6, 0, 12),
                Step::new(
                    "exercise owned root input",
                    Operation::Press(Owner::Browser, Key::ArrowDown, 0),
                ),
                Step::new(
                    "restore fluidity cursor",
                    Operation::Press(Owner::Browser, Key::ArrowUp, 0),
                ),
                Step::new(
                    "initialize owned fluidity launcher input",
                    Operation::OpenLauncher,
                ),
                observe("fluidity launcher is ready", Check::LauncherReady),
                Step::new(
                    "dismiss fluidity launcher",
                    Operation::Press(Owner::Launcher, Key::Escape, 0),
                ),
                observe("fluidity launcher closes", Check::LauncherClosed),
            ];
            for (zoom, label) in [
                (0.0, "kbd-fluidity-details"),
                (0.1, "kbd-fluidity-list"),
                (0.2, "kbd-fluidity-columns"),
                (0.3, "kbd-fluidity-small-icons"),
                (0.65, "kbd-fluidity-medium-icons"),
                (1.0, "kbd-fluidity-large-icons"),
            ] {
                steps.extend([
                    Step::new("warm visible icon size", Operation::HomeZoom(zoom)),
                    observe("visible native icons are ready", Check::AssetsReady),
                    Step::new(
                        "capture warmed native icons",
                        Operation::Capture(Viewport::Root, label),
                    ),
                ]);
            }
            steps.extend([
                Step::new(
                    "rapidly alternate icon sizes and viewport edges",
                    Operation::IconSweep,
                ),
                Step::new(
                    "restore fluidity appearance",
                    Operation::Appearance(Appearance::Wide),
                ),
                observe(
                    "fluidity final dimensions settle",
                    Check::Size(1200.0, 800.0),
                ),
                Step::new(
                    "restore detail view after rapid sweep",
                    Operation::HomeZoom(0.0),
                ),
                observe(
                    "detail icons remain available after sweep",
                    Check::AssetsReady,
                ),
                Step::new(
                    "capture icons after rapid sweep",
                    Operation::Capture(Viewport::Root, "kbd-fluidity-restored"),
                ),
                Step::new("finish fluidity fixture verification", Operation::Finish),
            ]);
            return steps;
        }
        let mut steps = vec![
            Step::new("prepare isolated keyboard model", Operation::Prepare),
            observe("generated directory is ready", Check::BrowserReady),
            seek("listing owns type-to-search input", Listing, Key::F6, 0, 12),
            Step::new(
                "typing directly into listing starts search",
                Operation::Text(Listing, "Keyboard"),
            ),
            observe("type-to-search transfers focus", Check::Focus(Search)),
            observe("type-to-search retains opening text", Check::SearchMatches),
            press("select type-to-search query", Search, Key::A, CTRL),
            press("clear type-to-search query", Search, Key::Backspace, 0),
            Step::new(
                "Ctrl+F accepts opening text",
                Operation::OpenText(Key::F, CTRL, "Keyboard"),
            ),
            observe("search owns focus", Check::Focus(Search)),
            observe(
                "search shortcut enables keyboard cues",
                Check::KeyboardCues(Viewport::Root, true),
            ),
            Step::new(
                "capture one search outline for keyboard input",
                Operation::Capture(Viewport::Root, "kbd-search-keyboard"),
            ),
            Step::new(
                "move the owned pointer over the search field",
                Operation::HoverFocus(Viewport::Root),
            ),
            observe(
                "pointer use hides root keyboard cues",
                Check::KeyboardCues(Viewport::Root, false),
            ),
            observe("pointer use preserves search focus", Check::Focus(Search)),
            Step::new(
                "capture pointer search without an inner ring",
                Operation::Capture(Viewport::Root, "kbd-search-pointer"),
            ),
            Step::new(
                "leave the search field with the owned pointer",
                Operation::LeavePointer(Viewport::Root),
            ),
            press("resume search keyboard input", Search, Key::ArrowLeft, 0),
            observe(
                "search keyboard input restores cues",
                Check::KeyboardCues(Viewport::Root, true),
            ),
            observe("search publishes generated matches", Check::SearchMatches),
            Step::new(
                "remember editor file state",
                Operation::Remember(Remember::Selection),
            ),
            press("F2 stays in search", Search, Key::F2, 0),
            press("Ctrl+A selects search text", Search, Key::A, CTRL),
            Step::new(
                "start IME composition",
                Operation::Compose(Composition::Preedit),
            ),
            press("F2 stays local during composition", Search, Key::F2, 0),
            observe(
                "composition cannot invoke file commands",
                Check::NoFileAction,
            ),
            Step::new("commit IME text", Operation::Compose(Composition::Commit)),
            observe("IME commit reaches search", Check::ImeCommitted),
            press("select committed search text", Search, Key::A, CTRL),
            Step::new(
                "restore matching search",
                Operation::Text(Search, "Keyboard"),
            ),
            observe("matching search restores results", Check::SearchMatches),
            seek("Tab reaches one result composite", Listing, Key::Tab, 0, 24),
            press("Home selects first result", Listing, Key::Home, 0),
            observe("first result selected", Check::Selection(&[0])),
            press("Shift+Down extends range", Listing, Key::ArrowDown, SHIFT),
            observe("two result range", Check::Selection(&[0, 1])),
            press("Shift+Down grows range", Listing, Key::ArrowDown, SHIFT),
            observe("three result range", Check::Selection(&[0, 1, 2])),
            press("Shift+Up shrinks range", Listing, Key::ArrowUp, SHIFT),
            observe("anchored range shrinks", Check::Selection(&[0, 1])),
            press("Ctrl+Down moves focus only", Listing, Key::ArrowDown, CTRL),
            observe(
                "Ctrl movement preserves selection",
                Check::Selection(&[0, 1]),
            ),
            observe("Ctrl movement reaches row two", Check::FocusedRow(2)),
            press("Ctrl+Space adds focused result", Listing, Key::Space, CTRL),
            observe(
                "Ctrl+Space adds exactly one result",
                Check::Selection(&[0, 1, 2]),
            ),
            press(
                "Ctrl+Down reaches unselected row",
                Listing,
                Key::ArrowDown,
                CTRL,
            ),
            press("Ctrl+Space toggles result on", Listing, Key::Space, CTRL),
            observe("four selected results", Check::Selection(&[0, 1, 2, 3])),
            press("Ctrl+Space toggles result off", Listing, Key::Space, CTRL),
            observe(
                "toggle removes exactly one result",
                Check::Selection(&[0, 1, 2]),
            ),
            observe("focus is independent of selection", Check::UnselectedFocus),
            Step::new(
                "capture unselected focused result",
                Operation::Capture(Viewport::Root, "kbd-focus-selection"),
            ),
            press(
                "Ctrl+Shift+Down adds anchored range",
                Listing,
                Key::ArrowDown,
                CTRL | SHIFT,
            ),
            observe(
                "additive range retains earlier selection",
                Check::Selection(&[0, 1, 2, 3, 4]),
            ),
            press("End reaches last result", Listing, Key::End, 0),
            observe("last result owns row focus", Check::FocusedLast),
            observe("last focused result is visible", Check::FocusedVisible),
            press("PageUp moves within results", Listing, Key::PageUp, 0),
            observe("page movement keeps focus visible", Check::FocusedVisible),
            press("Home resets result focus", Listing, Key::Home, 0),
            Step::new(
                "remember selection before Tab",
                Operation::Remember(Remember::Selection),
            ),
            press("Tab leaves results once", Listing, Key::Tab, 0),
            observe("Tab is outside the result composite", Check::TabLeftListing),
            Step::new(
                "Shift+Tab returns once",
                Operation::Press(Owner::RootRegion, Key::Tab, SHIFT),
            ),
            observe("Shift+Tab restores result composite", Check::Focus(Listing)),
            observe(
                "Tab traversal preserves selected rows",
                Check::SelectionPreserved,
            ),
            seek("Tab reaches Files toggle", FilesToggle, Key::Tab, 0, 24),
            press("Space toggles files off", FilesToggle, Key::Space, 0),
            observe("only matching folders remain", Check::FoldersOnly),
            observe(
                "kind toggle retains actual focus",
                Check::Focus(FilesToggle),
            ),
            Step::new(
                "capture focused footer toggle",
                Operation::Capture(Viewport::Root, "kbd-footer-toggle"),
            ),
            press("Space restores files", FilesToggle, Key::Space, 0),
            observe("both entry kinds restored", Check::AllKinds),
            seek("F6 returns to results", Listing, Key::F6, 0, 12),
            press("Home prepares safe context target", Listing, Key::Home, 0),
            Step::new(
                "remember context invoker",
                Operation::Remember(Remember::Invoker),
            ),
            press("Shift+F10 opens result context", Listing, Key::F10, SHIFT),
            observe("context menu owns keyboard", Check::MenuOpen),
            Step::new(
                "capture context root focus",
                Operation::Capture(Viewport::Root, "kbd-menu-root"),
            ),
            Step::new(
                "move the owned pointer over the context menu",
                Operation::HoverFocus(Viewport::Root),
            ),
            observe(
                "menu pointer hover has no keyboard outline",
                Check::KeyboardCues(Viewport::Root, false),
            ),
            observe("hover keeps context keyboard ownership", Check::MenuOpen),
            Step::new(
                "capture pointer context styling",
                Operation::Capture(Viewport::Root, "kbd-menu-pointer"),
            ),
            Step::new(
                "leave the context menu with the owned pointer",
                Operation::LeavePointer(Viewport::Root),
            ),
            Step::new(
                "End selects More actions",
                Operation::Press(Owner::Context, Key::End, 0),
            ),
            observe(
                "menu navigation restores keyboard cues",
                Check::KeyboardCues(Viewport::Root, true),
            ),
            Step::new(
                "remember parent menu focus",
                Operation::Remember(Remember::MenuParent),
            ),
            Step::new(
                "Right opens submenu without invoking",
                Operation::Press(Owner::Context, Key::ArrowRight, 0),
            ),
            observe("submenu is actually rendered", Check::MenuPanels(2)),
            Step::new(
                "capture submenu focus",
                Operation::Capture(Viewport::Root, "kbd-menu-child"),
            ),
            Step::new(
                "Escape closes innermost submenu",
                Operation::Press(Owner::Context, Key::Escape, 0),
            ),
            observe("Escape leaves parent menu", Check::MenuPanels(1)),
            observe(
                "submenu dismissal restores parent focus",
                Check::MenuParentRestored,
            ),
            Step::new(
                "Escape closes parent menu",
                Operation::Press(Owner::Context, Key::Escape, 0),
            ),
            observe("context root closes", Check::MenuClosed),
            observe(
                "context restores exact listing invoker",
                Check::InvokerRestored,
            ),
            Step::new(
                "remember region traversal selection",
                Operation::Remember(Remember::Selection),
            ),
            Step::new("F6 traverses every root region", Operation::Circuit(false)),
            Step::new(
                "Shift+F6 reverses the region cycle",
                Operation::Circuit(true),
            ),
            observe("region cycles retain selection", Check::SelectionPreserved),
            seek("F6 reaches sidebar composite", Sidebar, Key::F6, 0, 12),
            press("sidebar Down moves its cursor", Sidebar, Key::ArrowDown, 0),
            observe(
                "sidebar cursor does not navigate",
                Check::SidebarDidNotNavigate,
            ),
            press("sidebar Home finds Home destination", Sidebar, Key::Home, 0),
            press("Enter activates Home destination", Sidebar, Key::Enter, 0),
            observe("Home publishes generated history", Check::HomeReady),
            seek("F6 reaches Home composite", Listing, Key::F6, 0, 12),
            press("Home End finds last history item", Listing, Key::End, 0),
            press(
                "Space selects Home history without opening",
                Listing,
                Key::Space,
                0,
            ),
            observe("Home keyboard selection is visible", Check::HomeSelected),
            Step::new(
                "capture Home keyboard focus",
                Operation::Capture(Viewport::Root, "kbd-home"),
            ),
            Step::new(
                "Alt+Left returns from Home",
                Operation::Press(Owner::RootRegion, Key::ArrowLeft, ALT),
            ),
            observe(
                "history returns to generated directory",
                Check::BrowserReady,
            ),
            seek("return to listing before dialogs", Listing, Key::F6, 0, 12),
            press(
                "Home selects generated Inspector folder",
                Listing,
                Key::Home,
                0,
            ),
            observe(
                "Inspector target is an owned generated folder",
                Check::GeneratedFolderSelected,
            ),
            Step::new(
                "remember selection before Inspector",
                Operation::Remember(Remember::Selection),
            ),
            press(
                "Space opens built-in folder Inspector",
                Listing,
                Key::Space,
                0,
            ),
            observe(
                "folder Inspector content and width are ready",
                Check::InspectorReady,
            ),
            seek("F6 reaches Inspector region", Inspector, Key::F6, 0, 12),
            observe(
                "Inspector owns visible keyboard focus",
                Check::Focus(Inspector),
            ),
            Step::new(
                "capture native Inspector focus",
                Operation::Capture(Viewport::Root, "kbd-inspector"),
            ),
            seek(
                "F6 returns from Inspector to Listing",
                Listing,
                Key::F6,
                0,
                12,
            ),
            observe(
                "Inspector traversal preserves folder selection",
                Check::SelectionPreserved,
            ),
            press("Space closes built-in Inspector", Listing, Key::Space, 0),
            observe(
                "Inspector closes and its width settles",
                Check::InspectorClosed,
            ),
            observe(
                "Inspector close retains Listing focus",
                Check::Focus(Listing),
            ),
            Step::new(
                "remember F1 help invoker",
                Operation::Remember(Remember::Invoker),
            ),
            Step::new(
                "F1 opens keyboard guide",
                Operation::Press(Owner::Browser, Key::F1, 0),
            ),
            observe("keyboard guide owns modal focus", Check::HelpOpen),
            Step::new("keyboard guide geometry settles", Operation::SettleHelp),
            Step::new(
                "capture settled keyboard guide",
                Operation::Capture(Viewport::Root, "kbd-help"),
            ),
            Step::new(
                "Escape dismisses keyboard guide",
                Operation::Press(Owner::Help, Key::Escape, 0),
            ),
            observe("keyboard guide finishes closing", Check::HelpClosed),
            observe(
                "keyboard guide restores exact invoker",
                Check::InvokerRestored,
            ),
            Step::new(
                "remember Options invoker",
                Operation::Remember(Remember::Invoker),
            ),
            Step::new(
                "Ctrl+Comma opens Options with query",
                Operation::OpenText(Key::Comma, CTRL, "Round Corners"),
            ),
            observe("Options opens from keyboard", Check::OptionsOpen),
            observe("Options search owns focus", Check::Focus(OptionsSearch)),
            seek(
                "Tab reaches Options result composite",
                OptionsResults,
                Key::Tab,
                0,
                10,
            ),
            press(
                "Tab enters selected real option control",
                OptionsResults,
                Key::Tab,
                0,
            ),
            observe(
                "real Options control owns focus",
                Check::Focus(OptionsControl),
            ),
            Step::new(
                "remember appearance value",
                Operation::Remember(Remember::Options),
            ),
            press("Space toggles real checkbox", OptionsControl, Key::Space, 0),
            observe("keyboard checkbox changes radius", Check::CornersChanged),
            Step::new(
                "capture real option focus",
                Operation::Capture(Viewport::Root, "kbd-options-control"),
            ),
            press(
                "Space restores appearance checkbox",
                OptionsControl,
                Key::Space,
                0,
            ),
            observe(
                "checkbox roundtrip preserves appearance",
                Check::CornersRestored,
            ),
            seek(
                "return to sidebar visibility query",
                OptionsSearch,
                Key::Tab,
                SHIFT,
                10,
            ),
            press(
                "select sidebar visibility query",
                OptionsSearch,
                Key::A,
                CTRL,
            ),
            Step::new(
                "find Network section visibility option",
                Operation::Text(OptionsSearch, "Sidebar: Network"),
            ),
            seek(
                "Tab reaches Network section option",
                OptionsResults,
                Key::Tab,
                0,
                10,
            ),
            press(
                "Tab enters Network visibility checkbox",
                OptionsResults,
                Key::Tab,
                0,
            ),
            observe(
                "Network visibility starts enabled",
                Check::SidebarNetwork(true),
            ),
            press("Space hides Network section", OptionsControl, Key::Space, 0),
            observe(
                "Options hides Network section",
                Check::SidebarNetwork(false),
            ),
            Step::new(
                "capture integrated sidebar visibility option",
                Operation::Capture(Viewport::Root, "kbd-sidebar-visibility-options"),
            ),
            press(
                "Space restores Network section",
                OptionsControl,
                Key::Space,
                0,
            ),
            observe(
                "Options restores Network section",
                Check::SidebarNetwork(true),
            ),
            seek(
                "return to theme settings query",
                OptionsSearch,
                Key::Tab,
                SHIFT,
                10,
            ),
            press("select theme options query", OptionsSearch, Key::A, CTRL),
            Step::new(
                "find theme options",
                Operation::Text(OptionsSearch, "Color"),
            ),
            observe(
                "theme settings query keeps focus",
                Check::Focus(OptionsSearch),
            ),
            Step::new(
                "capture integrated theme settings",
                Operation::Capture(Viewport::Root, "kbd-theme-options"),
            ),
            press("select theme name query", OptionsSearch, Key::A, CTRL),
            Step::new(
                "find inline theme name",
                Operation::Text(OptionsSearch, "Theme Name"),
            ),
            seek(
                "Tab reaches theme name row",
                OptionsResults,
                Key::Tab,
                0,
                10,
            ),
            press("Tab enters inline theme name", OptionsResults, Key::Tab, 0),
            observe("inline theme name owns focus", Check::Focus(OptionsControl)),
            press("select draft theme name", OptionsControl, Key::A, CTRL),
            Step::new(
                "name isolated theme",
                Operation::Text(OptionsControl, "Owned Theme"),
            ),
            press("Enter saves named theme", OptionsControl, Key::Enter, 0),
            observe("both palettes saved through Options", Check::ThemeSaved),
            Step::new(
                "capture saved inline theme",
                Operation::Capture(Viewport::Root, "kbd-theme-saved"),
            ),
            seek(
                "return from theme name to query",
                OptionsSearch,
                Key::Tab,
                SHIFT,
                10,
            ),
            press("select color mode query", OptionsSearch, Key::A, CTRL),
            Step::new(
                "find Color Mode control",
                Operation::Text(OptionsSearch, "Color Mode"),
            ),
            seek(
                "Tab reaches Color Mode row",
                OptionsResults,
                Key::Tab,
                0,
                10,
            ),
            press("Tab enters Color Mode control", OptionsResults, Key::Tab, 0),
            press("Enter opens Color Mode menu", OptionsControl, Key::Enter, 0),
            Step::new(
                "capture color mode menu",
                Operation::Capture(Viewport::Root, "kbd-color-mode-menu"),
            ),
            Step::new(
                "Escape closes Color Mode menu",
                Operation::Press(Owner::Options, Key::Escape, 0),
            ),
            observe(
                "Color Mode menu returns focus",
                Check::Focus(OptionsControl),
            ),
            seek(
                "Shift+Tab returns to Options search",
                OptionsSearch,
                Key::Tab,
                SHIFT,
                10,
            ),
            press("select Options query", OptionsSearch, Key::A, CTRL),
            Step::new(
                "find launcher folder editor",
                Operation::Text(OptionsSearch, "Launcher Search Folders"),
            ),
            seek(
                "Tab reaches folder option composite",
                OptionsResults,
                Key::Tab,
                0,
                10,
            ),
            press(
                "Tab enters folder configure control",
                OptionsResults,
                Key::Tab,
                0,
            ),
            observe(
                "folder configure control focused",
                Check::Focus(OptionsControl),
            ),
            Step::new(
                "remember nested editor invoker",
                Operation::Remember(Remember::Editor),
            ),
            press(
                "Enter opens folder draft editor",
                OptionsControl,
                Key::Enter,
                0,
            ),
            observe("folder editor receives focus", Check::Focus(OptionsEditor)),
            press("select draft folder roots", OptionsEditor, Key::A, CTRL),
            Step::new(
                "edit draft only",
                Operation::Text(OptionsEditor, "X:\\keyboard-draft-only"),
            ),
            Step::new(
                "capture nested editor focus",
                Operation::Capture(Viewport::Root, "kbd-options-editor"),
            ),
            press(
                "Escape cancels nested editor",
                OptionsEditor,
                Key::Escape,
                0,
            ),
            observe(
                "draft cancel preserves roots and Options",
                Check::EditorCancelled,
            ),
            Step::new(
                "Escape closes Options",
                Operation::Press(Owner::Options, Key::Escape, 0),
            ),
            observe("Options closes", Check::OptionsClosed),
            observe("Options restores exact invoker", Check::InvokerRestored),
            Step::new(
                "remember palette invoker",
                Operation::Remember(Remember::Invoker),
            ),
            Step::new(
                "Ctrl+Shift+P accepts opening query",
                Operation::OpenText(Key::P, CTRL | SHIFT, "Options"),
            ),
            observe("command palette query is delivered", Check::PaletteOpen),
            observe(
                "palette search focus is visible",
                Check::Focus(PaletteSearch),
            ),
            Step::new(
                "capture command palette focus",
                Operation::Capture(Viewport::Root, "kbd-palette"),
            ),
            press(
                "Escape closes command palette",
                PaletteSearch,
                Key::Escape,
                0,
            ),
            observe("palette closes without execution", Check::PaletteClosed),
            observe("palette restores exact invoker", Check::InvokerRestored),
            Step::new(
                "remember GoTo invoker",
                Operation::Remember(Remember::Invoker),
            ),
            Step::new(
                "Ctrl+P accepts GoTo query",
                Operation::OpenText(Key::P, CTRL, "Keyboard"),
            ),
            observe("GoTo opens and receives query", Check::GoToOpen),
            observe("GoTo search owns focus", Check::Focus(GoToSearch)),
            press("Left edits GoTo text", GoToSearch, Key::ArrowLeft, 0),
            press("Home edits GoTo text", GoToSearch, Key::Home, 0),
            Step::new(
                "capture GoTo input focus",
                Operation::Capture(Viewport::Root, "kbd-goto"),
            ),
            press(
                "Escape closes GoTo without navigation",
                GoToSearch,
                Key::Escape,
                0,
            ),
            observe("GoTo closes", Check::GoToClosed),
            observe("GoTo restores exact invoker", Check::InvokerRestored),
            seek(
                "F6 reaches listing for safe rename cancel",
                Listing,
                Key::F6,
                0,
                12,
            ),
            press("Home selects rename fixture", Listing, Key::Home, 0),
            Step::new(
                "remember rename invoker",
                Operation::Remember(Remember::Invoker),
            ),
            press("F2 opens rename draft", Listing, Key::F2, 0),
            observe(
                "rename editor opens without changing file",
                Check::RenameOpen,
            ),
            Step::new(
                "Escape cancels rename draft",
                Operation::Press(Owner::Options, Key::Escape, 0),
            ),
            observe("rename draft cancelled", Check::RenameCancelled),
            observe("rename restores listing invoker", Check::InvokerRestored),
        ];
        let home_end = steps
            .iter()
            .position(|step| step.name == "Alt+Left returns from Home")
            .unwrap();
        steps.splice(home_end..home_end, home_layout_workflow());
        for (appearance, root_label, child_label) in [
            (Appearance::Wide, "kbd-wide-listing", "kbd-wide-launcher"),
            (
                Appearance::Compact,
                "kbd-compact-listing",
                "kbd-compact-launcher",
            ),
            (Appearance::Paper, "kbd-paper-listing", "kbd-paper-launcher"),
            (
                Appearance::LargerText,
                "kbd-large-text-listing",
                "kbd-large-text-launcher",
            ),
        ] {
            let (width, height) = if matches!(appearance, Appearance::Compact) {
                (640.0, 540.0)
            } else {
                (1200.0, 800.0)
            };
            steps.extend([
                Step::new(
                    "apply appearance fixture",
                    Operation::Appearance(appearance),
                ),
                observe("native root size settles", Check::Size(width, height)),
                seek(
                    "F6 focuses listing after appearance change",
                    Listing,
                    Key::F6,
                    0,
                    12,
                ),
                press(
                    "Home selects first appearance result",
                    Listing,
                    Key::Home,
                    0,
                ),
                press(
                    "Ctrl+Down separates appearance focus",
                    Listing,
                    Key::ArrowDown,
                    CTRL,
                ),
                observe("appearance focus is independent", Check::UnselectedFocus),
                observe("appearance focused row is visible", Check::FocusedVisible),
                Step::new(
                    "capture listing appearance focus",
                    Operation::Capture(Viewport::Root, root_label),
                ),
            ]);
            if matches!(appearance, Appearance::Compact) {
                steps.extend([
                    Step::new(
                        "remember compact guide invoker",
                        Operation::Remember(Remember::Invoker),
                    ),
                    Step::new(
                        "F1 opens compact keyboard guide",
                        Operation::Press(Owner::Browser, Key::F1, 0),
                    ),
                    observe("compact guide owns modal focus", Check::HelpOpen),
                    Step::new("compact guide geometry settles", Operation::SettleHelp),
                    observe("compact guide starts on Close", Check::Focus(HelpClose)),
                    press(
                        "Tab reaches compact guide scroll body",
                        HelpClose,
                        Key::Tab,
                        0,
                    ),
                    observe(
                        "compact guide scroll body owns focus",
                        Check::Focus(HelpScroll),
                    ),
                    press(
                        "End scrolls compact guide to its last shortcuts",
                        HelpScroll,
                        Key::End,
                        0,
                    ),
                    observe(
                        "compact guide reaches its scroll end",
                        Check::HelpScrolledEnd,
                    ),
                    Step::new(
                        "capture compact keyboard guide scroll focus",
                        Operation::Capture(Viewport::Root, "kbd-compact-help"),
                    ),
                    Step::new(
                        "Escape closes compact keyboard guide",
                        Operation::Press(Owner::Help, Key::Escape, 0),
                    ),
                    observe("compact guide finishes closing", Check::HelpClosed),
                    observe(
                        "compact guide restores exact invoker",
                        Check::InvokerRestored,
                    ),
                ]);
            }
            // Keep the screenshot's listing focus, then reach a different real
            // root invoker before opening the separate owned launcher viewport.
            // Exact restoration must cover editors and sidebar composites too.
            match appearance {
                Appearance::Wide => steps.push(seek(
                    "F6 reaches Search launcher invoker",
                    Search,
                    Key::F6,
                    0,
                    12,
                )),
                Appearance::Paper => steps.push(seek(
                    "F6 reaches Sidebar launcher invoker",
                    Sidebar,
                    Key::F6,
                    0,
                    12,
                )),
                Appearance::Compact | Appearance::LargerText => {}
            }
            steps.extend([
                Step::new(
                    "remember launcher invoker",
                    Operation::Remember(Remember::Invoker),
                ),
                Step::new("open owned launcher fixture", Operation::OpenLauncher),
                observe("owned launcher is ready", Check::LauncherReady),
                observe("launcher query owns focus", Check::Focus(LauncherQuery)),
                Step::new(
                    "type exact launcher file query",
                    Operation::Text(LauncherQuery, "Keyboard 07"),
                ),
                observe(
                    "launcher query filters actual results",
                    Check::LauncherExactResult,
                ),
                press("select launcher query text", LauncherQuery, Key::A, CTRL),
                Step::new(
                    "restore launcher result collection",
                    Operation::Text(LauncherQuery, "Keyboard"),
                ),
                observe("launcher result collection is ready", Check::LauncherReady),
                Step::new(
                    "remember launcher result index",
                    Operation::Remember(Remember::LauncherSelection),
                ),
                press(
                    "Down traverses launcher suggestions",
                    LauncherQuery,
                    Key::ArrowDown,
                    0,
                ),
                observe("launcher suggestion navigation works", Check::LauncherMoved),
                seek(
                    "Tab reaches one launcher result composite",
                    LauncherResults,
                    Key::Tab,
                    0,
                    10,
                ),
                press(
                    "End reaches last launcher result",
                    LauncherResults,
                    Key::End,
                    0,
                ),
                observe("launcher last result is focused", Check::LauncherLast),
                Step::new(
                    "watch focused launcher result geometry",
                    Operation::WatchLauncherRow,
                ),
                observe(
                    "launcher focused result stays visible",
                    Check::LauncherRowVisible,
                ),
                observe(
                    "launcher remains centered on secondary monitor",
                    Check::LauncherGeometry,
                ),
                Step::new(
                    "capture launcher appearance focus",
                    Operation::Capture(Viewport::Launcher, child_label),
                ),
            ]);
            if matches!(appearance, Appearance::Wide) {
                steps.extend([
                    Step::new(
                        "hover the focused launcher result",
                        Operation::HoverFocus(Viewport::Launcher),
                    ),
                    observe(
                        "launcher pointer use hides keyboard cues",
                        Check::KeyboardCues(Viewport::Launcher, false),
                    ),
                    observe(
                        "launcher pointer use retains result focus",
                        Check::Focus(LauncherResults),
                    ),
                    Step::new(
                        "capture launcher pointer styling",
                        Operation::Capture(Viewport::Launcher, "kbd-launcher-pointer"),
                    ),
                    Step::new(
                        "leave launcher with the owned pointer",
                        Operation::LeavePointer(Viewport::Launcher),
                    ),
                    press(
                        "restore launcher keyboard navigation",
                        LauncherResults,
                        Key::End,
                        0,
                    ),
                    observe(
                        "launcher keyboard input restores cues",
                        Check::KeyboardCues(Viewport::Launcher, true),
                    ),
                    observe(
                        "launcher keyboard resumes at its last result",
                        Check::LauncherLast,
                    ),
                    seek(
                        "Tab returns to calculator query",
                        LauncherQuery,
                        Key::Tab,
                        SHIFT,
                        10,
                    ),
                    Step::new("prepare isolated calculator mode", Operation::Calculator),
                    press("select calculator query", LauncherQuery, Key::A, CTRL),
                    Step::new(
                        "type owned calculation",
                        Operation::Text(LauncherQuery, "2 + 2 * 3"),
                    ),
                    observe(
                        "launcher exposes calculated copy value",
                        Check::LauncherCalculation,
                    ),
                    Step::new(
                        "capture calculator result",
                        Operation::Capture(Viewport::Launcher, "kbd-calculator"),
                    ),
                    Step::new(
                        "append a digit to the live calculation",
                        Operation::Text(LauncherQuery, "0"),
                    ),
                    observe(
                        "calculator updates retained result to 62",
                        Check::CalculatorAnswer("62"),
                    ),
                    Step::new(
                        "capture updated rolling calculator result",
                        Operation::Capture(Viewport::Launcher, "kbd-calculator-updated"),
                    ),
                    press(
                        "select live calculator equation",
                        LauncherQuery,
                        Key::A,
                        CTRL,
                    ),
                    Step::new(
                        "replace with a negative decimal equation",
                        Operation::Text(LauncherQuery, "-12.5 * 2"),
                    ),
                    observe(
                        "calculator handles sign and decimal update",
                        Check::CalculatorAnswer("-25"),
                    ),
                    Step::new(
                        "capture negative calculator update",
                        Operation::Capture(Viewport::Launcher, "kbd-calculator-negative"),
                    ),
                    seek(
                        "Tab reaches calculated result",
                        LauncherResults,
                        Key::Tab,
                        0,
                        10,
                    ),
                ]);
            }
            steps.extend([
                press(
                    "Escape dismisses owned launcher",
                    LauncherResults,
                    Key::Escape,
                    0,
                ),
                observe("owned launcher hides synchronously", Check::LauncherClosed),
                observe(
                    "launcher restores exact root invoker",
                    Check::InvokerRestored,
                ),
            ]);
        }
        steps.extend([
            Step::new(
                "open owned launcher for location reveal",
                Operation::OpenLauncher,
            ),
            observe("reveal launcher is ready", Check::LauncherReady),
            Step::new(
                "find owned reveal target",
                Operation::Text(LauncherQuery, "Keyboard 07"),
            ),
            observe("owned reveal target is exact", Check::LauncherExactResult),
            press(
                "Shift Enter reveals owned file without opening it",
                LauncherQuery,
                Key::Enter,
                SHIFT,
            ),
            observe(
                "launcher reveal selects the exact owned file",
                Check::LauncherRevealed,
            ),
            Step::new(
                "restore normal appearance for network",
                Operation::Appearance(Appearance::Wide),
            ),
            observe("network viewport size settles", Check::Size(1200.0, 800.0)),
            Step::new(
                "prepare owned offline network rows",
                Operation::NetworkPreview,
            ),
            seek("focus network sidebar preview", Sidebar, Key::F6, 0, 12),
            Step::new(
                "capture network offline and saved resources",
                Operation::Capture(Viewport::Root, "kbd-network"),
            ),
        ]);
        for (menu, label) in [
            ("burger", "kbd-burger-menu"),
            ("pane-more", "kbd-pane-more-menu"),
            ("places", "kbd-places-menu"),
            ("sidebar-background", "kbd-sidebar-sections-menu"),
            ("tab", "kbd-tab-menu"),
        ] {
            steps.extend([
                Step::new("open owned menu preview", Operation::MenuPreview(menu)),
                Step::new(
                    "capture grouped menu glyphs",
                    Operation::Capture(Viewport::Root, label),
                ),
                Step::new(
                    "Escape closes owned menu preview",
                    Operation::Press(Owner::Menu, Key::Escape, 0),
                ),
                observe("menu preview finishes closing", Check::NoPopup),
            ]);
        }
        for (kind, label) in [
            (foldex_shell::BuiltinKind::New, "kbd-new-submenu"),
            (foldex_shell::BuiltinKind::Compress, "kbd-compress-submenu"),
        ] {
            steps.extend([
                Step::new(
                    "open read-only native built-in panel",
                    Operation::BuiltinPreview(kind),
                ),
                observe(
                    "native built-in definitions are ready",
                    Check::BuiltinReady(kind),
                ),
                Step::new(
                    "capture native built-in panel",
                    Operation::Capture(Viewport::Root, label),
                ),
                Step::new(
                    "Escape closes native built-in panel",
                    Operation::Press(Owner::Context, Key::Escape, 0),
                ),
                observe("native child returns to context root", Check::MenuPanels(1)),
                Step::new(
                    "Escape closes native context root",
                    Operation::Press(Owner::Context, Key::Escape, 0),
                ),
                observe("native built-in panel closes", Check::MenuClosed),
            ]);
        }
        steps.extend([
            Step::new("seed owned cut-state fixture", Operation::CutPreview(true)),
            seek("focus listing for cut preview", Listing, Key::F6, 0, 12),
            press("reveal first cut folder and file", Listing, Key::Home, 0),
            Step::new(
                "capture cut text and icon opacity",
                Operation::Capture(Viewport::Root, "kbd-cut-details"),
            ),
            Step::new(
                "clear owned cut-state fixture",
                Operation::CutPreview(false),
            ),
            Step::new(
                "prepare owned permanent delete confirmation",
                Operation::PermanentPreview,
            ),
            observe("permanent confirmation is open", Check::DeletePreview),
            Step::new(
                "capture permanent delete confirmation",
                Operation::Capture(Viewport::Root, "kbd-permanent-delete"),
            ),
            Step::new(
                "Escape cancels permanent deletion",
                Operation::Press(Owner::Menu, Key::Escape, 0),
            ),
            observe(
                "permanent deletion cancels without a file action",
                Check::NoPopup,
            ),
        ]);
        for (appearance, label, width, height) in [
            (Appearance::Wide, "kbd-hotkeys-wide", 1200.0, 800.0),
            (Appearance::Compact, "kbd-hotkeys-compact", 640.0, 540.0),
            (Appearance::Paper, "kbd-hotkeys-paper", 1200.0, 800.0),
            (
                Appearance::LargerText,
                "kbd-hotkeys-large-text",
                1200.0,
                800.0,
            ),
        ] {
            steps.extend([
                Step::new(
                    "apply Hotkeys appearance",
                    Operation::Appearance(appearance),
                ),
                observe("Hotkeys root dimensions settle", Check::Size(width, height)),
                Step::new(
                    "open shared Hotkeys command surface",
                    Operation::HotkeysPreview,
                ),
                observe(
                    "Hotkeys query owns keyboard focus",
                    Check::Focus(HotkeysSearch),
                ),
                observe(
                    "Hotkeys surface and binding chips fit",
                    Check::HotkeysGeometry,
                ),
                Step::new(
                    "capture shared Hotkeys surface",
                    Operation::Capture(Viewport::Root, label),
                ),
            ]);
            if matches!(appearance, Appearance::Wide) {
                steps.extend([
                    seek(
                        "Tab reaches a binding's internal remove button",
                        HotkeysRemove,
                        Key::Tab,
                        0,
                        24,
                    ),
                    press(
                        "Space requests binding removal",
                        HotkeysRemove,
                        Key::Space,
                        0,
                    ),
                    observe(
                        "binding removal initially focuses Cancel",
                        Check::HotkeysConfirmation,
                    ),
                    Step::new(
                        "capture binding removal confirmation",
                        Operation::Capture(Viewport::Root, "kbd-hotkeys-remove-confirmation"),
                    ),
                    Step::new(
                        "Escape cancels binding removal",
                        Operation::Press(Owner::Hotkeys, Key::Escape, 0),
                    ),
                    observe(
                        "Cancel restores unchanged Hotkeys surface",
                        Check::HotkeysGeometry,
                    ),
                ]);
            }
            steps.extend([
                Step::new(
                    "Escape closes Hotkeys surface",
                    Operation::Press(Owner::Hotkeys, Key::Escape, 0),
                ),
                observe("Hotkeys relinquishes popup ownership", Check::NoPopup),
            ]);
        }
        steps.extend([
            Step::new(
                "restore final wide appearance",
                Operation::Appearance(Appearance::Wide),
            ),
            observe("final root dimensions settle", Check::Size(1200.0, 800.0)),
        ]);
        steps.extend(recycle_layout_workflow());
        steps.push(Step::new(
            "finish and validate untouched fixture",
            Operation::Finish,
        ));
        if std::env::args_os().any(|argument| argument == "--benchmark-keyboard-followup") {
            // Isolate the newly added surfaces for a short visual iteration.
            // The complete release workflow still covers every preceding step.
            let at = steps
                .iter()
                .position(|step| step.name == "apply Hotkeys appearance")
                .unwrap();
            let tail = steps.split_off(at);
            steps = vec![
                Step::new("prepare isolated followup model", Operation::Prepare),
                observe("followup generated directory is ready", Check::BrowserReady),
                seek("focus followup listing", Listing, Key::F6, 0, 12),
                Step::new(
                    "initialize owned followup launcher input",
                    Operation::OpenLauncher,
                ),
                observe("followup launcher is ready", Check::LauncherReady),
                Step::new(
                    "dismiss followup launcher",
                    Operation::Press(Owner::Launcher, Key::Escape, 0),
                ),
                observe("followup launcher closes", Check::LauncherClosed),
            ];
            steps.extend(tail);
        }
        steps
    })
}

fn modifiers(bits: u8) -> Modifiers {
    Modifiers {
        ctrl: bits & CTRL != 0,
        command: bits & CTRL != 0,
        shift: bits & SHIFT != 0,
        alt: bits & ALT != 0,
        mac_cmd: false,
    }
}
fn key_events(key: Key, bits: u8) -> Vec<egui::Event> {
    let modifiers = modifiers(bits);
    [true, false]
        .into_iter()
        .map(|pressed| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        })
        .collect()
}

impl Foldex {
    /// Main integration calls this once per root UI pass beside other workloads.
    /// The plugin installs only when --benchmark-keyboard created an isolated run.
    pub(super) fn keyboard_benchmark_step(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self
            .benchmark
            .as_mut()
            .and_then(|benchmark| benchmark.keyboard.take())
        else {
            return;
        };
        let now = Instant::now();
        if !run.installed {
            run.expected_steps = workflow().len();
            ctx.add_plugin(InputPlugin {
                shared: run.shared.clone(),
            });
            run.installed = true;
            ctx.data_mut(|data| data.insert_temp(Id::new("benchmark-probe-layout"), true));
        }
        if !run.finished {
            self.keyboard_capture_poll(ctx, &mut run);
            let transport_error = {
                let state = run.shared.lock().unwrap_or_else(|error| error.into_inner());
                state.capture_error.clone().or_else(|| {
                    state
                        .overflow
                        .then(|| "Owned keyboard input/capture transport overflowed".into())
                })
            };
            if !run.finished
                && let Some(error) = transport_error
            {
                run.fail(error);
            }
        }
        if !run.finished
            && self.started.elapsed() > Duration::from_millis(1500)
            && run.next.is_none_or(|next| now >= next)
        {
            let step = workflow()[run.phase];
            let since = *run.phase_started.get_or_insert(now);
            let phase_deadline = match step.operation {
                Operation::Capture(_, label) => {
                    run.pending_capture.is_none()
                        && !run.captured_labels.iter().any(|saved| saved == label)
                }
                _ => true,
            };
            if now.duration_since(since) >= PHASE_TIMEOUT && phase_deadline {
                let detail = self.keyboard_sample(&run, step.name);
                run.check(step.name, false, detail);
            } else {
                let complete = self.keyboard_operation(ctx, &mut run, step);
                if complete && !run.finished {
                    if run.samples.len() >= MAX_SAMPLES {
                        run.fail("Keyboard phase sample limit exceeded");
                    } else {
                        run.samples.push(self.keyboard_sample(&run, step.name));
                        run.phase += 1;
                        run.phase_started = None;
                        run.attempts = 0;
                    }
                }
            }
            run.next = Some(
                now + if matches!(step.operation, Operation::IconSweep) {
                    Duration::from_millis(16)
                } else {
                    STEP_DELAY
                },
            );
        }
        if !run.finished && run.phase == run.expected_steps {
            run.finished = true;
            run.shared
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .active = false;
        }
        if run.finished {
            let expires = now + Duration::from_millis(500);
            let benchmark = self.benchmark.as_mut().unwrap();
            benchmark.expires = benchmark.expires.min(expires);
        } else {
            // Only this explicit finite workload schedules these wakeups. Normal
            // application frames and a completed benchmark gain no idle timer.
            ctx.request_repaint_after(
                run.next
                    .unwrap_or(now)
                    .saturating_duration_since(now)
                    .min(STEP_DELAY),
            );
        }
        self.benchmark.as_mut().unwrap().keyboard = Some(run);
    }

    fn keyboard_operation(
        &mut self,
        ctx: &egui::Context,
        run: &mut KeyboardUiRun,
        step: Step,
    ) -> bool {
        match step.operation {
            Operation::Prepare => {
                let scope = self.benchmark.as_ref().unwrap().scope.clone();
                self.sidebar = true;
                self.inspector = false;
                self.side_filter.clear();
                self.bookmarks = vec![
                    scope.join("Keyboard Folder A"),
                    scope.join("Keyboard Folder B"),
                ];
                self.recent = VecDeque::from(vec![
                    scope.join("Keyboard 00.txt"),
                    scope.join("Keyboard 07.txt"),
                ]);
                self.home_history.entries.clear();
                for name in ["Keyboard Folder A", "Keyboard 00.txt", "Keyboard 07.txt"] {
                    self.record_home_usage(scope.join(name), crate::home_history::Access::Visited);
                }
                self.launcher.settings.file_roots = vec![scope.clone()];
                self.theme = Theme::default();
                self.theme.set_color_mode(foldex_ui::ColorMode::Dark, None);
                self.theme.apply(ctx);
                self.pane_mut().zoom = 0.0;
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(1200.0, 800.0)));
                run.path_before = scope;
                true
            }
            Operation::Press(owner, key, bits) => self.keyboard_key(ctx, run, owner, key, bits),
            Operation::OpenText(key, bits, text) => {
                if !self.keyboard_owner(run, Owner::Browser) {
                    run.fail(format!(
                        "Opening shortcut has no browser owner: {}",
                        step.name
                    ));
                    return false;
                }
                let mut events = key_events(key, bits);
                events.push(egui::Event::ModifiersChanged(Modifiers::NONE));
                events.push(egui::Event::Text(text.into()));
                queue_input(ctx, run, Viewport::Root, events, Modifiers::NONE)
            }
            Operation::Text(target, text) => {
                if !self.keyboard_focus(run, target) {
                    run.fail(format!("Text has no expected editor owner: {}", step.name));
                    return false;
                }
                queue_input(
                    ctx,
                    run,
                    target.viewport(),
                    vec![egui::Event::Text(text.into())],
                    Modifiers::NONE,
                )
            }
            Operation::HoverFocus(viewport) => {
                let Some(rect) = run.probe(viewport).and_then(|probe| probe.focus_rect) else {
                    return false;
                };
                queue_input(
                    ctx,
                    run,
                    viewport,
                    vec![egui::Event::PointerMoved(rect.center())],
                    Modifiers::NONE,
                )
            }
            Operation::LeavePointer(viewport) => queue_input(
                ctx,
                run,
                viewport,
                vec![egui::Event::PointerGone],
                Modifiers::NONE,
            ),
            Operation::Compose(stage) => {
                if !self.keyboard_focus(run, Target::Search) {
                    run.fail("IME has no search editor owner");
                    return false;
                }
                let events = match stage {
                    Composition::Preedit => vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                        text: "键盘".into(),
                        active_range_chars: None,
                    })],
                    Composition::Commit => {
                        vec![egui::Event::Ime(egui::ImeEvent::Commit("键盘".into()))]
                    }
                };
                queue_input(ctx, run, Viewport::Root, events, Modifiers::NONE)
            }
            Operation::Seek(target, key, bits, limit) => {
                if self.keyboard_focus(run, target) {
                    run.check(step.name, true, serde_json::json!({"keypresses":run.attempts,"target":format!("{target:?}")}));
                    return true;
                }
                if run.attempts >= limit {
                    let detail = self.keyboard_sample(run, step.name);
                    run.check(step.name, false, detail);
                    return false;
                }
                let owner = if target.viewport() == Viewport::Launcher {
                    Owner::Launcher
                } else if self.options {
                    Owner::Options
                } else if self.hotkeys_open {
                    Owner::Hotkeys
                } else {
                    Owner::RootRegion
                };
                if self.keyboard_key(ctx, run, owner, key, bits) {
                    run.attempts += 1;
                }
                false
            }
            Operation::Observe(check) => {
                let passed = self.keyboard_condition(run, check);
                if passed {
                    run.check(step.name, true, self.keyboard_sample(run, step.name));
                }
                passed
            }
            Operation::Remember(slot) => {
                let focus = run.probe(Viewport::Root).and_then(|probe| probe.focus);
                match slot {
                    Remember::Invoker => run.invoker = focus,
                    Remember::MenuParent => run.menu_parent = focus,
                    Remember::Selection => {
                        run.selection_before = self.pane().selected.intervals();
                        run.focused_row_before = self.pane().focused;
                        run.path_before = self.pane().path.clone();
                    }
                    Remember::Options => run.radius_before = self.theme.radius,
                    Remember::Editor => {
                        run.editor_invoker = focus;
                        run.roots_before = self.launcher.settings.file_roots.clone();
                    }
                    Remember::LauncherSelection => {
                        run.launcher_selected_before = self.launcher.selected
                    }
                }
                true
            }
            Operation::Capture(viewport, label) => {
                if run.captured_labels.iter().any(|saved| saved == label) {
                    return true;
                }
                if run.pending_capture.is_none() {
                    if run.captures.len() >= MAX_CAPTURES {
                        run.fail("Keyboard capture limit exceeded");
                        return false;
                    }
                    let Some(probe) = run.probe(viewport) else {
                        return false;
                    };
                    if !probe.input_focused
                        || !probe
                            .focus_rect
                            .is_some_and(|rect| geometry_visible(rect, probe.content))
                    {
                        return false;
                    }
                    run.pending_capture = Some(PendingCapture {
                        label: label.into(),
                        viewport,
                        since: Instant::now(),
                    });
                    ctx.send_viewport_cmd_to(
                        viewport.id(),
                        egui::ViewportCommand::Screenshot(egui::UserData::new(label.to_owned())),
                    );
                    ctx.request_repaint_of(viewport.id());
                }
                false
            }
            Operation::Circuit(backwards) => self.keyboard_circuit(ctx, run, backwards, step.name),
            Operation::Appearance(appearance) => {
                run.compact = matches!(appearance, Appearance::Compact);
                self.theme = if matches!(appearance, Appearance::Paper) {
                    Theme::paper()
                } else {
                    Theme::default()
                };
                if matches!(appearance, Appearance::LargerText) {
                    self.theme.font_size = 17.0;
                }
                if !matches!(appearance, Appearance::Paper) {
                    self.theme.set_color_mode(foldex_ui::ColorMode::Dark, None);
                }
                self.theme.apply(ctx);
                self.launcher_positioned = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(if run.compact {
                    vec2(640.0, 540.0)
                } else {
                    vec2(1200.0, 800.0)
                }));
                true
            }
            Operation::HomeZoom(zoom) => {
                if !self.pane().is_virtual()
                    && self.pane().path != self.benchmark.as_ref().unwrap().scope
                {
                    run.fail("Zoom workload has no generated or virtual owner");
                    return false;
                }
                self.pane_mut().zoom = zoom;
                keyboard::request_id(ctx, keyboard::listing_id(self.pane().id));
                ctx.request_repaint();
                true
            }
            Operation::IconSweep => {
                let (requested, presentable, retained) = self.assets.view_presented(self.pane().id);
                let metrics = self.assets.metrics();
                if requested == 0
                    || presentable != requested
                    || metrics.gpu_pixel_bytes > crate::assets::GPU_PIXEL_BUDGET
                    || metrics.gpu_entries > crate::assets::MAX_GPU_ENTRIES
                {
                    run.check(step.name, false, serde_json::json!({
                        "frame":run.attempts,"requested":requested,"presentable":presentable,"retained":retained,
                        "gpu_bytes":metrics.gpu_pixel_bytes,"gpu_entries":metrics.gpu_entries
                    }));
                    return false;
                }
                run.attempts += 1;
                if run.attempts >= 96 {
                    run.check(step.name, true, serde_json::json!({
                        "frames":run.attempts,"requested":requested,"presentable":presentable,"retained":retained,
                        "gpu_bytes":metrics.gpu_pixel_bytes,"gpu_entries":metrics.gpu_entries
                    }));
                    return true;
                }
                self.pane_mut().zoom = [0.0, 0.65, 0.1, 1.0, 0.2, 0.3][run.attempts as usize % 6];
                let size = [vec2(1200.0, 800.0), vec2(880.0, 640.0), vec2(1040.0, 720.0)]
                    [run.attempts as usize % 3];
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                ctx.request_repaint();
                false
            }
            Operation::HotkeysPreview => {
                self.hotkeys_open = true;
                self.hotkeys.benchmark_open_editor("");
                ctx.request_repaint();
                true
            }
            Operation::RecyclePreview => {
                self.benchmark_recycle_preview(ctx);
                true
            }
            Operation::RecycleConfirm(empty) => {
                self.recycle_action(if empty {
                    crate::recycle_bin::Intent::Empty
                } else {
                    crate::recycle_bin::Intent::RestoreAll
                });
                true
            }
            Operation::OpenLauncher => {
                // This bootstraps the owned window. All query, Tab, arrows, and
                // dismissal being tested below go through real child RawInput.
                self.open_launcher(false);
                self.launcher.set_mode(Mode::Files);
                self.launcher.set_query("");
                true
            }
            Operation::Calculator => {
                self.launcher.set_mode(Mode::Calculator);
                true
            }
            Operation::NetworkPreview => {
                self.benchmark_network_sidebar(
                    &[
                        (r"\\studio\projects", Some("Z:"), true),
                        (r"\\archive\shared", None, false),
                    ],
                    Some("The network is unavailable. Saved connections are shown."),
                );
                true
            }
            Operation::MenuPreview("sidebar-background") => {
                self.benchmark_open_sidebar_background_menu(ctx)
            }
            Operation::MenuPreview("tab") => {
                let pane = self.tabs[self.active].left.id;
                let trigger = Id::new(("tab", pane)).with("hit");
                let Some(response) = ctx.read_response(trigger) else {
                    return false;
                };
                self.tab_menu_id = Some(pane);
                self.tab_menu_anchor =
                    Rect::from_min_size(response.rect.left_bottom(), vec2(240.0, 0.0));
                ctx.memory_mut(|memory| memory.request_focus(trigger));
                self.tab_menu.close();
                self.tab_menu.toggle(ctx);
                true
            }
            Operation::MenuPreview(menu) => self.benchmark_open_menu(ctx, menu),
            Operation::BuiltinPreview(kind) => {
                let scope = &self.benchmark.as_ref().unwrap().scope;
                let path = if kind == foldex_shell::BuiltinKind::New {
                    scope.clone()
                } else {
                    scope.join("Keyboard 00.txt")
                };
                self.context.benchmark_open_builtin(ctx, path, kind)
            }
            Operation::PermanentPreview => {
                let scope = self.benchmark.as_ref().unwrap().scope.clone();
                self.request_delete(
                    vec![scope.join("Keyboard 00.txt")],
                    crate::pane::DeleteMode::Permanent,
                );
                true
            }
            Operation::CutPreview(active) => {
                let scope = self.benchmark.as_ref().unwrap().scope.clone();
                let paths = if active {
                    vec![
                        scope.join("Keyboard Folder A"),
                        scope.join("Keyboard 00.txt"),
                    ]
                } else {
                    Vec::new()
                };
                match self.assets.benchmark_set_cut_paths(&paths) {
                    Ok(()) => true,
                    Err(error) => {
                        run.fail(format!("Owned cut fixture failed: {error}"));
                        false
                    }
                }
            }
            Operation::WatchLauncherRow => {
                let Some(row) = self.launcher.rows.get(self.launcher.selected) else {
                    return false;
                };
                run.shared
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .watched[Viewport::Launcher.slot()] =
                    vec![Id::new(("launcher-result", row.id))];
                ctx.request_repaint_of(Viewport::Launcher.id());
                true
            }
            Operation::SettleHelp => {
                if !self.keyboard_condition(run, Check::HelpOpen) {
                    return false;
                }
                let Some(probe) = run.probe(Viewport::Root) else {
                    return false;
                };
                let Some(rect) =
                    ctx.memory(|memory| memory.area_rect(Id::new("keyboard-navigation-help")))
                else {
                    return false;
                };
                let width = 570.0 * self.theme.metrics().scale;
                if !geometry_visible(rect, probe.content)
                    || (rect.width() - width).abs() > 2.0
                    || !probe
                        .focus_rect
                        .is_some_and(|focus| rect.expand(1.0).contains_rect(focus))
                {
                    return false;
                }
                let now = Instant::now();
                if run.help_rect.is_none_or(|previous| {
                    (previous.min - rect.min).length() > 0.5
                        || (previous.max - rect.max).length() > 0.5
                }) {
                    run.help_rect = Some(rect);
                    run.help_stable_since = Some(now);
                    return false;
                }
                if run
                    .help_stable_since
                    .is_some_and(|since| now.duration_since(since) >= Duration::from_millis(250))
                {
                    run.check(step.name, true, serde_json::json!({
                        "help_rect":workload::rect_values(rect),"expected_width":width,
                        "stable_for_ms":run.help_stable_since.map(|since|now.duration_since(since).as_millis()),
                        "root":probe.report()
                    }));
                    return true;
                }
                false
            }
            Operation::Finish => {
                let scope = &self.benchmark.as_ref().unwrap().scope;
                let unchanged = workload::fixture_unchanged(scope).unwrap_or(false);
                run.check(
                    "generated fixture remains unchanged",
                    unchanged,
                    serde_json::json!({"scope":scope}),
                );
                if run.finished {
                    return false;
                }
                let batches = run
                    .shared
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .batches;
                run.check(
                    "both owned viewports received RawInput",
                    batches.iter().all(|count| *count > 0),
                    serde_json::json!({"batches":batches}),
                );
                !run.finished
            }
        }
    }

    fn keyboard_focus(&self, run: &KeyboardUiRun, target: Target) -> bool {
        let Some(probe) = run.probe(target.viewport()) else {
            return false;
        };
        let Some(focus) = probe.focus else {
            return false;
        };
        let options = Id::new("appearance-options-popup");
        match target {
            Target::Listing => focus == keyboard::listing_id(self.pane().id),
            Target::Sidebar => focus == keyboard::sidebar_id(),
            Target::Search => focus == Id::new((self.pane().id, "filter")),
            Target::Inspector => probe.region == Some(Region::Inspector),
            Target::FilesToggle => focus == Id::new((self.pane().id, "kind-count", false)),
            Target::OptionsSearch => focus == options.with("search"),
            Target::OptionsResults => focus == options.with("results-focus"),
            Target::OptionsEditor => focus == options.with("launcher-roots"),
            Target::OptionsControl => {
                probe.focus_layer == Some(options)
                    && ![
                        options.with("search"),
                        options.with("search").with("clear"),
                        options.with("results-focus"),
                        options.with("close"),
                        options.with("launcher-roots"),
                    ]
                    .contains(&focus)
            }
            Target::PaletteSearch => focus == Id::new("command-palette-input"),
            Target::GoToSearch => focus == Id::new("goto-input"),
            Target::HelpClose => focus == Id::new("keyboard-navigation-help").with("close"),
            Target::HelpScroll => focus == Id::new("keyboard-guide-focus"),
            Target::LauncherQuery => focus == Id::new("launcher-query"),
            Target::LauncherResults => focus == Id::new("launcher-results-focus"),
            Target::HotkeysSearch => focus == Id::new("hotkeys-search"),
            Target::HotkeysRemove => self
                .hotkeys
                .benchmark_editor_probe()
                .and_then(|probe| probe.rows.iter().find_map(|row| row.bindings.first()))
                .is_some_and(|binding| focus == Id::new(("hotkey-remove", binding.index))),
        }
    }

    fn keyboard_owner(&self, run: &KeyboardUiRun, owner: Owner) -> bool {
        match owner {
            Owner::Browser => !self.popup_active() && !self.launcher_open,
            Owner::RootRegion => {
                !self.popup_active()
                    && run
                        .probe(Viewport::Root)
                        .is_some_and(|probe| probe.focus.is_some())
            }
            Owner::Context => self.context.is_open(),
            // The inline name editor owns Escape while it is open.
            Owner::Options => self.options || self.renaming(),
            Owner::Help => self.keyboard_condition(run, Check::HelpOpen),
            Owner::Launcher => self.launcher_open,
            Owner::Menu => self.popup_active(),
            Owner::Hotkeys => self.hotkeys_open,
            Owner::Focus(target) => self.keyboard_focus(run, target),
        }
    }

    fn keyboard_key(
        &self,
        ctx: &egui::Context,
        run: &mut KeyboardUiRun,
        owner: Owner,
        key: Key,
        bits: u8,
    ) -> bool {
        if !self.keyboard_owner(run, owner) {
            run.fail(format!(
                "Keyboard input lost its expected owner at phase {}",
                run.phase
            ));
            return false;
        }
        // The sole result activation allowed here is Shift+Enter on an exact
        // generated file. It reveals inside Foldex; it never opens that file.
        let owned_reveal = key == Key::Enter
            && bits == SHIFT
            && matches!(owner, Owner::Focus(Target::LauncherQuery | Target::LauncherResults))
            && self.launcher.rows.len() == 1
            && self.launcher.rows.get(self.launcher.selected).is_some_and(|row| {
                matches!(&row.action, crate::launcher::ResultAction::Open(path) | crate::launcher::ResultAction::Browse(path)
                    if path == &self.benchmark.as_ref().unwrap().scope.join("Keyboard 07.txt"))
            });
        if key == Key::Enter
            && !owned_reveal
            && !matches!(
                owner,
                Owner::Focus(
                    Target::Sidebar
                        | Target::OptionsControl
                        | Target::OptionsSearch
                        | Target::OptionsResults
                )
            )
        {
            run.fail("Refusing result activation in keyboard benchmark");
            return false;
        }
        let viewport = match owner {
            Owner::Focus(target) => target.viewport(),
            Owner::Launcher => Viewport::Launcher,
            _ => Viewport::Root,
        };
        queue_input(ctx, run, viewport, key_events(key, bits), modifiers(bits))
    }

    fn keyboard_condition(&self, run: &KeyboardUiRun, check: Check) -> bool {
        let root = run.probe(Viewport::Root);
        let child = run.probe(Viewport::Launcher);
        let pane = self.pane();
        let ready = !pane.loading && pane.displayed_view_is_current();
        let scope = &self.benchmark.as_ref().unwrap().scope;
        match check {
            Check::BrowserReady => {
                !pane.is_home() && ready && &pane.path == scope && pane.view_len() >= 2
            }
            Check::SearchMatches => {
                ready
                    && pane.filter == "Keyboard"
                    && pane.view_len() == 34
                    && (0..pane.view_len()).all(|row| {
                        pane.path_at(row).is_some_and(|path| {
                            path.file_name().is_some_and(|name| {
                                name.to_string_lossy().to_lowercase().contains("keyboard")
                            })
                        })
                    })
            }
            Check::ImeCommitted => ready && pane.filter == "键盘" && pane.view_len() == 1,
            Check::NoFileAction => {
                !self.renaming()
                    && !self.deletion.is_open()
                    && pane.path == run.path_before
                    && !pane.address_mode
                    && pane.selected.intervals() == run.selection_before
                    && self.keyboard_focus(run, Target::Search)
            }
            Check::Focus(target) => {
                self.keyboard_focus(run, target)
                    && run.probe(target.viewport()).is_some_and(|probe| {
                        probe.input_focused
                            && probe
                                .focus_rect
                                .is_some_and(|rect| geometry_visible(rect, probe.content))
                    })
            }
            Check::KeyboardCues(viewport, expected) => run
                .probe(viewport)
                .is_some_and(|probe| probe.keyboard_cues == expected),
            Check::Selection(expected) => pane.selected.iter().collect::<Vec<_>>() == expected,
            Check::FocusedRow(row) => pane.focused == Some(row),
            Check::FocusedLast => pane.focused == pane.view_len().checked_sub(1),
            Check::FocusedVisible => {
                self.keyboard_focus(run, Target::Listing)
                    && pane
                        .focused
                        .is_some_and(|row| pane.visible_rows.contains(&row))
            }
            Check::UnselectedFocus => {
                self.keyboard_focus(run, Target::Listing)
                    && pane
                        .focused
                        .is_some_and(|row| !pane.selected.contains(&(row as u32)))
                    && !pane.selected.is_empty()
            }
            Check::SelectionPreserved => pane.selected.intervals() == run.selection_before,
            Check::TabLeftListing => root.is_some_and(|probe| {
                probe.focus.is_some() && probe.region != Some(Region::Listing(pane.id))
            }),
            Check::FoldersOnly => {
                ready && pane.query.kind == foldex_core::KindFilter::Folders && pane.view_len() == 2
            }
            Check::AllKinds => {
                ready && pane.query.kind == foldex_core::KindFilter::All && pane.view_len() >= 34
            }
            Check::MenuOpen => {
                self.context.is_open()
                    && root.is_some_and(|probe| probe.focus.is_some() && probe.focus != run.invoker)
            }
            Check::MenuPanels(count) => {
                self.context.is_open()
                    && root.is_some_and(|probe| {
                        probe.context_panels.len() == count
                            && probe
                                .context_panels
                                .iter()
                                .all(|rect| geometry_visible(*rect, probe.content))
                    })
            }
            Check::MenuParentRestored => {
                self.context.is_open()
                    && root.is_some_and(|probe| {
                        probe.focus == run.menu_parent && probe.focus.is_some()
                    })
            }
            Check::MenuClosed => !self.context.is_open(),
            Check::InvokerRestored => {
                !self.popup_active()
                    && root.is_some_and(|probe| {
                        probe.focus == run.invoker
                            && probe.focus.is_some()
                            && probe
                                .focus_rect
                                .is_some_and(|rect| geometry_visible(rect, probe.content))
                    })
            }
            Check::SidebarDidNotNavigate => {
                !pane.is_home()
                    && pane.path == run.path_before
                    && self.keyboard_focus(run, Target::Sidebar)
            }
            Check::HomeReady => pane.is_home() && !self.home.rows.is_empty(),
            Check::HomeLayout(zoom) => {
                pane.is_home() && pane.zoom == zoom && self.ui_context.data(|data| {
                    data.get_temp::<home_ui::HomeProbe>(Id::new((pane.id, "home-probe")))
                }).is_some_and(|probe| {
                    let search = pane.benchmark_footer_search_rect();
                    let view = pane.benchmark_footer_view_rect();
                    probe.mode == foldex_ui::view::ViewMode::from_zoom(zoom)
                        && probe.columns >= 1 && probe.asset_requests <= 128
                        && probe.footer.contains_rect(search) && probe.footer.contains_rect(view)
                        && search.right() <= pane.benchmark_footer_kind_rect(true).left()
                        && !pane.benchmark_footer_expand_rect().is_positive()
                        && root.is_some_and(|root| geometry_visible(probe.footer, root.content))
                })
            }
            Check::HomeFilter => pane.is_home() && pane.filter == "Keyboard 07"
                && self.ui_context.data(|data| data.get_temp::<home_ui::HomeProbe>(Id::new((pane.id, "home-probe"))))
                    .is_some_and(|probe| probe.recent == 1 && probe.recommended == 0)
                && self.ui_context.read_response(Id::new((pane.id, "filter")).with("clear"))
                    .is_some_and(|response| response.rect.is_positive()),
            Check::SidebarNetwork(visible) => self.sidebar_sections.preferences().sections.network == visible,
            Check::HotkeysGeometry => self.hotkeys_open && self.hotkeys.benchmark_editor_probe().is_some_and(|probe| {
                root.is_some_and(|root| geometry_visible(probe.outer, root.content))
                    && probe.outer.contains_rect(probe.search) && probe.outer.contains_rect(probe.close)
                    && probe.outer.contains_rect(probe.footer)
                    && probe.search.right() <= probe.close.left()
                    && probe.settings.bottom() <= probe.separator.top() + 0.1
                    && self.ui_context.read_response(Id::new("hotkeys-editor-window").with("clear")).is_none()
                    && probe.rows.iter().all(|row| {
                        row.rect.expand(0.1).contains_rect(row.text)
                            && row.rect.top() >= probe.separator.bottom() - 0.1
                            && (row.text.right() <= row.bindings_area.left() + 0.1
                                || row.text.bottom() < row.bindings_area.top())
                            && row.bindings.iter().all(|binding| {
                                binding.chip.contains_rect(binding.edit) && binding.chip.contains_rect(binding.remove)
                                    && binding.edit.right() <= binding.remove.left() + 0.1
                            })
                    })
            }),
            Check::HotkeysConfirmation => self.hotkeys_open && root.is_some_and(|probe| {
                probe.focus == Some(Id::new("hotkey-confirmation-cancel"))
            }),
            Check::RecycleLayout(zoom, count) => pane.is_recycle_bin() && !pane.loading && pane.zoom == zoom
                && pane.view_len() == count && recycle_ui::probe(&self.ui_context, pane.id).is_some_and(|probe| {
                    root.is_some_and(|root| geometry_visible(probe.footer, root.content)
                        && geometry_visible(probe.empty, root.content) && geometry_visible(probe.restore_all, root.content)
                        && geometry_visible(probe.address, root.content))
                        && probe.items == count && probe.footer.contains_rect(pane.benchmark_footer_search_rect())
                        && probe.footer.contains_rect(pane.benchmark_footer_view_rect())
                        && !probe.empty.intersects(probe.restore_all)
                        && !probe.empty.intersects(probe.address) && !probe.restore_all.intersects(probe.address)
                }),
            Check::RecycleConfirmation => self.recycle.is_open() && root.is_some_and(|probe| {
                [0,1].into_iter().any(|slot| probe.focus == Some(Id::new(("recycle-confirmation", slot)).with("cancel")))
            }),
            Check::HomeSelected => {
                pane.is_home()
                    && pane
                        .home_selected
                        .as_ref()
                        .is_some_and(|path| path.starts_with(scope))
                    && self.keyboard_focus(run, Target::Listing)
                    && self
                        .ui_context
                        .data(|data| {
                            data.get_temp::<home_ui::HomeProbe>(Id::new((pane.id, "home-probe")))
                        })
                        .is_some_and(|probe| probe.selected_visible)
            }
            Check::OptionsOpen => self.options,
            Check::OptionsClosed => !self.options,
            Check::CornersChanged => self.theme.radius != run.radius_before && self.options,
            Check::CornersRestored => self.theme.radius == run.radius_before && self.options,
            Check::EditorCancelled => {
                self.options
                    && self.launcher.settings.file_roots == run.roots_before
                    && root.is_some_and(|probe| {
                        probe.focus == run.editor_invoker && probe.focus.is_some()
                    })
            }
            Check::PaletteOpen => self.commands.is_open() && self.commands.query() == "Options",
            Check::PaletteClosed => self.commands.finished(),
            Check::GoToOpen => self
                .goto
                .as_ref()
                .is_some_and(|goto| goto.input_text() == "Keyboard"),
            Check::GoToClosed => self.goto.is_none(),
            Check::RenameOpen => self.renaming() && !self.deletion.is_open(),
            Check::RenameCancelled => !self.renaming() && !self.deletion.is_open(),
            Check::HelpOpen => {
                self.keyboard_help
                    && self.popup_active()
                    && root.is_some_and(|probe| {
                        probe.focus.is_some()
                            && probe.focus_layer == Some(Id::new("keyboard-navigation-help"))
                    })
            }
            Check::HelpClosed => !self.keyboard_help && !self.popup_active(),
            Check::HelpScrolledEnd => {
                self.keyboard_help
                    && self.keyboard_focus(run, Target::HelpScroll)
                    && self
                        .ui_context
                        .data(|data| {
                            data.get_temp::<(f32, f32, f32)>(Id::new("keyboard-guide-focus"))
                        })
                        .is_some_and(|(offset, maximum, height)| {
                            maximum > 1.0 && height > 1.0 && (offset - maximum).abs() <= 1.0
                        })
            }
            Check::GeneratedFolderSelected => {
                pane.focused == Some(0)
                    && pane.selected.len() == 1
                    && pane.selected.contains(&0)
                    && pane.entry_at(0).is_some_and(|entry| entry.is_dir)
                    && pane
                        .primary_selected_path()
                        .is_some_and(|path| path.starts_with(scope))
            }
            Check::InspectorReady => {
                self.inspector
                    && (self.inspector_width - self.inspector_target_width).abs() <= 1.0
                    && self.inspector_width
                        >= crate::pane::MIN_INSPECTOR_WIDTH * self.theme.metrics().scale
                    && self
                        .inspector_view
                        .as_ref()
                        .and_then(|view| view.path())
                        .is_some_and(|path| pane.primary_selected_path().as_deref() == Some(path))
                    && self
                        .ui_context
                        .read_response(Id::new("inspector-folder-focus"))
                        .is_some_and(|response| response.rect.is_positive())
            }
            Check::InspectorClosed => !self.inspector && self.inspector_width < 1.0,
            Check::LauncherReady => {
                self.launcher_open
                    && self.launcher_positioned
                    && !self.launcher.pending()
                    && !self.launcher.searching_files()
                    && self.launcher.rows.len() >= 2
                    && child.is_some()
            }
            Check::LauncherMoved => {
                self.launcher.selected != run.launcher_selected_before && self.launcher_open
            }
            Check::LauncherLast => {
                !self.launcher.rows.is_empty()
                    && self.launcher.selected + 1 == self.launcher.rows.len()
            }
            Check::LauncherExactResult => {
                !self.launcher.pending()
                    && !self.launcher.searching_files()
                    && self.launcher.rows.len() == 1
                    && self.launcher.rows[0].title.contains("Keyboard 07")
            }
            Check::LauncherCalculation => {
                self.launcher.rows.first().is_some_and(|row| {
                    row.group == crate::launcher::ResultGroup::Calculator
                        && matches!(&row.action, crate::launcher::ResultAction::Copy(answer) if answer == "8")
                })
            }
            Check::CalculatorAnswer(answer) => !self.launcher.pending() && self.launcher.rows.first().is_some_and(|row| {
                row.group == crate::launcher::ResultGroup::Calculator
                    && matches!(&row.action, crate::launcher::ResultAction::Copy(value) if value == answer)
            }) && self.launcher.calculator_presentation().is_some_and(|scene| {
                scene.answer == answer && !scene.pending && scene.copy_enabled
                    && child.is_some_and(|probe| scene.answer_rect.is_some_and(|rect| geometry_visible(rect, probe.content))
                        && scene.copy_rect.is_some_and(|rect| geometry_visible(rect, probe.content)))
            }),
            Check::LauncherRevealed => !self.launcher_open && ready
                && pane.path == *scope
                && pane.primary_selected_path().as_deref() == Some(scope.join("Keyboard 07.txt").as_path()),
            Check::ThemeSaved => self.theme.scheme.name == "Owned Theme"
                && self.options_popup.theme_preferences_saved(),
            Check::NoPopup => !self.popup_active(),
            Check::AssetsReady => {
                let (requested, ready) = self.assets.view_ready(pane.id);
                requested > 0 && requested == ready
            }
            Check::DeletePreview => self.deletion.is_open(),
            Check::BuiltinReady(kind) => self.context.benchmark_builtin_state(kind).is_ok_and(|rows| rows.is_some_and(|count| count > 0)),
            Check::LauncherClosed => {
                !self.launcher_open
                    && native::launcher_visible(&format!(
                        "Foldex Launcher · {}",
                        std::process::id()
                    )) == Some(false)
            }
            Check::LauncherRowVisible => {
                self.keyboard_focus(run, Target::LauncherResults)
                    && self
                        .launcher
                        .rows
                        .get(self.launcher.selected)
                        .is_some_and(|row| {
                            let id = Id::new(("launcher-result", row.id));
                            child.is_some_and(|probe| {
                                probe.watched.iter().any(|(watched, rect)| {
                                    *watched == id && geometry_visible(*rect, probe.content)
                                })
                            })
                        })
            }
            Check::LauncherGeometry => self.keyboard_launcher_geometry(),
            Check::Size(width, height) => root.is_some_and(|probe| {
                (probe.content.width() - width).abs() < 2.0
                    && probe.content.height() >= height - 2.0
                    && probe.content.height() <= height + 40.0
            }),
        }
    }

    fn keyboard_launcher_geometry(&self) -> bool {
        let title = format!("Foldex Launcher · {}", std::process::id());
        native::launcher_bounds(&title)
            .zip(self.launcher_secondary)
            .is_some_and(|(rect, monitor)| {
                !monitor.primary
                    && rect[0] >= monitor.x
                    && rect[1] >= monitor.y
                    && rect[2] <= monitor.x + monitor.width
                    && rect[3] <= monitor.y + monitor.height
                    && (rect[0] + rect[2] - 2 * monitor.x - monitor.width).abs() <= 2
                    && (rect[1] + rect[3] - 2 * monitor.y - monitor.height).abs() <= 2
                    && native::launcher_locked(&title) == Some(true)
            })
    }

    fn keyboard_circuit(
        &self,
        ctx: &egui::Context,
        run: &mut KeyboardUiRun,
        backwards: bool,
        name: &str,
    ) -> bool {
        let Some(probe) = run.probe(Viewport::Root) else {
            return false;
        };
        let Some(region) = probe.region else {
            return false;
        };
        if run.circuit_origin.is_none() {
            run.circuit_origin = Some(region);
            run.circuit_last = Some(region);
            if backwards {
                run.reverse_cycle.clear();
            } else {
                run.forward_cycle.clear();
            }
        } else if run.circuit_last != Some(region) {
            if !probe.input_focused
                || !probe
                    .focus_rect
                    .is_some_and(|rect| geometry_visible(rect, probe.content))
            {
                run.check(name, false, probe.report());
                return false;
            }
            {
                let trace = if backwards {
                    &mut run.reverse_cycle
                } else {
                    &mut run.forward_cycle
                };
                if trace.len() >= 12 || trace.contains(&region) {
                    run.fail("F6 region circuit repeated a region before returning to its start");
                    return false;
                }
                trace.push(region);
            }
            run.circuit_last = Some(region);
            if Some(region) == run.circuit_origin {
                let trace = if backwards {
                    &run.reverse_cycle
                } else {
                    &run.forward_cycle
                };
                let required = [
                    Region::Tabs,
                    Region::Sidebar,
                    Region::Toolbar(self.pane().id),
                    Region::Listing(self.pane().id),
                    Region::Search(self.pane().id),
                    Region::Footer(self.pane().id),
                ];
                let passed = required.iter().all(|region| trace.contains(region))
                    && (!backwards || reverse_cycle(&run.forward_cycle) == run.reverse_cycle);
                let detail = serde_json::json!({"regions":trace.iter().map(|region|format!("{region:?}")).collect::<Vec<_>>()});
                run.check(name, passed, detail);
                run.circuit_origin = None;
                run.circuit_last = None;
                return passed;
            }
        } else if run.attempts > 0 {
            return false;
        }
        if self.keyboard_key(
            ctx,
            run,
            Owner::RootRegion,
            Key::F6,
            if backwards { SHIFT } else { 0 },
        ) {
            run.attempts += 1;
        }
        false
    }

    fn keyboard_sample(&self, run: &KeyboardUiRun, stage: &str) -> serde_json::Value {
        let pane = self.pane();
        serde_json::json!({
            "phase":run.phase,"stage":stage,"path":pane.path,"home":pane.is_home(),
            "query":pane.filter,"rows":pane.view_len(),"loading":pane.loading,
            "display_current":pane.displayed_view_is_current(),
            "selected_intervals":pane.selected.intervals(),"focused_row":pane.focused,
            "visible_rows":[pane.visible_rows.start,pane.visible_rows.end],
            "rename_open":self.renaming(),"delete_dialog_open":self.deletion.is_open(),
            "options_open":self.options,"context_open":self.context.is_open(),
            "keyboard_help_open":self.keyboard_help,
            "keyboard_help_scroll":self.ui_context.data(|data|data.get_temp::<(f32,f32,f32)>(Id::new("keyboard-guide-focus"))),
            "inspector_open":self.inspector,"inspector_width":self.inspector_width,
            "inspector_path":self.inspector_view.as_ref().and_then(|view|view.path()),
            "launcher_open":self.launcher_open,"launcher_selected":self.launcher.selected,
            "launcher_rows":self.launcher.rows.len(),"theme":self.theme.name,"font_size":self.theme.font_size,
            "footer": {
                "search": workload::rect_values(pane.benchmark_footer_search_rect()),
                "folders": workload::rect_values(pane.benchmark_footer_kind_rect(true)),
                "files": workload::rect_values(pane.benchmark_footer_kind_rect(false)),
                "view": workload::rect_values(pane.benchmark_footer_view_rect()),
            },
            "calculator": self.launcher.calculator_presentation().map(|scene| serde_json::json!({
                "answer": scene.answer, "displayed_query": scene.displayed_query, "pending": scene.pending,
                "copy_enabled": scene.copy_enabled, "rolling": scene.rolling,
                "answer_rect": scene.answer_rect.map(workload::rect_values),
                "copy_rect": scene.copy_rect.map(workload::rect_values),
            })),
            "root":run.probe(Viewport::Root).as_ref().map(Probe::report),
            "launcher":run.probe(Viewport::Launcher).as_ref().map(Probe::report)
        })
    }

    fn keyboard_capture_poll(&self, ctx: &egui::Context, run: &mut KeyboardUiRun) {
        let received = run
            .capture_result
            .as_ref()
            .and_then(|receiver| match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Keyboard PNG worker disconnected".into()))
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            });
        if let Some(result) = received {
            run.capture_result = None;
            if let Some(pending) = run.pending_capture.take() {
                match result {
                    Ok(path) => {
                        run.captured_labels.push(pending.label.clone());
                        run.captures.push(path.clone());
                        run.check(&format!("native capture {}", pending.label), true, serde_json::json!({"path":path,"viewport":format!("{:?}",pending.viewport)}));
                    }
                    Err(error) => run.fail(error),
                }
            } else {
                run.fail("Keyboard PNG arrived without a capture request");
            }
        }
        if run
            .pending_capture
            .as_ref()
            .is_some_and(|pending| pending.since.elapsed() >= CAPTURE_TIMEOUT)
        {
            run.fail(if run.capture_result.is_some() {
                "Keyboard PNG worker timed out"
            } else {
                "Keyboard native screenshot receipt timed out"
            });
            return;
        }
        let captured = run
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .images
            .pop_front();
        let Some(captured) = captured else {
            return;
        };
        let valid = run.pending_capture.as_ref().is_some_and(|pending| {
            pending.label == captured.label && pending.viewport == captured.viewport
        });
        if !valid || run.capture_result.is_some() {
            run.fail("Keyboard screenshot did not match its single outstanding owned capture");
            return;
        }
        if captured.image.pixels.len() > 16_000_000 || captured.image.pixels.is_empty() {
            run.fail("Keyboard screenshot exceeds its bounded image budget");
            return;
        }
        // Screenshot delivery and encoding each receive a finite deadline.
        run.pending_capture.as_mut().unwrap().since = Instant::now();
        let path = self
            .benchmark
            .as_ref()
            .unwrap()
            .output
            .with_extension(format!("{}.png", captured.label));
        let image = captured.image;
        let wake = ctx.clone();
        let (send, receiver) = std::sync::mpsc::sync_channel(1);
        run.capture_result = Some(receiver);
        if let Err(error) = std::thread::Builder::new()
            .name("foldex-keyboard-png".into())
            .spawn(move || {
                let result = (|| -> Result<PathBuf, String> {
                    use image::ImageEncoder;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(|error| error.to_string())?;
                    let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
                    for pixel in &image.pixels {
                        bytes.extend_from_slice(&pixel.to_array());
                    }
                    image::codecs::png::PngEncoder::new(&mut file)
                        .write_image(
                            &bytes,
                            image.width() as u32,
                            image.height() as u32,
                            image::ExtendedColorType::Rgba8,
                        )
                        .map_err(|error| error.to_string())?;
                    Ok(path)
                })();
                let _ = send.send(result);
                wake.request_repaint();
            })
        {
            run.capture_result = None;
            run.fail(format!("Cannot start keyboard PNG worker: {error}"));
        }
    }
}

fn queue_input(
    ctx: &egui::Context,
    run: &mut KeyboardUiRun,
    viewport: Viewport,
    events: Vec<egui::Event>,
    modifiers: Modifiers,
) -> bool {
    let result = run
        .shared
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .queue(viewport, events, modifiers);
    match result {
        Ok(()) => {
            ctx.request_repaint_of(viewport.id());
            true
        }
        Err(error) => {
            run.fail(error);
            false
        }
    }
}

fn reverse_cycle(forward: &[Region]) -> Vec<Region> {
    forward
        .iter()
        .rev()
        .skip(1)
        .copied()
        .chain(forward.last().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_region_circuit_preserves_start_and_reverses_every_other_region() {
        let forward = [
            Region::Search(1),
            Region::Footer(1),
            Region::Tabs,
            Region::Sidebar,
            Region::Toolbar(1),
            Region::Listing(1),
        ];
        assert_eq!(
            reverse_cycle(&forward),
            [
                Region::Toolbar(1),
                Region::Sidebar,
                Region::Tabs,
                Region::Footer(1),
                Region::Search(1),
                Region::Listing(1)
            ]
        );
    }

    #[test]
    fn workflow_has_bounded_capture_count_and_never_activates_a_file_or_launcher_result() {
        let plan = workflow();
        let captures = plan
            .iter()
            .filter(|step| matches!(step.operation, Operation::Capture(_, _)))
            .count();
        assert_eq!(captures, 64);
        assert!(captures <= MAX_CAPTURES);
        assert!(plan.len() <= MAX_SAMPLES);
        let mut labels = std::collections::HashSet::new();
        for step in plan {
            if let Operation::Capture(_, label) = step.operation {
                assert!(
                    label.starts_with("kbd-"),
                    "capture must use its transport namespace"
                );
                assert!(labels.insert(label));
            }
            if let Operation::Press(owner, key, bits) = step.operation {
                assert!(!matches!(key, Key::Delete | Key::Insert));
                if matches!(owner, Owner::Help) {
                    assert_eq!(key, Key::Escape);
                }
                if key == Key::Enter {
                    assert!(
                        matches!(
                            owner,
                            Owner::Focus(Target::Sidebar | Target::OptionsControl)
                        ) || matches!(owner, Owner::Focus(Target::LauncherQuery)) && bits == SHIFT
                    );
                }
            }
        }
        assert!(matches!(plan.last().unwrap().operation, Operation::Finish));
    }

    #[test]
    fn synthetic_windows_modifier_pairs_keep_selection_and_editor_semantics() {
        let event = key_events(Key::ArrowDown, CTRL | SHIFT);
        assert!(matches!(event.as_slice(), [
            egui::Event::Key { pressed:true, modifiers, .. },
            egui::Event::Key { pressed:false, .. }
        ] if modifiers.ctrl && modifiers.command && modifiers.shift && !modifiers.mac_cmd));
    }
}
