//! Bounded state and owned RawInput transport for the opt-in keyboard workflow.
//! The egui input plugin also covers immediate child viewports. It never sends
//! desktop input, retains a Context, invokes a result, or accesses the clipboard.
use crate::keyboard::Region;
use egui::{Context, Event, Id, Modifiers, RawInput, Rect, ViewportId};
use std::{
    collections::VecDeque,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

pub(crate) const PHASE_TIMEOUT: Duration = Duration::from_secs(7);
pub(crate) const CAPTURE_TIMEOUT: Duration = Duration::from_secs(7);
pub(crate) const MAX_EVENTS: usize = 16;
pub(crate) const MAX_CHECKS: usize = 384;
pub(crate) const MAX_SAMPLES: usize = 640;
pub(crate) const MAX_CAPTURES: usize = 64;
const FIXTURE_ROWS: usize = 32;
const FIXTURE_TEXT: &[u8] = b"Generated Foldex keyboard fixture. Do not open or execute.\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Viewport {
    Root,
    Launcher,
}
impl Viewport {
    pub(crate) fn id(self) -> ViewportId {
        match self {
            Self::Root => ViewportId::ROOT,
            Self::Launcher => ViewportId::from_hash_of("foldex-focused-launcher"),
        }
    }
    pub(crate) fn slot(self) -> usize {
        match self {
            Self::Root => 0,
            Self::Launcher => 1,
        }
    }
    fn from_id(id: ViewportId) -> Option<Self> {
        if id == Self::Root.id() {
            Some(Self::Root)
        } else if id == Self::Launcher.id() {
            Some(Self::Launcher)
        } else {
            None
        }
    }
}

/// A rendered observation, taken in the viewport that actually owns the focus.
#[derive(Clone)]
pub(crate) struct Probe {
    pub frame: u64,
    pub focus: Option<Id>,
    pub region: Option<Region>,
    pub focus_rect: Option<Rect>,
    pub focus_layer: Option<Id>,
    pub watched: Vec<(Id, Rect)>,
    pub content: Rect,
    pub input_focused: bool,
    pub native_focused: bool,
    pub keyboard_cues: bool,
    pub context_panels: Vec<Rect>,
}
impl Probe {
    pub(crate) fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "frame":self.frame,
            "focus":self.focus.map(|id| format!("{id:?}")),
            "region":self.region.map(|region| format!("{region:?}")),
            "focus_rect":self.focus_rect.map(rect_values),
            "focus_layer":self.focus_layer.map(|id| format!("{id:?}")),
            "watched":self.watched.iter().map(|(id, rect)| serde_json::json!({
                "id":format!("{id:?}"),"rect":rect_values(*rect)
            })).collect::<Vec<_>>(),
            "content":rect_values(self.content),
            "input_focused":self.input_focused,
            "native_focused_before_injection":self.native_focused,
            "keyboard_cues":self.keyboard_cues,
            "context_panels":self.context_panels.iter().copied().map(rect_values).collect::<Vec<_>>()
        })
    }
}

pub(crate) fn rect_values(rect: Rect) -> [f32; 4] {
    [rect.left(), rect.top(), rect.right(), rect.bottom()]
}

pub(crate) fn geometry_visible(rect: Rect, viewport: Rect) -> bool {
    rect.is_finite()
        && viewport.is_finite()
        && rect.width() > 1.0
        && rect.height() > 1.0
        && viewport.expand(1.0).contains_rect(rect)
}

pub(crate) struct CapturedImage {
    pub label: String,
    pub viewport: Viewport,
    pub image: Arc<egui::ColorImage>,
}

