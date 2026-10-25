//! Preserve physical event order when winit converts clipboard chords into semantic events.
//! Stable file-panel clipboard presses bypass winit's synchronous text clipboard read.
use egui::{Event, Key, Modifiers, PointerButton};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug)]
pub struct Chord {
    pub key: Key,
    pub modifiers: Modifiers,
    pub repeat: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Clipboard {
    Copy,
    Cut,
    Paste,
}
impl Chord {
    fn clipboard(self) -> Option<Clipboard> {
        if self.modifiers.ctrl && self.key == Key::C
            || self.modifiers.ctrl && self.key == Key::Insert
        {
            Some(Clipboard::Copy)
        } else if self.modifiers.ctrl && self.key == Key::X
            || self.modifiers.shift && self.key == Key::Delete
        {
            Some(Clipboard::Cut)
        } else if self.modifiers.ctrl && self.key == Key::V
            || self.modifiers.shift && self.key == Key::Insert
        {
            Some(Clipboard::Paste)
        } else {
            None
        }
    }
    fn event(self) -> Event {
        Event::Key {
            key: self.key,
            physical_key: None,
            pressed: true,
            repeat: self.repeat,
            modifiers: self.modifiers,
        }
    }
}
#[derive(Clone, Copy)]
enum Marker {
    Key(Chord),
    Context(Chord),
    Pointer(PointerButton),
}
impl Marker {
    fn clipboard(self) -> Option<Chord> {
        if let Self::Key(chord) = self {
            chord.clipboard().map(|_| chord)
        } else {
            None
        }
    }
    fn matches(self, event: &Event) -> bool {
        match (self, event) {
            (
                Self::Key(chord) | Self::Context(chord),
                Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                },
            ) => {
                (chord.key == physical_key.unwrap_or(*key) || chord.key == *key)
                    && same_modifiers(chord.modifiers, *modifiers)
            }
            (Self::Key(chord), other) => {
                semantic(other).is_some_and(|s| chord.clipboard() == Some(s))
            }
            (
                Self::Pointer(a),
                Event::PointerButton {
                    button: b,
                    pressed: true,
                    ..
                },
            ) => a == *b,
            _ => false,
        }
    }
}
pub struct Batch {
    entries: [Option<Marker>; 64],
    len: usize,
    passthrough: bool,
    overflow: bool,
    intercepted: Option<Chord>,
}
impl Batch {
    const fn new() -> Self {
        Self {
            entries: [None; 64],
            len: 0,
            passthrough: false,
            overflow: false,
            intercepted: None,
        }
    }
    fn record(&mut self, chord: Chord, file_mode: bool) -> bool {
        let intercept =
            file_mode && !self.passthrough && !self.overflow && chord.clipboard().is_some();
        if self.len == self.entries.len() {
            self.overflow = true;
            return false;
        }
        self.entries[self.len] = Some(Marker::Key(chord));
        self.len += 1;
        if intercept {
            self.intercepted = Some(chord);
        }
        // A preceding shortcut may open a text field before this frame is painted.
        self.passthrough = true;
        intercept
    }
    fn pointer(&mut self, button: PointerButton) {
        self.passthrough = true;
        if self.len == self.entries.len() {
            self.overflow = true;
            return;
        }
        self.entries[self.len] = Some(Marker::Pointer(button));
        self.len += 1;
    }
}
static QUEUE: Mutex<Batch> = Mutex::new(Batch::new());
static FILE_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_file_mode(enabled: bool) {
    FILE_MODE.store(enabled, Ordering::Relaxed);
}
pub fn mark_pointer_input() {
    if let Ok(mut queue) = QUEUE.try_lock() {
        queue.passthrough = true;
    }
}
pub fn record_pointer(button: PointerButton) {
    if let Ok(mut queue) = QUEUE.try_lock() {
        queue.pointer(button);
    }
}
pub fn record(virtual_key: u32, modifiers: Modifiers, repeat: bool) -> bool {
    // egui has no Menu key variant. Normalize the dedicated Windows key to the
    // same context action as Shift+F10 before any control chooses its owner.
    if virtual_key == 0x5d && modifiers == Modifiers::NONE {
        if let Ok(mut queue) = QUEUE.try_lock() {
            if !repeat {
                let at = queue.len;
                let chord = Chord {
                    key: Key::F10,
                    modifiers: Modifiers::SHIFT,
                    repeat: false,
                };
                queue.record(chord, false);
                if queue.len > at {
                    queue.entries[at] = Some(Marker::Context(chord));
                }
            } else {
                queue.passthrough = true;
            }
        }
        return true;
    }
    if matches!(virtual_key, 0x10..=0x12 | 0xa0..=0xa5 | 0x5b | 0x5c) {
        return false;
    }
    let Some(key) = physical_key(virtual_key) else {
        mark_pointer_input();
        return false;
    };
    QUEUE
        .try_lock()
        .map(|mut q| {
            q.record(
                Chord {
                    key,
                    modifiers,
                    repeat,
                },
                FILE_MODE.load(Ordering::Relaxed),
            )
        })
        .unwrap_or(false)
}
pub fn inject_context_menu(input: &mut egui::RawInput) {
    let markers = QUEUE.try_lock().ok().and_then(|mut queue| {
        if !queue.entries[..queue.len]
            .iter()
            .any(|entry| matches!(entry, Some(Marker::Context(_))))
        {
            return None;
        }
        let markers: Vec<_> = queue.entries[..queue.len]
            .iter()
            .flatten()
            .copied()
            .collect();
        let len = queue.len;
        for entry in &mut queue.entries[..len] {
            if let Some(Marker::Context(chord)) = *entry {
                *entry = Some(Marker::Key(chord));
            }
        }
        Some(markers)
    });
    if let Some(markers) = markers.filter(|_| input.focused) {
        inject_menu_markers(&mut input.events, &markers);
    }
}

