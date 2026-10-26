//! Temporary foreground-only keyboard capture. Windows must see neither half
//! of a recorded press, otherwise Win/Alt can still activate shell shortcuts.
#[cfg(any(windows, test))]
use egui::Key;
use egui::{Event, Modifiers};
#[cfg(any(windows, test))]
use std::collections::VecDeque;

#[cfg(any(windows, test))]
const CAPACITY: usize = 512;
#[cfg(windows)]
pub(crate) const WAKE_MESSAGE: u32 = 0x8000 + 0x466;

#[derive(Default)]
pub(super) struct Input {
    pub events: Vec<Event>,
    pub lost_focus: bool,
    pub overflow: bool,
    pub unsupported: bool,
    pub cancelled: bool,
    pub modifiers: Option<Modifiers>,
    pub restore_modifiers: Option<Modifiers>,
}

#[cfg(any(windows, test))]
struct Capture {
    down: [bool; 256],
    owned: [bool; 256],
    /// Win/Alt keys already held when capture started; their release must not
    /// activate the Start menu or window menu.
    inherited: [bool; 256],
    dummy: bool,
    active: bool,
    events: VecDeque<Event>,
    lost_focus: bool,
    overflow: bool,
    unsupported: bool,
    cancelled: bool,
}

#[cfg(any(windows, test))]
impl Capture {
    fn new(down: [bool; 256]) -> Self {
        let mut inherited = [false; 256];
        for vk in [0x5b, 0x5c, 0xa4, 0xa5] {
            inherited[vk] = down[vk];
        }
        let mut capture = Self {
            down,
            owned: [false; 256],
            inherited,
            dummy: false,
            active: true,
            events: VecDeque::with_capacity(CAPACITY),
            lost_focus: false,
            overflow: false,
            unsupported: false,
            cancelled: false,
        };
        capture
            .events
            .push_back(Event::ModifiersChanged(capture.modifiers()));
        capture
    }

    fn modifiers(&self) -> Modifiers {
        let ctrl = self.down[0xa2] || self.down[0xa3];
        Modifiers {
            ctrl,
            command: ctrl,
            shift: self.down[0xa0] || self.down[0xa1],
            alt: self.down[0xa4] || self.down[0xa5],
            mac_cmd: self.down[0x5b] || self.down[0x5c],
        }
    }

    fn system_modifiers(&self) -> Modifiers {
        let mut visible = self.down;
        for (key, owned) in self.owned.iter().enumerate() {
            visible[key] &= !owned;
        }
        let ctrl = visible[0xa2] || visible[0xa3];
        Modifiers {
            ctrl,
            command: ctrl,
            alt: visible[0xa4] || visible[0xa5],
            shift: visible[0xa0] || visible[0xa1],
            mac_cmd: visible[0x5b] || visible[0x5c],
        }
    }

    fn stop(&mut self) {
        self.active = false;
        self.events
            .retain(|event| matches!(event, Event::Key { pressed: false, .. }));
    }

    fn focus(&mut self, foreground: bool) {
        if self.active && !foreground {
            self.stop();
            self.lost_focus = true;
            self.events.clear();
        }
    }

    fn draining(&self) -> bool {
        self.owned.iter().any(|down| *down)
    }

    fn key(&mut self, vk: u32, pressed: bool, foreground: bool) -> bool {
        self.focus(foreground);
        let Some(previous) = self.down.get_mut(vk as usize) else {
            return false;
        };
        let repeat = pressed && *previous;
        *previous = pressed;
        let owned = self.owned[vk as usize];
        if !pressed && self.inherited[vk as usize] {
            // A modifier pressed before capture reaches Windows alone on release,
            // which would open Start or the window menu. Ask the hook to cancel it.
            self.inherited[vk as usize] = false;
            self.dummy = true;
        }
        if !self.active || !foreground {
            if owned && !pressed {
                self.owned[vk as usize] = false;
                self.queue(vk, false);
            }
            // Drain only presses we intercepted. New keys in another app are
            // never captured, even while a previously held Win key releases.
            return owned;
        }
        if repeat {
            // Also suppress repeats of the Enter that opened the recorder.
            // Its original release still belongs to Windows.
            return true;
        }
        if pressed {
            self.owned[vk as usize] = true;
        } else {
            self.owned[vk as usize] = false;
        }
        if pressed && vk == 0x1b && self.modifiers().is_none() {
            // Escape must release capture even if the GUI is busy rendering.
            // Preserve ownership of its release so it cannot escape into a menu.
            self.cancelled = true;
            self.stop();
            return true;
        }
        self.queue(vk, pressed);
        pressed || owned
    }