pub(crate) struct InputBridge {
    pub active: bool,
    queues: [Vec<Event>; 2],
    modifiers: [Modifiers; 2],
    pointers: [Option<egui::Pos2>; 2],
    pub probes: [Option<Probe>; 2],
    pub watched: [Vec<Id>; 2],
    native_focused: [bool; 2],
    pub images: VecDeque<CapturedImage>,
    seen_captures: Vec<(String, Viewport)>,
    pub duplicate_captures: u32,
    pub capture_error: Option<String>,
    pub batches: [u32; 2],
    pub events: [u32; 2],
    pub overflow: bool,
}
impl Default for InputBridge {
    fn default() -> Self {
        Self {
            active: true,
            queues: std::array::from_fn(|_| Vec::with_capacity(MAX_EVENTS)),
            modifiers: [Modifiers::NONE; 2],
            pointers: [None; 2],
            probes: [None, None],
            watched: std::array::from_fn(|_| Vec::with_capacity(4)),
            native_focused: [false; 2],
            images: VecDeque::with_capacity(1),
            seen_captures: Vec::with_capacity(MAX_CAPTURES),
            duplicate_captures: 0,
            capture_error: None,
            batches: [0; 2],
            events: [0; 2],
            overflow: false,
        }
    }
}
impl InputBridge {
    pub(crate) fn queue(
        &mut self,
        viewport: Viewport,
        events: Vec<Event>,
        modifiers: Modifiers,
    ) -> Result<(), &'static str> {
        let slot = viewport.slot();
        if !self.active || !self.queues[slot].is_empty() {
            return Err("Input is already queued for this owned viewport");
        }
        if events.is_empty() || events.len() > MAX_EVENTS || events.iter().any(prohibited_event) {
            return Err("Unsafe or oversized keyboard input batch");
        }
        self.modifiers[slot] = modifiers;
        for event in &events {
            match event {
                Event::PointerMoved(pos) => self.pointers[slot] = Some(*pos),
                Event::PointerGone => self.pointers[slot] = None,
                _ => {}
            }
        }
        self.queues[slot] = events;
        Ok(())
    }

    pub(crate) fn input_pending(&self) -> bool {
        self.queues.iter().any(|queue| !queue.is_empty())
    }

    fn receive_capture(
        &mut self,
        declared: ViewportId,
        label: &str,
        image: &Arc<egui::ColorImage>,
    ) {
        let Some(viewport) = Viewport::from_id(declared) else {
            self.capture_error
                .get_or_insert_with(|| "Keyboard screenshot declared an unowned viewport".into());
            return;
        };
        if label.len() > 128 {
            self.capture_error.get_or_insert_with(|| {
                "Keyboard screenshot label exceeds its bounded budget".into()
            });
            return;
        }
        if self
            .seen_captures
            .iter()
            .any(|(seen, owner)| seen == label && *owner == viewport)
        {
            // Backends may deliver the same child screenshot in root RawInput
            // and again in an immediate pass. Keep only one image and remember
            // its identity after the consumer drains it or starts PNG encoding.
            self.duplicate_captures = self.duplicate_captures.saturating_add(1);
            return;
        }
        if self.seen_captures.iter().any(|(seen, _)| seen == label) {
            self.capture_error.get_or_insert_with(|| {
                "Keyboard screenshot label changed its declared viewport".into()
            });
            return;
        }
        if self.seen_captures.len() >= MAX_CAPTURES || !self.images.is_empty() {
            self.overflow = true;
            self.capture_error.get_or_insert_with(|| {
                "Keyboard screenshot exceeded its unique-label or single-image queue limit".into()
            });
            return;
        }
        self.seen_captures.push((label.to_owned(), viewport));
        self.images.push_back(CapturedImage {
            label: label.to_owned(),
            viewport,
            image: image.clone(),
        });
    }
}

/// Refuse semantic clipboard events as well as their keyboard aliases. Never
/// rely on the application being correct to prevent a clipboard side effect.
fn prohibited_event(event: &Event) -> bool {
    match event {
        Event::Copy | Event::Cut | Event::Paste(_) | Event::PointerButton { .. } => true,
        Event::Key { key, modifiers, .. } => {
            ((modifiers.ctrl || modifiers.command)
                && matches!(key, egui::Key::C | egui::Key::X | egui::Key::V))
                || matches!(key, egui::Key::Insert | egui::Key::Delete)
                || modifiers.mac_cmd
                || (modifiers.alt && *key == egui::Key::F4)
        }
        Event::Text(text) => text.len() > 4096,
        Event::Ime(egui::ImeEvent::Preedit { text, .. } | egui::ImeEvent::Commit(text)) => {
            text.len() > 4096
        }
        Event::Ime(_) | Event::ModifiersChanged(_) => false,
        // Owned hover samples cannot activate a file or touch desktop input.
        Event::PointerMoved(pos) => !pos.is_finite(),
        Event::PointerGone => false,
        _ => true,
    }
}

