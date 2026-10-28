//! Editable shortcuts with exact modifiers and variable-length key sequences.
//! Defaults are source-style mappings; personal File Pilot config is not imported.
use crate::commands::{COMMANDS, Command, Palette, SurfaceFocus};
use egui::{Event, Id, Key, Modifiers, Rect, Sense, UiBuilder, pos2, vec2};
use foldex_ui::{Icon, Theme, components, motion::MotionProfile};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Duration,
};

mod native_recorder;
#[cfg(windows)]
pub(crate) const RECORDING_WAKE_MESSAGE: u32 = native_recorder::WAKE_MESSAGE;

const MAX_CAPTURE_EVENTS: usize = 512;
const MAX_BINDINGS: usize = 512;
const MAX_EVENTS: usize = 32;
const MAX_QUEUED: usize = 16;
const SEQUENCE_SECONDS: f64 = 0.75;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeyStroke {
    pub key: String,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    /// Physical Super key (Windows or Mac Command), never the `command == ctrl` alias.
    #[serde(default)]
    pub cmd: bool,
}
impl KeyStroke {
    pub fn new(key: Key, modifiers: Modifiers) -> Self {
        Self {
            key: key.name().into(),
            ctrl: modifiers.ctrl,
            alt: modifiers.alt,
            shift: modifiers.shift,
            cmd: modifiers.mac_cmd,
        }
    }
    pub fn label(&self) -> String {
        let key = Key::from_name(&self.key);
        let name = key.map(key_label).unwrap_or(&self.key);
        let mut label = String::new();
        if self.ctrl {
            label.push_str("Ctrl+");
        }
        if self.alt {
            label.push_str("Alt+");
        }
        if self.shift {
            label.push_str("Shift+");
        }
        if self.cmd {
            label.push_str(if cfg!(target_os = "macos") {
                "Cmd+"
            } else {
                "Win+"
            });
        }
        label.extend(name.chars().take(32));
        label
    }
    fn compile(&self) -> Option<Stroke> {
        if self.key.len() > 64 {
            return None;
        }
        let key = Key::from_name(&self.key)?;
        if modifier_key(key) {
            return None;
        }
        Some(Stroke {
            key: shifted_base_key(key, self.shift),
            bits: self.ctrl as u8
                | ((self.alt as u8) << 1)
                | ((self.shift as u8) << 2)
                | ((self.cmd as u8) << 3),
        })
    }
    fn parse(text: &str) -> Option<Self> {
        let mut modifiers = Modifiers::NONE;
        let mut key = None;
        for part in text.split('+') {
            match part {
                "Ctrl" => modifiers.ctrl = true,
                "Alt" => modifiers.alt = true,
                "Shift" => modifiers.shift = true,
                "Cmd" | "Win" | "Super" => modifiers.mac_cmd = true,
                _ => key = Key::from_name(part),
            }
        }
        Some(Self::new(key?, modifiers))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub command: Command,
    pub sequence: Vec<KeyStroke>,
}
impl Binding {
    pub fn single(command: Command, key: Key, modifiers: Modifiers) -> Self {
        Self {
            command,
            sequence: vec![KeyStroke::new(key, modifiers)],
        }
    }
    pub fn label(&self) -> String {
        self.sequence
            .iter()
            .map(KeyStroke::label)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn launcher_shortcut(binding: &Binding) -> Option<String> {
    if binding.sequence.len() != 1 {
        return None;
    }
    let key = &binding.sequence[0];
    let stroke = key.compile()?;
    let name = match stroke.key {
        Key::ArrowLeft => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        Key::ArrowUp => "ArrowUp",
        Key::ArrowDown => "ArrowDown",
        Key::Plus => "Equals",
        key => key.name(),
    };
    let mut shortcut = String::new();
    for (held, modifier) in [
        (key.ctrl, "Ctrl+"),
        (key.alt, "Alt+"),
        (key.shift, "Shift+"),
        (key.cmd, "Win+"),
    ] {
        if held {
            shortcut.push_str(modifier);
        }
    }
    shortcut.push_str(name);
    crate::launcher::native::parse_hotkey(&shortcut).map(|_| shortcut)
}

fn launcher_stroke(shortcut: &str) -> Option<KeyStroke> {
    let (modifiers, virtual_key) = crate::launcher::native::parse_hotkey(shortcut)?;
    Some(KeyStroke {
        key: crate::native_keys::physical_key(virtual_key)?.name().into(),
        ctrl: modifiers & 2 != 0,
        alt: modifiers & 1 != 0,
        shift: modifiers & 4 != 0,
        cmd: modifiers & 8 != 0,
    })
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictKind {
    Duplicate,
    Prefix,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    pub first: usize,
    pub second: usize,
    pub kind: ConflictKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stroke {
    key: Key,
    bits: u8,
}
impl Stroke {
    fn event(key: Key, modifiers: Modifiers) -> Self {
        Self {
            key: shifted_base_key(key, modifiers.shift),
            bits: modifiers.ctrl as u8
                | ((modifiers.alt as u8) << 1)
                | ((modifiers.shift as u8) << 2)
                | ((modifiers.mac_cmd as u8) << 3),
        }
    }
    fn unmodified_or_shift(self) -> bool {
        self.bits & 0b1011 == 0
    }
}
// Native Windows capture reports the base virtual key, while egui can report
// its shifted symbol. Normalize both saved bindings and incoming events so a
// recorded chord also works after capture ends, including older symbol bindings.
fn shifted_base_key(key: Key, shift: bool) -> Key {
    if !shift {
        return key;
    }
    match key {
        Key::Plus => Key::Equals,
        Key::Colon => Key::Semicolon,
        Key::Pipe => Key::Backslash,
        Key::Questionmark => Key::Slash,
        Key::OpenCurlyBracket => Key::OpenBracket,
        Key::CloseCurlyBracket => Key::CloseBracket,
        Key::Exclamationmark => Key::Num1,
        key => key,
    }
}
// Keep shifted number chords aligned with their configured number-row keys.
fn shortcut_key(key: Key, physical: Option<Key>, modifiers: Modifiers) -> Key {
    if modifiers.shift
        && (modifiers.ctrl || modifiers.mac_cmd)
        && let Some(
            value @ (Key::Num0
            | Key::Num1
            | Key::Num2
            | Key::Num3
            | Key::Num4
            | Key::Num5
            | Key::Num6
            | Key::Num7
            | Key::Num8
            | Key::Num9),
        ) = physical
    {
        value
    } else {
        key
    }
}

#[derive(Clone)]
struct Compiled {
    index: usize,
    command: Command,
    sequence: Vec<Stroke>,
}
#[derive(Clone)]
struct Pending {
    sequence: Vec<Stroke>,
    deadline: f64,
    fallback: Option<Command>,
    owner: Option<egui::Id>,
}
#[derive(Clone)]
struct Recording {
    command: Command,
    replace: Option<usize>,
    strokes: Vec<KeyStroke>,
}

struct LauncherChange {
    requested: String,
    bindings: Vec<Binding>,
}

#[derive(Clone)]
enum EditorConfirmationAction {
    Remove { index: usize, binding: Binding },
    ResetAll,
}

struct EditorConfirmation {
    action: EditorConfirmationAction,
    return_to: Id,
    focus_cancel: bool,
    accepted: bool,
}

/// Read-only geometry for isolated native captures and headless interaction tests.
#[derive(Clone, Debug)]
pub(crate) struct EditorProbe {
    pub outer: Rect,
    pub header: Rect,
    pub search: Rect,
    pub close: Rect,
    pub body: Rect,
    pub body_clip: Rect,
    pub footer: Rect,
    pub content: Rect,
    pub settings: Rect,
    pub separator: Rect,
    pub reset_all: Rect,
    pub rows: Vec<EditorRowProbe>,
}

#[derive(Clone, Debug)]
pub(crate) struct EditorRowProbe {
    pub command: Command,
    pub rect: Rect,
    pub text: Rect,
    pub bindings_area: Rect,
    pub reset: Option<Rect>,
    pub add: Rect,
    pub bindings: Vec<EditorBindingProbe>,
}

#[derive(Clone, Debug)]
pub(crate) struct EditorBindingProbe {
    pub index: usize,
    pub chip: Rect,
    pub edit: Rect,
    pub remove: Rect,
    pub clip: Rect,
}

struct EditorRowLayout {
    height: f32,
    text: Rect,
    reset: Option<Rect>,
    add: Rect,
    bindings: Rect,
    binding_height: f32,
    chips: Vec<(usize, Rect)>,
}

struct EditorContentLayout {
    width: f32,
    setting_height: f32,
    separator_height: f32,
    height: f32,
    rows: Vec<(usize, EditorRowLayout)>,
}

pub struct Hotkeys {
    bindings: Vec<Binding>,
    compiled: Vec<Compiled>,
    labels: Vec<String>,
    displays: HashMap<Command, String>,
    conflicts: Vec<Conflict>,
    diagnostics: Vec<String>,
    pending: Option<Pending>,
    queue: VecDeque<Command>,
    frame: Option<u64>,
    processed_frame: Option<u64>,
    scan_cursor: usize,
    scan_count: usize,
    seen: [Option<Stroke>; MAX_EVENTS],
    seen_consumed: [bool; MAX_EVENTS],
    seen_len: usize,
    sequence_seconds: f64,
    revision: u64,
    recording: Option<Recording>,
    recording_cancel: bool,
    recording_held: Modifiers,
    raw_recorded: bool,
    recording_keys_down: HashSet<Key>,
    native_capture: native_recorder::Recorder,
    filter: String,
    editor_focus: bool,
    filter_rows: Vec<usize>,
    editor_motion: foldex_ui::motion::LayoutMotion,
    editor_scroll_reset: bool,
    recorded_frame: Option<u64>,
    super_keys: [bool; 2],
    launcher_change: Option<LauncherChange>,
    status: Option<String>,
    recording_return: Option<egui::Id>,
    recording_focus_capture: bool,
    recording_review_focus: bool,
    editor_selected: Option<Command>,
    editor_reveal: bool,
    editor_active: bool,
    editor_closing: bool,
    editor_height: f32,
    editor_opacity: f32,
    editor_frame: Option<u64>,
    editor_opened_frame: Option<u64>,
    editor_scope: SurfaceFocus,
    editor_controls: Vec<Id>,
    editor_confirmation: Option<EditorConfirmation>,
    editor_probe: Option<EditorProbe>,
}
impl Default for Hotkeys {
    fn default() -> Self {
        Self::from_bindings(default_bindings())
    }
}
impl Hotkeys {
    /// Load a saved custom map and add only the new, unclaimed keyboard
    /// defaults. Explicit user assignments (including sequence prefixes) win.
    /// Use this at session load; `from_bindings` remains an exact constructor.
    pub fn from_persisted_bindings(mut bindings: Vec<Binding>) -> Self {
        Self::add_keyboard_defaults(&mut bindings);
        Self::from_bindings(bindings)
    }

    pub fn add_keyboard_defaults(bindings: &mut Vec<Binding>) -> bool {
        let mut changed = false;
        // Session maps have no version marker. Recognize only the exact single
        // key panel bindings previously shipped; retain custom sequences and
        // bindings of Tab to any other command. Occupied replacements win.
        for (command, old_modifiers, key) in [
            (Command::NextPanel, Modifiers::NONE, Key::ArrowRight),
            (Command::PreviousPanel, Modifiers::SHIFT, Key::ArrowLeft),
        ] {
            let old = Stroke::event(Key::Tab, old_modifiers);
            let before = bindings.len();
            bindings.retain(|binding| {
                binding.command != command
                    || binding.sequence.len() != 1
                    || binding.sequence[0].compile() != Some(old)
            });
            if before == bindings.len() {
                continue;
            }
            changed = true;
            let replacement = Binding::single(command, key, Modifiers::CTRL | Modifiers::ALT);
            let stroke = replacement.sequence[0].compile();
            if bindings.len() < MAX_BINDINGS
                && !bindings.iter().any(|binding| {
                    binding.command == command
                        || binding.sequence.first().and_then(KeyStroke::compile) == stroke
                })
            {
                bindings.push(replacement);
            }
        }
        for (command, key, modifiers) in [
            (Command::NextFocusRegion, Key::F6, Modifiers::NONE),
            (Command::PreviousFocusRegion, Key::F6, Modifiers::SHIFT),
            (Command::KeyboardHelp, Key::F1, Modifiers::NONE),
            (Command::Properties, Key::Enter, Modifiers::ALT),
            (Command::DeletePermanent, Key::Delete, Modifiers::SHIFT),
            (Command::Undo, Key::Z, Modifiers::CTRL),
            (Command::Redo, Key::Y, Modifiers::CTRL),
        ] {
            if bindings.len() >= MAX_BINDINGS
                || bindings.iter().any(|binding| binding.command == command)
            {
                continue;
            }
            let candidate = Binding::single(command, key, modifiers);
            let stroke = candidate.sequence[0].compile();
            if bindings
                .iter()
                .any(|binding| binding.sequence.first().and_then(KeyStroke::compile) == stroke)
            {
                continue;
            }
            bindings.push(candidate);
            if command == Command::Redo && bindings.len() < MAX_BINDINGS {
                let alias =
                    Binding::single(Command::Redo, Key::Z, Modifiers::CTRL | Modifiers::SHIFT);
                if !bindings.iter().any(|binding| {
                    binding.sequence.first().and_then(KeyStroke::compile)
                        == alias.sequence[0].compile()
                }) {
                    bindings.push(alias);
                }
            }
            changed = true;
        }
        changed
    }
    pub fn from_bindings(bindings: Vec<Binding>) -> Self {
        let mut this = Self {
            bindings,
            compiled: Vec::new(),
            labels: Vec::new(),
            displays: HashMap::new(),
            conflicts: Vec::new(),
            diagnostics: Vec::new(),
            pending: None,
            queue: VecDeque::with_capacity(MAX_QUEUED),
            frame: None,
            processed_frame: None,
            scan_cursor: 0,
            scan_count: 0,
            seen: [None; MAX_EVENTS],
            seen_consumed: [false; MAX_EVENTS],
            seen_len: 0,
            sequence_seconds: SEQUENCE_SECONDS,
            revision: 0,
            recording: None,
            recording_cancel: false,
            recording_held: Modifiers::NONE,
            raw_recorded: false,
            recording_keys_down: HashSet::new(),
            native_capture: Default::default(),
            filter: String::new(),
            editor_focus: false,
            filter_rows: Vec::with_capacity(COMMANDS.len()),
            editor_motion: Default::default(),
            editor_scroll_reset: true,
            recorded_frame: None,
            super_keys: [false; 2],
            launcher_change: None,
            status: None,
            recording_return: None,
            recording_focus_capture: false,
            recording_review_focus: false,
            editor_selected: None,
            editor_reveal: false,
            editor_active: false,
            editor_closing: false,
            editor_height: 0.0,
            editor_opacity: 0.0,
            editor_frame: None,
            editor_opened_frame: None,
            editor_scope: SurfaceFocus::default(),
            editor_controls: Vec::new(),
            editor_confirmation: None,
            editor_probe: None,
        };
        this.rebuild();
        this
    }
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }
    pub fn attach_native_capture(&mut self, owner: isize) {
        self.native_capture.attach(owner);
    }

    /// Runs before egui's begin-pass focus navigation and before any underlying
    /// editor or command sees the keys. Only recorder review controls pass through.
    pub fn capture_raw_input(&mut self, input: &mut egui::RawInput) {
        let native = self.native_capture.drain();
        self.capture_input_batch(input, native);
    }

    fn capture_input_batch(&mut self, input: &mut egui::RawInput, native: native_recorder::Input) {
        if !native.events.is_empty() {
            let recording = self.is_recording();
            input.events.splice(
                0..0,
                native.events.into_iter().filter(|event| {
                    recording || matches!(event, Event::Key { pressed: false, .. })
                }),
            );
        }
        if let Some(modifiers) = native.restore_modifiers {
            input.events.insert(0, Event::ModifiersChanged(modifiers));
        }
        self.prepare_input(input);
        if !self.is_recording() {
            return;
        }
        let lost_focus = native.lost_focus
            || !self.native_capture.running()
                && input
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::WindowFocused(false)));
        if lost_focus || native.overflow || native.cancelled {
            self.cancel_recording();
            discard_recorded_events(&mut input.events);
            input.events.push(Event::ModifiersChanged(Modifiers::NONE));
            if native.overflow {
                self.status = Some(
                    "Recording stopped because too many key events arrived. Try again.".into(),
                );
            }
            return;
        }
        self.seen_len = 0;
        self.capture_events(&mut input.events, self.recording_held);
        if native.unsupported && self.is_recording() && !self.recording_cancel {
            self.status = Some("That key is not supported. Try another key combination.".into());
        }
        if let Some(modifiers) = native.modifiers.filter(|_| self.is_recording()) {
            self.recording_held = modifiers;
        }
        self.raw_recorded = self.is_recording();
        // Releases clear keys that were down before recording started. Neutral
        // UI modifiers keep captured Ctrl/Shift/Alt/Win out of widget navigation.
        for event in &mut input.events {
            if let Event::Key {
                pressed: false,
                modifiers,
                ..
            } = event
            {
                *modifiers = Modifiers::NONE;
            }
        }
        input.events.push(Event::ModifiersChanged(Modifiers::NONE));
        if self.recording_cancel {
            self.native_capture.stop();
        }
    }
    /// Keep Super separate from egui's Ctrl/Command alias. Windows reports it
    /// as key events, so carry its state across frames and preserve event order.
    pub fn prepare_input(&mut self, input: &mut egui::RawInput) {
        if cfg!(target_os = "macos") {
            return;
        }
        if !input.focused {
            self.super_keys = [false; 2];
            return;
        }
        for event in &mut input.events {
            match event {
                Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    match key {
                        Key::SuperLeft => self.super_keys[0] = *pressed,
                        Key::SuperRight => self.super_keys[1] = *pressed,
                        _ => {}
                    }
                    modifiers.mac_cmd |= self.super_keys.iter().any(|down| *down);
                }
                Event::ModifiersChanged(modifiers) => {
                    modifiers.mac_cmd |= self.super_keys.iter().any(|down| *down);
                }
                Event::WindowFocused(false) => self.super_keys = [false; 2],
                _ => {}
            }
        }
    }

    /// Migrate the actual global shortcut into the same map used by the editor,
    /// palette and persistence. The former unbound Launcher row has no separate map.
    pub fn set_launcher_shortcut(&mut self, shortcut: &str) {
        self.bindings
            .retain(|binding| binding.command != Command::Launcher);
        if let Some(stroke) = launcher_stroke(shortcut) {
            self.bindings.insert(
                0,
                Binding {
                    command: Command::Launcher,
                    sequence: vec![stroke],
                },
            );
        }
        self.rebuild();
    }

    pub fn apply_launcher_change(
        &mut self,
        launcher: &mut crate::launcher::Launcher,
        previous: Vec<Binding>,
    ) {
        let requested = self
            .bindings
            .iter()
            .find(|binding| binding.command == Command::Launcher)
            .and_then(launcher_shortcut)
            .unwrap_or_default();
        if crate::launcher::native::parse_hotkey(&requested)
            == crate::launcher::native::parse_hotkey(&launcher.settings.hotkey)
        {
            return;
        }
        let bindings = std::mem::replace(&mut self.bindings, previous);
        self.rebuild();
        match launcher.apply_launcher_shortcut(&requested) {
            Ok(()) => {
                self.launcher_change = Some(LauncherChange {
                    requested,
                    bindings,
                });
                self.status = Some("Registering launcher shortcut…".into());
                self.settle_launcher_change(launcher);
            }
            Err(error) => self.status = Some(error),
        }
    }

    /// Do not publish or save the proposed map until native registration succeeds.
    /// On conflict the old map, including any replaced bindings, stays intact.
    pub fn settle_launcher_change(&mut self, launcher: &mut crate::launcher::Launcher) -> bool {
        launcher.settle_launcher_shortcut();
        self.finish_launcher_change(launcher.native.hotkey_feedback())
    }
    fn finish_launcher_change(
        &mut self,
        feedback: crate::launcher::native::HotkeyFeedback,
    ) -> bool {
        let Some(change) = self.launcher_change.as_ref() else {
            return false;
        };
        if feedback.pending || feedback.requested != change.requested {
            return false;
        }
        let change = self.launcher_change.take().unwrap();
        if let Some(error) = feedback.error {
            self.status = Some(error);
        } else {
            self.replace_bindings(change.bindings);
            self.status = Some("Launcher shortcut saved".into());
        }
        true
    }

    pub fn record_active_launcher_shortcut(&mut self, ctx: &egui::Context, shortcut: &str) {
        if self.is_recording()
            && let Some(stroke) = launcher_stroke(shortcut)
        {
            self.dispatch_stroke(ctx, &stroke, false);
        }
    }