fn inject_menu_markers(events: &mut Vec<Event>, markers: &[Marker]) {
    let mut output = Vec::with_capacity(events.len() + 4);
    let mut next = 0;
    let append = |output: &mut Vec<Event>, marker: Marker| {
        if let Marker::Context(chord) = marker {
            output.push(chord.event());
            output.push(Event::Key {
                key: chord.key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: chord.modifiers,
            });
        }
    };
    for event in std::mem::take(events) {
        if let Some(offset) = markers[next..]
            .iter()
            .position(|marker| marker.matches(&event))
        {
            let found = next + offset;
            for &marker in &markers[next..found] {
                append(&mut output, marker);
            }
            next = found + 1;
        }
        output.push(event);
    }
    for &marker in &markers[next..] {
        append(&mut output, marker);
    }
    *events = output;
}
pub fn take() -> Batch {
    QUEUE
        .try_lock()
        .map(|mut q| std::mem::replace(&mut *q, Batch::new()))
        .unwrap_or_else(|_| Batch::new())
}
fn semantic(event: &Event) -> Option<Clipboard> {
    match event {
        Event::Copy => Some(Clipboard::Copy),
        Event::Cut => Some(Clipboard::Cut),
        Event::Paste(_) => Some(Clipboard::Paste),
        _ => None,
    }
}
fn same_modifiers(a: Modifiers, b: Modifiers) -> bool {
    a.ctrl == b.ctrl && a.alt == b.alt && a.shift == b.shift && a.mac_cmd == b.mac_cmd
}

/// Run after ordinary controls claim pointer/text focus, before command dispatch.
/// Raw clipboard payloads are moved, never copied. Only missing key events are added.
pub fn merge(events: &mut Vec<Event>, batch: Batch) {
    if batch.overflow {
        // Only the first non-modifier press can be intercepted: any earlier
        // pointer/key sets passthrough. It precedes the retained raw input and
        // must survive even when the rest of the marker stream overflows.
        if let Some(chord) = batch.intercepted {
            events.insert(0, chord.event());
        }
        return;
    }
    if !batch.entries[..batch.len]
        .iter()
        .flatten()
        .any(|c| c.clipboard().is_some())
    {
        return;
    }
    let markers: Vec<_> = batch.entries[..batch.len]
        .iter()
        .flatten()
        .copied()
        .collect();
    let mut output = Vec::with_capacity(
        events.len() + markers.iter().filter(|m| m.clipboard().is_some()).count(),
    );
    let mut next = 0;
    for event in std::mem::take(events) {
        let matched = markers[next..].iter().position(|m| m.matches(&event));
        if let Some(offset) = matched {
            let found = next + offset;
            for marker in &markers[next..found] {
                if let Some(chord) = marker.clipboard() {
                    output.push(chord.event());
                }
            }
            if semantic(&event).is_some() {
                if let Some(chord) = markers[found].clipboard() {
                    output.push(chord.event());
                }
            }
            next = found + 1;
        }
        output.push(event);
    }
    for marker in &markers[next..] {
        if let Some(chord) = marker.clipboard() {
            output.push(chord.event());
        }
    }
    *events = output;
}