pub(crate) struct InputPlugin {
    pub shared: Arc<Mutex<InputBridge>>,
}
impl egui::Plugin for InputPlugin {
    fn debug_name(&self) -> &'static str {
        "foldex-owned-keyboard-benchmark"
    }

    fn input_hook(&mut self, _ctx: &Context, input: &mut RawInput) {
        let mut state = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !state.active {
            return;
        }
        for event in &input.events {
            if let Event::Screenshot {
                viewport_id,
                user_data,
                image,
            } = event
                && let Some(label) = user_data
                    .data
                    .as_ref()
                    .and_then(|value| value.downcast_ref::<String>())
                && label.starts_with("kbd-")
            {
                // Screenshot ownership comes from the event, independently of
                // which viewport's RawInput happened to carry its callback.
                state.receive_capture(*viewport_id, label, image);
            }
        }
        let Some(viewport) = Viewport::from_id(input.viewport_id) else {
            return;
        };
        let slot = viewport.slot();
        state.native_focused[slot] = input.focused;
        // A benchmark must not accidentally replay real mouse, clipboard, or
        // keyboard events alongside its script. Keep renderer screenshot data.
        input
            .events
            .retain(|event| matches!(event, Event::Screenshot { .. }));
        input.events.push(match state.pointers[slot] {
            Some(pos) => Event::PointerMoved(pos),
            None => Event::PointerGone,
        });
        input.focused = true;
        if let Some(info) = input.viewports.get_mut(&input.viewport_id) {
            info.focused = Some(true);
        }
        input
            .events
            .push(Event::ModifiersChanged(state.modifiers[slot]));
        if !state.queues[slot].is_empty() {
            state.batches[slot] += 1;
            state.events[slot] += state.queues[slot].len() as u32;
            input.events.append(&mut state.queues[slot]);
        }
        state.modifiers[slot] = Modifiers::NONE;
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx();
        let Some(viewport) = Viewport::from_id(ctx.viewport_id()) else {
            return;
        };
        let focus = ctx.memory(|memory| memory.focused());
        let response = focus.and_then(|id| ctx.read_response(id));
        let focus_rect = response.as_ref().map(|response| response.rect);
        let focus_layer = response.as_ref().map(|response| response.layer_id.id);
        let watch_ids = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .watched[viewport.slot()]
        .clone();
        let watched = watch_ids
            .into_iter()
            .filter_map(|id| ctx.read_response(id).map(|response| (id, response.rect)))
            .collect();
        let mut context_panels = Vec::with_capacity(7);
        if viewport == Viewport::Root {
            for level in 0..7_usize {
                let id = Id::new(("foldex-native-context", level));
                if let Some(rect) = ctx.memory(|memory| memory.area_rect(id))
                    && rect.is_positive()
                    && [
                        rect.center(),
                        rect.left_center() + egui::vec2(4.0, 0.0),
                        rect.right_center() - egui::vec2(4.0, 0.0),
                    ]
                    .into_iter()
                    .any(|point| ctx.layer_id_at(point).is_some_and(|layer| layer.id == id))
                {
                    context_panels.push(rect);
                }
            }
        }
        let mut probe = Probe {
            frame: ctx.cumulative_frame_nr(),
            focus,
            region: crate::keyboard::focused_region(ctx),
            focus_rect,
            focus_layer,
            watched,
            content: ctx.content_rect(),
            input_focused: ctx.input(|input| input.focused),
            native_focused: false,
            keyboard_cues: foldex_ui::focus::keyboard_visible(ctx),
            context_panels,
        };
        let mut state = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.active {
            probe.native_focused = state.native_focused[viewport.slot()];
            state.probes[viewport.slot()] = Some(probe);
        }
    }
}

pub(crate) struct PendingCapture {
    pub label: String,
    pub viewport: Viewport,
    pub since: Instant,
}