    pub fn recording_label(&self) -> Option<String> {
        self.recording.as_ref().map(|recording| {
            Binding {
                command: recording.command,
                sequence: recording.strokes.clone(),
            }
            .label()
        })
    }
    pub fn launcher_error(&mut self, error: String) {
        self.status = Some(error);
    }
    pub fn replace_bindings(&mut self, bindings: Vec<Binding>) {
        self.bindings = bindings;
        self.rebuild();
    }
    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn focus_editor(&mut self) {
        self.editor_focus = true;
        self.editor_scroll_reset = true;
        self.editor_motion.clear();
        self.recording_return = None;
    }
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }
    /// Call when the owner of the shortcut editor closes.
    pub fn cancel_recording(&mut self) {
        self.recording = None;
        self.recording_cancel = false;
        self.recording_held = Modifiers::NONE;
        self.super_keys = [false; 2];
        self.raw_recorded = false;
        self.recording_keys_down.clear();
        self.native_capture.stop();
        self.cancel_pending();
    }
    pub fn display_for(&self, command: Command) -> &str {
        self.displays
            .get(&command)
            .map(String::as_str)
            .unwrap_or("")
    }
    pub fn update_palette(&self, palette: &mut Palette) {
        palette.set_shortcut_displays(
            COMMANDS
                .iter()
                .map(|entry| (entry.command, self.display_for(entry.command))),
        );
    }
    pub fn set_sequence_timeout(&mut self, duration: Duration) {
        self.sequence_seconds = duration.as_secs_f64().clamp(0.1, 2.0);
    }
    pub fn cancel_pending(&mut self) {
        self.pending = None;
        self.queue.clear();
    }
    pub fn reset_defaults(&mut self) {
        self.bindings = default_bindings();
        self.rebuild();
    }
    pub fn reset_command(&mut self, command: Command) {
        self.bindings.retain(|binding| binding.command != command);
        self.bindings.extend(
            default_bindings()
                .into_iter()
                .filter(|binding| binding.command == command),
        );
        self.rebuild();
    }
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.bindings.len() {
            return false;
        }
        self.bindings.remove(index);
        self.rebuild();
        true
    }
    fn rebuild(&mut self) {
        self.cancel_recording();
        self.compiled.clear();
        self.labels.clear();
        self.displays.clear();
        self.conflicts.clear();
        self.diagnostics.clear();
        if self.bindings.len() > MAX_BINDINGS {
            self.bindings.truncate(MAX_BINDINGS);
            self.diagnostics
                .push("Only the first 512 shortcut bindings were loaded".into());
        }
        for (index, binding) in self.bindings.iter_mut().enumerate() {
            if binding.sequence.is_empty() {
                self.diagnostics.push(format!(
                    "Binding {} must contain a key combination",
                    index + 1
                ));
                self.labels.push("Invalid sequence".into());
                continue;
            }
            let sequence: Option<Vec<_>> =
                binding.sequence.iter().map(KeyStroke::compile).collect();
            if let Some(sequence) = sequence {
                for (key, stroke) in binding.sequence.iter_mut().zip(&sequence) {
                    key.key = stroke.key.name().into();
                }
                self.compiled.push(Compiled {
                    index,
                    command: binding.command,
                    sequence,
                });
            } else {
                self.diagnostics.push(format!(
                    "Binding {} has an unknown or modifier-only key",
                    index + 1
                ));
            }
            let label = binding.label();
            self.labels.push(label.clone());
            self.displays
                .entry(binding.command)
                .and_modify(|display| {
                    display.push_str("  /  ");
                    display.push_str(&label);
                })
                .or_insert(label);
        }
        for (index, left) in self.compiled.iter().enumerate() {
            for right in &self.compiled[index + 1..] {
                let kind = if left.sequence == right.sequence {
                    Some(ConflictKind::Duplicate)
                } else if left.sequence.starts_with(&right.sequence)
                    || right.sequence.starts_with(&left.sequence)
                {
                    Some(ConflictKind::Prefix)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    self.conflicts.push(Conflict {
                        first: left.index,
                        second: right.index,
                        kind,
                    });
                }
            }
        }
        self.cancel_pending();
        self.processed_frame = None;
        self.revision = self.revision.wrapping_add(1);
        self.refilter_editor();
    }
    fn conflicting_indices(&self, binding: &Binding, ignore: Option<usize>) -> Vec<usize> {
        let Some(sequence) = binding
            .sequence
            .iter()
            .map(KeyStroke::compile)
            .collect::<Option<Vec<_>>>()
        else {
            return vec![];
        };
        if sequence.is_empty() {
            return vec![];
        }
        self.compiled
            .iter()
            .filter(|existing| {
                Some(existing.index) != ignore
                    && (existing.sequence.starts_with(&sequence)
                        || sequence.starts_with(&existing.sequence))
            })
            .map(|existing| existing.index)
            .collect()
    }
    pub fn assign(
        &mut self,
        binding: Binding,
        replace: Option<usize>,
        replace_conflicts: bool,
    ) -> Result<(), String> {
        if replace.is_some_and(|index| index >= self.bindings.len()) {
            return Err("The binding to replace no longer exists".into());
        }
        if binding.sequence.is_empty()
            || binding
                .sequence
                .iter()
                .any(|stroke| stroke.compile().is_none())
        {
            return Err("Choose a valid key combination or sequence".into());
        }
        if binding.command == Command::Launcher && launcher_shortcut(&binding).is_none() {
            return Err("The global launcher shortcut needs one key combination with Ctrl, Alt, Shift or Win.".into());
        }
        let conflicts = self.conflicting_indices(&binding, replace);
        if !conflicts.is_empty() && !replace_conflicts {
            return Err("This key or sequence is already assigned".into());
        }
        if replace.is_none() && conflicts.is_empty() && self.bindings.len() >= MAX_BINDINGS {
            return Err("The shortcut binding limit has been reached".into());
        }
        self.bindings = self
            .bindings
            .iter()
            .enumerate()
            .filter(|(index, existing)| {
                Some(*index) != replace
                    && !conflicts.contains(index)
                    && (binding.command != Command::Launcher
                        || existing.command != Command::Launcher)
            })
            .map(|(_, binding)| binding.clone())
            .collect();
        self.bindings.push(binding);
        self.rebuild();
        Ok(())
    }
    fn begin_frame(&mut self, ctx: &egui::Context, allow_file_ops: bool) -> f64 {
        let frame = ctx.cumulative_frame_nr();
        if self.frame != Some(frame) {
            self.frame = Some(frame);
            self.seen_len = 0;
            self.scan_cursor = 0;
            self.scan_count = 0;
        }
        let now = ctx.input(|input| input.time);
        if !ctx.input(|input| input.focused) {
            self.cancel_pending();
            return now;
        }
        // A new editor can request focus before it has created TextEditState.
        // A plain prefix belongs to the control that received its first key:
        // cancel it on an ownership transition, before delivering a timeout.
        // A stable non-text composite can still complete its own sequences.
        let focused = ctx.memory(|memory| memory.focused());
        if self.pending.as_ref().is_some_and(|pending| {
            pending.sequence[0].unmodified_or_shift()
                && (pending.owner != focused || ctx.text_edit_focused())
        }) {
            self.pending = None;
        }
        if let Some(pending) = self.pending.as_ref() {
            if now >= pending.deadline {
                let fallback = pending.fallback;
                self.pending = None;
                if let Some(command) = fallback {
                    self.enqueue(command, allow_file_ops);
                }
            } else {
                ctx.request_repaint_after(Duration::from_secs_f64(pending.deadline - now));
            }
        }
        now
    }
    fn enqueue(&mut self, command: Command, allow_file_ops: bool) {
        if allowed(command, allow_file_ops) && self.queue.len() < MAX_QUEUED {
            self.queue.push_back(command);
        }
    }
    fn pop(&mut self, allow_file_ops: bool) -> Option<Command> {
        while let Some(command) = self.queue.pop_front() {
            if allowed(command, allow_file_ops) {
                return Some(command);
            }
        }
        None
    }
    fn mark_seen(&mut self, stroke: Stroke) -> bool {
        if self.seen_len < MAX_EVENTS {
            self.seen[self.seen_len] = Some(stroke);
            self.seen_consumed[self.seen_len] = false;
            self.seen_len += 1;
            true
        } else {
            false
        }
    }
    fn native_duplicate(&mut self, stroke: Stroke) -> bool {
        if let Some(index) =
            self.seen[..self.seen_len]
                .iter()
                .enumerate()
                .find_map(|(index, seen)| {
                    (*seen == Some(stroke) && !self.seen_consumed[index]).then_some(index)
                })
        {
            self.seen_consumed[index] = true;
            true
        } else {
            false
        }
    }
    fn feed(
        &mut self,
        stroke: Stroke,
        now: f64,
        allow_file_ops: bool,
        owner: Option<egui::Id>,
    ) -> bool {
        if let Some(pending) = self.pending.take() {
            if stroke.key == Key::Escape && stroke.bits == 0 {
                return true;
            }
            let mut sequence = pending.sequence;
            sequence.push(stroke);
            if self.feed_sequence(
                sequence,
                now,
                allow_file_ops,
                pending.owner,
                pending.fallback,
            ) {
                return true;
            }
            if let Some(command) = pending.fallback {
                self.enqueue(command, allow_file_ops);
            }
        }
        self.feed_sequence(vec![stroke], now, allow_file_ops, owner, None)
    }
    fn feed_sequence(
        &mut self,
        sequence: Vec<Stroke>,
        now: f64,
        allow_file_ops: bool,
        owner: Option<egui::Id>,
        fallback: Option<Command>,
    ) -> bool {
        let mut single = None;
        let mut prefix = false;
        for binding in &self.compiled {
            if !binding.sequence.starts_with(&sequence) || !allowed(binding.command, allow_file_ops)
            {
                continue;
            }
            if binding.sequence.len() > sequence.len() {
                prefix = true;
            } else if single.is_none() {
                single = Some(binding.command);
            }
        }
        if prefix {
            self.pending = Some(Pending {
                sequence,
                deadline: now + self.sequence_seconds,
                fallback: single.or(fallback),
                owner,
            });
            true
        } else if let Some(command) = single {
            self.enqueue(command, allow_file_ops);
            true
        } else {
            false
        }
    }
    /// Call once per native physical press, before logical dispatch. Same-frame
    /// logical/clipboard events are consumed when the mapping handles the press.
    /// Text/selection ownership still takes priority. Separate presses count even
    /// when they have identical chords and arrive in the same frame.
    pub fn dispatch_stroke(
        &mut self,
        ctx: &egui::Context,
        stroke: &KeyStroke,
        allow_file_ops: bool,
    ) -> Option<Command> {
        let allow_file_ops = allow_file_ops && !crate::keyboard::composing(ctx);
        let now = self.begin_frame(ctx, allow_file_ops);
        if !ctx.input(|input| input.focused) && !self.native_capture.running() {
            if self.recording.is_some() {
                self.cancel_recording();
            }
            return None;
        }
        let stroke = stroke.compile()?;
        if self.recording.is_some() {
            if self.mark_seen(stroke) {
                self.capture(stroke);
                if consume_native_input(ctx, stroke) {
                    self.seen_consumed[self.seen_len - 1] = true;
                }
            }
            return None;
        }
        if text_owns(ctx, stroke) || !ctx.input(|input| input.focused) {
            return self.pop(allow_file_ops);
        }
        let owner = ctx.memory(|memory| memory.focused());
        if self.seen_len < MAX_EVENTS && self.feed(stroke, now, allow_file_ops, owner) {
            self.mark_seen(stroke);
            if consume_native_input(ctx, stroke) {
                self.seen_consumed[self.seen_len - 1] = true;
            }
        }
        if let Some(pending) = self.pending.as_ref() {
            ctx.request_repaint_after(Duration::from_secs_f64((pending.deadline - now).max(0.0)));
        }
        self.pop(allow_file_ops)
    }
    pub fn command_for(&self, stroke: &KeyStroke, allow_file_ops: bool) -> Option<Command> {
        let stroke = stroke.compile()?;
        self.compiled
            .iter()
            .find(|binding| {
                binding.sequence == [stroke] && allowed(binding.command, allow_file_ops)
            })
            .map(|binding| binding.command)
    }
    /// Consume up to32 logical presses per frame, yielding after one command.
    /// Remaining keys/text can then go to a popup opened by that command. Call
    /// again to dispatch another command while the owner still accepts input.
    /// Copy/Cut/Paste stay with egui; native bridges can use `dispatch_stroke`.
    pub fn dispatch(&mut self, ctx: &egui::Context, allow_file_ops: bool) -> Option<Command> {
        self.dispatch_inner(ctx, allow_file_ops, false).0
    }

    /// Let assigned shortcuts consume their own text, then yield at the first
    /// unhandled text event so the explorer can hand this batch to its search.
    pub(crate) fn dispatch_before_text(
        &mut self,
        ctx: &egui::Context,
        allow_file_ops: bool,
    ) -> (Option<Command>, bool) {
        self.dispatch_inner(ctx, allow_file_ops, true)
    }

    fn dispatch_inner(
        &mut self,
        ctx: &egui::Context,
        allow_file_ops: bool,
        stop_at_text: bool,
    ) -> (Option<Command>, bool) {
        let allow_file_ops = allow_file_ops && !crate::keyboard::composing(ctx);
        let now = self.begin_frame(ctx, allow_file_ops);
        if self.recording.is_some() {
            return (None, false);
        }
        // A timeout or a native prefix mismatch may already have queued work.
        // Deliver it before touching keys that can belong to the resulting UI.
        if let Some(command) = self.pop(allow_file_ops) {
            if !self.queue.is_empty() {
                ctx.request_repaint();
            }
            return (Some(command), false);
        }
        let frame = ctx.cumulative_frame_nr();
        let mut reached_text = false;
        if self.processed_frame != Some(frame) && ctx.input(|input| input.focused) {
            let owns_text = ctx.text_edit_focused();
            let protected = editing_owned(ctx);
            let owner = ctx.memory(|memory| memory.focused());
            // Outside the file listing, an existing control owns its plain
            // activation/navigation keys. Function keys and modified global
            // shortcuts still pass through the normal text/clipboard rules.
            let control_owns_plain = !allow_file_ops && owner.is_some();
            ctx.input_mut(|input| {
                let mut index = self.scan_cursor.min(input.events.len());
                while index < input.events.len() && self.scan_count < MAX_EVENTS {
                    if stop_at_text
                        && matches!(input.events.get(index), Some(Event::Text(text))
                            if crate::keyboard::printable_search_text(text))
                    {
                        reached_text = true;
                        break;
                    }
                    let stroke = match input.events.get(index) {
                        Some(Event::Key {
                            key,
                            physical_key,
                            modifiers,
                            pressed: true,
                            repeat: false,
                        }) if !modifier_key(*key) => Some(Stroke::event(
                            shortcut_key(*key, *physical_key, *modifiers),
                            *modifiers,
                        )),
                        _ => None,
                    };
                    let Some(stroke) = stroke else {
                        index += 1;
                        continue;
                    };
                    self.scan_count += 1;
                    if protected_stroke(stroke, owns_text || control_owns_plain, protected) {
                        index += 1;
                        continue;
                    }
                    let duplicate = self.native_duplicate(stroke);
                    // A mismatched continuation belongs to the UI opened by the
                    // prefix fallback. Leave that key and its text intact.
                    if !duplicate {
                        if let Some(pending) = self.pending.as_ref() {
                            let cancels = stroke.key == Key::Escape && stroke.bits == 0;
                            let mut sequence = pending.sequence.clone();
                            sequence.push(stroke);
                            let completes = self.compiled.iter().any(|binding| {
                                binding.sequence.starts_with(&sequence)
                                    && allowed(binding.command, allow_file_ops)
                            });
                            if !cancels && !completes && pending.fallback.is_some() {
                                let fallback = pending.fallback.unwrap();
                                self.pending = None;
                                self.enqueue(fallback, allow_file_ops);
                                if !self.queue.is_empty() {
                                    break;
                                }
                            }
                        }
                    }
                    let consumed = if duplicate {
                        true
                    } else {
                        self.feed(stroke, now, allow_file_ops, owner)
                    };
                    if consumed {
                        consume_event(&mut input.events, index, stroke);
                    } else {
                        index += 1;
                    }
                    if !self.queue.is_empty() {
                        break;
                    }
                }
                self.scan_cursor = index;
                if index >= input.events.len() || self.scan_count >= MAX_EVENTS {
                    self.processed_frame = Some(frame);
                }
            });
        }
        if let Some(pending) = self.pending.as_ref() {
            ctx.request_repaint_after(Duration::from_secs_f64((pending.deadline - now).max(0.0)));
        }
        let command = self.pop(allow_file_ops);
        if !self.queue.is_empty() {
            ctx.request_repaint();
        }
        (command, reached_text)
    }
    pub fn dispatch_many(&mut self, ctx: &egui::Context, allow_file_ops: bool) -> Vec<Command> {
        let mut commands = Vec::new();
        while commands.len() < MAX_QUEUED {
            let Some(command) = self.dispatch(ctx, allow_file_ops) else {
                break;
            };
            commands.push(command);
        }
        commands
    }
    fn capture(&mut self, stroke: Stroke) {
        if stroke.key == Key::Escape && stroke.bits == 0 {
            self.recording_cancel = true;
            return;
        }
        self.status = None;
        if let Some(recording) = self.recording.as_mut() {
            // The Windows global launcher registration is a chord. Ordinary
            // command bindings keep every consecutive combination until Assign.
            if recording.command == Command::Launcher {
                recording.strokes.clear();
            }
            recording.strokes.push(KeyStroke {
                key: stroke.key.name().into(),
                ctrl: stroke.bits & 1 != 0,
                alt: stroke.bits & 2 != 0,
                shift: stroke.bits & 4 != 0,
                cmd: stroke.bits & 8 != 0,
            });
        }
    }
    pub fn record(&mut self, command: Command) {
        self.start_recording(command, None);
    }
    fn start_recording(&mut self, command: Command, replace: Option<usize>) {
        self.cancel_recording();
        if let Err(error) = self.native_capture.start() {
            self.status = Some(crate::friendly::present(
                "Couldn't start recording the shortcut",
                &error,
            ));
            return;
        }
        let replace = if command == Command::Launcher {
            replace.or_else(|| {
                self.bindings
                    .iter()
                    .position(|binding| binding.command == command)
            })
        } else {
            replace
        };
        self.cancel_pending();
        self.seen_len = 0;
        self.recorded_frame = None;
        self.recording_cancel = false;
        self.status = None;
        self.recording_focus_capture = true;
        self.recording_review_focus = false;
        self.recording = Some(Recording {
            command,
            replace,
            strokes: Vec::new(),
        });
    }
    fn refilter_editor(&mut self) {
        self.editor_scroll_reset = true;
        let matcher = foldex_match::Matcher::new(&self.filter);
        self.filter_rows.clear();
        for (index, entry) in COMMANDS.iter().enumerate() {
            if matcher.score(entry.title).is_some()
                || matcher.score(entry.description).is_some()
                || matcher.score(self.display_for(entry.command)).is_some()
            {
                self.filter_rows.push(index);
            }
        }
        if self.editor_selected.is_none_or(|command| {
            !self
                .filter_rows
                .iter()
                .any(|index| COMMANDS[*index].command == command)
        }) {
            self.editor_selected = self
                .filter_rows
                .first()
                .map(|index| COMMANDS[*index].command);
        }
    }
    fn poll_recording(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.focused) && !self.native_capture.running() {
            self.cancel_recording();
            return;
        }
        self.begin_frame(ctx, false);
        let frame = ctx.cumulative_frame_nr();
        if self.recorded_frame == Some(frame) {
            return;
        }
        self.recorded_frame = Some(frame);
        if std::mem::take(&mut self.raw_recorded) {
            return;
        }
        ctx.input_mut(|input| self.capture_events(&mut input.events, input.modifiers));
    }
    fn capture_events(&mut self, events: &mut Vec<Event>, modifiers: Modifiers) {
        self.recording_held = modifiers;
        if events.iter().filter(|event| recording_event(event)).count() > MAX_CAPTURE_EVENTS {
            self.cancel_recording();
            self.status =
                Some("Recording stopped because too many key events arrived. Try again.".into());
            discard_recorded_events(events);
            return;
        }
        let mut index = 0;
        let mut count = 0;
        let mut strokes = [None; MAX_CAPTURE_EVENTS];
        let mut logical = [0u16; MAX_CAPTURE_EVENTS];
        let mut normalized = [0u16; MAX_CAPTURE_EVENTS];
        let mut slots = 0;
        let mut last_clipboard = [None; 3];
        while index < events.len()
            && count < MAX_CAPTURE_EVENTS
            && self.recording.is_some()
            && !self.recording_cancel
        {
            if let Some(Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                ..
            }) = events.get_mut(index)
            {
                let key = physical_key.unwrap_or(*key);
                if *pressed {
                    *repeat |= !self.recording_keys_down.insert(key);
                } else {
                    self.recording_keys_down.remove(&key);
                }
            }
            match events.get(index) {
                Some(Event::ModifiersChanged(modifiers)) => {
                    self.recording_held = *modifiers;
                    events.remove(index);
                    continue;
                }
                Some(Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                }) if modifier_key(*key) => {
                    self.recording_held = *modifiers;
                    if *pressed {
                        events.remove(index);
                    } else {
                        index += 1;
                    }
                    continue;
                }
                Some(Event::Key { modifiers, .. }) => self.recording_held = *modifiers,
                _ => {}
            }
            if matches!(events.get(index), Some(Event::Key { key, modifiers, .. })
                if *key == Key::Tab && !modifiers.ctrl && !modifiers.alt && !modifiers.command && !modifiers.mac_cmd
                || self.recording_review_focus && modifiers.is_none() && matches!(key, Key::Enter | Key::Space))
            {
                if matches!(events.get(index), Some(Event::Key { key: Key::Tab, .. })) {
                    self.recording_review_focus = true;
                }
                index += 1;
                continue;
            }
            if let Some(Event::Key {
                key,
                physical_key,
                modifiers,
                pressed: true,
                repeat: true,
            }) = events.get(index)
            {
                let stroke =
                    Stroke::event(shortcut_key(*key, *physical_key, *modifiers), *modifiers);
                count += 1;
                consume_event(events, index, stroke);
                continue;
            }
            let item = match events.get(index) {
                Some(Event::Key {
                    key,
                    physical_key,
                    modifiers,
                    pressed: true,
                    repeat: false,
                }) if !modifier_key(*key) => {
                    let stroke =
                        Stroke::event(shortcut_key(*key, *physical_key, *modifiers), *modifiers);
                    if let Some(slot) = clipboard_stroke_slot(stroke) {
                        last_clipboard[slot] = Some(stroke);
                    }
                    Some((stroke, false))
                }
                Some(Event::Copy) => Some((
                    self.recording_clipboard(Key::C, self.recording_held, &last_clipboard),
                    true,
                )),
                Some(Event::Cut) => Some((
                    self.recording_clipboard(Key::X, self.recording_held, &last_clipboard),
                    true,
                )),
                Some(Event::Paste(_)) => Some((
                    self.recording_clipboard(Key::V, self.recording_held, &last_clipboard),
                    true,
                )),
                _ => None,
            };
            let Some((stroke, semantic)) = item else {
                index += 1;
                continue;
            };
            count += 1;
            let slot = strokes[..slots]
                .iter()
                .position(|existing| *existing == Some(stroke))
                .unwrap_or_else(|| {
                    let slot = slots;
                    strokes[slot] = Some(stroke);
                    slots += 1;
                    slot
                });
            let native = self.seen[..self.seen_len]
                .iter()
                .filter(|seen| **seen == Some(stroke))
                .count() as u16;
            let capture = if semantic {
                normalized[slot] += 1;
                normalized[slot] > logical[slot].max(native)
            } else {
                logical[slot] += 1;
                logical[slot] > normalized[slot].max(native)
            };
            if capture {
                self.capture(stroke);
            }
            consume_event(events, index, stroke);
        }
        // Composition and text aliases belong to the captured chord, never
        // to the covered shortcut search or root command dispatcher.
        events.retain(|event| {
            !matches!(
                event,
                Event::Text(_) | Event::Ime(_) | Event::Copy | Event::Cut | Event::Paste(_)
            )
        });
        if self.recording_cancel {
            events.retain(|event| {
                !matches!(
                    event,
                    Event::Key { pressed: true, .. } | Event::ModifiersChanged(_)
                )
            });
        }
    }
    fn recording_clipboard(
        &self,
        key: Key,
        modifiers: Modifiers,
        last: &[Option<Stroke>; 3],
    ) -> Stroke {
        last[clipboard_slot(key).unwrap()]
            .or_else(|| {
                self.seen[..self.seen_len]
                    .iter()
                    .rev()
                    .flatten()
                    .find(|stroke| clipboard_stroke_slot(**stroke) == clipboard_slot(key))
                    .copied()
            })
            .unwrap_or_else(|| Stroke::event(key, clipboard_modifiers(modifiers)))
    }
    /// Open an isolated capture without recording or synthesizing native input.
    /// The owner sets its existing `hotkeys_open` flag, then renders normally.
    pub(crate) fn benchmark_open_editor(&mut self, query: &str) {
        self.filter = query.chars().take(256).collect();
        self.refilter_editor();
        self.focus_editor();
    }

    pub(crate) fn benchmark_editor_probe(&self) -> Option<&EditorProbe> {
        self.editor_probe.as_ref()
    }

    /// Returns true after assignments, removal or reset; persist `bindings()`.
    pub fn editor_ui(&mut self, ui: &mut egui::Ui) -> bool {
        self.editor_ui_themed(ui, &Theme::default())
    }

    pub fn editor_ui_themed(&mut self, ui: &mut egui::Ui, theme: &Theme) -> bool {
        self.editor_popup_ui(ui.ctx(), theme, &mut true)
    }

    /// The same searchable surface, layout, controls and focus scope as Commands.
    pub fn editor_popup_ui(&mut self, ctx: &egui::Context, theme: &Theme, open: &mut bool) -> bool {
        let m = theme.metrics();
        let frame = ctx.cumulative_frame_nr();
        let area_id = Id::new("hotkeys-editor-window");
        let search_id = Id::new("hotkeys-search");
        let list_id = Id::new("hotkey-commands-focus");
        let close_id = area_id.with("close");
        if !self.editor_active {
            self.editor_active = true;
            self.editor_closing = false;
            self.editor_height = 0.0;
            self.editor_opacity = 0.0;
            self.editor_opened_frame = Some(frame);
            self.editor_frame = None;
            self.editor_focus = true;
        }
        if !*open {
            self.editor_closing = true;
        }
        if self.recording.is_some() {
            self.poll_recording(ctx);
        }
        // Remember children before drawing: a click that dismisses a child must
        // not also dismiss its owner, including the child's final modal pass.
        let child_open = self.recording.is_some()
            || self.editor_confirmation.is_some()
            || egui::Popup::is_any_open(ctx)
            || ctx.memory(|memory| {
                memory.top_modal_layer().is_some_and(|layer| {
                    layer != egui::LayerId::new(egui::Order::Foreground, area_id)
                })
            });
        let mut members = vec![search_id, close_id];
        if self.editor_controls.is_empty() {
            members.extend([area_id.with("reset-all"), list_id]);
        }
        for &id in &self.editor_controls {
            if !members.contains(&id) {
                members.push(id);
            }
        }
        self.editor_scope
            .update_animated(ctx, search_id, &members, self.editor_closing, false);
        if !self.editor_closing && !child_open && self.launcher_change.is_none() {
            self.editor_scope.redirect_text(ctx, list_id, search_id);
            if self.editor_scope.tab(ctx, &members) {
                self.editor_focus = false;
            }
            if self.editor_scope.accepts_keys(ctx, &members) {
                if crate::commands::take_key(ctx, Modifiers::NONE, Key::Escape) {
                    self.editor_closing = true;
                }
                if ctx.memory(|memory| memory.has_focus(list_id) || memory.has_focus(search_id)) {
                    let mut at = self
                        .filter_rows
                        .iter()
                        .position(|index| Some(COMMANDS[*index].command) == self.editor_selected)
                        .unwrap_or(0);
                    let page = ((ctx.content_rect().height() - 175.0 * m.scale) / (42.0 * m.scale))
                        .floor()
                        .max(1.0) as isize;
                    for (key, delta) in [
                        (Key::ArrowUp, -1),
                        (Key::ArrowDown, 1),
                        (Key::PageUp, -page),
                        (Key::PageDown, page),
                    ] {
                        if crate::commands::take_key(ctx, Modifiers::NONE, key) {
                            at = (at as isize + delta)
                                .clamp(0, self.filter_rows.len().saturating_sub(1) as isize)
                                as usize;
                            self.editor_reveal = true;
                        }
                    }
                    if (self.filter.is_empty() || ctx.memory(|memory| memory.has_focus(list_id)))
                        && crate::commands::take_key(ctx, Modifiers::NONE, Key::Home)
                    {
                        at = 0;
                        self.editor_reveal = true;
                    }
                    if (self.filter.is_empty() || ctx.memory(|memory| memory.has_focus(list_id)))
                        && crate::commands::take_key(ctx, Modifiers::NONE, Key::End)
                    {
                        at = self.filter_rows.len().saturating_sub(1);
                        self.editor_reveal = true;
                    }
                    self.editor_selected = self
                        .filter_rows
                        .get(at)
                        .map(|index| COMMANDS[*index].command);
                    if crate::commands::take_key(ctx, Modifiers::NONE, Key::Enter)
                        && let Some(command) = self.editor_selected
                    {
                        self.recording_return = Some(list_id);
                        self.start_recording(command, None);
                        self.recording_keys_down = ctx.input(|input| input.keys_down.clone());
                        self.recorded_frame = Some(frame);
                    }
                }
            }
        }
        let defaults = default_bindings();
        let screen = ctx.content_rect();
        let width = (screen.width() - 40.0 * m.scale)
            .max(1.0)
            .min(720.0 * m.scale);
        let content_width = (width - 2.0 * m.inset - 4.0 * m.scale).max(1.0);
        let content = self.editor_content_layout(ctx, theme, content_width, &defaults);
        let maximum_body = (screen.height() - 175.0 * m.scale)
            .min(420.0 * m.scale)
            .min((screen.height() - m.popup_header - 29.0 * m.scale - 30.0).max(1.0))
            .max(1.0);
        let body_height = content.height.min(maximum_body);
        let target = m.popup_header + body_height + 29.0 * m.scale;
        let gains = foldex_ui::motion::MotionGains::new(
            ctx.input(|input| input.stable_dt),
            theme.duration(),
        );
        let mut moving = false;
        if self.editor_frame != Some(frame) {
            self.editor_frame = Some(frame);
            moving |= foldex_ui::motion::popup_gains(ctx, theme.duration()).step_pixels(
                &mut self.editor_height,
                if self.editor_closing { 0.0 } else { target },
                MotionProfile::Standard,
            );
            moving |= gains.step_normalized(
                &mut self.editor_opacity,
                if self.editor_closing { 0.0 } else { 1.0 },
                MotionProfile::Standard,
            );
        }
        if self.editor_closing && self.editor_height <= 0.25 && self.editor_opacity <= 0.01 {
            self.editor_scope.update(ctx, search_id, &members, false);
            self.editor_active = false;
            self.editor_controls.clear();
            self.editor_probe = None;
            self.editor_motion.clear();
            self.cancel_recording();
            self.editor_confirmation = None;
            *open = false;
            return false;
        }
        self.editor_motion
            .set_pixels_per_point(ctx.pixels_per_point());
        self.editor_motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(ctx, theme.duration()));
        let top = (screen.top() + 32.0 + 47.0 * m.scale)
            .min(screen.bottom() - target.min(screen.height() - 30.0) - 15.0)
            .max(screen.top() + 15.0);
        let outer = Rect::from_min_size(
            pos2(screen.center().x - width * 0.5, top),
            vec2(width, self.editor_height),
        );
        let layout = components::PopupLayout::with_body_height(outer, theme, body_height);
        let mut probe = EditorProbe {
            outer,
            header: layout.header,
            search: components::popup_input_rect(layout.header, 1),
            close: components::popup_action_rect(layout.header, 0, 1),
            body: layout.body,
            body_clip: layout.body.intersect(outer).intersect(screen),
            footer: layout.footer,
            content: Rect::NOTHING,
            settings: Rect::NOTHING,
            separator: Rect::NOTHING,
            reset_all: Rect::NOTHING,
            rows: Vec::new(),
        };
        let mut changed = false;
        // Keep the modal backdrop during closing and child dialogs, as dialog() does.
        components::popup_surface(
            ctx,
            egui::Area::new(area_id)
                .order(egui::Order::Foreground)
                .fixed_pos(outer.min)
                .fade_in(false),
            true,
            |ui| {
                ui.set_min_size(outer.size());
                ui.set_max_size(outer.size());
                ui.set_clip_rect(outer.intersect(screen));
                ui.set_opacity(self.editor_opacity);
                if self.editor_closing
                    || self.launcher_change.is_some()
                    || self.recording.is_some()
                    || self.editor_confirmation.is_some()
                {
                    components::disable_interaction(ui);
                }
                ui.painter().rect_filled(
                    outer,
                    theme.radius.saturating_add(5),
                    Theme::color(theme.surface),
                );
                ui.painter().rect_stroke(
                    outer,
                    theme.radius.saturating_add(5),
                    egui::Stroke::new(1.0, theme.line()),
                    egui::StrokeKind::Inside,
                );
                components::popup_input_icon(ui, layout.header, theme, Icon::Search);
                ui.scope_builder(UiBuilder::new().max_rect(probe.search), |ui| {
                    let acquire = self.editor_focus && components::can_focus(ui);
                    if acquire {
                        ui.memory_mut(|memory| memory.request_focus(search_id));
                    }
                    let response = components::text_input_at(
                        ui,
                        probe.search,
                        search_id,
                        theme,
                        &mut self.filter,
                        "Find a command or shortcut…",
                        256,
                    );
                    if acquire {
                        self.editor_focus = ui.is_sizing_pass()
                            || !response.response.has_focus()
                            || self.editor_opened_frame == Some(frame);
                        if self.editor_focus {
                            ctx.request_repaint();
                        }
                    }
                    if response.response.changed() {
                        self.refilter_editor();
                        ctx.request_repaint();
                    }
                    crate::commands::result_input(ctx, search_id, list_id, !self.editor_closing);
                });
                if crate::commands::control(
                    ui,
                    theme,
                    probe.close,
                    close_id,
                    Icon::Close,
                    false,
                    gains,
                    &mut moving,
                )
                .on_hover_text("Close · Escape")
                .clicked()
                {
                    self.editor_closing = true;
                    ctx.request_repaint();
                }
                ui.painter().line_segment(
                    [layout.header.left_bottom(), layout.header.right_bottom()],
                    egui::Stroke::new(1.0, theme.line()),
                );
                self.editor_controls.clear();
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(layout.body)
                        .accessibility_parent(list_id),
                    |ui| {
                        ui.set_clip_rect(probe.body_clip);
                        crate::commands::result_surface(ui, ui.clip_rect(), list_id, "Hotkeys");
                        changed |= self.editor_rows(ui, theme, &content, &mut probe);
                    },
                );
                ui.painter().line_segment(
                    [layout.footer.left_top(), layout.footer.right_top()],
                    egui::Stroke::new(1.0, theme.line()),
                );
                let status = self
                    .status
                    .as_deref()
                    .or_else(|| self.diagnostics.first().map(String::as_str));
                if let Some(status) = status {
                    let mut small = theme.clone();
                    small.font_size = (theme.font_size - 2.0).max(10.0);
                    components::elided_label(
                        ui.painter(),
                        layout.footer.shrink2(vec2(10.0 * m.scale, 0.0)),
                        &small,
                        status,
                        theme.dim(),
                        false,
                    );
                } else {
                    components::popup_hints(
                        ui.painter(),
                        layout.footer,
                        theme,
                        &[
                            components::PopupHint::new("↑ ↓", "navigate"),
                            components::PopupHint::new("Enter", "assign"),
                            components::PopupHint::new("Tab", "controls"),
                            components::PopupHint::new("Esc", "close"),
                        ],
                        &[
                            components::PopupHint::new("↑ ↓", "navigate"),
                            components::PopupHint::new("Enter", "assign"),
                            components::PopupHint::new("Esc", "close"),
                        ],
                    );
                }
            },
        );
        changed |= self.editor_overlays(ctx, theme);
        if !self.editor_closing
            && !child_open
            && self.recording.is_none()
            && self.editor_confirmation.is_none()
            && !egui::Popup::is_any_open(ctx)
            && self.editor_opened_frame != Some(frame)
            && ctx.input(|input| {
                input.pointer.any_pressed()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|point| !outer.contains(point))
            })
        {
            // Keep the modal surface alive through the release and closing
            // animation so the dismissing click cannot reach the file list.
            self.editor_closing = true;
            ctx.request_repaint();
        }
        if moving
            || self.editor_motion.is_moving()
            || (self.editor_height - if self.editor_closing { 0.0 } else { target }).abs() > 0.25
        {
            ctx.request_repaint();
        }
        let probing = cfg!(test)
            || ctx.data(|data| {
                data.get_temp::<bool>(Id::new("benchmark-probe-layout"))
                    .unwrap_or(false)
            });
        self.editor_probe = probing.then_some(probe);
        changed
    }

    fn command_is_modified(&self, command: Command, defaults: &[Binding]) -> bool {
        let actual: Vec<_> = self
            .bindings
            .iter()
            .filter(|binding| binding.command == command)
            .collect();
        let expected: Vec<_> = defaults
            .iter()
            .filter(|binding| binding.command == command)
            .collect();
        actual.len() != expected.len()
            || actual.iter().any(|binding| {
                actual
                    .iter()
                    .filter(|other| other.sequence == binding.sequence)
                    .count()
                    != expected
                        .iter()
                        .filter(|other| other.sequence == binding.sequence)
                        .count()
            })
    }

    fn editor_text_height(ctx: &egui::Context, theme: &Theme) -> f32 {
        ctx.fonts_mut(|fonts| {
            fonts.row_height(&theme.font())
                + fonts.row_height(&egui::FontId::proportional(
                    (theme.font_size - 1.0).max(9.0),
                ))
        }) + 2.0 * theme.metrics().scale
    }

    fn editor_content_layout(
        &self,
        ctx: &egui::Context,
        theme: &Theme,
        width: f32,
        defaults: &[Binding],
    ) -> EditorContentLayout {
        let m = theme.metrics();
        let setting_height = (42.0 * m.scale)
            .max(Self::editor_text_height(ctx, theme) + 12.0 * m.scale)
            .max(m.control + 12.0 * m.scale);
        let separator_height = 12.0 * m.scale + 1.0 / ctx.pixels_per_point();
        let rows: Vec<_> = self
            .filter_rows
            .iter()
            .map(|&index| {
                (
                    index,
                    self.editor_row_layout(ctx, theme, width, COMMANDS[index].command, defaults),
                )
            })
            .collect();
        let height = setting_height
            + separator_height
            + if rows.is_empty() { setting_height } else { 0.0 }
            + rows.iter().map(|(_, row)| row.height).sum::<f32>();
        EditorContentLayout {
            width,
            setting_height,
            separator_height,
            height,
            rows,
        }
    }

    fn editor_row_layout(
        &self,
        ctx: &egui::Context,
        theme: &Theme,
        width: f32,
        command: Command,
        defaults: &[Binding],
    ) -> EditorRowLayout {
        let m = theme.metrics();
        let inset = (12.0 * m.scale).min(width * 0.04);
        let inner = (width - 2.0 * inset).max(1.0);
        let control = m.control.min(inner * 0.22).max(1.0);
        let gap = m.gap.min(inner * 0.03);
        let bindings_width = (inner * 0.58 - control - gap)
            .max(2.0 * control)
            .min((inner - control - gap).max(1.0));
        let bindings_right = width - inset - control - gap;
        let bindings_left = bindings_right - bindings_width;
        let modified = self.command_is_modified(command, defaults);
        let reset_left = bindings_left - control - gap;
        let text_right = if modified {
            reset_left - gap
        } else {
            bindings_left - gap
        };
        let mut lines: Vec<Vec<(usize, f32)>> = vec![Vec::new()];
        let mut line_width = 0.0;
        // Leave the shared floating scrollbar its permanent four-point slot.
        let chip_limit = (bindings_width - 4.0 * m.scale).max(1.0);
        for (index, binding) in self.bindings.iter().enumerate() {
            if binding.command != command {
                continue;
            }
            let label = ctx
                .fonts_mut(|fonts| {
                    fonts.layout_no_wrap(self.labels[index].clone(), theme.font(), theme.fg())
                })
                .size()
                .x;
            let chip_width = (label + 16.0 * m.scale + control)
                .max(2.0 * control)
                .min(chip_limit);
            if !lines.last().unwrap().is_empty() && line_width + gap + chip_width > chip_limit {
                lines.push(Vec::new());
                line_width = 0.0;
            }
            if line_width > 0.0 {
                line_width += gap;
            }
            lines.last_mut().unwrap().push((index, chip_width));
            line_width += chip_width;
        }
        let binding_height = lines.len() as f32 * (m.control + gap) - gap;
        let visible = binding_height.min(3.0 * m.control + 2.0 * gap);
        let text_height = Self::editor_text_height(ctx, theme);
        let height = (42.0 * m.scale).max(text_height.max(visible) + 14.0 * m.scale);
        let mut chips = Vec::new();
        for (line, bindings) in lines.iter().enumerate() {
            let used = bindings.iter().map(|(_, width)| width).sum::<f32>()
                + gap * bindings.len().saturating_sub(1) as f32;
            let mut x = chip_limit - used;
            for &(index, width) in bindings {
                chips.push((
                    index,
                    Rect::from_min_size(
                        pos2(x, line as f32 * (m.control + gap)),
                        vec2(width, m.control),
                    ),
                ));
                x += width + gap;
            }
        }
        EditorRowLayout {
            height,
            text: Rect::from_min_max(pos2(inset, 0.0), pos2(text_right.max(inset), height)),
            reset: modified.then(|| {
                Rect::from_center_size(
                    pos2(reset_left + control * 0.5, height * 0.5),
                    vec2(control, control),
                )
            }),
            add: Rect::from_center_size(
                pos2(width - inset - control * 0.5, height * 0.5),
                vec2(control, control),
            ),
            bindings: Rect::from_min_size(
                pos2(bindings_left, (height - visible) * 0.5),
                vec2(bindings_width, visible),
            ),
            binding_height,
            chips,
        }
    }

    fn editor_rows(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        content: &EditorContentLayout,
        probe: &mut EditorProbe,
    ) -> bool {
        let m = theme.metrics();
        let area_id = Id::new("hotkeys-editor-window");
        let list_id = Id::new("hotkey-commands-focus");
        let width = content.width;
        let mut scroll = components::menu_scroll_area_for_content(
            ui,
            theme,
            "hotkey-editor-list",
            ui.max_rect().height(),
            content.height,
        );
        if self.editor_scroll_reset {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        let mut edit = None;
        let mut remove = None;
        let mut reset = None;
        let mut reset_all = None;
        let output = scroll.show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let origin = ui.cursor().min.to_vec2();
            probe.content = Rect::from_min_size(origin.to_pos2(), vec2(width, content.height));
            let settings = ui
                .allocate_exact_size(vec2(width, content.setting_height), Sense::hover())
                .0;
            probe.settings = settings;
            let reset_width = (ui
                .painter()
                .layout_no_wrap("Reset All".into(), theme.font(), theme.fg())
                .size()
                .x
                + 16.0 * m.scale)
                .min(settings.width() * 0.42);
            probe.reset_all = Rect::from_center_size(
                pos2(
                    settings.right() - 12.0 * m.scale - reset_width * 0.5,
                    settings.center().y,
                ),
                vec2(reset_width, m.control),
            );
            self.editor_text(
                ui,
                theme,
                Rect::from_min_max(
                    settings.min + vec2(12.0 * m.scale, 0.0),
                    pos2(
                        (probe.reset_all.left() - m.gap).max(settings.left()),
                        settings.bottom(),
                    ),
                ),
                "Hotkeys",
                "Customize keyboard shortcuts for every command.",
            );
            let reset_button = components::text_button_at(
                ui,
                probe.reset_all,
                area_id.with("reset-all"),
                theme,
                "Reset All",
                false,
            )
            .on_hover_text("Restore every command's default shortcuts");
            self.editor_controls.push(reset_button.id);
            if reset_button.clicked() {
                reset_all = Some(reset_button.id);
            }
            self.editor_controls.push(list_id);
            probe.separator = ui
                .allocate_exact_size(vec2(width, content.separator_height), Sense::hover())
                .0;
            ui.painter().hline(
                probe.separator.x_range(),
                probe.separator.center().y,
                egui::Stroke::new(1.0, theme.line()),
            );
            let commands_clip = ui.clip_rect().intersect(Rect::from_min_max(
                probe.separator.left_bottom(),
                probe.content.max,
            ));
            for (index, layout) in &content.rows {
                let entry = &COMMANDS[*index];
                let target = ui
                    .allocate_exact_size(vec2(width, layout.height), Sense::hover())
                    .0;
                let placement = self.editor_motion.place_in_view(
                    Id::new(("hotkey-row", entry.command)),
                    target.translate(-origin),
                    true,
                    ui.clip_rect().translate(-origin),
                );
                let rect = Rect::from_min_size(placement.rect.min + origin, target.size());
                let mut row_ui =
                    ui.new_child(UiBuilder::new().id_salt(entry.command).max_rect(rect));
                row_ui.set_clip_rect(row_ui.clip_rect().intersect(rect).intersect(commands_clip));
                row_ui.multiply_opacity(placement.opacity);
                let hit = row_ui.interact(
                    rect,
                    Id::new(("hotkey-command", entry.command)),
                    Sense::CLICK,
                );
                crate::commands::result_row(
                    &row_ui,
                    &hit,
                    list_id,
                    Id::new("hotkeys-search"),
                    entry.title,
                    self.editor_selected == Some(entry.command),
                    true,
                );
                if hit.clicked() {
                    self.editor_selected = Some(entry.command);
                    ui.memory_mut(|memory| memory.request_focus(list_id));
                }
                if hit.hovered() {
                    row_ui
                        .painter()
                        .rect_filled(rect, theme.radius, Theme::color(theme.hover));
                }
                if self.editor_selected == Some(entry.command) {
                    if ui.memory(|memory| memory.has_focus(list_id)) {
                        row_ui.painter().rect_filled(
                            rect,
                            theme.radius,
                            Theme::color(theme.selection).gamma_multiply(0.72),
                        );
                    }
                    components::focus_ring(
                        &row_ui,
                        rect,
                        list_id.with(entry.command),
                        theme,
                        ui.memory(|memory| memory.has_focus(list_id)),
                    );
                    if self.editor_reveal {
                        ui.scroll_to_rect(rect, None);
                    }
                }
                let at = rect.min.to_vec2();
                let mut row_probe = EditorRowProbe {
                    command: entry.command,
                    rect,
                    // egui rounds the allocated row to its layout grid. Use
                    // that actual boundary for the full-height text column.
                    text: layout.text.translate(at).intersect(rect),
                    bindings_area: layout.bindings.translate(at),
                    reset: layout.reset.map(|rect| rect.translate(at)),
                    add: layout.add.translate(at),
                    bindings: Vec::new(),
                };
                self.editor_text(
                    &row_ui,
                    theme,
                    row_probe.text,
                    entry.title,
                    entry.description,
                );
                if let Some(rect) = row_probe.reset {
                    let response = components::icon_button_at(
                        &row_ui,
                        rect,
                        Id::new(("hotkey-reset", entry.command)),
                        theme,
                        Icon::Refresh,
                        false,
                    )
                    .on_hover_text("Restore this command's default shortcuts");
                    components::label_control(
                        &response,
                        &format!("Reset shortcuts for {}", entry.title),
                    );
                    self.editor_controls.push(response.id);
                    if response.has_focus() {
                        ui.scroll_to_rect(row_probe.rect, None);
                    }
                    if response.clicked() {
                        reset = Some(entry.command);
                    }
                }
                let bindings_rect = layout.bindings.translate(at);
                let mut binding_focused = false;
                row_ui.scope_builder(UiBuilder::new().max_rect(bindings_rect), |ui| {
                    ui.set_clip_rect(bindings_rect.intersect(ui.clip_rect()));
                    components::menu_scroll_area_for_content(
                        ui,
                        theme,
                        ("hotkey-bindings", entry.command),
                        bindings_rect.height(),
                        layout.binding_height,
                    )
                    .show(ui, |ui| {
                        let content = ui
                            .allocate_exact_size(
                                vec2(
                                    (bindings_rect.width() - 4.0 * m.scale).max(1.0),
                                    layout.binding_height,
                                ),
                                Sense::hover(),
                            )
                            .0;
                        if layout.chips.is_empty() {
                            components::elided_label(
                                ui.painter(),
                                content,
                                theme,
                                "Unassigned",
                                theme.dim(),
                                false,
                            );
                        }
                        for &(binding_index, chip) in &layout.chips {
                            let chip = chip.translate(content.min.to_vec2());
                            let remove_width = m.control.min(chip.width() * 0.46);
                            let edit_rect = Rect::from_min_max(
                                chip.min,
                                pos2(chip.right() - remove_width, chip.bottom()),
                            );
                            let remove_rect =
                                Rect::from_min_max(pos2(edit_rect.right(), chip.top()), chip.max);
                            let conflict = self.conflicts.iter().any(|conflict| {
                                conflict.first == binding_index || conflict.second == binding_index
                            });
                            let mut tone = theme.clone();
                            if conflict {
                                tone.accent = theme.danger;
                            }
                            ui.painter().rect_filled(
                                chip,
                                theme.radius,
                                Theme::color(theme.chrome),
                            );
                            let button = components::text_button_at(
                                ui,
                                edit_rect,
                                Id::new(("hotkey-binding", binding_index)),
                                &tone,
                                &self.labels[binding_index],
                                conflict,
                            )
                            .on_hover_text(if conflict {
                                "Conflicting binding — click to change"
                            } else {
                                "Click to change this binding"
                            });
                            components::label_control(
                                &button,
                                &format!(
                                    "{} · change shortcut for {}",
                                    self.labels[binding_index], entry.title,
                                ),
                            );
                            let remove_button = components::icon_button_at(
                                ui,
                                remove_rect,
                                Id::new(("hotkey-remove", binding_index)),
                                theme,
                                Icon::Close,
                                false,
                            )
                            .on_hover_text("Remove this binding…");
                            components::label_control(
                                &remove_button,
                                &format!(
                                    "Remove {} shortcut for {}",
                                    self.labels[binding_index], entry.title,
                                ),
                            );
                            self.editor_controls.extend([button.id, remove_button.id]);
                            if button.has_focus() || remove_button.has_focus() {
                                binding_focused = true;
                                ui.scroll_to_rect(chip, None);
                            }
                            if remove_button.clicked() {
                                remove = Some((binding_index, remove_button.id));
                            } else if button.clicked() {
                                edit = Some((entry.command, Some(binding_index), button.id));
                            }
                            row_probe.bindings.push(EditorBindingProbe {
                                index: binding_index,
                                chip,
                                edit: edit_rect,
                                remove: remove_rect,
                                clip: ui.clip_rect(),
                            });
                        }
                    });
                });
                if binding_focused {
                    ui.scroll_to_rect(rect, None);
                }
                let add = components::icon_button_at(
                    &row_ui,
                    row_probe.add,
                    Id::new(("hotkey-add", entry.command)),
                    theme,
                    if entry.command == Command::Launcher {
                        Icon::Keyboard
                    } else {
                        Icon::Plus
                    },
                    false,
                )
                .on_hover_text(if entry.command == Command::Launcher {
                    "Change the global launcher shortcut"
                } else {
                    "Assign another shortcut"
                });
                components::label_control(&add, &format!("Assign shortcut for {}", entry.title));
                self.editor_controls.push(add.id);
                if add.has_focus() {
                    ui.scroll_to_rect(rect, None);
                }
                if add.clicked() {
                    edit = Some((entry.command, None, add.id));
                }
                probe.rows.push(row_probe);
            }
            if content.rows.is_empty() {
                let rect = ui
                    .allocate_exact_size(vec2(width, content.setting_height), Sense::hover())
                    .0;
                components::elided_label(
                    ui.painter(),
                    rect,
                    theme,
                    "No matching commands",
                    theme.dim(),
                    true,
                );
            }
        });
        probe.content = Rect::from_min_size(probe.content.min, output.content_size);
        if !ui.is_sizing_pass() {
            self.editor_scroll_reset = false;
            self.editor_reveal = false;
        }
        let changed = if let Some(command) = reset {
            self.reset_command(command);
            self.editor_selected = Some(command);
            self.editor_reveal = true;
            // A modified-only control disappears after reset.
            crate::keyboard::request_id(ui.ctx(), list_id);
            true
        } else {
            false
        };
        if let Some((index, return_to)) = remove {
            self.cancel_pending();
            self.editor_confirmation = Some(EditorConfirmation {
                action: EditorConfirmationAction::Remove {
                    index,
                    binding: self.bindings[index].clone(),
                },
                return_to,
                focus_cancel: true,
                accepted: false,
            });
        } else if let Some(return_to) = reset_all {
            self.cancel_pending();
            self.editor_confirmation = Some(EditorConfirmation {
                action: EditorConfirmationAction::ResetAll,
                return_to,
                focus_cancel: true,
                accepted: false,
            });
        } else if let Some((command, index, return_to)) = edit {
            self.recording_return = Some(return_to);
            self.start_recording(command, index);
            self.recording_keys_down = ui.input(|input| input.keys_down.clone());
            self.recorded_frame = Some(ui.ctx().cumulative_frame_nr());
        }
        changed
    }

    fn editor_text(
        &self,
        ui: &egui::Ui,
        theme: &Theme,
        rect: Rect,
        title: &str,
        description: &str,
    ) {
        if !rect.intersects(ui.clip_rect()) || rect.width() < 1.0 {
            return;
        }
        let matcher = foldex_match::Matcher::new(&self.filter);
        let title = ui.painter().layout_job(crate::commands::highlight(
            title,
            &matcher.match_indices(title).unwrap_or_default(),
            theme.font(),
            theme.fg(),
            theme.accent(),
            rect.width(),
        ));
        let description = ui.painter().layout_job(crate::commands::highlight(
            description,
            &matcher.match_indices(description).unwrap_or_default(),
            egui::FontId::proportional((theme.font_size - 1.0).max(9.0)),
            theme.dim(),
            theme.accent(),
            rect.width(),
        ));
        let gap = 2.0 * theme.metrics().scale;
        let top = rect.center().y - (title.size().y + description.size().y + gap) * 0.5;
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        painter.galley(pos2(rect.left(), top), title.clone(), theme.fg());
        painter.galley(
            pos2(rect.left(), top + title.size().y + gap),
            description,
            theme.dim(),
        );
    }

    fn editor_confirmation_ui(&mut self, ctx: &egui::Context, theme: &Theme) -> bool {
        let Some(mut confirmation) = self.editor_confirmation.take() else {
            return false;
        };
        let m = theme.metrics();
        let (title, description, accept_label) = match &confirmation.action {
            EditorConfirmationAction::Remove { binding, .. } => {
                let command = COMMANDS
                    .iter()
                    .find(|entry| entry.command == binding.command)
                    .map_or("this command", |entry| entry.title);
                (
                    "Remove shortcut",
                    format!("Remove {} from {}?", binding.label(), command),
                    "Remove",
                )
            }
            EditorConfirmationAction::ResetAll => (
                "Reset all hotkeys",
                "Restore every command's default shortcuts? Your custom bindings will be replaced."
                    .into(),
                "Reset All",
            ),
        };
        let mut open = true;
        let mut accept = false;
        components::dialog(
            ctx,
            Id::new("hotkey-confirmation"),
            theme,
            title,
            390.0 * m.scale,
            &mut open,
            |ui| {
                ui.add(egui::Label::new(&description).wrap());
                ui.horizontal_wrapped(|ui| {
                    let cancel_width = ui
                        .painter()
                        .layout_no_wrap("Cancel".into(), theme.font(), theme.fg())
                        .size()
                        .x
                        + 16.0 * m.scale;
                    let rect = ui
                        .allocate_exact_size(vec2(cancel_width, m.control), Sense::hover())
                        .0;
                    let cancel = components::text_button_at(
                        ui,
                        rect,
                        Id::new("hotkey-confirmation-cancel"),
                        theme,
                        "Cancel",
                        false,
                    );
                    if confirmation.focus_cancel && components::can_focus(ui) {
                        cancel.request_focus();
                        confirmation.focus_cancel = ui.is_sizing_pass() || !cancel.has_focus();
                    }
                    if cancel.clicked() {
                        ui.close();
                    }
                    let accept_width = ui
                        .painter()
                        .layout_no_wrap(accept_label.into(), theme.font(), theme.fg())
                        .size()
                        .x
                        + 16.0 * m.scale;
                    let rect = ui
                        .allocate_exact_size(vec2(accept_width, m.control), Sense::hover())
                        .0;
                    let button = components::text_button_at(
                        ui,
                        rect,
                        Id::new("hotkey-confirmation-accept"),
                        theme,
                        accept_label,
                        true,
                    );
                    if button.clicked() && !confirmation.accepted {
                        accept = true;
                        ui.close();
                    }
                });
            },
        );
        let mut changed = false;
        if accept {
            confirmation.accepted = true;
            match &confirmation.action {
                EditorConfirmationAction::Remove { index, binding } => {
                    // Indices can change if a caller replaces bindings while this
                    // modal is open. Never remove a different binding by accident.
                    if self.bindings.get(*index) == Some(binding) {
                        changed = self.remove(*index);
                    }
                }
                EditorConfirmationAction::ResetAll => {
                    self.reset_defaults();
                    changed = true;
                }
            }
        }
        if open {
            self.editor_confirmation = Some(confirmation);
        } else {
            let return_to = if confirmation.accepted {
                Id::new("hotkey-commands-focus")
            } else {
                confirmation.return_to
            };
            crate::keyboard::request_id(ctx, return_to);
        }
        changed
    }

    fn editor_overlays(&mut self, ctx: &egui::Context, theme: &Theme) -> bool {
        let m = theme.metrics();
        let mut changed = self.editor_confirmation_ui(ctx, theme);
        if self.recording.is_some() {
            self.poll_recording(ctx);
        }
        if let Some(recording) = self.recording.clone() {
            let candidate = Binding {
                command: recording.command,
                sequence: recording.strokes.clone(),
            };
            let conflicts = self.conflicting_indices(&candidate, recording.replace);
            let mut assign = None;
            let mut cancel = false;
            let mut clear = false;
            let title = COMMANDS
                .iter()
                .find(|entry| entry.command == recording.command)
                .map_or("Command", |entry| entry.title);
            let mut recording_open = true;
            foldex_ui::components::dialog(
                ctx,
                egui::Id::new("hotkey-recording"),
                theme,
                "Assign shortcut",
                370.0 * m.scale,
                &mut recording_open,
                |ui| {
                    if self.recording_cancel {
                        ui.close();
                    }
                    ui.label(egui::RichText::new(title).strong());
                    let capture_id = egui::Id::new("hotkey-recording-capture");
                    let (capture_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), m.control),
                        egui::Sense::hover(),
                    );
                    let capture = foldex_ui::components::PopupFocus::surface(
                        ui,
                        capture_rect,
                        capture_id,
                        "Record a shortcut",
                    )
                    .interact(Sense::click());
                    if capture.clicked() {
                        capture.request_focus();
                        self.recording_review_focus = false;
                    }
                    if self.recording_focus_capture && foldex_ui::components::can_focus(ui) {
                        capture.request_focus();
                        self.recording_focus_capture = ui.is_sizing_pass() || !capture.has_focus();
                    }
                    ui.painter().rect_filled(
                        capture_rect,
                        theme.radius,
                        foldex_ui::Theme::color(theme.chrome),
                    );
                    let captured_label = candidate.label();
                    let held = held_modifiers_label(self.recording_held);
                    let preview = if captured_label.is_empty() {
                        held
                    } else if held.is_empty() {
                        captured_label
                    } else {
                        format!("{captured_label}  ·  {held}")
                    };
                    foldex_ui::components::elided_label(
                        ui.painter(),
                        capture_rect.shrink2(egui::vec2(8.0 * m.scale, 0.0)),
                        theme,
                        if preview.is_empty() {
                            "Press a key combination…"
                        } else {
                            &preview
                        },
                        theme.fg(),
                        false,
                    );
                    foldex_ui::components::focus_ring(
                        ui,
                        capture_rect,
                        capture_id,
                        theme,
                        capture.has_focus(),
                    );
                    ui.label(if recording.strokes.is_empty() {
                        "Hold any modifiers and press a key."
                    } else if recording.command == Command::Launcher {
                        "This combination opens the launcher system-wide."
                    } else {
                        "Keep pressing combinations to extend the sequence, then assign it."
                    });
                    if recording.command == Command::Launcher
                        && !recording.strokes.is_empty()
                        && launcher_shortcut(&candidate).is_none()
                    {
                        ui.label(
                            egui::RichText::new("Include Ctrl, Alt, Shift or Win with one key.")
                                .color(Theme::color(theme.danger)),
                        );
                    }
                    if let Some(status) = &self.status {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(status).color(Theme::color(theme.danger)),
                            )
                            .wrap(),
                        );
                    }
                    for &index in &conflicts {
                        let binding = &self.bindings[index];
                        let name = COMMANDS
                            .iter()
                            .find(|entry| entry.command == binding.command)
                            .map_or("Command", |entry| entry.title);
                        ui.label(
                            egui::RichText::new(format!("{}: {}", name, self.labels[index]))
                                .color(ui.visuals().warn_fg_color),
                        );
                    }
                    ui.horizontal_wrapped(|ui| {
                        self.recording_review_focus = false;
                        let ready = !recording.strokes.is_empty()
                            && (recording.command != Command::Launcher
                                || launcher_shortcut(&candidate).is_some());
                        let assign_button = foldex_ui::components::text_button_enabled(
                            ui,
                            theme,
                            "Assign",
                            true,
                            ready && conflicts.is_empty(),
                        );
                        self.recording_review_focus |= assign_button.has_focus();
                        if assign_button.clicked() {
                            assign = Some(false);
                        }
                        if !conflicts.is_empty() {
                            let replace_button = foldex_ui::components::text_button_enabled(
                                ui,
                                theme,
                                "Replace conflicts",
                                false,
                                ready,
                            );
                            self.recording_review_focus |= replace_button.has_focus();
                            if replace_button.clicked() {
                                assign = Some(true);
                            }
                        }
                        let clear_button =
                            foldex_ui::components::text_button(ui, theme, "Clear", false);
                        self.recording_review_focus |= clear_button.has_focus();
                        if clear_button.clicked() {
                            clear = true;
                        }
                        let cancel_button =
                            foldex_ui::components::text_button(ui, theme, "Cancel", false);
                        self.recording_review_focus |= cancel_button.has_focus();
                        if cancel_button.clicked() {
                            ui.close();
                        }
                    });
                },
            );
            if components::dialog_is_closing(ctx, Id::new("hotkey-recording")) {
                self.recording_cancel = true;
                self.native_capture.stop();
            }
            cancel |= !recording_open;
            if clear {
                self.status = None;
                self.recording_review_focus = false;
                if let Some(recording) = self.recording.as_mut() {
                    recording.strokes.clear();
                    self.recording_focus_capture = true;
                }
            }
            if cancel {
                self.cancel_recording();
                self.editor_focus = self.recording_return.is_none();
                ctx.request_repaint();
            }
            if let Some(replace_conflicts) = assign {
                match self.assign(candidate, recording.replace, replace_conflicts) {
                    Ok(()) => {
                        changed = true;
                        self.cancel_recording();
                        self.recording_return = Some(Id::new("hotkey-commands-focus"));
                        self.editor_selected = Some(recording.command);
                        self.editor_reveal = true;
                        self.editor_focus = false;
                        self.status = None;
                    }
                    Err(error) => self.status = Some(error),
                }
            }
        }
        if self.recording.is_none()
            && let Some(id) = self.recording_return.take()
        {
            crate::keyboard::request_id(ctx, id);
        }
        changed
    }
}