/// Newly opened editors need semantic clipboard events, not duplicate synthesized keys.
pub fn strip_synthetic(events: &mut Vec<Event>) {
    events.retain(|event| !matches!(event, Event::Key { key, physical_key: None, pressed: true, modifiers, repeat } if Chord { key: *key, modifiers: *modifiers, repeat: *repeat }.clipboard().is_some()));
}

pub(crate) fn physical_key(vk: u32) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] = [
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    const FUNCTIONS: [Key; 24] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
    ];
    Some(match vk {
        0x41..=0x5a => LETTERS[(vk - 0x41) as usize],
        0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
        0x60..=0x69 => DIGITS[(vk - 0x60) as usize],
        0x70..=0x87 => FUNCTIONS[(vk - 0x70) as usize],
        0x08 => Key::Backspace,
        0x09 => Key::Tab,
        0x0d => Key::Enter,
        0x1b => Key::Escape,
        0x20 => Key::Space,
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x23 => Key::End,
        0x24 => Key::Home,
        0x25 => Key::ArrowLeft,
        0x26 => Key::ArrowUp,
        0x27 => Key::ArrowRight,
        0x28 => Key::ArrowDown,
        0x2d => Key::Insert,
        0x2e => Key::Delete,
        0xba => Key::Semicolon,
        0xbb => Key::Equals,
        0xbc => Key::Comma,
        0xbd => Key::Minus,
        0xbe => Key::Period,
        0xbf => Key::Slash,
        0xc0 => Key::Backtick,
        0xdb => Key::OpenBracket,
        0xdc | 0xe2 => Key::Backslash,
        0xdd => Key::CloseBracket,
        0xde => Key::Quote,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn chord(key: Key) -> Chord {
        Chord {
            key,
            modifiers: Modifiers::CTRL,
            repeat: false,
        }
    }
    fn real(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL,
        }
    }
    fn keys(events: &[Event]) -> Vec<Key> {
        events
            .iter()
            .filter_map(|e| {
                if let Event::Key {
                    key, pressed: true, ..
                } = e
                {
                    Some(*key)
                } else {
                    None
                }
            })
            .collect()
    }
    #[test]
    fn dedicated_menu_key_becomes_one_context_chord_without_a_stuck_function_key() {
        assert!(record(0x5d, Modifiers::NONE, false));
        let mut input = egui::RawInput {
            focused: true,
            ..Default::default()
        };
        inject_context_menu(&mut input);
        assert!(matches!(input.events.as_slice(), [
            Event::Key { key: Key::F10, modifiers, pressed: true, .. },
            Event::Key { key: Key::F10, pressed: false, .. },
        ] if *modifiers == Modifiers::SHIFT));
        input.events.clear();
        inject_context_menu(&mut input);
        assert!(input.events.is_empty());
    }

    #[test]
    fn menu_normalization_preserves_order_and_prevents_later_clipboard_interception() {
        let context = Chord {
            key: Key::F10,
            modifiers: Modifiers::SHIFT,
            repeat: false,
        };
        let mut batch = Batch::new();
        batch.record(context, false);
        batch.entries[0] = Some(Marker::Context(context));
        assert!(!batch.record(chord(Key::C), true));
        let mut events = vec![Event::Copy];
        let markers: Vec<_> = batch.entries[..batch.len]
            .iter()
            .flatten()
            .copied()
            .collect();
        inject_menu_markers(&mut events, &markers);
        assert_eq!(keys(&events), vec![Key::F10]);
        assert!(matches!(events.last(), Some(Event::Copy)));

        let mut events = vec![real(Key::F), Event::Text("f".into())];
        inject_menu_markers(
            &mut events,
            &[Marker::Key(chord(Key::F)), Marker::Context(context)],
        );
        assert_eq!(keys(&events), vec![Key::F, Key::F10]);
        assert!(matches!(&events[1], Event::Text(text) if text == "f"));
    }

    #[test]
    fn mixed_logical_and_semantic_chords_preserve_physical_order() {
        let mut batch = Batch::new();
        batch.record(chord(Key::G), false);
        batch.record(chord(Key::C), false);
        let mut events = vec![real(Key::G), Event::Copy];
        merge(&mut events, batch);
        assert_eq!(keys(&events), vec![Key::G, Key::C]);
        assert!(matches!(events.last(), Some(Event::Copy)));
    }
    #[test]
    fn file_paste_without_text_is_inserted_between_surrounding_logical_keys() {
        let mut batch = Batch::new();
        for key in [Key::G, Key::V, Key::P] {
            batch.record(chord(key), false);
        }
        let mut events = vec![real(Key::G), real(Key::P)];
        merge(&mut events, batch);
        assert_eq!(keys(&events), vec![Key::G, Key::V, Key::P]);
    }
    #[test]
    fn payload_survives_popup_handoff_without_duplicate_delete_or_insert_keys() {
        let mut batch = Batch::new();
        batch.record(chord(Key::P), false);
        batch.record(
            Chord {
                key: Key::Insert,
                modifiers: Modifiers::SHIFT,
                repeat: false,
            },
            false,
        );
        let mut events = vec![real(Key::P), Event::Paste("generated fixture path".into())];
        merge(&mut events, batch);
        strip_synthetic(&mut events);
        assert_eq!(keys(&events), vec![Key::P]);
        assert!(matches!(events.last(),Some(Event::Paste(s)) if s=="generated fixture path"));
    }
    #[test]
    fn synchronous_text_clipboard_is_bypassed_only_in_an_unambiguous_file_context() {
        let mut batch = Batch::new();
        assert!(batch.record(chord(Key::V), true));
        let mut batch = Batch::new();
        batch.record(chord(Key::P), true);
        assert!(!batch.record(chord(Key::V), true));
        let mut batch = Batch::new();
        batch.passthrough = true;
        assert!(!batch.record(chord(Key::V), true));
        let mut batch = Batch::new();
        assert!(!batch.record(chord(Key::V), false));
    }
    #[test]
    fn overflow_keeps_raw_input_intact_instead_of_guessing_chord_order() {
        let mut batch = Batch::new();
        for _ in 0..65 {
            batch.record(chord(Key::C), false);
        }
        let mut events = vec![real(Key::G), Event::Copy];
        merge(&mut events, batch);
        assert_eq!(keys(&events), vec![Key::G]);
    }
    #[test]
    fn intercepted_clipboard_key_keeps_its_position_before_a_later_pointer_press() {
        let mut batch = Batch::new();
        assert!(batch.record(chord(Key::V), true));
        batch.pointer(PointerButton::Primary);
        let mut events = vec![Event::PointerButton {
            pos: egui::pos2(50.0, 50.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }];
        merge(&mut events, batch);
        assert!(matches!(events[0], Event::Key { key: Key::V, .. }));
        assert!(matches!(events[1], Event::PointerButton { .. }));
    }
    #[test]
    fn overflow_recovers_the_clipboard_press_already_intercepted_from_winit() {
        let mut batch = Batch::new();
        assert!(batch.record(chord(Key::V), true));
        for _ in 0..65 {
            assert!(!batch.record(chord(Key::G), true));
        }
        let mut events = vec![real(Key::G), Event::Text("fixture".into())];
        merge(&mut events, batch);
        assert_eq!(keys(&events), [Key::V, Key::G]);
        assert!(matches!(events.last(), Some(Event::Text(text)) if text == "fixture"));
    }
}