pub(crate) struct KeyboardUiRun {
    pub phase: usize,
    pub expected_steps: usize,
    pub phase_started: Option<Instant>,
    pub next: Option<Instant>,
    pub attempts: u8,
    pub installed: bool,
    pub compact: bool,
    pub finished: bool,
    pub checks: Vec<serde_json::Value>,
    pub samples: Vec<serde_json::Value>,
    pub errors: Vec<String>,
    pub captures: Vec<PathBuf>,
    pub captured_labels: Vec<String>,
    pub pending_capture: Option<PendingCapture>,
    pub capture_result: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    pub shared: Arc<Mutex<InputBridge>>,
    pub invoker: Option<Id>,
    pub menu_parent: Option<Id>,
    pub editor_invoker: Option<Id>,
    pub selection_before: Vec<(u32, u32)>,
    pub focused_row_before: Option<usize>,
    pub path_before: PathBuf,
    pub radius_before: u8,
    pub roots_before: Vec<PathBuf>,
    pub launcher_selected_before: usize,
    pub help_rect: Option<Rect>,
    pub help_stable_since: Option<Instant>,
    pub circuit_origin: Option<Region>,
    pub circuit_last: Option<Region>,
    pub forward_cycle: Vec<Region>,
    pub reverse_cycle: Vec<Region>,
}
impl Default for KeyboardUiRun {
    fn default() -> Self {
        Self {
            phase: 0,
            expected_steps: 0,
            phase_started: None,
            next: None,
            attempts: 0,
            installed: false,
            compact: false,
            finished: false,
            checks: Vec::new(),
            samples: Vec::new(),
            errors: Vec::new(),
            captures: Vec::new(),
            captured_labels: Vec::new(),
            pending_capture: None,
            capture_result: None,
            shared: Arc::new(Mutex::new(InputBridge::default())),
            invoker: None,
            menu_parent: None,
            editor_invoker: None,
            selection_before: Vec::new(),
            focused_row_before: None,
            path_before: PathBuf::new(),
            radius_before: 0,
            roots_before: Vec::new(),
            launcher_selected_before: 0,
            circuit_origin: None,
            circuit_last: None,
            help_rect: None,
            help_stable_since: None,
            forward_cycle: Vec::with_capacity(12),
            reverse_cycle: Vec::with_capacity(12),
        }
    }
}
impl KeyboardUiRun {
    pub(crate) fn fail(&mut self, error: impl Into<String>) {
        if self.finished {
            return;
        }
        if self.errors.len() < 32 {
            self.errors.push(error.into());
        }
        self.finished = true;
        self.shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active = false;
    }

    pub(crate) fn check(&mut self, name: &str, passed: bool, detail: serde_json::Value) {
        if self.finished {
            return;
        }
        if self.checks.len() >= MAX_CHECKS {
            self.fail("Keyboard check limit exceeded");
            return;
        }
        self.checks
            .push(serde_json::json!({"check":name,"passed":passed,"detail":detail}));
        if !passed {
            self.fail(format!("Keyboard check failed: {name}"));
        }
    }

    pub(crate) fn probe(&self, viewport: Viewport) -> Option<Probe> {
        self.shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .probes[viewport.slot()]
        .clone()
    }

    pub(crate) fn report(&self) -> serde_json::Value {
        let state = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let complete = self.expected_steps > 0 && self.phase == self.expected_steps;
        serde_json::json!({
            "schema":1,"workload":"owned-native-keyboard-workflow",
            "phase":self.phase,"planned_steps":self.expected_steps,
            "finished":self.finished,
            "passed":self.finished && complete && self.errors.is_empty() && !state.overflow
                && state.capture_error.is_none()
                && !self.checks.is_empty() && self.capture_result.is_none()
                && self.pending_capture.is_none() && !state.input_pending(),
            "checks":self.checks,"samples":self.samples,"errors":self.errors,
            "captures":self.captures,"captured_labels":self.captured_labels,
            "raw_input_batches":{"root":state.batches[0],"launcher":state.batches[1]},
            "raw_input_events":{"root":state.events[0],"launcher":state.events[1]},
            "forward_regions":self.forward_cycle.iter().map(|region|format!("{region:?}")).collect::<Vec<_>>(),
            "reverse_regions":self.reverse_cycle.iter().map(|region|format!("{region:?}")).collect::<Vec<_>>(),
            "queue_overflow":state.overflow,
            "capture_transport_error":state.capture_error,
            "duplicate_capture_callbacks":state.duplicate_captures,
            "unique_capture_callbacks":state.seen_captures.len(),
            "pending_input":state.input_pending(),
            "root_probe":state.probes[0].as_ref().map(Probe::report),
            "launcher_probe":state.probes[1].as_ref().map(Probe::report),
            "global_input_sent":false,"scripted_clipboard_operations":false,
            "scripted_result_invocations":false,"presentation_measured":false,
            "viewport_focus_is_injected":true,"capture_visual_review":"pending"
        })
    }
}