fn default_bindings() -> Vec<Binding> {
    let mut bindings: Vec<_> = COMMANDS
        .iter()
        .filter(|entry| !matches!(entry.command, Command::NextPanel | Command::PreviousPanel))
        .filter_map(|entry| {
            KeyStroke::parse(entry.shortcut).map(|stroke| Binding {
                command: entry.command,
                sequence: vec![stroke],
            })
        })
        .collect();
    for (command, key, modifiers) in [
        (Command::Redo, Key::Z, Modifiers::CTRL | Modifiers::SHIFT),
        (Command::GoTo, Key::F4, Modifiers::NONE),
        (Command::EditAddress, Key::D, Modifiers::ALT),
        (Command::Filter, Key::F3, Modifiers::NONE),
        (
            Command::NextPanel,
            Key::ArrowRight,
            Modifiers::CTRL | Modifiers::ALT,
        ),
        (
            Command::PreviousPanel,
            Key::ArrowLeft,
            Modifiers::CTRL | Modifiers::ALT,
        ),
        (
            Command::ExpandedSearch,
            Key::F,
            Modifiers::CTRL | Modifiers::SHIFT,
        ),
    ] {
        bindings.push(Binding::single(command, key, modifiers));
    }
    Hotkeys::add_keyboard_defaults(&mut bindings);
    bindings
}