    fn queue(&mut self, vk: u32, pressed: bool) {
        let key = match vk {
            0xa0 => Some(Key::ShiftLeft),
            0xa1 => Some(Key::ShiftRight),
            0xa2 => Some(Key::ControlLeft),
            0xa3 => Some(Key::ControlRight),
            0xa4 => Some(Key::AltLeft),
            0xa5 => Some(Key::AltRight),
            0x5b => Some(Key::SuperLeft),
            0x5c => Some(Key::SuperRight),
            _ => crate::native_keys::physical_key(vk),
        };
        let Some(key) = key else {
            self.unsupported |= pressed;
            return;
        };
        if self.events.len() >= CAPACITY {
            self.events.clear();
            self.overflow = true;
            self.stop();
            return;
        }
        self.events.push_back(Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: self.modifiers(),
        });
    }

    fn drain(&mut self) -> Input {
        Input {
            modifiers: Some(self.modifiers()),
            events: self.events.drain(..).collect(),
            lost_focus: std::mem::take(&mut self.lost_focus),
            overflow: std::mem::take(&mut self.overflow),
            unsupported: std::mem::take(&mut self.unsupported),
            cancelled: std::mem::take(&mut self.cancelled),
            restore_modifiers: None,
        }
    }
}

#[cfg(any(windows, test))]
fn sided_key(vk: u32, scan: u32, extended: bool) -> u32 {
    match vk {
        0x10 => {
            if scan == 0x36 {
                0xa1
            } else {
                0xa0
            }
        }
        0x11 => {
            if extended {
                0xa3
            } else {
                0xa2
            }
        }
        0x12 => {
            if extended {
                0xa5
            } else {
                0xa4
            }
        }
        _ => vk,
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::{
        cell::RefCell,
        ptr,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU32, Ordering},
            mpsc,
        },
        thread::JoinHandle,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::*,
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
            },
            WindowsAndMessaging::*,
        },
    };

    /// Marks the harmless key tap that cancels Start/menu activation so the hook
    /// never records its own injected input.
    const DUMMY_MARKER: usize = 0x464f_4c58;
    fn owns_foreground(owner: HWND) -> bool {
        let foreground = unsafe { GetForegroundWindow() };
        !foreground.is_null()
            && (foreground == owner || unsafe { GetAncestor(foreground, GA_ROOTOWNER) } == owner)
    }

    fn send_dummy_tap() {
        let key = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0xe8,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: DUMMY_MARKER,
                },
            },
        };
        let inputs = [key(0), key(KEYEVENTF_KEYUP)];
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            );
        }
    }

    type Shared = Arc<Mutex<Capture>>;
    struct Session {
        owner: HWND,
        capture: Shared,
    }
    thread_local! {
        static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    }

    #[derive(Default)]
    pub(crate) struct Recorder {
        owner: isize,
        capture: Option<Shared>,
        thread: Option<JoinHandle<()>>,
        thread_id: Arc<AtomicU32>,
        restore_modifiers: Option<Modifiers>,
    }
    impl Recorder {
        /// True while the hook thread owns focus tracking for this recording.
        pub fn running(&self) -> bool {
            self.thread.is_some()
                && self.capture.as_ref().is_some_and(|capture| {
                    capture
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .active
                })
        }

        pub fn attach(&mut self, owner: isize) {
            self.owner = owner;
        }

        pub fn start(&mut self) -> Result<(), String> {
            // No native owner in headless egui tests or non-native viewports.
            if self.owner == 0 {
                return Ok(());
            }
            self.cleanup();
            if self.thread.is_some() {
                return Err("Release the recorded keys before starting another recording.".into());
            }
            if !owns_foreground(self.owner as HWND) {
                return Err("Focus Foldex before recording a shortcut.".into());
            }
            let mut down = [false; 256];
            for (vk, held) in down.iter_mut().enumerate() {
                *held = unsafe { GetAsyncKeyState(vk as i32) } < 0;
            }
            let capture = Arc::new(Mutex::new(Capture::new(down)));
            let (ready, started) = mpsc::sync_channel(1);
            let owner = self.owner;
            let state = capture.clone();
            let thread_id = self.thread_id.clone();
            self.thread_id.store(0, Ordering::Release);
            let thread = std::thread::Builder::new()
                .name("foldex-shortcut-recorder".into())
                .stack_size(256 * 1024)
                .spawn(move || run(owner, state, thread_id, ready))
                .map_err(|error| format!("Could not start keyboard capture: {error}"))?;
            self.capture = Some(capture);
            self.thread = Some(thread);
            match started.recv_timeout(Duration::from_secs(2)) {
                Ok(Ok(())) => Ok(()),
                result => {
                    self.stop();
                    match result {
                        Ok(Err(code)) => Err(format!(
                            "Windows could not start keyboard capture (error {code})."
                        )),
                        _ => Err("Keyboard capture did not start. Try recording again.".into()),
                    }
                }
            }
        }

        pub fn stop(&mut self) {
            if let Some(capture) = &self.capture {
                let mut capture = capture.lock().unwrap_or_else(|error| error.into_inner());
                self.restore_modifiers = Some(capture.system_modifiers());
                capture.stop();
            }
            self.cleanup();
        }

        pub fn drain(&mut self) -> Input {
            let mut input = self
                .capture
                .as_ref()
                .map_or_else(Input::default, |capture| {
                    let mut capture = capture.lock().unwrap_or_else(|error| error.into_inner());
                    capture.focus(owns_foreground(self.owner as HWND));
                    capture.drain()
                });
            input.restore_modifiers = self.restore_modifiers.take();
            self.cleanup();
            input
        }

        fn cleanup(&mut self) {
            let done = self.capture.as_ref().is_some_and(|capture| {
                let capture = capture.lock().unwrap_or_else(|error| error.into_inner());
                !capture.active && !capture.draining()
            });
            if done {
                self.finish();
            }
        }

        fn finish(&mut self) {
            if let Some(thread) = self.thread.take() {
                let id = self.thread_id.load(Ordering::Acquire);
                if id != 0 {
                    unsafe {
                        PostThreadMessageW(id, WM_QUIT, 0, 0);
                    }
                }
                let _ = thread.join();
            }
            self.capture = None;
        }
    }
    impl Drop for Recorder {
        fn drop(&mut self) {
            // Shutdown must not keep the application alive waiting for a key.
            if let Some(capture) = &self.capture {
                capture
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .stop();
            }
            self.finish();
        }
    }

    fn run(
        owner: isize,
        capture: Shared,
        thread_id: Arc<AtomicU32>,
        ready: mpsc::SyncSender<Result<(), u32>>,
    ) {
        let mut message: MSG = unsafe { std::mem::zeroed() };
        // Create this thread's message queue before publishing its ID. Keeping
        // the hook on its own pump prevents a slow paint from losing capture.
        unsafe {
            PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }
        thread_id.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
        if !capture
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
        {
            return;
        }
        SESSION.with(|slot| {
            *slot.borrow_mut() = Some(Session {
                owner: owner as HWND,
                capture: capture.clone(),
            })
        });
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(callback),
                GetModuleHandleW(ptr::null()),
                0,
            )
        };
        if hook.is_null() {
            let _ = ready.send(Err(unsafe { GetLastError() }));
            SESSION.with(|slot| *slot.borrow_mut() = None);
            return;
        }
        if ready.send(Ok(())).is_ok() {
            loop {
                let done = {
                    let capture = capture.lock().unwrap_or_else(|error| error.into_inner());
                    !capture.active && !capture.draining()
                };
                let got = if done {
                    -2
                } else {
                    unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) }
                };
                if got <= 0 {
                    break;
                }
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        unsafe {
            UnhookWindowsHookEx(hook);
        }
        SESSION.with(|slot| *slot.borrow_mut() = None);
    }

    unsafe extern "system" fn callback(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32
            && data != 0
            && matches!(
                message as u32,
                WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
            )
        {
            // Windows owns this structure for the duration of this callback.
            let key = unsafe { &*(data as *const KBDLLHOOKSTRUCT) };
            if key.dwExtraInfo == DUMMY_MARKER {
                return unsafe { CallNextHookEx(ptr::null_mut(), code, message, data) };
            }
            let mut dummy = false;
            let consumed = SESSION.with(|slot| {
                let slot = slot.borrow();
                let Some(session) = slot.as_ref() else {
                    return false;
                };
                let (consumed, lost_focus, done) = {
                    let mut capture = session
                        .capture
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let foreground = owns_foreground(session.owner);
                    let vk = sided_key(key.vkCode, key.scanCode, key.flags & LLKHF_EXTENDED != 0);
                    let pressed = matches!(message as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                    let consumed = capture.key(vk, pressed, foreground);
                    dummy = std::mem::take(&mut capture.dummy);
                    (
                        consumed,
                        capture.lost_focus,
                        !capture.active && !capture.draining(),
                    )
                };
                // No egui locks, clipboard reads, I/O, or rendering in this hook.
                if consumed || lost_focus {
                    unsafe {
                        PostMessageW(session.owner, WAKE_MESSAGE, 0, 0);
                    }
                }
                if done {
                    unsafe {
                        PostQuitMessage(0);
                    }
                }
                consumed
            });
            if consumed {
                return 1;
            }
            if dummy {
                send_dummy_tap();
            }
        }
        unsafe { CallNextHookEx(ptr::null_mut(), code, message, data) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn native_hook_installs_pumps_and_unhooks_without_intercepting_any_desktop_input() {
            // An invalid foreground owner makes every real keyboard event pass
            // through. Exercise the native hook lifecycle without sending input
            // or focusing another application on the runner's desktop.
            let capture = Arc::new(Mutex::new(Capture::new([false; 256])));
            let thread_id = Arc::new(AtomicU32::new(0));
            let (ready, started) = mpsc::sync_channel(1);
            let state = capture.clone();
            let id = thread_id.clone();
            let worker = std::thread::spawn(move || run(-1, state, id, ready));
            let result = started.recv_timeout(Duration::from_secs(5));
            capture.lock().unwrap().stop();
            let id = thread_id.load(Ordering::Acquire);
            if id != 0 {
                unsafe {
                    PostThreadMessageW(id, WM_QUIT, 0, 0);
                }
            }
            let completed = worker.join();
            assert!(
                matches!(result, Ok(Ok(()))),
                "Native hook startup failed: {result:?}"
            );
            assert!(completed.is_ok());
            assert_ne!(id, 0);
            assert!(!capture.lock().unwrap().draining());
        }

        #[test]
        fn recording_refuses_to_start_when_its_window_does_not_own_focus() {
            let mut recorder = Recorder::default();
            recorder.attach(-1);
            assert!(recorder.start().unwrap_err().contains("Focus Foldex"));
            assert!(recorder.thread.is_none() && recorder.capture.is_none());
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Input;
    #[derive(Default)]
    pub(crate) struct Recorder;
    impl Recorder {
        pub fn running(&self) -> bool {
            false
        }
        pub fn attach(&mut self, _: isize) {}
        pub fn start(&mut self) -> Result<(), String> {
            Input::default()
        }
    }
}

pub(super) use imp::Recorder;
#[cfg(test)]
mod tests {
    use super::*;

    fn press(capture: &mut Capture, vk: u32) {
        assert!(capture.key(vk, true, true));
    }
    fn release(capture: &mut Capture, vk: u32) {
        assert!(capture.key(vk, false, true));
    }

    #[test]
    fn windows_and_alt_are_suppressed_on_both_edges_and_full_chord_is_preserved() {
        for win in [0x5b, 0x5c] {
            let mut capture = Capture::new([false; 256]);
            for key in [win, 0xa2, 0xa0, 0xa4, 0x41] {
                press(&mut capture, key);
            }
            let batch = capture.drain();
            assert!(batch.events.iter().any(|event| matches!(event,
                Event::Key { key: Key::A, pressed: true, modifiers, .. }
                    if modifiers.ctrl && modifiers.alt && modifiers.shift && modifiers.mac_cmd)));
            for key in [0x41, 0xa4, 0xa0, 0xa2, win] {
                release(&mut capture, key);
            }
            assert_eq!(capture.modifiers(), Modifiers::NONE);
            assert!(!capture.draining());
            assert_eq!(capture.system_modifiers(), Modifiers::NONE);
        }
    }

    #[test]
    fn each_modifier_tracks_both_sides_until_the_last_release() {
        for (left, right, bit) in [
            (0xa2, 0xa3, 0),
            (0xa4, 0xa5, 1),
            (0xa0, 0xa1, 2),
            (0x5b, 0x5c, 3),
        ] {
            let held = |m: Modifiers| [m.ctrl, m.alt, m.shift, m.mac_cmd][bit];
            let mut capture = Capture::new([false; 256]);
            press(&mut capture, left);
            press(&mut capture, right);
            release(&mut capture, left);
            assert!(held(capture.modifiers()));
            release(&mut capture, right);
            assert!(!held(capture.modifiers()));
        }
    }

    #[test]
    fn modifier_order_does_not_change_the_recorded_combination() {
        fn permutations(keys: &mut [u32], at: usize, out: &mut Vec<Vec<u32>>) {
            if at == keys.len() {
                out.push(keys.to_vec());
                return;
            }
            for next in at..keys.len() {
                keys.swap(at, next);
                permutations(keys, at + 1, out);
                keys.swap(at, next);
            }
        }
        let mut orders = vec![];
        permutations(&mut [0x5b, 0xa2, 0xa0, 0xa4], 0, &mut orders);
        assert_eq!(orders.len(), 24);
        for keys in orders {
            let mut capture = Capture::new([false; 256]);
            for key in keys {
                press(&mut capture, key);
            }
            press(&mut capture, 0x41);
            let Event::Key { modifiers, .. } = capture.drain().events.pop().unwrap() else {
                panic!();
            };
            assert!(modifiers.ctrl && modifiers.alt && modifiers.shift && modifiers.mac_cmd);
        }
    }

    #[test]
    fn focus_loss_cancels_capture_and_never_collects_other_application_keys() {
        let mut capture = Capture::new([false; 256]);
        press(&mut capture, 0x5b);
        assert!(!capture.key(0x41, true, false));
        assert!(!capture.key(0x41, false, false));
        let batch = capture.drain();
        assert!(batch.lost_focus && batch.events.is_empty());
        assert!(capture.key(0x5b, false, false));
        assert!(!capture.draining());
        assert!(
            !capture.key(0x42, true, true),
            "Refocusing must not silently restart capture"
        );
        assert!(!capture.key(0x42, false, true));
    }

    #[test]
    fn cancellation_drains_owned_repeats_and_releases_then_restores_normal_input() {
        let mut capture = Capture::new([false; 256]);
        press(&mut capture, 0x5b);
        press(&mut capture, 0xa2);
        capture.stop();
        assert_eq!(capture.system_modifiers(), Modifiers::NONE);
        assert!(capture.key(0x5b, true, false));
        assert!(!capture.key(0x41, true, false));
        assert!(!capture.key(0x41, false, false));
        assert!(capture.key(0xa2, false, false));
        assert!(capture.key(0x5b, false, false));
        assert!(!capture.draining());
        assert!(!capture.key(0x5b, true, true));
        assert!(!capture.key(0x5b, false, true));
    }

    #[test]
    fn keys_held_before_arming_do_not_repeat_or_leave_windows_modifiers_stuck() {
        let mut held = [false; 256];
        held[0x0d] = true;
        held[0xa2] = true;
        let mut capture = Capture::new(held);
        for _ in 0..1000 {
            assert!(capture.key(0x0d, true, true));
        }
        assert_eq!(capture.events.len(), 1);
        assert!(!capture.key(0x0d, false, true));
        press(&mut capture, 0xa0);
        assert!(capture.modifiers().ctrl && capture.modifiers().shift);
        capture.stop();
        assert!(capture.system_modifiers().ctrl);
        assert!(!capture.system_modifiers().shift);
        assert!(!capture.key(0xa2, false, true));
        assert!(capture.key(0xa0, false, true));
        assert_eq!(capture.system_modifiers(), Modifiers::NONE);
    }

    #[test]
    fn repeated_keys_do_not_repeat_the_recorded_chord_or_overflow_the_queue() {
        let mut capture = Capture::new([false; 256]);
        press(&mut capture, 0x41);
        for _ in 0..2000 {
            press(&mut capture, 0x41);
        }
        release(&mut capture, 0x41);
        assert_eq!(capture.events.len(), 3);
        assert!(!capture.overflow);
        press(&mut capture, 0x41);
        assert_eq!(
            capture.events.len(),
            4,
            "A separate physical press must remain recordable"
        );
    }

    #[test]
    fn overflow_fails_closed_and_still_drains_every_intercepted_press() {
        let mut capture = Capture::new([false; 256]);
        press(&mut capture, 0x5b);
        while capture.active {
            assert!(capture.key(0x41, true, true));
            if capture.active {
                assert!(capture.key(0x41, false, true));
            }
        }
        let batch = capture.drain();
        assert!(batch.overflow && batch.events.is_empty());
        assert!(capture.key(0x5b, false, false));
        if capture.owned[0x41] {
            assert!(capture.key(0x41, false, false));
        }
        assert!(!capture.draining());
    }

    #[test]
    fn unsupported_keys_are_reported_and_do_not_activate_os_actions() {
        let mut capture = Capture::new([false; 256]);
        press(&mut capture, 0xaf); // volume up is not a supported binding key
        release(&mut capture, 0xaf);
        assert!(capture.drain().unsupported);
        assert!(!capture.drain().unsupported);
        assert!(!capture.key(256, true, true));
    }

    #[test]
    fn generic_windows_modifier_codes_are_normalized_to_the_correct_side() {
        for (vk, scan, extended, expected) in [
            (0x10, 0x2a, false, 0xa0),
            (0x10, 0x36, false, 0xa1),
            (0x11, 0x1d, false, 0xa2),
            (0x11, 0x1d, true, 0xa3),
            (0x12, 0x38, false, 0xa4),
            (0x12, 0x38, true, 0xa5),
            (0x5b, 0x5b, true, 0x5b),
        ] {
            assert_eq!(sided_key(vk, scan, extended), expected);
        }
    }

    #[test]
    fn escape_releases_capture_immediately_without_waiting_for_a_gui_frame() {
        let mut capture = Capture::new([false; 256]);
        press(&mut capture, 0x41);
        press(&mut capture, 0x1b);
        assert!(!capture.active);
        assert!(
            !capture.key(0x5b, true, true),
            "New presses must immediately return to Windows"
        );
        assert!(!capture.key(0x5b, false, true));
        assert!(capture.key(0x41, false, true));
        assert!(capture.key(0x1b, false, true));
        let batch = capture.drain();
        assert!(batch.cancelled);
        assert!(
            batch
                .events
                .iter()
                .all(|event| matches!(event, Event::Key { pressed: false, .. }))
        );
        assert!(!capture.draining());
    }

    #[test]
    fn modified_escape_is_recorded_without_releasing_capture() {
        for modifier in [0xa2, 0xa0, 0xa4, 0x5b] {
            let mut capture = Capture::new([false; 256]);
            press(&mut capture, modifier);
            press(&mut capture, 0x1b);
            assert!(capture.active && !capture.cancelled);
            release(&mut capture, 0x1b);
            release(&mut capture, modifier);
        }
    }
}