pub(crate) fn validate_args(args: &[OsString]) -> Result<bool, &'static str> {
    let keyboard = args.iter().any(|arg| arg == "--benchmark-keyboard");
    if !keyboard {
        return Ok(false);
    }
    if !args.iter().any(|arg| arg == "--secondary-monitor") {
        return Err("Keyboard benchmark requires --secondary-monitor");
    }
    if args.iter().any(|arg| {
        matches!(
            arg.to_str(),
            Some(
                "--benchmark-ui"
                    | "--benchmark-launcher"
                    | "--benchmark-fixes"
                    | "--benchmark-refinement"
                    | "--benchmark-stress"
                    | "--benchmark-many-tabs"
                    | "--benchmark-full-search"
                    | "--benchmark-navigation-list"
                    | "--benchmark-resize"
                    | "--benchmark-search"
                    | "--benchmark-gpu"
                    | "--benchmark-monitors"
            )
        )
    }) {
        return Err("Keyboard benchmark owns its generated scope and cannot combine workloads");
    }
    Ok(true)
}

/// The caller owns a newly created preferences directory and its cleanup guard.
/// No caller-provided directory is accepted as a keyboard investigation target.
pub(crate) fn create_fixture(state_dir: &Path) -> io::Result<PathBuf> {
    let scope = state_dir.join("keyboard-fixture");
    fs::create_dir(&scope)?;
    for row in 0..FIXTURE_ROWS {
        write_new(&scope.join(format!("Keyboard {row:02}.txt")), FIXTURE_TEXT)?;
    }
    for name in ["Keyboard Folder A", "Keyboard Folder B"] {
        let folder = scope.join(name);
        fs::create_dir(&folder)?;
        write_new(&folder.join("Nested Keyboard.txt"), FIXTURE_TEXT)?;
    }
    write_new(&scope.join("键盘.txt"), FIXTURE_TEXT)?;
    Ok(fs::canonicalize(scope)?)
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)
}