fn allowed(command: Command, allow_file_ops: bool) -> bool {
    allow_file_ops
        || !matches!(
            command,
            Command::Copy
                | Command::Undo
                | Command::Redo
                | Command::Cut
                | Command::Paste
                | Command::CopyPath
                | Command::Rename
                | Command::Recycle
                | Command::DeletePermanent
                | Command::NewFolder
                | Command::NewFile
                | Command::SelectAll
                | Command::InvertSelection
                | Command::Properties
        )
}
fn clipboard_modifiers(modifiers: Modifiers) -> Modifiers {
    if modifiers.ctrl || modifiers.mac_cmd {
        modifiers
    } else {
        Modifiers::CTRL
    }
}
fn clipboard_slot(key: Key) -> Option<usize> {
    match key {
        Key::C => Some(0),
        Key::X => Some(1),
        Key::V => Some(2),
        _ => None,
    }
}
fn clipboard_stroke_slot(stroke: Stroke) -> Option<usize> {
    let ctrl = stroke.bits & 1 != 0;
    let command = stroke.bits & 0b1001 != 0;
    let shift = stroke.bits & 4 != 0;
    if command && stroke.key == Key::C || ctrl && stroke.key == Key::Insert {
        Some(0)
    } else if command && stroke.key == Key::X || shift && stroke.key == Key::Delete {
        Some(1)
    } else if command && stroke.key == Key::V || shift && stroke.key == Key::Insert {
        Some(2)
    } else {
        None
    }
}
fn recording_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Key { .. }
            | Event::ModifiersChanged(_)
            | Event::Text(_)
            | Event::Ime(_)
            | Event::Copy
            | Event::Cut
            | Event::Paste(_)
    )
}

fn discard_recorded_events(events: &mut Vec<Event>) {
    events.retain(|event| {
        !recording_event(event) || matches!(event, Event::Key { pressed: false, .. })
    });
    for event in events {
        if let Event::Key { modifiers, .. } = event {
            *modifiers = Modifiers::NONE;
        }
    }
}

fn held_modifiers_label(modifiers: Modifiers) -> String {
    [
        (modifiers.ctrl, "Ctrl"),
        (modifiers.alt, "Alt"),
        (modifiers.shift, "Shift"),
        (
            modifiers.mac_cmd,
            if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Win"
            },
        ),
    ]
    .into_iter()
    .filter_map(|(held, label)| held.then_some(label))
    .collect::<Vec<_>>()
    .join("+")
}

fn modifier_key(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}
fn key_label(key: Key) -> &'static str {
    match key {
        Key::Comma => ",",
        Key::Period => ".",
        Key::Minus => "-",
        Key::Plus => "+",
        Key::Equals => "=",
        Key::Slash => "/",
        Key::Backslash => "\\",
        Key::Quote => "'",
        _ => key.name(),
    }
}
fn editing_owned(ctx: &egui::Context) -> bool {
    ctx.text_edit_focused()
        || ctx
            .plugin_opt::<egui::text_selection::LabelSelectionState>()
            .is_some_and(|selection| selection.lock().has_selection())
}
fn protected_stroke(stroke: Stroke, text_focus: bool, editing: bool) -> bool {
    let function = stroke
        .key
        .name()
        .strip_prefix('F')
        .is_some_and(|suffix| suffix.parse::<u8>().is_ok());
    (text_focus && stroke.unmodified_or_shift() && !function)
        || (editing
            && stroke.bits & 0b1001 != 0
            && stroke.bits & 2 == 0
            && matches!(
                stroke.key,
                Key::C
                    | Key::X
                    | Key::V
                    | Key::A
                    | Key::Z
                    | Key::Y
                    | Key::Insert
                    | Key::Backspace
                    | Key::Delete
                    | Key::ArrowLeft
                    | Key::ArrowRight
                    | Key::Home
                    | Key::End
            ))
}
fn text_owns(ctx: &egui::Context, stroke: Stroke) -> bool {
    protected_stroke(stroke, ctx.text_edit_focused(), editing_owned(ctx))
}
fn clipboard_event(event: &Event, stroke: Stroke) -> bool {
    matches!(
        (clipboard_stroke_slot(stroke), event),
        (Some(0), Event::Copy) | (Some(1), Event::Cut) | (Some(2), Event::Paste(_))
    )
}
fn consume_native_input(ctx: &egui::Context, stroke: Stroke) -> bool {
    ctx.input_mut(|input| {
        let logical = input.events.iter().position(|event| {
            matches!(event, Event::Key { key, modifiers, pressed: true, repeat: false, .. }
                if Stroke::event(*key, *modifiers) == stroke)
        });
        if let Some(index) = logical {
            // A physical press has at most one logical Key representation.
            input.events.remove(index);
        }
        if let Some(index) = input
            .events
            .iter()
            .position(|event| clipboard_event(event, stroke))
        {
            input.events.remove(index);
        }
        logical.is_some()
    })
}
fn consume_event(events: &mut Vec<Event>, index: usize, stroke: Stroke) {
    let logical_key = match events.remove(index) {
        Event::Key { key, .. } => Some(key),
        _ => None,
    };
    if logical_key.is_some()
        && events
            .get(index)
            .is_some_and(|event| clipboard_event(event, stroke))
    {
        events.remove(index);
    }
    if stroke.unmodified_or_shift() {
        // Text aliases use the original logical symbol (e.g. ':'), even when
        // shortcut matching normalized the chord to its base key (';').
        let key = logical_key.unwrap_or(stroke.key);
        let expected = if key == Key::Space {
            " "
        } else {
            key.symbol_or_name()
        };
        if matches!(events.get(index),Some(Event::Text(text)) if text.eq_ignore_ascii_case(expected))
        {
            events.remove(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    fn up(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers,
        }
    }
    fn raw(time: f64, events: Vec<Event>) -> egui::RawInput {
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            ..Default::default()
        }
    }
    fn run(
        hotkeys: &mut Hotkeys,
        ctx: &egui::Context,
        time: f64,
        events: Vec<Event>,
        allow: bool,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let mut output = ctx.run_ui(raw(time, events), |ui| {
            commands.extend(hotkeys.dispatch_many(ui.ctx(), allow))
        });
        output.textures_delta.clear();
        commands
    }
    fn sequence(command: Command, keys: &[Key]) -> Binding {
        Binding {
            command,
            sequence: keys
                .iter()
                .map(|&key| KeyStroke::new(key, Modifiers::NONE))
                .collect(),
        }
    }

    #[test]
    fn recorder_preserves_every_modifier_and_arbitrary_sequence_length() {
        let ctx = egui::Context::default();
        let mut hotkeys = Hotkeys::from_bindings(vec![]);
        hotkeys.record(Command::Refresh);
        let modifiers = Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT;
        // Exceed the per-frame event budget across frames: it must not impose
        // a limit on a binding's total length. Include plain Enter and Space.
        let keys: Vec<_> = (0..40)
            .map(|at| [Key::A, Key::Enter, Key::Space, Key::B][at % 4])
            .collect();
        for (frame, &pressed) in keys.iter().enumerate() {
            let mut events = vec![];
            if frame == 0 {
                events.push(key(Key::SuperLeft, Modifiers::NONE));
            }
            events.push(key(pressed, modifiers));
            events.push(up(pressed, modifiers));
            events.push(Event::Text("ignored".into()));
            let mut input = raw(frame as f64 / 60.0, events);
            hotkeys.prepare_input(&mut input);
            let mut output = ctx.run_ui(input, |ui| hotkeys.poll_recording(ui.ctx()));
            output.textures_delta.clear();
        }
        let captured = hotkeys.recording.as_ref().unwrap().strokes.clone();
        assert_eq!(captured.len(), keys.len());
        assert!(
            captured
                .iter()
                .all(|stroke| stroke.ctrl && stroke.alt && stroke.shift && stroke.cmd)
        );
        assert!(captured[0].label().contains(if cfg!(target_os = "macos") {
            "Cmd+"
        } else {
            "Win+"
        }));
        hotkeys
            .assign(
                Binding {
                    command: Command::Refresh,
                    sequence: captured,
                },
                None,
                false,
            )
            .unwrap();
        let saved = serde_json::to_string(hotkeys.bindings()).unwrap();
        let mut restored = Hotkeys::from_bindings(serde_json::from_str(&saved).unwrap());
        let mut modifiers = modifiers;
        modifiers.mac_cmd = true;
        for (at, &pressed) in keys.iter().enumerate() {
            let commands = run(
                &mut restored,
                &ctx,
                1.0 + at as f64 / 60.0,
                vec![key(pressed, modifiers), up(pressed, modifiers)],
                true,
            );
            assert_eq!(
                commands,
                if at + 1 == keys.len() {
                    vec![Command::Refresh]
                } else {
                    vec![]
                }
            );
        }
        // Releasing one Super key must preserve the other; focus loss clears both.
        let mut input = raw(
            2.0,
            vec![
                key(Key::SuperRight, Modifiers::NONE),
                up(Key::SuperLeft, Modifiers::NONE),
                key(Key::A, Modifiers::NONE),
            ],
        );
        hotkeys.prepare_input(&mut input);
        assert!(
            matches!(input.events.last(), Some(Event::Key { modifiers, .. }) if modifiers.mac_cmd)
        );
        input.focused = false;
        hotkeys.prepare_input(&mut input);
        let mut input = raw(2.1, vec![key(Key::A, Modifiers::NONE)]);
        hotkeys.prepare_input(&mut input);
        assert!(
            matches!(input.events.last(), Some(Event::Key { modifiers, .. }) if !modifiers.mac_cmd)
        );
    }

    #[test]
    fn raw_capture_previews_modifiers_without_exposing_them_to_ui_navigation() {
        for (key, modifiers, name) in [
            (Key::ControlLeft, Modifiers::CTRL, "Ctrl"),
            (Key::ShiftRight, Modifiers::SHIFT, "Shift"),
            (Key::AltLeft, Modifiers::ALT, "Alt"),
            (
                Key::SuperLeft,
                Modifiers {
                    mac_cmd: true,
                    ..Modifiers::NONE
                },
                if cfg!(target_os = "macos") {
                    "Cmd"
                } else {
                    "Win"
                },
            ),
        ] {
            let mut hotkeys = Hotkeys::default();
            hotkeys.record(Command::Parent);
            let mut input = raw(
                0.0,
                vec![
                    key_event_for_modifier(key, modifiers),
                    Event::ModifiersChanged(modifiers),
                ],
            );
            hotkeys.capture_raw_input(&mut input);
            assert_eq!(held_modifiers_label(hotkeys.recording_held), name);
            assert!(hotkeys.recording.as_ref().unwrap().strokes.is_empty());
            assert!(
                !input
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Key { pressed: true, .. }))
            );
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(input, |ui| {
                assert!(ui.input(|input| input.keys_down.is_empty() && input.modifiers.is_none()));
                hotkeys.poll_recording(ui.ctx());
                assert_eq!(held_modifiers_label(hotkeys.recording_held), name);
            });
            output.textures_delta.clear();
        }
    }

    fn key_event_for_modifier(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn raw_capture_hides_arrows_before_egui_can_move_keyboard_focus() {
        let ctx = egui::Context::default();
        let first = Id::new("recording-priority-first");
        let second = Id::new("recording-priority-second");
        let mut hotkeys = Hotkeys::default();
        let mut output = ctx.run_ui(raw(0.0, vec![]), |ui| {
            ui.interact(ui.max_rect(), first, Sense::focusable_noninteractive())
                .request_focus();
        });
        output.textures_delta.clear();
        hotkeys.record(Command::Parent);
        let mut input = raw(
            0.1,
            vec![
                key(Key::ArrowDown, Modifiers::NONE),
                Event::Text("private".into()),
                Event::Ime(egui::ImeEvent::Commit("private".into())),
            ],
        );
        hotkeys.capture_raw_input(&mut input);
        let mut output = ctx.run_ui(input, |ui| {
            ui.interact(ui.max_rect(), first, Sense::focusable_noninteractive());
            ui.interact(ui.max_rect(), second, Sense::focusable_noninteractive());
            assert_eq!(ui.memory(|memory| memory.focused()), Some(first));
            assert!(!ui.input(|input| input.events.iter().any(recording_event))
                || ui.input(|input| input.events.iter().all(|event| matches!(event, Event::ModifiersChanged(modifiers) if modifiers.is_none()))));
        });
        output.textures_delta.clear();
        assert_eq!(
            hotkeys.recording.as_ref().unwrap().strokes,
            vec![KeyStroke::new(Key::ArrowDown, Modifiers::NONE)]
        );
    }

    #[test]
    fn raw_capture_retains_event_order_and_releases_without_sticky_modifiers() {
        let mut hotkeys = Hotkeys::from_bindings(vec![]);
        hotkeys.record(Command::Refresh);
        let ctrl = Modifiers::CTRL;
        let mut input = raw(
            0.0,
            vec![
                key(Key::A, Modifiers::NONE),
                up(Key::A, Modifiers::NONE),
                key(Key::SuperLeft, Modifiers::NONE),
                key(Key::ControlLeft, ctrl),
                key(Key::A, ctrl),
                up(Key::A, ctrl),
                up(Key::SuperLeft, ctrl),
                up(Key::ControlLeft, Modifiers::NONE),
            ],
        );
        hotkeys.capture_raw_input(&mut input);
        let captured = &hotkeys.recording.as_ref().unwrap().strokes;
        assert_eq!(captured.len(), 2);
        assert!(!captured[0].cmd && !captured[0].ctrl);
        assert!(captured[1].cmd && captured[1].ctrl);
        assert!(hotkeys.recording_held.is_none());
        assert!(input.events.iter().all(|event| matches!(event,
            Event::Key { pressed: false, modifiers, .. } | Event::ModifiersChanged(modifiers) if modifiers.is_none())));
        hotkeys.cancel_recording();
        let mut ordinary = raw(0.1, vec![key(Key::A, Modifiers::NONE)]);
        hotkeys.capture_raw_input(&mut ordinary);
        assert!(
            matches!(ordinary.events[0], Event::Key { pressed: true, modifiers, .. } if modifiers.is_none())
        );
    }

    #[test]
    fn native_modifier_snapshot_wins_over_duplicate_logical_releases() {
        let mut hotkeys = Hotkeys::from_bindings(vec![]);
        hotkeys.record(Command::Refresh);
        let full = Modifiers {
            ctrl: true,
            alt: true,
            shift: true,
            mac_cmd: true,
            command: true,
        };
        let remaining = Modifiers {
            ctrl: false,
            command: false,
            ..full
        };
        let mut input = raw(0.0, vec![up(Key::ControlLeft, Modifiers::NONE)]);
        hotkeys.capture_input_batch(
            &mut input,
            native_recorder::Input {
                events: vec![key(Key::A, full)],
                modifiers: Some(remaining),
                ..Default::default()
            },
        );
        assert_eq!(hotkeys.recording_held, remaining);
        let stroke = &hotkeys.recording.as_ref().unwrap().strokes[0];
        assert!(stroke.ctrl && stroke.shift && stroke.alt && stroke.cmd);
    }

    #[test]
    fn raw_focus_loss_even_within_one_frame_cancels_without_leaking_keys() {
        let original = default_bindings();
        let mut hotkeys = Hotkeys::from_bindings(original.clone());
        hotkeys.record(Command::Parent);
        let mut input = raw(
            0.0,
            vec![
                Event::WindowFocused(false),
                Event::WindowFocused(true),
                key(Key::F5, Modifiers::NONE),
            ],
        );
        hotkeys.capture_raw_input(&mut input);
        assert!(!hotkeys.is_recording());
        assert_eq!(hotkeys.bindings(), original);
        assert!(
            !input
                .events
                .iter()
                .any(|event| matches!(event, Event::Key { pressed: true, .. }))
        );
        let mut normal = raw(0.1, vec![key(Key::F5, Modifiers::NONE)]);
        hotkeys.capture_raw_input(&mut normal);
        assert!(normal.events.iter().any(|event| matches!(
            event,
            Event::Key {
                key: Key::F5,
                pressed: true,
                ..
            }
        )));
    }

    #[test]
    fn escape_consumes_the_rest_of_the_recorded_burst_and_preserves_bindings() {
        let mut hotkeys = Hotkeys::default();
        let original = hotkeys.bindings().to_vec();
        hotkeys.record(Command::Parent);
        let mut input = raw(
            0.0,
            vec![
                key(Key::Escape, Modifiers::NONE),
                key(Key::F5, Modifiers::NONE),
                Event::Copy,
                Event::Text("private".into()),
            ],
        );
        hotkeys.capture_raw_input(&mut input);
        assert!(hotkeys.recording_cancel);
        assert_eq!(hotkeys.bindings(), original);
        assert!(input.events.iter().all(
            |event| matches!(event, Event::ModifiersChanged(modifiers) if modifiers.is_none())
        ));
    }

    #[test]
    fn raw_capture_detects_repeats_before_egui_and_preserves_separate_presses() {
        let mut hotkeys = Hotkeys::from_bindings(vec![]);
        hotkeys.record(Command::Refresh);
        let mut input = raw(
            0.0,
            vec![
                key(Key::A, Modifiers::NONE),
                key(Key::A, Modifiers::NONE),
                up(Key::A, Modifiers::NONE),
                key(Key::A, Modifiers::NONE),
            ],
        );
        hotkeys.capture_raw_input(&mut input);
        assert_eq!(hotkeys.recording.as_ref().unwrap().strokes.len(), 2);
        assert!(
            input
                .events
                .iter()
                .all(|event| !matches!(event, Event::Key { pressed: true, .. }))
        );
    }

    #[test]
    fn raw_capture_accepts_fast_sequences_beyond_the_command_dispatch_budget() {
        let mut hotkeys = Hotkeys::from_bindings(vec![]);
        hotkeys.record(Command::Refresh);
        let events = (0..100)
            .flat_map(|_| [key(Key::A, Modifiers::CTRL), up(Key::A, Modifiers::CTRL)])
            .collect();
        let mut input = raw(0.0, events);
        hotkeys.capture_raw_input(&mut input);
        assert_eq!(hotkeys.recording.as_ref().unwrap().strokes.len(), 100);
    }

    const SHIFTED_SYMBOLS: [(Key, Key); 7] = [
        (Key::Equals, Key::Plus),
        (Key::Semicolon, Key::Colon),
        (Key::Backslash, Key::Pipe),
        (Key::Slash, Key::Questionmark),
        (Key::OpenBracket, Key::OpenCurlyBracket),
        (Key::CloseBracket, Key::CloseCurlyBracket),
        (Key::Num1, Key::Exclamationmark),
    ];

    #[test]
    fn native_recorded_shifted_symbols_match_logical_dispatch_after_json_roundtrip() {
        for (base, logical) in SHIFTED_SYMBOLS {
            for modifiers in [
                Modifiers::SHIFT,
                Modifiers::CTRL | Modifiers::SHIFT,
                Modifiers::ALT | Modifiers::SHIFT,
                Modifiers {
                    shift: true,
                    mac_cmd: true,
                    ..Modifiers::NONE
                },
                Modifiers {
                    ctrl: true,
                    alt: true,
                    shift: true,
                    mac_cmd: true,
                    ..Modifiers::NONE
                },
            ] {
                let mut hotkeys = Hotkeys::from_bindings(vec![]);
                hotkeys.record(Command::Refresh);
                let mut input = raw(0.0, vec![]);
                hotkeys.capture_input_batch(
                    &mut input,
                    native_recorder::Input {
                        events: vec![key(base, modifiers), up(base, modifiers)],
                        ..Default::default()
                    },
                );
                let sequence = hotkeys.recording.as_ref().unwrap().strokes.clone();
                hotkeys
                    .assign(
                        Binding {
                            command: Command::Refresh,
                            sequence,
                        },
                        None,
                        false,
                    )
                    .unwrap();
                let json = serde_json::to_string(hotkeys.bindings()).unwrap();
                let mut restored = Hotkeys::from_bindings(serde_json::from_str(&json).unwrap());
                let mut event = key(logical, modifiers);
                if let Event::Key { physical_key, .. } = &mut event {
                    *physical_key = Some(base);
                }
                let mut events = vec![event];
                if !modifiers.ctrl && !modifiers.alt && !modifiers.mac_cmd {
                    events.push(Event::Text(logical.symbol_or_name().into()));
                }
                let ctx = egui::Context::default();
                let mut output = ctx.run_ui(raw(0.1, events), |ui| {
                    assert_eq!(
                        restored.dispatch_many(ui.ctx(), true),
                        [Command::Refresh],
                        "{base:?} {logical:?} {modifiers:?}"
                    );
                    assert!(!ui.input(|input| {
                        input
                            .events
                            .iter()
                            .any(|event| matches!(event, Event::Text(_)))
                    }));
                });
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn saved_shifted_symbol_bindings_match_the_native_base_key() {
        for (base, logical) in SHIFTED_SYMBOLS {
            let modifiers = Modifiers::CTRL | Modifiers::SHIFT;
            let mut hotkeys =
                Hotkeys::from_bindings(vec![Binding::single(Command::Refresh, logical, modifiers)]);
            assert_eq!(
                hotkeys.command_for(&KeyStroke::new(base, modifiers), true),
                Some(Command::Refresh),
                "{logical:?}"
            );
            assert_eq!(
                run(
                    &mut hotkeys,
                    &egui::Context::default(),
                    0.0,
                    vec![key(base, modifiers)],
                    true
                ),
                [Command::Refresh]
            );
        }
    }

    #[test]
    fn unshifted_symbol_bindings_remain_distinct_from_the_base_key() {
        for (base, logical) in SHIFTED_SYMBOLS {
            for modifiers in [Modifiers::NONE, Modifiers::CTRL] {
                assert_ne!(
                    KeyStroke::new(base, modifiers).compile(),
                    KeyStroke::new(logical, modifiers).compile()
                );
            }
        }
    }

    #[test]
    fn raw_overflow_discards_partial_bindings_and_keeps_releases_for_ui_cleanup() {
        let mut hotkeys = Hotkeys::default();
        let original = hotkeys.bindings().to_vec();
        hotkeys.record(Command::Parent);
        let mut events = vec![key(Key::F5, Modifiers::NONE); MAX_CAPTURE_EVENTS + 1];
        events.push(up(Key::Enter, Modifiers::CTRL));
        let mut input = raw(0.0, events);
        hotkeys.capture_raw_input(&mut input);
        assert!(!hotkeys.is_recording());
        assert_eq!(hotkeys.bindings(), original);
        assert!(hotkeys.status.as_deref().unwrap().contains("too many"));
        assert!(input.events.iter().any(|event| matches!(event, Event::Key { key: Key::Enter, pressed: false, modifiers, .. } if modifiers.is_none())));
    }

    #[test]
    fn late_native_events_after_cancellation_never_execute_application_shortcuts() {
        let mut hotkeys = Hotkeys::default();
        let mut input = raw(0.0, vec![]);
        hotkeys.capture_input_batch(
            &mut input,
            native_recorder::Input {
                events: vec![key(Key::F5, Modifiers::NONE), up(Key::F5, Modifiers::NONE)],
                ..Default::default()
            },
        );
        assert!(
            !input
                .events
                .iter()
                .any(|event| matches!(event, Event::Key { pressed: true, .. }))
        );
        assert!(
            input
                .events
                .iter()
                .any(|event| matches!(event, Event::Key { pressed: false, .. }))
        );
    }

    #[test]
    fn native_escape_and_overflow_cancel_before_any_widget_receives_input() {
        for overflow in [false, true] {
            let mut hotkeys = Hotkeys::default();
            let original = hotkeys.bindings().to_vec();
            hotkeys.record(Command::Parent);
            let mut input = raw(0.0, vec![key(Key::F5, Modifiers::NONE)]);
            hotkeys.capture_input_batch(
                &mut input,
                native_recorder::Input {
                    cancelled: !overflow,
                    overflow,
                    modifiers: Some(Modifiers::CTRL),
                    ..Default::default()
                },
            );
            assert!(!hotkeys.is_recording());
            assert!(hotkeys.recording_held.is_none());
            assert_eq!(hotkeys.bindings(), original);
            assert!(
                !input
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Key { pressed: true, .. }))
            );
        }
    }

    #[test]
    fn restoring_os_modifiers_does_not_overwrite_new_real_modifier_changes() {
        let mut hotkeys = Hotkeys::default();
        let mut input = raw(
            0.0,
            vec![
                Event::ModifiersChanged(Modifiers::NONE),
                Event::ModifiersChanged(Modifiers::SHIFT),
            ],
        );
        hotkeys.capture_input_batch(
            &mut input,
            native_recorder::Input {
                restore_modifiers: Some(Modifiers::CTRL),
                ..Default::default()
            },
        );
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(input, |ui| {
            assert_eq!(ui.input(|input| input.modifiers), Modifiers::SHIFT)
        });
        output.textures_delta.clear();
    }

    #[test]
    fn raw_repeat_tracking_uses_physical_keys_when_shift_changes_the_logical_release() {
        let mut hotkeys = Hotkeys::from_bindings(vec![]);
        hotkeys.record(Command::Refresh);
        let modifiers = Modifiers::CTRL | Modifiers::SHIFT;
        let down = Event::Key {
            key: Key::Exclamationmark,
            physical_key: Some(Key::Num1),
            pressed: true,
            repeat: false,
            modifiers,
        };
        let release = Event::Key {
            key: Key::Num1,
            physical_key: Some(Key::Num1),
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let mut input = raw(0.0, vec![down.clone(), release.clone(), down, release]);
        hotkeys.capture_raw_input(&mut input);
        assert_eq!(
            hotkeys.recording.as_ref().unwrap().strokes,
            vec![KeyStroke::new(Key::Num1, modifiers); 2]
        );
    }

    #[test]
    fn deep_sequence_conflicts_and_longest_prefix_fallback_are_exact() {
        let bindings = vec![
            sequence(Command::Back, &[Key::G]),
            sequence(Command::Parent, &[Key::G, Key::H, Key::J]),
            sequence(Command::Refresh, &[Key::G, Key::H, Key::J, Key::K]),
            sequence(Command::Forward, &[Key::G, Key::H, Key::L, Key::K]),
        ];
        let mut hotkeys = Hotkeys::from_bindings(bindings);
        assert_eq!(hotkeys.conflicts().len(), 4);
        assert!(
            hotkeys
                .assign(
                    sequence(Command::GoTo, &[Key::G, Key::H, Key::J]),
                    None,
                    false
                )
                .is_err()
        );
        let ctx = egui::Context::default();
        assert!(
            run(
                &mut hotkeys,
                &ctx,
                0.0,
                vec![
                    key(Key::G, Modifiers::NONE),
                    key(Key::H, Modifiers::NONE),
                    key(Key::J, Modifiers::NONE)
                ],
                true
            )
            .is_empty()
        );
        assert_eq!(
            run(&mut hotkeys, &ctx, 1.0, vec![], true),
            [Command::Parent]
        );
        let events = [Key::G, Key::H, Key::J, Key::K]
            .into_iter()
            .flat_map(|pressed| [up(pressed, Modifiers::NONE), key(pressed, Modifiers::NONE)])
            .collect();
        assert_eq!(
            run(&mut hotkeys, &ctx, 1.1, events, true),
            [Command::Refresh]
        );
    }

    #[test]
    fn actual_launcher_shortcut_replaces_the_old_separate_command_binding() {
        let mut hotkeys = Hotkeys::default();
        hotkeys
            .assign(
                Binding::single(Command::Launcher, Key::K, Modifiers::CTRL),
                None,
                false,
            )
            .unwrap();
        hotkeys.set_launcher_shortcut("Win+Ctrl+Shift+Alt+A");
        let bindings: Vec<_> = hotkeys
            .bindings()
            .iter()
            .filter(|binding| binding.command == Command::Launcher)
            .collect();
        assert_eq!(bindings.len(), 1);
        let shortcut = launcher_shortcut(bindings[0]).unwrap();
        assert_eq!(
            crate::launcher::native::parse_hotkey(&shortcut),
            Some((0x400f, b'A' as u32))
        );
        assert_eq!(hotkeys.display_for(Command::Launcher), bindings[0].label());
        let mut palette = Palette::new();
        hotkeys.update_palette(&mut palette);
        for text in [
            "win+control+ESC",
            "alt+arrowleft",
            "shift+f24",
            "Ctrl+Comma",
        ] {
            hotkeys.set_launcher_shortcut(text);
            let current = hotkeys
                .bindings()
                .iter()
                .find(|binding| binding.command == Command::Launcher)
                .unwrap();
            assert_eq!(
                crate::launcher::native::parse_hotkey(&launcher_shortcut(current).unwrap()),
                crate::launcher::native::parse_hotkey(text)
            );
        }
        hotkeys.set_launcher_shortcut("");
        assert!(hotkeys.display_for(Command::Launcher).is_empty());
        assert!(
            !hotkeys
                .bindings()
                .iter()
                .any(|binding| binding.command == Command::Launcher)
        );
    }

    #[test]
    fn launcher_registration_commits_the_whole_map_only_after_success() {
        use crate::launcher::native::HotkeyFeedback;
        let original = vec![
            Binding::single(
                Command::Launcher,
                Key::Space,
                Modifiers::CTRL | Modifiers::ALT,
            ),
            Binding::single(Command::Refresh, Key::K, Modifiers::CTRL),
        ];
        let proposed = vec![Binding::single(Command::Launcher, Key::K, Modifiers::CTRL)];
        let mut hotkeys = Hotkeys::from_bindings(original.clone());
        for error in [Some("Shortcut is in use".to_owned()), None] {
            hotkeys.launcher_change = Some(LauncherChange {
                requested: "Ctrl+K".into(),
                bindings: proposed.clone(),
            });
            assert!(!hotkeys.finish_launcher_change(HotkeyFeedback {
                requested: "Ctrl+K".into(),
                pending: true,
                ..Default::default()
            }));
            assert_eq!(hotkeys.bindings(), original);
            assert!(hotkeys.finish_launcher_change(HotkeyFeedback {
                requested: "Ctrl+K".into(),
                error: error.clone(),
                ..Default::default()
            }));
            assert_eq!(
                hotkeys.bindings(),
                if error.is_some() {
                    &original
                } else {
                    &proposed
                }
            );
        }
    }

    fn editor_frame(
        hotkeys: &mut Hotkeys,
        ctx: &egui::Context,
        theme: &Theme,
        frame: u32,
        size: egui::Vec2,
        events: Vec<Event>,
    ) -> (bool, bool) {
        let mut changed = false;
        let mut open = true;
        let mut input = raw(frame as f64 / 60.0, events);
        input.screen_rect = Some(Rect::from_min_size(egui::Pos2::ZERO, size));
        hotkeys.capture_raw_input(&mut input);
        let mut output = ctx.run_ui(input, |ui| {
            theme.apply(ui.ctx());
            changed |= hotkeys.editor_popup_ui(ui.ctx(), theme, &mut open);
        });
        output.textures_delta.clear();
        (changed, open)
    }

    fn editor_settle(
        hotkeys: &mut Hotkeys,
        ctx: &egui::Context,
        theme: &Theme,
        frame: &mut u32,
        size: egui::Vec2,
    ) {
        for _ in 0..5 {
            editor_frame(hotkeys, ctx, theme, *frame, size, vec![]);
            *frame += 1;
        }
    }

    fn editor_click(
        hotkeys: &mut Hotkeys,
        ctx: &egui::Context,
        theme: &Theme,
        frame: &mut u32,
        size: egui::Vec2,
        at: egui::Pos2,
    ) -> bool {
        let mut changed = false;
        for pressed in [true, false] {
            changed |= editor_frame(
                hotkeys,
                ctx,
                theme,
                *frame,
                size,
                vec![
                    Event::PointerMoved(at),
                    Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    },
                ],
            )
            .0;
            *frame += 1;
        }
        changed
    }

    fn editor_binding(hotkeys: &Hotkeys, command: Command) -> EditorBindingProbe {
        hotkeys
            .benchmark_editor_probe()
            .unwrap()
            .rows
            .iter()
            .find(|row| row.command == command)
            .unwrap()
            .bindings[0]
            .clone()
    }

    fn editor_host_frame(
        hotkeys: &mut Hotkeys,
        ctx: &egui::Context,
        theme: &Theme,
        frame: u32,
        events: Vec<Event>,
        open: &mut bool,
    ) -> bool {
        let mut clicked_file = false;
        let mut output = ctx.run_ui(raw(frame as f64 / 60.0, events), |ui| {
            theme.apply(ui.ctx());
            let invoker = components::text_button_at(
                ui,
                Rect::from_min_size(pos2(8.0, 8.0), vec2(110.0, 28.0)),
                Id::new("hotkeys-invoker"),
                theme,
                "Open hotkeys",
                false,
            );
            if frame == 0 {
                invoker.request_focus();
            }
            clicked_file = components::text_button_at(
                ui,
                Rect::from_min_size(pos2(8.0, 650.0), vec2(110.0, 28.0)),
                Id::new("underlying-file"),
                theme,
                "A file",
                false,
            )
            .clicked();
            if frame > 0 && *open {
                hotkeys.editor_popup_ui(ui.ctx(), theme, open);
            }
        });
        output.textures_delta.clear();
        clicked_file
    }

    #[test]
    fn editor_outside_press_keeps_the_dismissing_click_from_files_and_returns_focus() {
        for reduced_motion in [false, true] {
            let ctx = egui::Context::default();
            let theme = Theme {
                reduced_motion,
                ..Default::default()
            };
            let mut hotkeys = Hotkeys::default();
            let mut open = true;
            for frame in 0..35 {
                assert!(!editor_host_frame(
                    &mut hotkeys,
                    &ctx,
                    &theme,
                    frame,
                    vec![],
                    &mut open,
                ));
            }
            assert!(open && !hotkeys.editor_closing);
            let point = pos2(60.0, 664.0);
            assert!(
                !hotkeys
                    .benchmark_editor_probe()
                    .unwrap()
                    .outer
                    .contains(point)
            );
            let mut frame = 35;
            for pressed in [true, false] {
                assert!(!editor_host_frame(
                    &mut hotkeys,
                    &ctx,
                    &theme,
                    frame,
                    vec![
                        Event::PointerMoved(point),
                        Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Modifiers::NONE,
                        },
                    ],
                    &mut open,
                ));
                frame += 1;
            }
            for _ in 0..120 {
                assert!(!editor_host_frame(
                    &mut hotkeys,
                    &ctx,
                    &theme,
                    frame,
                    vec![],
                    &mut open,
                ));
                frame += 1;
                if !open {
                    break;
                }
            }
            assert!(!open && !hotkeys.editor_active);
            for _ in 0..3 {
                editor_host_frame(&mut hotkeys, &ctx, &theme, frame, vec![], &mut open);
                frame += 1;
            }
            assert_eq!(
                ctx.memory(|memory| memory.focused()),
                Some(Id::new("hotkeys-invoker"))
            );
        }
    }

    #[test]
    fn editor_child_dismissal_does_not_also_close_the_owner() {
        for child in 0..3 {
            let ctx = egui::Context::default();
            let theme = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            let size = vec2(1000.0, 700.0);
            let original = Binding::single(Command::Parent, Key::P, Modifiers::CTRL);
            let mut hotkeys = Hotkeys::from_bindings(vec![original.clone()]);
            hotkeys.benchmark_open_editor("Parent");
            let mut frame = 0;
            editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
            let chip = editor_binding(&hotkeys, Command::Parent);
            match child {
                0 | 1 => {
                    editor_click(
                        &mut hotkeys,
                        &ctx,
                        &theme,
                        &mut frame,
                        size,
                        if child == 0 {
                            chip.edit.center()
                        } else {
                            chip.remove.center()
                        },
                    );
                    editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
                    assert!(hotkeys.is_recording() || hotkeys.editor_confirmation.is_some());
                }
                _ => egui::Popup::open_id(&ctx, Id::new("hotkey-native-child")),
            }
            editor_click(&mut hotkeys, &ctx, &theme, &mut frame, size, pos2(5.0, 5.0));
            assert!(hotkeys.editor_active && !hotkeys.editor_closing);
            assert_eq!(hotkeys.bindings(), &[original]);
            egui::Popup::close_all(&ctx);
            editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
            assert!(hotkeys.editor_active && !hotkeys.editor_closing);
        }
    }

    #[test]
    fn editor_filtered_header_has_one_close_button_and_one_header_tab_stop() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let size = vec2(1000.0, 700.0);
        let mut hotkeys = Hotkeys::default();
        hotkeys.benchmark_open_editor("Commands");
        let mut frame = 0;
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let clear = Id::new("hotkeys-editor-window").with("clear");
        assert!(ctx.read_response(clear).is_none());
        assert!(!hotkeys.editor_controls.contains(&clear));
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![key(Key::Tab, Modifiers::NONE)],
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("hotkeys-editor-window").with("close"))
        );
        assert_eq!(hotkeys.filter, "Commands");
    }

    #[test]
    fn editor_keeps_recording_until_keyboard_review_assigns_the_whole_sequence() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let size = vec2(1000.0, 700.0);
        let mut hotkeys = Hotkeys::from_bindings(vec![]);
        hotkeys.benchmark_open_editor("Refresh");
        let mut frame = 0;
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        hotkeys.record(Command::Refresh);
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let modifiers = Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT;
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![key(Key::SuperLeft, Modifiers::NONE)],
        );
        frame += 1;
        for pressed in [Key::A, Key::B, Key::C] {
            editor_frame(
                &mut hotkeys,
                &ctx,
                &theme,
                frame,
                size,
                vec![key(pressed, modifiers), up(pressed, modifiers)],
            );
            frame += 1;
        }
        let captured = hotkeys.recording.as_ref().unwrap().strokes.clone();
        assert_eq!(captured.len(), 3);
        assert!(
            captured
                .iter()
                .all(|stroke| stroke.cmd && stroke.ctrl && stroke.alt && stroke.shift)
        );
        assert!(hotkeys.bindings().is_empty());
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![
                up(Key::SuperLeft, Modifiers::NONE),
                Event::ModifiersChanged(Modifiers::NONE),
                key(Key::Tab, Modifiers::NONE),
            ],
        );
        frame += 1;
        let (changed, _) = editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![key(Key::Enter, Modifiers::NONE)],
        );
        assert!(changed);
        assert!(!hotkeys.is_recording());
        assert_eq!(
            hotkeys.bindings(),
            &[Binding {
                command: Command::Refresh,
                sequence: captured
            }]
        );
    }

    #[test]
    fn clearing_a_reviewed_shortcut_immediately_returns_enter_to_the_recorder() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let size = vec2(1000.0, 700.0);
        let mut hotkeys = Hotkeys::from_bindings(vec![]);
        hotkeys.benchmark_open_editor("Refresh");
        let mut frame = 0;
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        hotkeys.record(Command::Refresh);
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![key(Key::A, Modifiers::NONE), up(Key::A, Modifiers::NONE)],
        );
        frame += 1;
        for _ in 0..2 {
            editor_frame(
                &mut hotkeys,
                &ctx,
                &theme,
                frame,
                size,
                vec![
                    key(Key::Tab, Modifiers::NONE),
                    up(Key::Tab, Modifiers::NONE),
                ],
            );
            frame += 1;
        }
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![key(Key::Space, Modifiers::NONE)],
        );
        frame += 1;
        assert!(hotkeys.recording.as_ref().unwrap().strokes.is_empty());
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![
                up(Key::Space, Modifiers::NONE),
                key(Key::Enter, Modifiers::NONE),
            ],
        );
        assert_eq!(
            hotkeys.recording.as_ref().unwrap().strokes,
            vec![KeyStroke::new(Key::Enter, Modifiers::NONE)]
        );
        assert!(hotkeys.bindings().is_empty());
    }

    #[test]
    fn closing_recording_releases_capture_before_its_dialog_animation_finishes() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let size = vec2(1000.0, 700.0);
        let mut hotkeys = Hotkeys::from_bindings(vec![]);
        hotkeys.benchmark_open_editor("Refresh");
        let mut frame = 0;
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        hotkeys.record(Command::Refresh);
        for _ in 0..4 {
            editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        }
        let close = ctx
            .read_response(Id::new("hotkey-recording").with("close"))
            .unwrap()
            .rect
            .center();
        editor_click(&mut hotkeys, &ctx, &theme, &mut frame, size, close);
        assert!(components::dialog_is_closing(
            &ctx,
            Id::new("hotkey-recording")
        ));
        assert!(
            hotkeys.recording_cancel,
            "Stop native capture when closing starts, not after the animation"
        );
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![key(Key::A, Modifiers::CTRL)],
        );
        assert!(
            hotkeys
                .recording
                .as_ref()
                .is_none_or(|recording| recording.strokes.is_empty())
        );
        assert!(hotkeys.bindings().is_empty());
    }

    #[test]
    fn editor_settings_separator_and_measured_rows_settle_at_supported_fonts_and_viewports() {
        for font_size in [10.0, 14.0, 17.0, 22.0, 24.0] {
            for size in [vec2(1200.0, 760.0), vec2(640.0, 360.0), vec2(380.0, 520.0)] {
                let ctx = egui::Context::default();
                let theme = Theme {
                    font_size,
                    reduced_motion: true,
                    ..Default::default()
                };
                let mut hotkeys = Hotkeys::default();
                let mut frame = 0;
                for query in ["", "Commands", "no-such-command-827491"] {
                    hotkeys.benchmark_open_editor(query);
                    editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
                    let probe = hotkeys.benchmark_editor_probe().unwrap();
                    let m = theme.metrics();
                    let viewport = Rect::from_min_size(egui::Pos2::ZERO, size);
                    let case = format!("font={font_size}, viewport={size:?}, query={query:?}");
                    assert!(
                        viewport.contains_rect(probe.outer),
                        "{case}, outer={:?}",
                        probe.outer
                    );
                    assert_eq!(probe.body, probe.body_clip, "{case}");
                    assert!(
                        probe.settings.contains_rect(probe.reset_all),
                        "{case}, settings={:?}, reset_all={:?}",
                        probe.settings,
                        probe.reset_all
                    );
                    assert!(
                        probe.settings.bottom() <= probe.separator.top() + 0.1,
                        "{case}, settings={:?}, separator={:?}",
                        probe.settings,
                        probe.separator
                    );
                    assert!(
                        probe.separator.height() >= 12.0 * m.scale,
                        "{case}, separator={:?}, scale={}",
                        probe.separator,
                        m.scale
                    );
                    assert!(
                        probe.search.right() < probe.close.left(),
                        "{case}, search={:?}, close={:?}",
                        probe.search,
                        probe.close
                    );
                    if query.is_empty() {
                        assert_eq!(probe.rows.len(), COMMANDS.len(), "{case}");
                    }
                    let mut previous_bottom = probe.separator.bottom();
                    for row in &probe.rows {
                        let row_case = format!(
                            "{case}, command={:?}, row={:?}, text={:?}, bindings_area={:?}, add={:?}",
                            row.command, row.rect, row.text, row.bindings_area, row.add,
                        );
                        assert!(
                            row.rect.top() >= previous_bottom - 0.1,
                            "{row_case}, previous_bottom={previous_bottom}"
                        );
                        assert!(
                            row.rect.contains_rect(row.text),
                            "{row_case}, row_edges={:?}, text_edges={:?}",
                            (
                                row.rect.left(),
                                row.rect.top(),
                                row.rect.right(),
                                row.rect.bottom()
                            ),
                            (
                                row.text.left(),
                                row.text.top(),
                                row.text.right(),
                                row.text.bottom()
                            ),
                        );
                        assert!(row.rect.contains_rect(row.bindings_area), "{row_case}");
                        assert!(row.rect.contains_rect(row.add), "{row_case}");
                        assert!(row.text.right() <= row.bindings_area.left(), "{row_case}");
                        for (index, binding) in row.bindings.iter().enumerate() {
                            if row.bindings.get(index + 1).is_some_and(|next| {
                                (next.chip.top() - binding.chip.top()).abs() < 0.1
                            }) {
                                continue;
                            }
                            // ScrollArea::begin rounds the content origin to
                            // physical pixels, then to the egui layout grid.
                            let rounding = 0.5 / ctx.pixels_per_point() + egui::emath::GUI_ROUNDING;
                            let expected_right = row.bindings_area.right() - 4.0 * m.scale;
                            let offset = binding.chip.right() - expected_right;
                            assert!(
                                offset.abs() <= rounding,
                                "{row_case}, line_end_chip={:?}, expected_right={expected_right}, offset={offset}, rounding={rounding}",
                                binding.chip,
                            );
                        }
                        for binding in &row.bindings {
                            if binding.clip.is_positive() {
                                assert!(
                                    probe.body_clip.contains_rect(binding.clip),
                                    "{row_case}, binding={binding:?}, body_clip={:?}",
                                    probe.body_clip
                                );
                                assert!(
                                    binding.clip.top() >= probe.separator.bottom(),
                                    "{row_case}, binding={binding:?}, separator={:?}",
                                    probe.separator
                                );
                            }
                        }
                        previous_bottom = row.rect.bottom();
                    }
                    let measured_height = probe.settings.height()
                        + probe.separator.height()
                        + if probe.rows.is_empty() {
                            probe.settings.height()
                        } else {
                            0.0
                        }
                        + probe.rows.iter().map(|row| row.rect.height()).sum::<f32>();
                    assert!(
                        (probe.content.height() - measured_height).abs() <= 0.1,
                        "{case}, content={:?}, measured_height={measured_height}",
                        probe.content
                    );
                    let before = (probe.outer, probe.separator, probe.content);
                    editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
                    let probe = hotkeys.benchmark_editor_probe().unwrap();
                    assert_eq!(
                        before,
                        (probe.outer, probe.separator, probe.content),
                        "{case}"
                    );
                }
            }
        }
    }

    #[test]
    fn editor_uses_shared_popup_geometry_and_only_modified_commands_have_reset_icons() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let size = vec2(1200.0, 760.0);
        let mut hotkeys = Hotkeys::default();
        let mut frame = 0;
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let probe = hotkeys.benchmark_editor_probe().unwrap();
        assert_eq!(probe.outer.width(), 720.0 * theme.metrics().scale);
        assert_eq!(probe.header.height(), theme.metrics().popup_header);
        assert_eq!(probe.search, components::popup_input_rect(probe.header, 1));
        assert_eq!(
            probe.close,
            components::popup_action_rect(probe.header, 0, 1)
        );
        assert_eq!(probe.rows.len(), COMMANDS.len());
        assert!(probe.rows.iter().all(|row| row.reset.is_none()));
        assert!(probe.reset_all.top() >= probe.header.bottom());
        assert!(probe.separator.top() >= probe.settings.bottom());
        assert!(
            ctx.read_response(Id::new("hotkeys-editor-window").with("close"))
                .is_some()
        );

        let mut bindings = hotkeys.bindings().to_vec();
        bindings.push(sequence(Command::Parent, &[Key::K, Key::P]));
        hotkeys.replace_bindings(bindings);
        hotkeys.benchmark_open_editor("Parent");
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let probe = hotkeys.benchmark_editor_probe().unwrap();
        assert!(
            probe
                .rows
                .iter()
                .all(|row| row.reset.is_some() == (row.command == Command::Parent))
        );
        let row = probe
            .rows
            .iter()
            .find(|row| row.command == Command::Parent)
            .unwrap();
        let reset = row.reset.unwrap();
        assert!(row.text.right() <= reset.left());
        assert!(reset.right() <= row.bindings_area.left());
        assert!(reset.right() <= row.add.left());
        assert!(editor_click(
            &mut hotkeys,
            &ctx,
            &theme,
            &mut frame,
            size,
            reset.center()
        ));
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let row = hotkeys
            .benchmark_editor_probe()
            .unwrap()
            .rows
            .iter()
            .find(|row| row.command == Command::Parent)
            .unwrap();
        assert!(row.reset.is_none());
        assert!(!hotkeys.command_is_modified(Command::Parent, &default_bindings()));
    }

    #[test]
    fn editor_pointer_remove_confirmation_cancels_or_accepts_without_starting_recording() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let size = vec2(1000.0, 700.0);
        let binding = sequence(Command::Parent, &[Key::K, Key::P]);
        let mut hotkeys = Hotkeys::from_bindings(vec![binding.clone()]);
        hotkeys.benchmark_open_editor("Parent");
        let original_revision = hotkeys.revision();
        let mut frame = 0;
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let chip = editor_binding(&hotkeys, Command::Parent);
        assert!(chip.chip.contains_rect(chip.edit) && chip.chip.contains_rect(chip.remove));
        assert_eq!(chip.edit.right(), chip.remove.left());
        assert!(chip.clip.contains(chip.remove.center()));
        assert!(!editor_click(
            &mut hotkeys,
            &ctx,
            &theme,
            &mut frame,
            size,
            chip.remove.center()
        ));
        assert!(hotkeys.editor_confirmation.is_some());
        assert!(!hotkeys.is_recording());
        assert_eq!(hotkeys.bindings(), &[binding.clone()]);
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let cancel = ctx
            .read_response(Id::new("hotkey-confirmation-cancel"))
            .unwrap()
            .rect
            .center();
        assert!(!editor_click(
            &mut hotkeys,
            &ctx,
            &theme,
            &mut frame,
            size,
            cancel
        ));
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        assert!(hotkeys.editor_confirmation.is_none());
        assert_eq!(hotkeys.bindings(), &[binding]);
        assert_eq!(hotkeys.revision(), original_revision);
        assert_eq!(hotkeys.filter, "Parent");

        let chip = editor_binding(&hotkeys, Command::Parent);
        editor_click(
            &mut hotkeys,
            &ctx,
            &theme,
            &mut frame,
            size,
            chip.remove.center(),
        );
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let accept = ctx
            .read_response(Id::new("hotkey-confirmation-accept"))
            .unwrap()
            .rect
            .center();
        assert!(editor_click(
            &mut hotkeys,
            &ctx,
            &theme,
            &mut frame,
            size,
            accept
        ));
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        assert!(hotkeys.editor_confirmation.is_none());
        assert!(!hotkeys.is_recording());
        assert!(hotkeys.bindings().is_empty());
        assert_eq!(hotkeys.revision(), original_revision.wrapping_add(1));
    }

    #[test]
    fn editor_binding_body_records_replacement_and_keeps_exact_two_chord_capture() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let size = vec2(1000.0, 700.0);
        let original = sequence(Command::Parent, &[Key::K, Key::P]);
        let mut hotkeys = Hotkeys::from_bindings(vec![original.clone()]);
        hotkeys.benchmark_open_editor("Parent");
        let mut frame = 0;
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let chip = editor_binding(&hotkeys, Command::Parent);
        editor_click(
            &mut hotkeys,
            &ctx,
            &theme,
            &mut frame,
            size,
            chip.edit.center(),
        );
        let recording = hotkeys.recording.as_ref().unwrap();
        assert_eq!(recording.replace, Some(chip.index));
        assert!(recording.strokes.is_empty());
        assert!(hotkeys.editor_confirmation.is_none());
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![
                key(Key::J, Modifiers::CTRL | Modifiers::ALT),
                Event::Text("j".into()),
            ],
        );
        frame += 1;
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![key(Key::L, Modifiers::SHIFT), Event::Text("L".into())],
        );
        frame += 1;
        let recording = hotkeys.recording.as_ref().unwrap();
        assert_eq!(
            recording.strokes,
            vec![
                KeyStroke::new(Key::J, Modifiers::CTRL | Modifiers::ALT),
                KeyStroke::new(Key::L, Modifiers::SHIFT),
            ]
        );
        assert_eq!(hotkeys.filter, "Parent");
        editor_frame(
            &mut hotkeys,
            &ctx,
            &theme,
            frame,
            size,
            vec![key(Key::Escape, Modifiers::NONE)],
        );
        frame += 1;
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        assert!(hotkeys.editor_active && !hotkeys.editor_closing);
        assert!(!hotkeys.is_recording());
        assert_eq!(hotkeys.bindings(), &[original]);
    }

    #[test]
    fn editor_keyboard_confirmation_owns_enter_escape_and_tab_without_owner_activation() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let size = vec2(1000.0, 700.0);
        let original = Binding::single(Command::Parent, Key::P, Modifiers::CTRL);
        let mut hotkeys = Hotkeys::from_bindings(vec![original.clone()]);
        hotkeys.benchmark_open_editor("Parent");
        let mut frame = 0;
        editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
        let remove_id = Id::new(("hotkey-remove", 0usize));
        for accept in [false, true] {
            ctx.memory_mut(|memory| memory.request_focus(remove_id));
            editor_frame(
                &mut hotkeys,
                &ctx,
                &theme,
                frame,
                size,
                vec![key(Key::Enter, Modifiers::NONE)],
            );
            frame += 1;
            editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
            assert!(hotkeys.editor_confirmation.is_some());
            assert!(!hotkeys.is_recording());
            assert!(ctx.memory(|memory| memory.has_focus(Id::new("hotkey-confirmation-cancel"))));
            if accept {
                editor_frame(
                    &mut hotkeys,
                    &ctx,
                    &theme,
                    frame,
                    size,
                    vec![key(Key::Tab, Modifiers::NONE)],
                );
                frame += 1;
                assert!(
                    ctx.memory(|memory| memory.has_focus(Id::new("hotkey-confirmation-accept")))
                );
            }
            editor_frame(
                &mut hotkeys,
                &ctx,
                &theme,
                frame,
                size,
                vec![key(
                    if accept { Key::Enter } else { Key::Escape },
                    Modifiers::NONE,
                )],
            );
            frame += 1;
            editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
            assert!(hotkeys.editor_confirmation.is_none());
            assert!(hotkeys.editor_active && !hotkeys.editor_closing);
            assert!(!hotkeys.is_recording());
            if accept {
                assert!(hotkeys.bindings().is_empty());
            } else {
                assert_eq!(hotkeys.bindings(), &[original.clone()]);
            }
        }
    }

    #[test]
    fn editor_narrow_large_font_multiple_chords_stay_inside_bounded_binding_columns() {
        for (size, font_size) in [(vec2(640.0, 540.0), 17.0), (vec2(380.0, 520.0), 22.0)] {
            let ctx = egui::Context::default();
            let theme = Theme {
                font_size,
                reduced_motion: true,
                ..Default::default()
            };
            let bindings = [
                Key::A,
                Key::B,
                Key::C,
                Key::D,
                Key::E,
                Key::F,
                Key::G,
                Key::H,
            ]
            .map(|key| Binding {
                command: Command::Parent,
                sequence: vec![
                    KeyStroke::new(key, Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT),
                    KeyStroke::new(Key::P, Modifiers::CTRL | Modifiers::SHIFT),
                ],
            })
            .to_vec();
            let mut hotkeys = Hotkeys::from_bindings(bindings);
            hotkeys.benchmark_open_editor("Parent");
            let mut frame = 0;
            editor_settle(&mut hotkeys, &ctx, &theme, &mut frame, size);
            let probe = hotkeys.benchmark_editor_probe().unwrap();
            assert!(Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(probe.outer));
            assert!(probe.search.right() <= probe.close.left());
            assert!(probe.header.bottom() <= probe.body.top());
            assert!(probe.body.bottom() <= probe.footer.top());
            let row = probe
                .rows
                .iter()
                .find(|row| row.command == Command::Parent)
                .unwrap();
            let m = theme.metrics();
            assert!(
                row.rect.height()
                    <= 3.0 * m.control
                        + 2.0 * m.gap
                        + Hotkeys::editor_text_height(&ctx, &theme)
                        + 22.0 * m.scale
                        + 0.1
            );
            assert!(row.text.right() <= row.reset.unwrap().left());
            assert!(row.text.right() <= row.bindings_area.left());
            assert!(row.rect.contains_rect(row.add));
            assert_eq!(row.bindings.len(), hotkeys.bindings().len());
            for binding in &row.bindings {
                assert!(binding.chip.contains_rect(binding.edit));
                assert!(binding.chip.contains_rect(binding.remove));
                assert_eq!(binding.edit.right(), binding.remove.left());
                assert!(binding.clip.right() <= row.add.left());
                assert!(binding.clip.height() <= 3.0 * m.control + 2.0 * m.gap + 0.1);
                assert!(binding.chip.right() <= row.add.left());
            }
        }
    }

    #[test]
    fn focus_defaults_work_while_editing_and_leave_ordinary_tab_unbound() {
        let mut hotkeys = Hotkeys::default();
        for (key, modifiers, command) in [
            (Key::F6, Modifiers::NONE, Command::NextFocusRegion),
            (Key::F6, Modifiers::SHIFT, Command::PreviousFocusRegion),
            (Key::F1, Modifiers::NONE, Command::KeyboardHelp),
        ] {
            assert_eq!(
                hotkeys.command_for(&KeyStroke::new(key, modifiers), false),
                Some(command)
            );
        }
        assert_eq!(
            hotkeys.command_for(&KeyStroke::new(Key::Tab, Modifiers::NONE), true),
            None
        );
        assert_eq!(
            hotkeys.command_for(&KeyStroke::new(Key::Tab, Modifiers::SHIFT), true),
            None
        );
        let ctx = egui::Context::default();
        let mut text = "editable".to_owned();
        let mut output = ctx.run_ui(raw(0.0, vec![]), |ui| {
            ui.add(egui::TextEdit::singleline(&mut text))
                .request_focus();
        });
        output.textures_delta.clear();
        assert_eq!(
            run(
                &mut hotkeys,
                &ctx,
                0.1,
                vec![key(Key::F6, Modifiers::NONE)],
                false
            ),
            [Command::NextFocusRegion]
        );
    }

    #[test]
    fn keyboard_default_migration_preserves_custom_commands_chords_and_prefixes() {
        let custom_help = Binding::single(Command::KeyboardHelp, Key::H, Modifiers::CTRL);
        let claimed_f1 = Binding::single(Command::Refresh, Key::F1, Modifiers::NONE);
        let prefix_f6 = Binding {
            command: Command::GoTo,
            sequence: vec![
                KeyStroke::new(Key::F6, Modifiers::NONE),
                KeyStroke::new(Key::G, Modifiers::NONE),
            ],
        };
        let mut bindings = vec![custom_help.clone(), claimed_f1.clone(), prefix_f6.clone()];
        assert!(Hotkeys::add_keyboard_defaults(&mut bindings));
        assert!(
            bindings.contains(&custom_help)
                && bindings.contains(&claimed_f1)
                && bindings.contains(&prefix_f6)
        );
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.command == Command::NextFocusRegion)
        );
        assert_eq!(
            bindings
                .iter()
                .filter(|binding| binding.command == Command::KeyboardHelp)
                .count(),
            1
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.command == Command::PreviousFocusRegion)
        );
        assert!(!Hotkeys::add_keyboard_defaults(&mut bindings));
    }

    #[test]
    fn file_history_defaults_dispatch_both_redo_chords_and_preserve_custom_migrations() {
        let defaults = Hotkeys::default();
        for (key, modifiers, command) in [
            (Key::Z, Modifiers::CTRL, Command::Undo),
            (Key::Y, Modifiers::CTRL, Command::Redo),
            (Key::Z, Modifiers::CTRL | Modifiers::SHIFT, Command::Redo),
        ] {
            assert_eq!(
                defaults.command_for(&KeyStroke::new(key, modifiers), true),
                Some(command)
            );
            assert_eq!(
                defaults.command_for(&KeyStroke::new(key, modifiers), false),
                None
            );
        }
        let custom = Binding::single(Command::Undo, Key::U, Modifiers::CTRL | Modifiers::ALT);
        let prefix = Binding {
            command: Command::GoTo,
            sequence: vec![
                KeyStroke::new(Key::Y, Modifiers::CTRL),
                KeyStroke::new(Key::G, Modifiers::NONE),
            ],
        };
        let mut saved = vec![custom.clone(), prefix.clone()];
        Hotkeys::add_keyboard_defaults(&mut saved);
        assert!(saved.contains(&custom) && saved.contains(&prefix));
        assert!(!saved.contains(&Binding::single(Command::Undo, Key::Z, Modifiers::CTRL)));
        assert!(!saved.iter().any(|binding| binding.command == Command::Redo));
        assert!(!Hotkeys::add_keyboard_defaults(&mut saved));
    }

    #[test]
    fn file_history_shortcuts_remain_available_to_the_focused_text_editor() {
        let mut hotkeys = Hotkeys::default();
        let ctx = egui::Context::default();
        let mut text = "editor owns its history".to_owned();
        let mut output = ctx.run_ui(raw(0.0, vec![]), |ui| {
            ui.add(egui::TextEdit::singleline(&mut text).id(egui::Id::new("history-editor")))
                .request_focus();
        });
        output.textures_delta.clear();
        for (frame, keycode, modifiers) in [
            (1, Key::Z, Modifiers::CTRL),
            (2, Key::Y, Modifiers::CTRL),
            (3, Key::Z, Modifiers::CTRL | Modifiers::SHIFT),
        ] {
            let mut output = ctx.run_ui(raw(frame as f64 * 0.1, vec![key(keycode, modifiers)]), |ui| {
                assert!(hotkeys.dispatch_many(ui.ctx(), true).is_empty());
                assert!(ui.input(|input| input.events.iter().any(|event| matches!(event, Event::Key { key, pressed: true, .. } if *key == keycode))));
                ui.add(egui::TextEdit::singleline(&mut text).id(egui::Id::new("history-editor")));
            });
            output.textures_delta.clear();
        }
    }

    #[test]
    fn legacy_panel_tab_migration_retires_only_exact_shipped_bindings_and_respects_conflicts() {
        let custom_tab = Binding::single(Command::Refresh, Key::Tab, Modifiers::NONE);
        let custom_sequence = Binding {
            command: Command::NextPanel,
            sequence: vec![
                KeyStroke::new(Key::Tab, Modifiers::NONE),
                KeyStroke::new(Key::N, Modifiers::NONE),
            ],
        };
        let occupied = Binding::single(
            Command::GoTo,
            Key::ArrowLeft,
            Modifiers::CTRL | Modifiers::ALT,
        );
        let mut bindings = vec![
            Binding::single(Command::NextPanel, Key::Tab, Modifiers::NONE),
            Binding::single(Command::PreviousPanel, Key::Tab, Modifiers::SHIFT),
            custom_tab.clone(),
            custom_sequence.clone(),
            occupied.clone(),
        ];
        Hotkeys::add_keyboard_defaults(&mut bindings);
        assert!(
            bindings.contains(&custom_tab)
                && bindings.contains(&custom_sequence)
                && bindings.contains(&occupied)
        );
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.command == Command::PreviousPanel)
        );
        assert!(!bindings.iter().any(|binding| binding.sequence.len() == 1
            && matches!(binding.command, Command::NextPanel | Command::PreviousPanel)
            && binding.sequence[0].key == "Tab"));
        let mut inherited = vec![Binding::single(
            Command::NextPanel,
            Key::Tab,
            Modifiers::NONE,
        )];
        Hotkeys::add_keyboard_defaults(&mut inherited);
        assert!(inherited.contains(&Binding::single(
            Command::NextPanel,
            Key::ArrowRight,
            Modifiers::CTRL | Modifiers::ALT
        )));
    }

    #[test]
    fn recorder_keeps_tab_and_review_activation_with_the_dialog_controls() {
        let ctx = egui::Context::default();
        let mut hotkeys = Hotkeys::default();
        hotkeys.record(Command::Parent);
        for (frame, events, expected) in [
            (0, vec![key(Key::Tab, Modifiers::NONE)], 0),
            (1, vec![key(Key::K, Modifiers::CTRL)], 1),
            (
                2,
                vec![
                    key(Key::Tab, Modifiers::SHIFT),
                    key(Key::Enter, Modifiers::NONE),
                ],
                1,
            ),
        ] {
            let mut output = ctx.run_ui(raw(frame as f64 / 60.0, events), |ui| {
                hotkeys.poll_recording(ui.ctx());
                assert_eq!(hotkeys.recording.as_ref().unwrap().strokes.len(), expected);
                if frame != 1 {
                    assert!(ui.input(|input| !input.events.is_empty()));
                }
            });
            output.textures_delta.clear();
        }
    }

    #[test]
    fn a_focused_composite_does_not_suppress_custom_non_text_shortcuts() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("hotkeys-non-text-focus-test");
        let mut hotkeys = Hotkeys::from_bindings(vec![Binding::single(
            Command::Refresh,
            Key::R,
            Modifiers::NONE,
        )]);
        let mut output = ctx.run_ui(raw(0.0, vec![]), |ui| {
            ui.interact(ui.max_rect(), id, egui::Sense::focusable_noninteractive())
                .request_focus();
        });
        output.textures_delta.clear();
        assert_eq!(
            run(
                &mut hotkeys,
                &ctx,
                0.1,
                vec![key(Key::R, Modifiers::NONE)],
                true
            ),
            [Command::Refresh]
        );
    }

    #[test]
    fn a_focused_button_keeps_space_for_activation_before_global_dispatch() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("hotkeys-focused-button-space");
        let theme = foldex_ui::Theme::default();
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 28.0));
        let mut hotkeys = Hotkeys::default();
        let mut output = ctx.run_ui(raw(0.0, vec![]), |ui| {
            foldex_ui::components::text_button_at(ui, rect, id, &theme, "Action", false)
                .request_focus();
        });
        output.textures_delta.clear();
        let mut clicked = false;
        let mut output = ctx.run_ui(
            raw(
                0.1,
                vec![key(Key::Space, Modifiers::NONE), Event::Text(" ".into())],
            ),
            |ui| {
                assert_eq!(hotkeys.dispatch(ui.ctx(), false), None);
                assert!(ui.input(|input| input.events.iter().any(|event| matches!(
                    event,
                    Event::Key {
                        key: Key::Space,
                        pressed: true,
                        ..
                    }
                ))));
                clicked |=
                    foldex_ui::components::text_button_at(ui, rect, id, &theme, "Action", false)
                        .clicked();
            },
        );
        output.textures_delta.clear();
        assert!(clicked, "Space must still activate the focused button");
        assert!(!hotkeys.queue.contains(&Command::ToggleInspector));
        assert_eq!(
            run(
                &mut hotkeys,
                &ctx,
                0.2,
                vec![key(Key::F6, Modifiers::NONE)],
                false
            ),
            [Command::NextFocusRegion]
        );
    }

    #[test]
    fn focused_collections_keep_plain_and_shift_keys_but_allow_function_and_modified_shortcuts() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("hotkeys-focused-collection-space");
        let mut bindings = vec![
            Binding::single(Command::ToggleInspector, Key::Space, Modifiers::NONE),
            Binding::single(Command::ToggleInspector, Key::Space, Modifiers::SHIFT),
            Binding::single(Command::Refresh, Key::R, Modifiers::NONE),
            Binding::single(Command::Options, Key::Comma, Modifiers::CTRL),
        ];
        Hotkeys::add_keyboard_defaults(&mut bindings);
        let mut hotkeys = Hotkeys::from_bindings(bindings);
        let mut output = ctx.run_ui(raw(0.0, vec![]), |ui| {
            ui.interact(ui.max_rect(), id, egui::Sense::focusable_noninteractive())
                .request_focus();
        });
        output.textures_delta.clear();
        for (frame, key_code, modifiers, expected) in [
            (1, Key::Space, Modifiers::NONE, None),
            (2, Key::Space, Modifiers::SHIFT, None),
            (3, Key::R, Modifiers::NONE, None),
            (4, Key::F6, Modifiers::NONE, Some(Command::NextFocusRegion)),
            (5, Key::F1, Modifiers::NONE, Some(Command::KeyboardHelp)),
            (6, Key::Comma, Modifiers::CTRL, Some(Command::Options)),
        ] {
            let mut output = ctx.run_ui(
                raw(
                    frame as f64 * 0.1,
                    vec![up(key_code, modifiers), key(key_code, modifiers)],
                ),
                |ui| {
                    assert_eq!(hotkeys.dispatch(ui.ctx(), false), expected);
                    if expected.is_none() {
                        assert!(
                            ui.input(|input| input.events.iter().any(|event| matches!(event,
                        Event::Key { key, pressed: true, .. } if *key == key_code)))
                        );
                    }
                    ui.interact(ui.max_rect(), id, egui::Sense::focusable_noninteractive());
                },
            );
            output.textures_delta.clear();
        }
        assert_eq!(
            run(
                &mut hotkeys,
                &ctx,
                0.7,
                vec![
                    up(Key::Space, Modifiers::NONE),
                    key(Key::Space, Modifiers::NONE)
                ],
                true
            ),
            [Command::ToggleInspector]
        );
    }

    #[test]
    fn delete_defaults_dispatch_exact_modifiers_and_consume_the_native_cut_alias() {
        for (modifiers, expected) in [
            (Modifiers::NONE, Some(Command::Recycle)),
            (Modifiers::SHIFT, Some(Command::DeletePermanent)),
            (Modifiers::CTRL, None),
            (Modifiers::ALT, None),
            (Modifiers::CTRL | Modifiers::SHIFT, None),
            (Modifiers::ALT | Modifiers::SHIFT, None),
        ] {
            let ctx = egui::Context::default();
            let mut hotkeys = Hotkeys::default();
            let mut events = vec![key(Key::Delete, modifiers)];
            if modifiers == Modifiers::SHIFT {
                events.push(Event::Cut);
            }
            let mut commands = Vec::new();
            let mut output = ctx.run_ui(raw(0.0, events), |ui| {
                commands.extend(hotkeys.dispatch_many(ui.ctx(), true));
                if expected.is_some() {
                    assert!(!ui.input(|input| input.events.iter().any(|event| {
                        matches!(
                            event,
                            Event::Key {
                                key: Key::Delete,
                                pressed: true,
                                ..
                            } | Event::Cut
                        )
                    })));
                }
            });
            output.textures_delta.clear();
            assert_eq!(commands, expected.into_iter().collect::<Vec<_>>());
        }
    }

    #[test]
    fn permanent_delete_migration_preserves_custom_bindings_and_claimed_shift_delete_prefixes() {
        let default = Binding::single(Command::DeletePermanent, Key::Delete, Modifiers::SHIFT);
        let mut empty = Vec::new();
        Hotkeys::add_keyboard_defaults(&mut empty);
        assert!(empty.contains(&default));
        assert!(!Hotkeys::add_keyboard_defaults(&mut empty));
        for existing in [
            Binding::single(
                Command::DeletePermanent,
                Key::D,
                Modifiers::CTRL | Modifiers::ALT,
            ),
            Binding::single(Command::Cut, Key::Delete, Modifiers::SHIFT),
            Binding {
                command: Command::GoTo,
                sequence: vec![
                    KeyStroke::new(Key::Delete, Modifiers::SHIFT),
                    KeyStroke::new(Key::G, Modifiers::NONE),
                ],
            },
        ] {
            let mut saved = vec![existing.clone()];
            Hotkeys::add_keyboard_defaults(&mut saved);
            assert!(saved.contains(&existing));
            assert!(!saved.contains(&default));
        }
        let mut rebound = Hotkeys::from_persisted_bindings(vec![
            Binding::single(Command::Recycle, Key::D, Modifiers::CTRL),
            Binding::single(
                Command::DeletePermanent,
                Key::D,
                Modifiers::CTRL | Modifiers::SHIFT,
            ),
        ]);
        assert_eq!(
            rebound.command_for(&KeyStroke::new(Key::Delete, Modifiers::NONE), true),
            None
        );
        assert_eq!(
            rebound.command_for(&KeyStroke::new(Key::Delete, Modifiers::SHIFT), true),
            None
        );
        assert_eq!(
            run(
                &mut rebound,
                &egui::Context::default(),
                0.0,
                vec![key(Key::D, Modifiers::CTRL | Modifiers::SHIFT)],
                true
            ),
            [Command::DeletePermanent]
        );
    }

    #[test]
    fn delete_keys_stay_with_text_editors_and_ime_instead_of_file_commands() {
        for modifiers in [Modifiers::NONE, Modifiers::SHIFT] {
            let ctx = egui::Context::default();
            let mut hotkeys = Hotkeys::default();
            let mut text = "draft".to_owned();
            let editor = egui::Id::new("delete-editor");
            let mut output = ctx.run_ui(raw(0.0, vec![]), |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).id(editor))
                    .request_focus();
            });
            output.textures_delta.clear();
            let mut output = ctx.run_ui(raw(0.1, vec![key(Key::Delete, modifiers)]), |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).id(editor));
                assert!(hotkeys.dispatch_many(ui.ctx(), true).is_empty());
                assert!(ui.input(|input| input.events.iter().any(|event| {
                    matches!(
                        event,
                        Event::Key {
                            key: Key::Delete,
                            pressed: true,
                            ..
                        }
                    )
                })));
            });
            output.textures_delta.clear();

            let ctx = egui::Context::default();
            let mut hotkeys = Hotkeys::default();
            let mut output = ctx.run_ui(
                raw(
                    0.0,
                    vec![
                        Event::Ime(egui::ImeEvent::Preedit {
                            text: "候補".into(),
                            active_range_chars: None,
                        }),
                        key(Key::Delete, modifiers),
                    ],
                ),
                |ui| {
                    assert!(hotkeys.dispatch_many(ui.ctx(), true).is_empty());
                    assert_eq!(
                        hotkeys.dispatch_stroke(
                            ui.ctx(),
                            &KeyStroke::new(Key::Delete, modifiers),
                            true
                        ),
                        None
                    );
                    assert!(ui.input(|input| input.events.iter().any(|event| {
                        matches!(
                            event,
                            Event::Key {
                                key: Key::Delete,
                                pressed: true,
                                ..
                            }
                        )
                    })));
                },
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn defaults_have_exact_modifiers_multiple_bindings_and_no_conflicts() {
        let hotkeys = Hotkeys::default();
        assert!(hotkeys.conflicts().is_empty());
        assert!(hotkeys.diagnostics().is_empty());
        for entry in COMMANDS.iter().filter(|entry| !entry.shortcut.is_empty()) {
            let stroke = KeyStroke::parse(entry.shortcut)
                .unwrap_or_else(|| panic!("Invalid default shortcut: {}", entry.shortcut));
            assert_eq!(hotkeys.command_for(&stroke, true), Some(entry.command));
        }
        assert_eq!(
            hotkeys.command_for(&KeyStroke::new(Key::S, Modifiers::CTRL), true),
            Some(Command::SplitBelow)
        );
        assert_eq!(
            hotkeys.command_for(
                &KeyStroke::new(Key::S, Modifiers::CTRL | Modifiers::SHIFT),
                true
            ),
            Some(Command::SplitRight)
        );
        assert_eq!(
            hotkeys.command_for(&KeyStroke::new(Key::N, Modifiers::CTRL), true),
            Some(Command::NewWindow)
        );
        assert_eq!(
            hotkeys.command_for(&KeyStroke::new(Key::F4, Modifiers::NONE), true),
            Some(Command::GoTo)
        );
        assert!(hotkeys.display_for(Command::GoTo).contains("F4"));
        assert!(hotkeys.display_for(Command::DuplicateTab).is_empty());
        let alias = Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            KeyStroke::new(Key::C, alias),
            KeyStroke::new(Key::C, Modifiers::CTRL)
        );
    }

    #[test]
    fn key_names_and_binding_json_roundtrip_without_egui_serde_features() {
        for &key in Key::ALL {
            if !modifier_key(key) {
                let stroke = KeyStroke::new(key, Modifiers::NONE);
                assert_eq!(stroke.compile().unwrap().key, key);
            }
        }
        let bindings = vec![
            sequence(Command::Parent, &[Key::G, Key::G]),
            Binding::single(Command::Options, Key::Comma, Modifiers::CTRL),
        ];
        let json = serde_json::to_string(&bindings).unwrap();
        assert!(json.contains("\"command\":\"parent\""));
        let decoded: Vec<Binding> = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, bindings);
        assert_eq!(
            Hotkeys::from_bindings(decoded).display_for(Command::Parent),
            "G, G"
        );
    }

    #[test]
    fn two_strokes_work_in_one_frame_and_do_not_leak_text() {
        let mut hotkeys =
            Hotkeys::from_bindings(vec![sequence(Command::Parent, &[Key::G, Key::G])]);
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let mut remaining = Vec::new();
        let events = vec![
            key(Key::G, Modifiers::NONE),
            Event::Text("g".into()),
            up(Key::G, Modifiers::NONE),
            key(Key::G, Modifiers::NONE),
            Event::Text("g".into()),
        ];
        let mut output = ctx.run_ui(raw(0.0, events), |ui| {
            commands.extend(hotkeys.dispatch_many(ui.ctx(), true));
            remaining = ui.ctx().input(|input| input.events.clone());
        });
        output.textures_delta.clear();
        assert_eq!(commands, [Command::Parent]);
        assert!(
            !remaining
                .iter()
                .any(|event| matches!(event, Event::Text(_)))
        );
    }

    #[test]
    fn prefixes_timeout_and_reprocess_a_mismatched_next_key() {
        let bindings = vec![
            sequence(Command::Back, &[Key::G]),
            sequence(Command::Parent, &[Key::G, Key::G]),
            sequence(Command::Refresh, &[Key::R]),
        ];
        let mut hotkeys = Hotkeys::from_bindings(bindings);
        let ctx = egui::Context::default();
        assert!(
            hotkeys
                .conflicts()
                .iter()
                .any(|conflict| conflict.kind == ConflictKind::Prefix)
        );
        assert!(
            run(
                &mut hotkeys,
                &ctx,
                0.0,
                vec![key(Key::G, Modifiers::NONE)],
                true
            )
            .is_empty()
        );
        assert_eq!(
            run(
                &mut hotkeys,
                &ctx,
                0.1,
                vec![key(Key::R, Modifiers::NONE)],
                true
            ),
            [Command::Back, Command::Refresh]
        );
        assert!(
            run(
                &mut hotkeys,
                &ctx,
                0.2,
                vec![up(Key::G, Modifiers::NONE), key(Key::G, Modifiers::NONE)],
                true
            )
            .is_empty()
        );
        assert_eq!(run(&mut hotkeys, &ctx, 1.1, vec![], true), [Command::Back]);
        assert!(hotkeys.pending.is_none());
    }

    #[test]
    fn conflict_assignment_requires_explicit_replacement_and_removal_is_respected() {
        let mut hotkeys = Hotkeys::default();
        let ctrl_c = KeyStroke::new(Key::C, Modifiers::CTRL);
        let binding = Binding::single(Command::GoTo, Key::C, Modifiers::CTRL);
        assert!(hotkeys.assign(binding.clone(), None, false).is_err());
        hotkeys.assign(binding, None, true).unwrap();
        assert_eq!(hotkeys.command_for(&ctrl_c, true), Some(Command::GoTo));
        let index = hotkeys
            .bindings
            .iter()
            .position(|binding| binding.sequence == vec![ctrl_c.clone()])
            .unwrap();
        assert!(hotkeys.remove(index));
        assert_eq!(hotkeys.command_for(&ctrl_c, true), None);
        hotkeys.reset_command(Command::Copy);
        assert_eq!(hotkeys.command_for(&ctrl_c, true), Some(Command::Copy));
    }

    #[test]
    fn file_gating_and_windows_native_key_deduplication_follow_rebinding() {
        let mut hotkeys = Hotkeys::from_bindings(vec![Binding::single(
            Command::GoTo,
            Key::C,
            Modifiers::CTRL,
        )]);
        let ctx = egui::Context::default();
        let stroke = KeyStroke::new(Key::C, Modifiers::CTRL);
        let mut commands = Vec::new();
        let mut output = ctx.run_ui(raw(0.0, vec![key(Key::C, Modifiers::CTRL)]), |ui| {
            if let Some(command) = hotkeys.dispatch_stroke(ui.ctx(), &stroke, false) {
                commands.push(command);
            }
            commands.extend(hotkeys.dispatch_many(ui.ctx(), false));
        });
        output.textures_delta.clear();
        assert_eq!(commands, [Command::GoTo]);
        let mut defaults = Hotkeys::default();
        assert_eq!(defaults.command_for(&stroke, false), None);
        assert!(
            run(
                &mut defaults,
                &egui::Context::default(),
                0.0,
                vec![key(Key::C, Modifiers::CTRL)],
                false
            )
            .is_empty()
        );
    }

    #[test]
    fn properties_default_and_saved_map_migration_use_unclaimed_alt_enter() {
        let defaults = Hotkeys::default();
        let properties = KeyStroke::new(Key::Enter, Modifiers::ALT);
        assert_eq!(
            defaults.command_for(&properties, true),
            Some(Command::Properties)
        );
        assert_eq!(defaults.command_for(&properties, false), None);
        assert_eq!(
            defaults.command_for(&KeyStroke::new(Key::E, Modifiers::CTRL), true),
            Some(Command::ExpandedSearch)
        );
        let mut saved = vec![Binding::single(Command::Refresh, Key::F5, Modifiers::NONE)];
        assert!(Hotkeys::add_keyboard_defaults(&mut saved));
        assert!(saved.contains(&Binding::single(
            Command::Properties,
            Key::Enter,
            Modifiers::ALT
        )));
        assert!(!Hotkeys::add_keyboard_defaults(&mut saved));
    }

    #[test]
    fn properties_migration_preserves_custom_commands_alt_enter_bindings_and_prefixes() {
        let custom_properties = Binding::single(
            Command::Properties,
            Key::P,
            Modifiers::CTRL | Modifiers::ALT,
        );
        let custom_alt_enter = Binding::single(Command::GoTo, Key::Enter, Modifiers::ALT);
        let custom_prefix = Binding {
            command: Command::GoTo,
            sequence: vec![
                KeyStroke::new(Key::Enter, Modifiers::ALT),
                KeyStroke::new(Key::G, Modifiers::NONE),
            ],
        };
        for existing in [custom_properties, custom_alt_enter, custom_prefix] {
            let mut saved = vec![existing.clone()];
            Hotkeys::add_keyboard_defaults(&mut saved);
            assert!(saved.contains(&existing));
            assert!(!saved.contains(&Binding::single(
                Command::Properties,
                Key::Enter,
                Modifiers::ALT
            )));
        }
    }

    #[test]
    fn properties_leave_native_and_logical_alt_enter_untouched_without_file_target_permission() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("properties-focused-control");
        let mut hotkeys = Hotkeys::default();
        let mut output = ctx.run_ui(raw(0.0, vec![]), |ui| {
            ui.interact(ui.max_rect(), id, egui::Sense::focusable_noninteractive())
                .request_focus();
        });
        output.textures_delta.clear();
        let mut output = ctx.run_ui(raw(0.1, vec![key(Key::Enter, Modifiers::ALT)]), |ui| {
            assert_eq!(hotkeys.dispatch_stroke(ui.ctx(), &KeyStroke::new(Key::Enter, Modifiers::ALT), false), None);
            assert_eq!(hotkeys.dispatch(ui.ctx(), false), None);
            assert!(ui.input(|input| input.events.iter().any(|event| matches!(event,
                Event::Key { key: Key::Enter, pressed: true, modifiers, .. } if modifiers.matches_exact(Modifiers::ALT)))));
            ui.interact(ui.max_rect(), id, egui::Sense::focusable_noninteractive());
        });
        output.textures_delta.clear();
        assert_eq!(
            run(
                &mut hotkeys,
                &ctx,
                0.2,
                vec![
                    up(Key::Enter, Modifiers::ALT),
                    key(Key::Enter, Modifiers::ALT)
                ],
                true
            ),
            [Command::Properties]
        );
    }

    #[test]
    fn focused_text_owns_clipboard_even_when_ctrl_c_is_rebound_to_a_global_command() {
        let ctx = egui::Context::default();
        let mut text = String::from("Text owns copy");
        let mut hotkeys = Hotkeys::from_bindings(vec![Binding::single(
            Command::GoTo,
            Key::C,
            Modifiers::CTRL,
        )]);
        let mut first = ctx.run_ui(raw(0.0, vec![]), |ui| {
            ui.add(egui::TextEdit::singleline(&mut text).id(egui::Id::new("owned-text")))
                .request_focus();
        });
        first.textures_delta.clear();
        let mut result = None;
        let mut second = ctx.run_ui(
            raw(0.1, vec![key(Key::C, Modifiers::CTRL), Event::Copy]),
            |ui| {
                result = hotkeys.dispatch_stroke(
                    ui.ctx(),
                    &KeyStroke::new(Key::C, Modifiers::CTRL),
                    true,
                );
                ui.add(egui::TextEdit::singleline(&mut text).id(egui::Id::new("owned-text")));
            },
        );
        second.textures_delta.clear();
        assert_eq!(result, None);
        assert_eq!(text, "Text owns copy");
        assert!(protected_stroke(
            Stroke::event(Key::C, Modifiers::CTRL),
            false,
            true
        ));
    }

    #[test]
    fn recording_deduplicates_native_logical_and_clipboard_events() {
        let mut hotkeys = Hotkeys::default();
        hotkeys.record(Command::Parent);
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            raw(0.0, vec![key(Key::C, Modifiers::CTRL), Event::Copy]),
            |ui| {
                hotkeys.dispatch_stroke(ui.ctx(), &KeyStroke::new(Key::C, Modifiers::CTRL), false);
                hotkeys.editor_ui(ui);
            },
        );
        output.textures_delta.clear();
        let recording = hotkeys.recording.as_ref().unwrap();
        assert_eq!(recording.strokes.len(), 1);
        assert_eq!(recording.strokes[0].key, "C");
        let mut next = ctx.run_ui(
            raw(
                0.1,
                vec![key(Key::G, Modifiers::NONE), Event::Text("g".into())],
            ),
            |ui| {
                hotkeys.editor_ui(ui);
            },
        );
        next.textures_delta.clear();
        assert_eq!(
            hotkeys
                .recording
                .as_ref()
                .unwrap()
                .strokes
                .iter()
                .map(|stroke| stroke.key.as_str())
                .collect::<Vec<_>>(),
            ["C", "G"]
        );
    }

    #[test]
    fn invalid_sequences_are_reported_and_dispatch_work_is_bounded() {
        let bad = Binding {
            command: Command::Back,
            sequence: vec![KeyStroke {
                key: "NotAKey".into(),
                ctrl: false,
                alt: false,
                shift: false,
                cmd: false,
            }],
        };
        let hotkeys = Hotkeys::from_bindings(vec![bad]);
        assert_eq!(hotkeys.diagnostics().len(), 1);
        assert!(hotkeys.compiled.is_empty());
        let mut hotkeys = Hotkeys::from_bindings(vec![sequence(Command::Refresh, &[Key::F5])]);
        let events = (0..100)
            .flat_map(|_| [key(Key::F5, Modifiers::NONE), up(Key::F5, Modifiers::NONE)])
            .collect();
        let commands = run(&mut hotkeys, &egui::Context::default(), 0.0, events, true);
        assert!(commands.len() <= MAX_QUEUED);
    }

    #[test]
    fn recording_clears_previous_native_dedupe_when_regular_dispatch_is_paused() {
        let mut hotkeys = Hotkeys::default();
        hotkeys.record(Command::Parent);
        let ctx = egui::Context::default();
        let mut first = ctx.run_ui(raw(0.0, vec![Event::Copy]), |ui| {
            hotkeys.dispatch_stroke(ui.ctx(), &KeyStroke::new(Key::C, Modifiers::CTRL), false);
            hotkeys.editor_ui(ui);
        });
        first.textures_delta.clear();
        assert_eq!(hotkeys.recording.as_ref().unwrap().strokes.len(), 1);
        let mut second = ctx.run_ui(
            raw(
                0.1,
                vec![up(Key::C, Modifiers::CTRL), key(Key::C, Modifiers::CTRL)],
            ),
            |ui| {
                hotkeys.editor_ui(ui);
            },
        );
        second.textures_delta.clear();
        assert_eq!(hotkeys.recording.as_ref().unwrap().strokes.len(), 2);
    }

    #[test]
    fn recording_keeps_logical_modifiers_when_clipboard_event_arrives_after_release() {
        let mut hotkeys = Hotkeys::default();
        hotkeys.record(Command::Parent);
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            raw(
                0.0,
                vec![key(Key::C, Modifiers::CTRL | Modifiers::SHIFT), Event::Copy],
            ),
            |ui| {
                hotkeys.editor_ui(ui);
            },
        );
        output.textures_delta.clear();
        let recording = hotkeys.recording.as_ref().unwrap();
        assert_eq!(recording.strokes.len(), 1);
        assert!(recording.strokes[0].shift);
    }

    #[test]
    fn native_rebound_clipboard_event_cannot_reach_new_popup_input() {
        let mut hotkeys = Hotkeys::from_bindings(vec![Binding::single(
            Command::GoTo,
            Key::V,
            Modifiers::CTRL,
        )]);
        let ctx = egui::Context::default();
        let mut result = None;
        let mut remaining = Vec::new();
        let mut output = ctx.run_ui(
            raw(0.0, vec![Event::Paste("clipboard text".into())]),
            |ui| {
                result = hotkeys.dispatch_stroke(
                    ui.ctx(),
                    &KeyStroke::new(Key::V, Modifiers::CTRL),
                    true,
                );
                remaining = ui.ctx().input(|input| input.events.clone());
            },
        );
        output.textures_delta.clear();
        assert_eq!(result, Some(Command::GoTo));
        assert!(
            !remaining
                .iter()
                .any(|event| matches!(event, Event::Paste(_)))
        );
    }

    #[test]
    fn separate_native_presses_of_same_chord_in_one_frame_are_not_collapsed() {
        let mut hotkeys = Hotkeys::from_bindings(vec![Binding {
            command: Command::Parent,
            sequence: vec![KeyStroke::new(Key::C, Modifiers::CTRL); 2],
        }]);
        let ctx = egui::Context::default();
        let mut results = Vec::new();
        let mut output = ctx.run_ui(raw(0.0, vec![Event::Copy, Event::Copy]), |ui| {
            for _ in 0..2 {
                if let Some(command) = hotkeys.dispatch_stroke(
                    ui.ctx(),
                    &KeyStroke::new(Key::C, Modifiers::CTRL),
                    true,
                ) {
                    results.push(command);
                }
            }
            results.extend(hotkeys.dispatch_many(ui.ctx(), true));
        });
        output.textures_delta.clear();
        assert_eq!(results, [Command::Parent]);
    }

    #[test]
    fn dispatch_yields_before_consuming_keys_that_belong_to_new_popup() {
        let mut hotkeys = Hotkeys::from_bindings(vec![
            Binding::single(Command::GoTo, Key::P, Modifiers::CTRL),
            Binding::single(Command::Refresh, Key::G, Modifiers::NONE),
        ]);
        let ctx = egui::Context::default();
        let mut command = None;
        let mut remaining = Vec::new();
        let mut output = ctx.run_ui(
            raw(
                0.0,
                vec![
                    key(Key::P, Modifiers::CTRL),
                    key(Key::G, Modifiers::NONE),
                    Event::Text("g".into()),
                ],
            ),
            |ui| {
                command = hotkeys.dispatch(ui.ctx(), true);
                hotkeys.cancel_pending(); // The app opens GoTo and yields input ownership.
                remaining = ui.ctx().input(|input| input.events.clone());
            },
        );
        output.textures_delta.clear();
        assert_eq!(command, Some(Command::GoTo));
        assert!(
            remaining
                .iter()
                .any(|event| matches!(event, Event::Text(text) if text == "g"))
        );
        assert!(remaining.iter().any(|event| matches!(
            event,
            Event::Key {
                key: Key::G,
                pressed: true,
                ..
            }
        )));
    }

    #[test]
    fn prefix_fallback_yields_the_mismatching_key_to_resulting_popup() {
        let mut hotkeys = Hotkeys::from_bindings(vec![
            sequence(Command::GoTo, &[Key::G]),
            sequence(Command::Parent, &[Key::G, Key::G]),
            sequence(Command::Refresh, &[Key::R]),
        ]);
        let ctx = egui::Context::default();
        assert!(
            run(
                &mut hotkeys,
                &ctx,
                0.0,
                vec![key(Key::G, Modifiers::NONE)],
                true
            )
            .is_empty()
        );
        let mut command = None;
        let mut remaining = Vec::new();
        let mut output = ctx.run_ui(
            raw(
                0.1,
                vec![key(Key::R, Modifiers::NONE), Event::Text("r".into())],
            ),
            |ui| {
                command = hotkeys.dispatch(ui.ctx(), true);
                remaining = ui.ctx().input(|input| input.events.clone());
            },
        );
        output.textures_delta.clear();
        assert_eq!(command, Some(Command::GoTo));
        assert!(
            remaining
                .iter()
                .any(|event| matches!(event, Event::Text(text) if text == "r"))
        );
        assert!(hotkeys.queue.is_empty());
    }

    #[test]
    fn text_focus_cancels_plain_prefix_before_timeout_and_recording_can_be_cancelled() {
        let mut hotkeys = Hotkeys::from_bindings(vec![
            sequence(Command::Back, &[Key::G]),
            sequence(Command::Parent, &[Key::G, Key::G]),
            sequence(Command::Refresh, &[Key::R]),
        ]);
        let ctx = egui::Context::default();
        assert!(
            run(
                &mut hotkeys,
                &ctx,
                0.0,
                vec![key(Key::G, Modifiers::NONE)],
                true
            )
            .is_empty()
        );
        let input_id = egui::Id::new("new-text-owner");
        let mut command = None;
        let mut output = ctx.run_ui(raw(1.0, vec![]), |ui| {
            // The editor is about to be drawn: focus is already requested,
            // but TextEditState does not exist until its widget is created.
            ui.memory_mut(|memory| memory.request_focus(input_id));
            command = hotkeys.dispatch(ui.ctx(), true);
            ui.memory_mut(|memory| memory.surrender_focus(input_id));
        });
        output.textures_delta.clear();
        assert_eq!(command, None);
        assert!(hotkeys.pending.is_none());
        hotkeys.record(Command::Back);
        assert!(hotkeys.is_recording());
        hotkeys.cancel_recording();
        assert!(!hotkeys.is_recording());
        assert_eq!(
            run(
                &mut hotkeys,
                &ctx,
                1.1,
                vec![key(Key::R, Modifiers::NONE)],
                true
            ),
            [Command::Refresh]
        );
    }

    #[test]
    fn pending_plain_sequence_completes_in_the_same_non_text_composite() {
        let mut hotkeys = Hotkeys::from_bindings(vec![
            sequence(Command::Back, &[Key::G]),
            sequence(Command::Parent, &[Key::G, Key::G]),
        ]);
        let ctx = egui::Context::default();
        let id = egui::Id::new("sequence-composite-owner");
        for frame in 0..2 {
            let mut command = None;
            let mut output = ctx.run_ui(
                raw(
                    frame as f64 * 0.1,
                    if frame == 0 {
                        vec![key(Key::G, Modifiers::NONE)]
                    } else {
                        vec![up(Key::G, Modifiers::NONE), key(Key::G, Modifiers::NONE)]
                    },
                ),
                |ui| {
                    let owner =
                        ui.interact(ui.max_rect(), id, egui::Sense::focusable_noninteractive());
                    if frame == 0 {
                        owner.request_focus();
                    }
                    assert!(!ui.ctx().text_edit_focused());
                    command = hotkeys.dispatch(ui.ctx(), true);
                },
            );
            output.textures_delta.clear();
            assert_eq!(
                command,
                if frame == 0 {
                    None
                } else {
                    Some(Command::Parent)
                }
            );
        }
        assert!(hotkeys.pending.is_none());
    }

    #[test]
    fn a_real_text_editor_cancels_a_prefix_even_when_reusing_the_same_focus_id() {
        let mut hotkeys = Hotkeys::from_bindings(vec![
            sequence(Command::Back, &[Key::G]),
            sequence(Command::Parent, &[Key::G, Key::G]),
        ]);
        let ctx = egui::Context::default();
        let id = egui::Id::new("sequence-owner-becomes-text");
        let mut output = ctx.run_ui(raw(0.0, vec![key(Key::G, Modifiers::NONE)]), |ui| {
            ui.interact(ui.max_rect(), id, egui::Sense::focusable_noninteractive())
                .request_focus();
            assert_eq!(hotkeys.dispatch(ui.ctx(), true), None);
        });
        output.textures_delta.clear();
        assert!(hotkeys.pending.is_some());
        let mut text = "draft".to_owned();
        let mut command = None;
        let mut output = ctx.run_ui(raw(1.0, vec![]), |ui| {
            ui.add(egui::TextEdit::singleline(&mut text).id(id))
                .request_focus();
            assert!(ui.ctx().text_edit_focused());
            command = hotkeys.dispatch(ui.ctx(), true);
        });
        output.textures_delta.clear();
        assert_eq!(command, None);
        assert!(hotkeys.pending.is_none());
        assert_eq!(text, "draft");
    }

    #[test]
    fn clipboard_repeat_does_not_complete_a_recorded_two_key_sequence() {
        let mut hotkeys = Hotkeys::default();
        hotkeys.record(Command::Parent);
        let ctx = egui::Context::default();
        let mut first = ctx.run_ui(
            raw(0.0, vec![key(Key::C, Modifiers::CTRL), Event::Copy]),
            |ui| {
                hotkeys.editor_ui(ui);
            },
        );
        first.textures_delta.clear();
        let mut repeat = key(Key::C, Modifiers::CTRL);
        if let Event::Key { repeat, .. } = &mut repeat {
            *repeat = true;
        }
        let mut second = ctx.run_ui(raw(0.1, vec![repeat, Event::Copy]), |ui| {
            hotkeys.editor_ui(ui);
        });
        second.textures_delta.clear();
        assert_eq!(hotkeys.recording.as_ref().unwrap().strokes.len(), 1);
    }

    #[test]
    fn clipboard_alias_recording_keeps_the_original_chord_without_double_capture() {
        for (keycode, modifiers, semantic) in [
            (
                Key::Insert,
                Modifiers::SHIFT,
                Event::Paste("fixture".into()),
            ),
            (Key::Insert, Modifiers::CTRL | Modifiers::SHIFT, Event::Copy),
            (Key::Delete, Modifiers::SHIFT, Event::Cut),
        ] {
            let mut hotkeys = Hotkeys::default();
            hotkeys.record(Command::Parent);
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(raw(0.0, vec![key(keycode, modifiers), semantic]), |ui| {
                hotkeys.editor_ui(ui);
            });
            output.textures_delta.clear();
            assert_eq!(
                hotkeys.recording.as_ref().unwrap().strokes,
                [KeyStroke::new(keycode, modifiers)]
            );
        }
    }
}