pub(crate) fn fixture_unchanged(scope: &Path) -> io::Result<bool> {
    let mut expected: Vec<String> = (0..FIXTURE_ROWS)
        .map(|row| format!("Keyboard {row:02}.txt"))
        .collect();
    expected.extend([
        "Keyboard Folder A".into(),
        "Keyboard Folder B".into(),
        "键盘.txt".into(),
    ]);
    let mut actual = fs::read_dir(scope)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<_>>>()?;
    expected.sort();
    actual.sort();
    if expected != actual {
        return Ok(false);
    }
    for name in expected {
        let path = scope.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Ok(false);
        }
        if name.starts_with("Keyboard Folder ") {
            if !metadata.is_dir() {
                return Ok(false);
            }
            let children = fs::read_dir(&path)?.collect::<io::Result<Vec<_>>>()?;
            if children.len() != 1 || children[0].file_name() != "Nested Keyboard.txt" {
                return Ok(false);
            }
            let child = fs::symlink_metadata(children[0].path())?;
            if !child.is_file()
                || child.file_type().is_symlink()
                || child.len() != FIXTURE_TEXT.len() as u64
                || fs::read(children[0].path())? != FIXTURE_TEXT
            {
                return Ok(false);
            }
        } else if !metadata.is_file()
            || metadata.len() != FIXTURE_TEXT.len() as u64
            || fs::read(path)? != FIXTURE_TEXT
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn keyboard_mode_requires_secondary_and_refuses_user_scopes_or_other_workloads() {
        assert_eq!(validate_args(&args(&["foldex"])), Ok(false));
        assert!(validate_args(&args(&["--benchmark-keyboard"])).is_err());
        let valid = ["--benchmark-keyboard", "--secondary-monitor"];
        assert_eq!(validate_args(&args(&valid)), Ok(true));
        for flag in [
            "--benchmark-ui",
            "--benchmark-fixes",
            "--benchmark-search",
            "--benchmark-launcher",
        ] {
            let mut combined = valid.to_vec();
            combined.push(flag);
            assert!(validate_args(&args(&combined)).is_err());
        }
    }

    fn key(key: egui::Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn input_transport_rejects_clipboard_aliases_destructive_keys_and_queue_overwrite() {
        let mut bridge = InputBridge::default();
        for event in [
            Event::Copy,
            Event::Cut,
            Event::Paste("text".into()),
            key(egui::Key::C, Modifiers::CTRL),
            key(egui::Key::X, Modifiers::CTRL),
            key(egui::Key::V, Modifiers::CTRL),
            key(egui::Key::Delete, Modifiers::NONE),
            key(egui::Key::Insert, Modifiers::SHIFT),
        ] {
            assert!(
                bridge
                    .queue(Viewport::Root, vec![event], Modifiers::NONE)
                    .is_err()
            );
        }
        assert!(
            bridge
                .queue(
                    Viewport::Root,
                    vec![key(egui::Key::Tab, Modifiers::NONE); MAX_EVENTS + 1],
                    Modifiers::NONE
                )
                .is_err()
        );
        bridge
            .queue(
                Viewport::Launcher,
                vec![key(egui::Key::ArrowDown, Modifiers::NONE)],
                Modifiers::NONE,
            )
            .unwrap();
        assert!(
            bridge
                .queue(
                    Viewport::Launcher,
                    vec![key(egui::Key::Escape, Modifiers::NONE)],
                    Modifiers::NONE
                )
                .is_err()
        );
        assert!(bridge.queues[0].is_empty());
    }

    #[test]
    fn input_transport_allows_owned_hover_but_never_pointer_activation() {
        assert!(!prohibited_event(&Event::PointerMoved(egui::pos2(
            12.0, 24.0
        ))));
        assert!(!prohibited_event(&Event::PointerGone));
        assert!(prohibited_event(&Event::PointerMoved(egui::pos2(
            f32::NAN,
            0.0
        ))));
        for pressed in [false, true] {
            assert!(prohibited_event(&Event::PointerButton {
                pos: egui::pos2(12.0, 24.0),
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }));
        }
    }

    #[test]
    fn plugin_routes_child_raw_input_without_leaking_it_to_root_or_unknown_viewports() {
        let shared = Arc::new(Mutex::new(InputBridge::default()));
        shared
            .lock()
            .unwrap()
            .queue(
                Viewport::Launcher,
                vec![key(egui::Key::ArrowDown, Modifiers::NONE)],
                Modifiers::NONE,
            )
            .unwrap();
        let mut plugin = InputPlugin {
            shared: shared.clone(),
        };
        let ctx = Context::default();
        let mut root = RawInput::default();
        root.events.push(Event::Copy);
        egui::Plugin::input_hook(&mut plugin, &ctx, &mut root);
        assert!(
            !root
                .events
                .iter()
                .any(|event| matches!(event, Event::Copy | Event::Key { .. }))
        );
        assert!(shared.lock().unwrap().input_pending());
        let mut child = RawInput {
            viewport_id: Viewport::Launcher.id(),
            ..Default::default()
        };
        egui::Plugin::input_hook(&mut plugin, &ctx, &mut child);
        assert!(child.events.iter().any(|event| matches!(
            event,
            Event::Key {
                key: egui::Key::ArrowDown,
                ..
            }
        )));
        assert!(!shared.lock().unwrap().input_pending());
        let mut unknown = RawInput {
            viewport_id: ViewportId::from_hash_of("unowned"),
            events: vec![Event::Copy],
            ..Default::default()
        };
        egui::Plugin::input_hook(&mut plugin, &ctx, &mut unknown);
        assert!(matches!(unknown.events.as_slice(), [Event::Copy]));
    }

    fn screenshot(label: &str, viewport: ViewportId) -> Event {
        Event::Screenshot {
            viewport_id: viewport,
            user_data: egui::UserData::new(label.to_owned()),
            image: Arc::new(egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE])),
        }
    }

    #[test]
    fn child_screenshot_in_root_raw_input_keeps_its_declared_owned_viewport() {
        let shared = Arc::new(Mutex::new(InputBridge::default()));
        let mut plugin = InputPlugin {
            shared: shared.clone(),
        };
        let mut input = RawInput {
            events: vec![screenshot("kbd-child", Viewport::Launcher.id())],
            ..Default::default()
        };
        egui::Plugin::input_hook(&mut plugin, &Context::default(), &mut input);
        let state = shared.lock().unwrap();
        let captured = state.images.front().unwrap();
        assert_eq!(captured.viewport, Viewport::Launcher);
        assert_eq!(captured.label, "kbd-child");
        assert_eq!(state.seen_captures.len(), 1);
        assert!(!state.overflow && state.capture_error.is_none());
    }

    #[test]
    fn duplicate_screenshot_callbacks_are_ignored_before_and_after_image_drain() {
        let shared = Arc::new(Mutex::new(InputBridge::default()));
        let mut plugin = InputPlugin {
            shared: shared.clone(),
        };
        let event = screenshot("kbd-child-repeat", Viewport::Launcher.id());
        let mut root = RawInput {
            events: vec![event.clone(), event.clone()],
            ..Default::default()
        };
        egui::Plugin::input_hook(&mut plugin, &Context::default(), &mut root);
        {
            let mut state = shared.lock().unwrap();
            assert_eq!(state.images.len(), 1);
            assert_eq!(state.duplicate_captures, 1);
            state.images.pop_front();
        }
        let mut child = RawInput {
            viewport_id: Viewport::Launcher.id(),
            events: vec![event],
            ..Default::default()
        };
        egui::Plugin::input_hook(&mut plugin, &Context::default(), &mut child);
        let mut state = shared.lock().unwrap();
        assert!(state.images.is_empty());
        assert_eq!(state.seen_captures.len(), 1);
        assert_eq!(state.duplicate_captures, 2);
        assert!(!state.overflow && state.capture_error.is_none());
        let image = Arc::new(egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]));
        state.receive_capture(Viewport::Root.id(), "kbd-next", &image);
        assert_eq!(state.images.front().unwrap().viewport, Viewport::Root);
        assert_eq!(state.seen_captures.len(), 2);
    }

    #[test]
    fn genuine_capture_owner_mismatches_and_queue_or_identity_limits_still_fail() {
        let image = Arc::new(egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]));
        let mut changed = InputBridge::default();
        changed.receive_capture(Viewport::Root.id(), "kbd-owner", &image);
        changed.images.pop_front();
        changed.receive_capture(Viewport::Launcher.id(), "kbd-owner", &image);
        assert!(
            changed
                .capture_error
                .as_deref()
                .unwrap()
                .contains("changed its declared viewport")
        );
        assert!(changed.images.is_empty());

        let mut unowned = InputBridge::default();
        unowned.receive_capture(ViewportId::from_hash_of("unowned"), "kbd-unowned", &image);
        assert!(
            unowned
                .capture_error
                .as_deref()
                .unwrap()
                .contains("unowned viewport")
        );

        let mut queued = InputBridge::default();
        queued.receive_capture(Viewport::Root.id(), "kbd-first", &image);
        queued.receive_capture(Viewport::Launcher.id(), "kbd-unexpected-second", &image);
        assert!(queued.overflow && queued.capture_error.is_some());
        assert_eq!(queued.images.len(), 1);
        assert_eq!(queued.images.front().unwrap().label, "kbd-first");

        let mut capped = InputBridge::default();
        for index in 0..MAX_CAPTURES {
            capped.receive_capture(Viewport::Root.id(), &format!("kbd-limit-{index}"), &image);
            capped.images.pop_front();
        }
        capped.receive_capture(Viewport::Root.id(), "kbd-over-limit", &image);
        assert!(capped.overflow && capped.capture_error.is_some());
        assert_eq!(capped.seen_captures.len(), MAX_CAPTURES);
        assert!(capped.images.is_empty());
    }

    #[test]
    fn geometry_and_incomplete_reports_cannot_claim_success() {
        let viewport = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 540.0));
        assert!(geometry_visible(viewport.shrink(2.0), viewport));
        assert!(!geometry_visible(Rect::NOTHING, viewport));
        assert!(!geometry_visible(
            viewport.translate(egui::vec2(15.0, 0.0)),
            viewport
        ));
        let mut run = KeyboardUiRun::default();
        run.expected_steps = 100;
        run.phase = 10;
        run.finished = true;
        assert_eq!(run.report()["passed"], false);
        run.finished = false;
        run.fail("Phase deadline");
        run.fail("Repeated error in later frame");
        run.check("late worker result", false, serde_json::Value::Null);
        assert_eq!(run.report()["errors"][0], "Phase deadline");
        assert_eq!(run.errors.len(), 1);
        assert!(run.checks.is_empty());
    }

    #[test]
    fn generated_fixture_is_new_only_and_detects_edited_or_missing_entries() {
        let base = std::env::temp_dir();
        let path = base.join(format!(
            "foldex-keyboard-fixture-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let _owner = super::super::Fixture {
            path: path.clone(),
            base,
        };
        let scope = create_fixture(&path).unwrap();
        assert!(fixture_unchanged(&scope).unwrap());
        assert!(create_fixture(&path).is_err());
        fs::write(scope.join("Keyboard 03.txt"), b"changed").unwrap();
        assert!(!fixture_unchanged(&scope).unwrap());
    }
}
